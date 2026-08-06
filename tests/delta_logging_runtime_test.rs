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

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::{Duration as ChronoDuration, Utc};
use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, DaemonConfig, GovernorConfig,
    PricingConfig, SprintConfig,
};
use claude_governor::governor::run_governor_cycle;
use claude_governor::poller::{UsageData, UsagePoller};
use claude_governor::schedule::Promotion;
use claude_governor::snapshot_fixtures::snapshot_pair_5h;
use claude_governor::state::PrevUsageSnapshot;
use tempfile::TempDir;

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
/// snapshot's own `taken_at` is deliberately not used — the cycle stamps its
/// snapshots with `Utc::now()`, so the fixture timestamps could not survive
/// anyway.
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
        timestamp: now,
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
/// count, so a host with no `~/.claude` data still reaches them.
fn drive_cycle(poller: &mut FakePoller, state_path: &std::path::Path) -> anyhow::Result<()> {
    let alert_config = AlertConfig::default();
    let composite_risk_config = CompositeRiskConfig::default();
    let cone_scaling_config = ConeScalingConfig::default();
    let pricing_config = minimal_pricing_config();
    let agents: HashMap<String, AgentConfig> = HashMap::new();
    let promotions: Vec<Promotion> = Vec::new();

    run_governor_cycle(
        poller,
        state_path,
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
