//! Runtime proof that the window-delta log lines actually reach the log.
//!
//! `format_window_deltas` and `format_no_previous_snapshot` are pure string
//! builders with their own unit tests, so those tests pass whether or not the
//! `log::info!` calls that emit them still exist in `run_governor_cycle`. This
//! binary closes that gap: it drives two real cycles through
//! `claude_governor::governor::run_governor_cycle` and asserts on the records a
//! captured logger received.
//!
//! It lives in its own integration-test binary for two reasons:
//!
//! - it must own the process-global `log` logger to see the INFO records, and
//!   the in-crate `#[cfg(test)]` modules share one binary (and one logger) with
//!   every other test in the crate — the same reasoning as
//!   `tests/heartbeat_orphan_cleanup_test.rs`;
//! - `MockPoller` is `#[cfg(test)]` and therefore unreachable from `tests/`, so
//!   the harness defines its own `UsagePoller` and needs no credentials or
//!   network. The existing two-cycle test in `src/governor.rs`
//!   (`test_first_poll_and_second_poll_complete_flow`) uses the *real* `Poller`,
//!   whose poll fails without credentials — which silently skips both log lines.
//!
//! Scope: presence and ordering of the two lines. Verifying the numbers they
//! carry against the fixture inputs is a separate bead.
//!
//! The reconcile summary line is pinned here too (claudego-6f000c99): CLAUDE.md
//! §3 documents triaging the daemon with `journalctl --user -u
//! claude-governor -n 30 | grep reconcile`, so every cycle — every decision,
//! live or dry-run — must emit at least one line containing "reconcile" that
//! names the capacity decision and the per-pool start/stop actions. The
//! reconcile scenarios seed the act half's own input state and drive
//! [`run_act_cycle`] directly rather than going through [`run_governor_cycle`]:
//! the observe half recomputes the capacity forecast from collector data a test
//! does not have, and ADR-002 turns a missing forecast into a hold, which would
//! make every decision NoChange and the contract untestable.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{Duration as ChronoDuration, Utc};
use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, DaemonConfig, GovernorConfig,
    PricingConfig, SprintConfig,
};
use claude_governor::governor::{run_act_cycle, run_governor_cycle, CyclePaths, ScalingDecision};
use claude_governor::poller::{UsageData, UsagePoller};
use claude_governor::schedule::Promotion;
use claude_governor::snapshot_fixtures::snapshot_pair_5h;
use claude_governor::state::{self, PrevUsageSnapshot};
use tempfile::TempDir;

struct ActEnvGuard {
    path: String,
    decisions: Option<String>,
    ledger_logs: Option<String>,
}

impl Drop for ActEnvGuard {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.path);
        match &self.decisions {
            Some(value) => std::env::set_var("CGOV_DECISIONS_PATH", value),
            None => std::env::remove_var("CGOV_DECISIONS_PATH"),
        }
        match &self.ledger_logs {
            Some(value) => std::env::set_var("CGOV_LEDGER_LOGS_DIR", value),
            None => std::env::remove_var("CGOV_LEDGER_LOGS_DIR"),
        }
    }
}

// ---------------------------------------------------------------------------
// Log capture
// ---------------------------------------------------------------------------

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

/// Serializes whole test bodies against the shared captured-log buffer and the
/// process environment. Tests in one binary share a process and run in
/// threads, so records from two tests can interleave in the buffer; the
/// reconcile test additionally swaps PATH / CGOV_DECISIONS_PATH /
/// CGOV_LEDGER_LOGS_DIR. Both tests must hold this for their whole body.
static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

fn init_logger() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&TEST_LOGGER).expect("this binary owns the global logger");
        // INFO is the level both delta lines are emitted at; anything stricter
        // would make this test pass for the wrong reason.
        log::set_max_level(log::LevelFilter::Info);
    });
}

/// Number of records captured so far — used to slice the log per cycle so an
/// assertion about cycle 2 cannot be satisfied by a record from cycle 1.
fn log_len() -> usize {
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .len()
}

/// Print every record captured in `from..to`, verbatim, under a banner.
///
/// The assertions below only check that a line is present; this exists so the
/// exact rendered text can be read off a real run (`cargo test -- --nocapture`)
/// and pasted into a write-up rather than reconstructed by hand from the
/// format strings. It is inert under the default harness capture.
fn dump_cycle(label: &str, from: usize, to: usize) {
    let logs = TEST_LOGS.get_or_init(|| Mutex::new(Vec::new()));
    let logs = logs.lock().unwrap();
    println!("===== BEGIN {label} =====");
    for (level, msg) in logs.iter().take(to).skip(from) {
        println!("[{level}] {msg}");
    }
    println!("===== END {label} =====");
}

/// Records captured at or after `from`, filtered to those containing `pattern`.
fn logs_containing_since(from: usize, pattern: &str) -> Vec<(log::Level, String)> {
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .iter()
        .skip(from)
        .filter(|(_, msg)| msg.contains(pattern))
        .cloned()
        .collect()
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write test executable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make test executable runnable");
}

fn fake_act_environment(dir: &TempDir, sessions: &str) -> (ActEnvGuard, PathBuf) {
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("create fake-bin directory");
    let sessions_path = dir.path().join("sessions");
    std::fs::write(&sessions_path, sessions).expect("seed fake tmux sessions");
    let decisions_path = dir.path().join("decisions.jsonl");
    let ledger_logs = dir.path().join("ledger-logs");
    std::fs::create_dir(&ledger_logs).expect("create fake ledger log directory");

    let tmux = format!(
        "#!/bin/sh\ncase \"$1\" in\n  list-sessions) cat '{sessions}';;\n  *) exit 0;;\nesac\n",
        sessions = sessions_path.display()
    );
    write_executable(&bin.join("tmux"), &tmux);

    let old_path = std::env::var("PATH").unwrap_or_default();
    let old_decisions = std::env::var("CGOV_DECISIONS_PATH").ok();
    let old_ledger_logs = std::env::var("CGOV_LEDGER_LOGS_DIR").ok();
    std::env::set_var("PATH", format!("{}:{}", bin.display(), old_path));
    std::env::set_var("CGOV_DECISIONS_PATH", &decisions_path);
    std::env::set_var("CGOV_LEDGER_LOGS_DIR", &ledger_logs);

    (
        ActEnvGuard {
            path: old_path,
            decisions: old_decisions,
            ledger_logs: old_ledger_logs,
        },
        sessions_path,
    )
}

fn act_agent(name: &str, heartbeat_dir: &Path) -> AgentConfig {
    serde_json::from_value(serde_json::json!({
        "launch_cmd": "launch-stub",
        "session_pattern": format!("{name}-*"),
        "heartbeat_dir": heartbeat_dir,
        "min_workers": 0,
        "max_workers": 4,
        "subscription": false,
    }))
    .expect("valid act-cycle test agent")
}

fn act_config(agents: &HashMap<String, AgentConfig>) -> GovernorConfig {
    GovernorConfig {
        pricing: PricingConfig {
            models: HashMap::new(),
        },
        sprint: SprintConfig::default(),
        daemon: DaemonConfig::default(),
        alerts: AlertConfig {
            enabled: false,
            ..AlertConfig::default()
        },
        composite_risk: CompositeRiskConfig::default(),
        cone_scaling: ConeScalingConfig::default(),
        agents: agents.clone(),
        credentials_path: None,
    }
}

fn forecast_for_target(target: u32) -> state::CapacityForecast {
    let window = state::WindowForecast {
        current_utilization: 10.0,
        hours_remaining: 100.0,
        safe_worker_count: Some(target),
        safe_worker_count_p75: Some(target),
        binding: true,
        ..state::WindowForecast::default()
    };
    state::CapacityForecast {
        five_hour: window.clone(),
        seven_day: window.clone(),
        weekly_scoped: window,
        binding_window: "five_hour".to_string(),
        ..state::CapacityForecast::default()
    }
}

// ---------------------------------------------------------------------------
// Credential-free poller
// ---------------------------------------------------------------------------

/// Returns a scripted sequence of readings, one per cycle.
///
/// `UsagePoller` is the seam `run_governor_cycle` polls through, so this reaches
/// the real cycle body — including the delta branch — with no credentials, no
/// network, and no production change.
struct FakePoller {
    readings: Vec<UsageData>,
    polls: usize,
}

impl FakePoller {
    fn new(readings: Vec<UsageData>) -> Self {
        Self { readings, polls: 0 }
    }
}

impl UsagePoller for FakePoller {
    fn poll_usage(&mut self) -> anyhow::Result<UsageData> {
        let reading = self.readings.get(self.polls).cloned().ok_or_else(|| {
            anyhow::anyhow!("FakePoller ran out of readings at poll {}", self.polls)
        })?;
        self.polls += 1;
        Ok(reading)
    }
}

/// Turn a fixture snapshot into the `UsageData` shape a poll returns.
///
/// Only the three window percentages carry over; `resets_at` is set to a real
/// future instant because the cycle parses those strings downstream. The
/// snapshot's own `taken_at` is also carried through as the poll timestamp, so
/// the rendered delta interval describes the fixture readings rather than the
/// time spent running this test.
fn usage_data_from(snapshot: &PrevUsageSnapshot) -> UsageData {
    let now = Utc::now();
    let five_hour_reset = now + ChronoDuration::hours(4);
    let seven_day_reset = now + ChronoDuration::hours(120);

    UsageData {
        five_hour_utilization: snapshot.five_hour_pct,
        five_hour_resets_at: five_hour_reset.to_rfc3339(),
        five_hour_hours_remaining: 4.0,
        seven_day_utilization: snapshot.seven_day_pct,
        seven_day_resets_at: seven_day_reset.to_rfc3339(),
        seven_day_hours_remaining: 120.0,
        weekly_scoped_utilization: snapshot.weekly_scoped_pct,
        weekly_scoped_resets_at: seven_day_reset.to_rfc3339(),
        weekly_scoped_hours_remaining: 120.0,
        // Held constant across both polls: a change here triggers the
        // model-rotation EMA reset, which is unrelated noise for this test.
        weekly_scoped_model: None,
        // Empty, so `scoped_weekly()` is None and the cycle falls back to
        // `weekly_scoped_utilization` above.
        limits: vec![],
        timestamp: snapshot.taken_at,
        stale: false,
    }
}

// ---------------------------------------------------------------------------
// Cycle wiring
// ---------------------------------------------------------------------------

fn minimal_pricing_config() -> GovernorConfig {
    GovernorConfig {
        pricing: PricingConfig {
            models: HashMap::new(),
        },
        sprint: SprintConfig::default(),
        daemon: DaemonConfig::default(),
        alerts: AlertConfig::default(),
        composite_risk: CompositeRiskConfig::default(),
        cone_scaling: ConeScalingConfig::default(),
        agents: HashMap::new(),
        credentials_path: None,
    }
}

/// Drive one cycle against `poller`, with everything else at defaults.
///
/// `dry_run = true` keeps the cycle off the tmux scaling path; the delta log
/// sites run before the collector pass, the fleet-aggregate read and the worker
/// count, so a host with no `~/.claude` data still reaches them. The cycle's
/// `~`-rooted paths (collector state, calibration log) are rooted at
/// `state_path`'s directory — the test's `TempDir` — so the cycle never reads
/// or writes the host's live `~/.needle` state.
fn drive_cycle(poller: &mut FakePoller, state_path: &std::path::Path) -> anyhow::Result<()> {
    let cycle_paths = CyclePaths::under(
        state_path
            .parent()
            .expect("state_path must live in a directory"),
    );
    let alert_config = AlertConfig::default();
    let composite_risk_config = CompositeRiskConfig::default();
    let cone_scaling_config = ConeScalingConfig::default();
    let pricing_config = minimal_pricing_config();
    let agents: HashMap<String, AgentConfig> = HashMap::new();
    let promotions: Vec<Promotion> = Vec::new();

    run_governor_cycle(
        poller,
        state_path,
        &cycle_paths,
        true, // dry_run
        60,   // loop_interval
        2.0,  // hysteresis_band
        3,    // max_up_per_cycle
        2,    // max_down_per_cycle
        90.0, // target_ceiling
        &alert_config,
        &agents,
        0, // pre_scale_minutes (disabled)
        &promotions,
        &composite_risk_config,
        &cone_scaling_config,
        &pricing_config,
    )
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// Two cycles, no credentials: cycle 1 must log the no-baseline line and cycle 2
/// the window-deltas line.
///
/// This is one test rather than two because the captured log is process-global;
/// splitting it would let two tests interleave records in the shared buffer.
#[test]
fn two_cycles_emit_the_delta_log_lines() {
    let _env_guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init_logger();

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let state_path = temp_dir.path().join("governor-state.json");

    // A realistic prev/curr reading pair: 5h 12.5% → 18.2%, 7d 45.2% → 46.8%,
    // 7ds 38.7% → 40.3%.
    let (first_reading, second_reading) = snapshot_pair_5h();
    let mut poller = FakePoller::new(vec![
        usage_data_from(&first_reading),
        usage_data_from(&second_reading),
    ]);

    // --- Cycle 1: no state file on disk, so no baseline exists -------------
    let cycle1_start = log_len();
    drive_cycle(&mut poller, &state_path).expect("cycle 1 should complete");
    let cycle1_end = log_len();
    dump_cycle("CYCLE 1", cycle1_start, cycle1_end);

    let no_baseline = logs_containing_since(cycle1_start, "no previous snapshot");
    assert_eq!(
        no_baseline.len(),
        1,
        "cycle 1 should log the no-baseline line exactly once; captured records were: {:?}",
        &TEST_LOGS.get().unwrap().lock().unwrap()[cycle1_start..cycle1_end]
    );
    assert_eq!(
        no_baseline[0].0,
        log::Level::Info,
        "the no-baseline line must be INFO, not a lower level an operator would not see by default"
    );
    assert!(
        logs_containing_since(cycle1_start, "window deltas:").is_empty(),
        "cycle 1 has no baseline, so it must not claim a delta"
    );

    // --- Cycle 2: cycle 1's reading rotates into previous ------------------
    drive_cycle(&mut poller, &state_path).expect("cycle 2 should complete");
    dump_cycle("CYCLE 2", cycle1_end, log_len());

    let deltas = logs_containing_since(cycle1_end, "window deltas:");
    assert_eq!(
        deltas.len(),
        1,
        "cycle 2 should log the window-deltas line exactly once; captured records were: {:?}",
        &TEST_LOGS.get().unwrap().lock().unwrap()[cycle1_end..]
    );
    assert_eq!(
        deltas[0].0,
        log::Level::Info,
        "the window-deltas line must be INFO, not a lower level an operator would not see by default"
    );
    assert!(
        deltas[0].1.contains("Δt=5h0m"),
        "Delta-t must use the five-hour fixture interval, not the cycle wall-clock gap: {}",
        deltas[0].1
    );
    assert!(
        deltas[0]
            .1
            .contains("2026-03-18T10:00:00.000Z → 2026-03-18T15:00:00.000Z"),
        "delta timestamps must come from the fixture readings: {}",
        deltas[0].1
    );
    assert!(
        logs_containing_since(cycle1_end, "no previous snapshot").is_empty(),
        "cycle 2 has a baseline, so it must not report one missing"
    );

    // Both readings were consumed — proof the cycles polled rather than
    // short-circuiting somewhere before the poll.
    assert_eq!(
        poller.polls, 2,
        "each cycle should have polled exactly once"
    );
}

/// The operator's `journalctl ... | grep reconcile` workflow must expose the
/// decision and the pool-level moves for every dry-run act cycle. Use the real
/// act path with a fake tmux census so the two cycles exercise opposite moves:
/// a planned start followed by a planned stop.
#[test]
fn dry_run_reconcile_lines_name_decision_and_pool_actions() {
    let _env_guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init_logger();

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let (_act_env, sessions_path) = fake_act_environment(&temp_dir, "pool-1\n");
    let state_path = temp_dir.path().join("governor-state.json");
    let heartbeat_dir = temp_dir.path().join("heartbeats");
    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), act_agent("pool", &heartbeat_dir));
    let config = act_config(&agents);
    let alerts = AlertConfig {
        enabled: false,
        ..AlertConfig::default()
    };

    let mut state = state::GovernorState::new();
    state.capacity_forecast = forecast_for_target(3);
    state::save_state(&state, &state_path).expect("seed scale-up state");

    let cycle1_start = log_len();
    let decision = run_act_cycle(
        &state_path,
        true,
        0.0,
        2,
        2,
        90.0,
        &alerts,
        &agents,
        0,
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &config,
        Utc::now(),
    )
    .expect("scale-up dry-run cycle should complete");
    assert_eq!(decision, ScalingDecision::ScaleUp(2));

    let cycle1_lines = logs_containing_since(cycle1_start, "reconcile:");
    assert_eq!(
        cycle1_lines.len(),
        1,
        "each cycle must emit exactly one reconcile summary: {cycle1_lines:?}"
    );
    assert!(
        cycle1_lines[0].1.contains("decision=ScaleUp(2)"),
        "summary must identify the capacity decision: {}",
        cycle1_lines[0].1
    );
    assert!(
        cycle1_lines[0].1.contains("pools: pool start 2 (1 -> 3)"),
        "summary must identify the planned per-pool start: {}",
        cycle1_lines[0].1
    );

    // Change only the fake census and forecast: the next dry-run cycle sees
    // three workers and a target of one, so its summary must name the stop.
    std::fs::write(&sessions_path, "pool-1\npool-2\npool-3\n")
        .expect("seed fake scale-down census");
    let mut state = state::load_state(&state_path).expect("reload scale-up state");
    state.capacity_forecast = forecast_for_target(1);
    state::save_state(&state, &state_path).expect("seed scale-down state");

    let cycle2_start = log_len();
    let decision = run_act_cycle(
        &state_path,
        true,
        0.0,
        2,
        2,
        90.0,
        &alerts,
        &agents,
        0,
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &config,
        Utc::now(),
    )
    .expect("scale-down dry-run cycle should complete");
    assert_eq!(decision, ScalingDecision::ScaleDown(2));

    let cycle2_lines = logs_containing_since(cycle2_start, "reconcile:");
    assert_eq!(
        cycle2_lines.len(),
        1,
        "each cycle must emit exactly one reconcile summary: {cycle2_lines:?}"
    );
    assert!(
        cycle2_lines[0].1.contains("decision=ScaleDown(2)"),
        "summary must identify the capacity decision: {}",
        cycle2_lines[0].1
    );
    assert!(
        cycle2_lines[0].1.contains("pools: pool stop 2 (3 -> 1)"),
        "summary must identify the planned per-pool stop: {}",
        cycle2_lines[0].1
    );
}
