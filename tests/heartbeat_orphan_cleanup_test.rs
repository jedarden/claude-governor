//! Tests for orphaned heartbeat file cleanup.
//!
//! Plan spec (Component 6, "Heartbeat file format"): heartbeat files older than 60
//! seconds are stale; stale heartbeats are verified against `tmux list-sessions`, and
//! if the tmux session no longer exists the heartbeat file is removed.
//!
//! This lives in its own integration-test binary so it owns the process-global `log`
//! logger and can assert on the INFO record emitted at removal time — the unit tests in
//! `src/worker.rs` share a binary (and therefore a logger) with the rest of the crate.

use std::sync::{Mutex, OnceLock};

use chrono::{Duration as ChronoDuration, Utc};
use claude_governor::worker::{count_workers, WorkerConfig};
use tempfile::TempDir;

/// Captured (level, message) pairs from the governor's logging.
static TEST_LOGS: OnceLock<Mutex<Vec<(log::Level, String)>>> = OnceLock::new();

struct TestLogger;

impl log::Log for TestLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        let logs = TEST_LOGS.get_or_init(|| Mutex::new(Vec::new()));
        logs.lock()
            .unwrap()
            .push((record.level(), format!("{}", record.args())));
    }
    fn flush(&self) {}
}

static TEST_LOGGER: TestLogger = TestLogger;

fn init_logger() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&TEST_LOGGER).expect("this binary owns the global logger");
        log::set_max_level(log::LevelFilter::Info);
    });
}

fn logs_containing(pattern: &str) -> Vec<(log::Level, String)> {
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, msg)| msg.contains(pattern))
        .cloned()
        .collect()
}

/// Session prefix that cannot collide with a real tmux session on the host,
/// so "the tmux session no longer exists" is guaranteed for these tests.
const TEST_PREFIX: &str = "cgov-orphan-cleanup-test-";

fn test_config(temp: &TempDir) -> WorkerConfig {
    WorkerConfig {
        launch_cmd: "true".to_string(),
        heartbeat_dir: temp.path().join("heartbeats"),
        graceful_timeout_secs: 1,
        session_prefix: TEST_PREFIX.to_string(),
    }
}

/// Write a heartbeat file `age_secs` old for `session`; returns its path.
fn write_heartbeat(config: &WorkerConfig, session: &str, age_secs: i64) -> std::path::PathBuf {
    std::fs::create_dir_all(&config.heartbeat_dir).unwrap();
    let timestamp = Utc::now() - ChronoDuration::seconds(age_secs);
    let heartbeat = serde_json::json!({
        "session": session,
        "timestamp": timestamp.to_rfc3339(),
        "is_idle": true,
        "current_task": null,
        "model": "sonnet",
    });
    let path = config.heartbeat_dir.join(format!("{session}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&heartbeat).unwrap()).unwrap();
    path
}

#[test]
fn stale_heartbeat_for_dead_session_is_deleted_and_logged_at_info() {
    init_logger();

    let temp = TempDir::new().unwrap();
    let config = test_config(&temp);
    let session = format!("{TEST_PREFIX}dead");

    // Stale: comfortably past the 60s threshold. No tmux session by this name exists.
    let path = write_heartbeat(&config, &session, 120);
    assert!(path.exists(), "heartbeat file should exist before cleanup");

    let count = count_workers(&config);

    // The orphaned file is gone from disk...
    assert!(
        !path.exists(),
        "orphaned heartbeat file should have been removed from disk"
    );

    // ...and excluded from the heartbeat count.
    assert_eq!(
        count.heartbeat_count, 0,
        "removed heartbeat must not be counted"
    );

    // The removal is logged at INFO with both the session id and the file path.
    // Filter by this test's unique session id — sibling tests share the capture buffer.
    let removal_logs = logs_containing(&session);
    assert_eq!(
        removal_logs.len(),
        1,
        "expected exactly one removal log, got: {removal_logs:?}"
    );
    let (level, msg) = &removal_logs[0];
    assert_eq!(*level, log::Level::Info, "removal must log at INFO: {msg}");
    assert!(
        msg.contains("removed orphaned heartbeat"),
        "removal log should say what happened: {msg}"
    );
    assert!(
        msg.contains(&session),
        "removal log must name the session: {msg}"
    );
    assert!(
        msg.contains(&path.display().to_string()),
        "removal log must include the file path: {msg}"
    );
}

#[test]
fn fresh_heartbeat_is_retained_and_not_logged_as_removed() {
    init_logger();

    let temp = TempDir::new().unwrap();
    let config = test_config(&temp);
    let session = format!("{TEST_PREFIX}fresh");

    // Well inside the 60s staleness threshold — no tmux verification, no removal.
    let path = write_heartbeat(&config, &session, 5);

    let count = count_workers(&config);

    assert!(path.exists(), "fresh heartbeat file must not be removed");
    assert_eq!(count.heartbeat_count, 1, "fresh heartbeat must be counted");
    assert!(
        logs_containing(&session).is_empty(),
        "no removal should be logged for a fresh heartbeat"
    );
}

#[test]
fn only_the_orphaned_file_is_removed_from_a_mixed_directory() {
    init_logger();

    let temp = TempDir::new().unwrap();
    let config = test_config(&temp);
    let fresh_session = format!("{TEST_PREFIX}mixed-fresh");
    let stale_session = format!("{TEST_PREFIX}mixed-stale");

    let fresh_path = write_heartbeat(&config, &fresh_session, 5);
    let stale_path = write_heartbeat(&config, &stale_session, 300);

    let count = count_workers(&config);

    assert!(fresh_path.exists(), "live worker's heartbeat must survive");
    assert!(!stale_path.exists(), "orphaned heartbeat must be deleted");
    assert_eq!(
        count.heartbeat_count, 1,
        "only the live worker should be counted"
    );

    let removal_logs = logs_containing(&stale_session);
    assert_eq!(
        removal_logs.len(),
        1,
        "expected one removal log for the orphan, got: {removal_logs:?}"
    );
    assert_eq!(removal_logs[0].0, log::Level::Info);
}
