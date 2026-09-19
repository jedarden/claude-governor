//! Integration test for the safe-mode stdout notification emitted by `cgov scale`.
//!
//! When an operator pins the fleet with a manual scale override while the governor is in
//! safe mode, the write still succeeds. Under the documented contract it is an engaged
//! emergency brake — never safe mode alone — that suspends a
//! stored pin. The operator is warned on stdout either way:
//!
//! ```text
//! NOTE: Safe mode remains active; this override applies unless the emergency brake engages
//! ```
//!
//! These tests exercise the real `cgov` binary end-to-end and capture its actual stdout,
//! rather than re-implementing the emission logic in-test — so a regression that removes
//! or reorders the `println!` in `run_scale_command` fails here.
//!
//! Isolation: `cgov` resolves its state file via `dirs::config_dir()` and its log file via
//! `dirs::data_local_dir()`. Both honour the XDG environment variables on Linux (falling
//! back to `$HOME`), so every child process runs with `HOME`, `XDG_CONFIG_HOME`, and
//! `XDG_DATA_HOME` pointed at a fresh `TempDir`. Nothing in the developer's real
//! `~/.config/claude-governor` is read or written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use claude_governor::state::{self, GovernorState, ManualOverride, WorkerState};
use tempfile::TempDir;

/// The exact notification text under test. Kept as a constant so the assertions and the
/// failure messages can never drift apart.
const SAFE_MODE_NOTICE: &str =
    "NOTE: Safe mode remains active; this override applies unless the emergency brake engages";

/// The exact log line written to `governor.log` when a manual scale happens in safe mode.
/// This is the operator's audit trail; it is deliberately *not* printed to stdout.
const SAFE_MODE_LOG_WARNING: &str = "[governor] WARN: manual scale override during safe mode";

/// Path `cgov` will resolve as its state file, given `XDG_CONFIG_HOME` = `root`.
fn state_path_in(root: &Path) -> PathBuf {
    root.join("config")
        .join("claude-governor")
        .join("governor-state.json")
}

/// Path `cgov` will resolve as its log file, given `XDG_DATA_HOME` = `root/data`.
fn log_path_in(root: &Path) -> PathBuf {
    root.join("data")
        .join("claude-governor")
        .join("governor.log")
}

/// Build a minimal but valid governor state with one worker agent, optionally in safe mode.
///
/// The worker range (1..=10) is wide enough that the scale counts used by these tests pass
/// `run_scale_command`'s min/max validation.
fn make_state(safe_mode_active: bool) -> GovernorState {
    let mut state = GovernorState::new();

    state.workers.insert(
        "test-agent".to_string(),
        WorkerState {
            current: 2,
            target: 2,
            min: 1,
            max: 10,
        },
    );

    if safe_mode_active {
        state.safe_mode.active = true;
        state.safe_mode.entered_at = Some(chrono::Utc::now());
        state.safe_mode.trigger = Some("median_error".to_string());
        state.safe_mode.median_error_at_entry = Some(16.0);
        state.safe_mode.predictions_since_entry = 5;
    }

    state
}

/// Write `state` into an isolated temp home and run `cgov <args...>` against it.
///
/// Returns the temp dir (so the caller can inspect the resulting state file) alongside the
/// captured process output.
fn run_cgov(state: &GovernorState, args: &[&str]) -> (TempDir, Output) {
    let temp = TempDir::new().expect("failed to create temp dir");
    let root = temp.path();

    state::save_state(state, &state_path_in(root)).expect("failed to write test state");

    let output = Command::new(env!("CARGO_BIN_EXE_cgov"))
        .args(args)
        // Point every path-resolution mechanism at the temp dir. `HOME` covers the
        // `dirs` fallbacks; the XDG vars cover the primary lookups.
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .expect("failed to run cgov binary");

    (temp, output)
}

/// Decode captured stdout, failing loudly (with stderr) if the command did not succeed.
fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "cgov exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout.clone()).expect("cgov stdout was not valid UTF-8")
}

/// Core case: safe mode active + a manual `cgov scale` ⇒ the notification is printed to stdout.
///
/// Verifies that:
/// 1. The scale itself succeeds and reports the new target (the notification is an addition
///    to the normal output, not a replacement for it).
/// 2. The exact notification line appears on stdout — not stderr, not only the log file.
/// 3. It appears *after* the confirmation line, so the operator reads "what happened" before
///    "what will happen next".
#[test]
fn scale_during_safe_mode_prints_stdout_notification() {
    let (_temp, output) = run_cgov(&make_state(true), &["scale", "4"]);
    let stdout = stdout_of(&output);

    let confirmation = "Manual override stored: fleet target 4";
    assert!(
        stdout.contains(confirmation),
        "expected the scale confirmation on stdout, got:\n{stdout}"
    );

    assert!(
        stdout.contains(SAFE_MODE_NOTICE),
        "expected the safe-mode notification on stdout, got:\n{stdout}"
    );

    let confirmation_at = stdout.find(confirmation).unwrap();
    let notice_at = stdout.find(SAFE_MODE_NOTICE).unwrap();
    assert!(
        notice_at > confirmation_at,
        "the safe-mode notification should follow the scale confirmation, got:\n{stdout}"
    );
}

/// The notification must describe reality: safe mode is still active in the persisted state
/// after the manual scale, and the requested override was actually stored.
///
/// Without this, the first test could pass against a build that prints the notice while
/// silently clearing safe mode — making the message a lie.
#[test]
fn scale_during_safe_mode_keeps_safe_mode_active_and_applies_target() {
    let (temp, output) = run_cgov(&make_state(true), &["scale", "4"]);
    stdout_of(&output);

    let reloaded =
        state::load_state(&state_path_in(temp.path())).expect("failed to reload state file");

    assert!(
        reloaded.safe_mode.active,
        "safe mode should still be active after a manual scale"
    );
    let stored = reloaded
        .manual_override
        .expect("the manual override should have been persisted");
    assert_eq!(
        stored.target, 4,
        "the requested count is stored raw (pre-clamp)"
    );
    assert_eq!(stored.source, "cli");
    assert!(
        stored.expires_at.is_some(),
        "the default TTL binds the override until it expires"
    );
}

/// Negative control: with safe mode inactive, the notification must not appear.
///
/// This is what makes the positive test meaningful — it proves the line is conditional on
/// safe mode rather than printed unconditionally by every `scale` invocation.
#[test]
fn scale_without_safe_mode_prints_no_notification() {
    let (_temp, output) = run_cgov(&make_state(false), &["scale", "3"]);
    let stdout = stdout_of(&output);

    assert!(
        stdout.contains("Manual override stored: fleet target 3"),
        "expected the scale confirmation on stdout, got:\n{stdout}"
    );
    assert!(
        !stdout.contains(SAFE_MODE_NOTICE),
        "the safe-mode notification must not appear when safe mode is inactive, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("Safe mode"),
        "no safe-mode messaging at all is expected outside safe mode, got:\n{stdout}"
    );
}

#[test]
fn scale_clear_removes_the_persistent_pin() {
    let mut state = make_state(false);
    state.manual_override = Some(ManualOverride {
        target: 4,
        set_at: chrono::Utc::now(),
        expires_at: None,
        source: "cli".to_string(),
    });

    let (temp, output) = run_cgov(&state, &["scale", "--clear"]);
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Manual override cleared (target 4"),
        "clear should report the removed pin, got:\n{stdout}"
    );
    assert!(
        state::load_state(&state_path_in(temp.path()))
            .unwrap()
            .manual_override
            .is_none(),
        "clear must remove the persistent pin from state"
    );
}

/// The TTL clauses of the contract, checked on the stored state file rather than only on
/// the stdout wording: `cgov scale N` with no `--ttl` binds for the documented default of
/// 2 hours (`MANUAL_OVERRIDE_DEFAULT_TTL_HOURS`), and `--ttl 0` stores no expiry at all —
/// it holds until an explicit `cgov scale --clear`.
#[test]
fn scale_stores_the_default_two_hour_ttl_and_ttl_zero_holds_until_clear() {
    // Default: the stored expiry is exactly set_at + 2h. Both timestamps are
    // stamped from the same `now` inside the CLI, so the round-tripped state
    // pins the default exactly.
    let (temp, output) = run_cgov(&make_state(false), &["scale", "3"]);
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Manual override stored: fleet target 3"),
        "expected the scale confirmation on stdout, got:\n{stdout}"
    );
    let ov = state::load_state(&state_path_in(temp.path()))
        .unwrap()
        .manual_override
        .expect("scale stored a pin");
    assert_eq!(ov.source, "cli");
    assert_eq!(
        (ov.expires_at.expect("the default TTL stores an expiry") - ov.set_at).num_minutes(),
        120,
        "no --ttl means the documented default of 2 hours"
    );
    assert!(
        stdout.contains("binding until"),
        "the confirmation must tell the operator when the pin lapses, got:\n{stdout}"
    );

    // `--ttl 0`: no expiry is stored — the clock never ends the pin.
    let (temp, output) = run_cgov(&make_state(false), &["scale", "3", "--ttl", "0"]);
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("binding until `cgov scale --clear`"),
        "ttl 0 must be reported as hold-until-clear, got:\n{stdout}"
    );
    let stored = state::load_state(&state_path_in(temp.path())).unwrap();
    assert_eq!(
        stored
            .manual_override
            .expect("scale stored a pin")
            .expires_at,
        None,
        "--ttl 0 must store no expiry: the pin holds until cgov scale --clear"
    );
}

/// The log half of the pair: a manual scale in safe mode must leave an audit line in
/// `governor.log`, timestamped, and must *not* leak that line onto stdout.
///
/// The pre-existing unit test for this message re-implements the write inside the test body
/// (it appends the line itself, then asserts the line is present), so it passes even if
/// `run_scale_command` logs nothing at all. This test runs the real binary instead, so
/// deleting the `append_to_governor_log` call in `run_scale_command` fails here.
#[test]
fn scale_during_safe_mode_writes_warning_to_log_file() {
    let (temp, output) = run_cgov(&make_state(true), &["scale", "4"]);
    let stdout = stdout_of(&output);

    let log_path = log_path_in(temp.path());
    let log = std::fs::read_to_string(&log_path).unwrap_or_else(|e| {
        panic!(
            "expected a governor log at {}, but it could not be read: {e}",
            log_path.display()
        )
    });

    let warning_line = log
        .lines()
        .find(|line| line.contains(SAFE_MODE_LOG_WARNING))
        .unwrap_or_else(|| panic!("expected the safe-mode warning in the log, got:\n{log}"));

    // The line is prefixed with an RFC3339 timestamp, which is what makes it an audit record
    // rather than a bare message. Parsing it (rather than sniffing for a 'T') is what would
    // actually catch a malformed prefix.
    let timestamp = warning_line
        .split_once(&format!(" {SAFE_MODE_LOG_WARNING}"))
        .map(|(ts, _)| ts)
        .unwrap_or_else(|| panic!("warning line had no timestamp prefix: {warning_line}"));
    assert!(
        timestamp.parse::<chrono::DateTime<chrono::Utc>>().is_ok(),
        "log timestamp {timestamp:?} is not RFC3339, full line: {warning_line}"
    );

    // The two messages are addressed to different audiences: the WARN is for the log,
    // the NOTE is for the operator at the terminal.
    assert!(
        !stdout.contains(SAFE_MODE_LOG_WARNING),
        "the WARN line belongs in the log only, but appeared on stdout:\n{stdout}"
    );
    assert!(
        stdout.contains(SAFE_MODE_NOTICE),
        "expected the stdout notification alongside the log warning, got:\n{stdout}"
    );
}

/// Negative control for the log warning: no safe mode, no audit line.
#[test]
fn scale_without_safe_mode_writes_no_warning_to_log_file() {
    let (temp, output) = run_cgov(&make_state(false), &["scale", "3"]);
    stdout_of(&output);

    // The log file may not exist at all if nothing was logged — that is a pass.
    let log = std::fs::read_to_string(log_path_in(temp.path())).unwrap_or_default();
    assert!(
        !log.contains(SAFE_MODE_LOG_WARNING),
        "the safe-mode warning must not be logged outside safe mode, got:\n{log}"
    );
}

/// `compute_target_workers` derives a computed target from each worker's
/// `min`/`max`/`current` and the capacity forecast — it never reads the
/// per-agent `worker.target` field. The persistent fleet pin is applied later
/// by `run_act_cycle`, so this pure computation remains independent of it.
#[test]
fn per_agent_target_field_does_not_influence_computed_target() {
    use claude_governor::config::{CompositeRiskConfig, ConeScalingConfig};
    use claude_governor::governor::compute_target_workers;

    let composite_risk = CompositeRiskConfig::default();
    let cone_scaling = ConeScalingConfig::default();

    // Two states identical in every respect except the manually scaled `target`.
    let untouched = make_state(true);
    let mut after_manual_scale = make_state(true);
    for worker in after_manual_scale.workers.values_mut() {
        worker.target = 9;
    }

    let target_untouched = compute_target_workers(&untouched, 80.0, &composite_risk, &cone_scaling);
    let target_after_scale =
        compute_target_workers(&after_manual_scale, 80.0, &composite_risk, &cone_scaling);

    assert_eq!(
        target_untouched, target_after_scale,
        "a per-agent target leaked into the computed-target function; override precedence \
         belongs to the act-cycle seam"
    );
}

/// Build a safe-mode state carrying a *populated* binding-window forecast, with the worker's
/// `target` manually scaled to `manual_target`.
///
/// The wide cone (`cone_ratio` = p75/p25 = 4.0) drives `compute_target_workers` down the
/// conservative p75 branch, so the next cycle's target comes from `safe_worker_count_p75`.
fn make_state_with_forecast(manual_target: u32) -> GovernorState {
    let mut state = make_state(true);
    for worker in state.workers.values_mut() {
        worker.target = manual_target;
    }

    state.capacity_forecast.five_hour = state::WindowForecast {
        target_ceiling: 85.0,
        current_utilization: 60.0,
        remaining_pct: 25.0,
        hours_remaining: 3.0,
        fleet_pct_per_hour: 6.0,
        predicted_exhaustion_hours: 4.0,
        binding: true,
        safe_worker_count: Some(5),
        safe_worker_count_p75: Some(FORECAST_DERIVED_TARGET),
        exh_hrs_p25: 2.0,
        exh_hrs_p50: 4.0,
        exh_hrs_p75: 8.0,
        cone_ratio: 4.0,
        ..Default::default()
    };
    state.capacity_forecast.binding_window = "five_hour".to_string();

    state
}

/// The target the forecast in `make_state_with_forecast` implies: its `safe_worker_count_p75`.
///
/// Deliberately distinct from both the manual scale targets used below and the workers'
/// `current` (2), so an assertion on it cannot be satisfied by the manual target leaking
/// through *or* by the "hold at current" fallback.
const FORECAST_DERIVED_TARGET: u32 = 3;

/// Stronger form of the test above: the computed-target function returns a
/// forecast-derived target regardless of stale per-agent target fields.
///
/// The preceding test builds its state from `make_state`, whose `capacity_forecast` is empty.
/// With no forecast, `compute_target_workers` short-circuits to "hold at current" — so that
/// test never exercises the forecast-driven branch and only compares two runs against each
/// other. Here the binding window is populated, so
/// the recomputed target is pinned to a concrete third value that neither the manual target
/// nor the hold-at-current fallback could produce.
#[test]
fn computed_target_uses_forecast_not_stale_per_agent_target() {
    use claude_governor::config::{CompositeRiskConfig, ConeScalingConfig};
    use claude_governor::governor::compute_target_workers;

    let composite_risk = CompositeRiskConfig::default();
    let cone_scaling = ConeScalingConfig::default();

    // Every manual override an operator could have applied — including the worker max —
    // must be recomputed away to the same forecast-derived target.
    for manual_target in [2, 4, 9, 10] {
        let target = compute_target_workers(
            &make_state_with_forecast(manual_target),
            85.0,
            &composite_risk,
            &cone_scaling,
        );

        assert_eq!(
            target, FORECAST_DERIVED_TARGET,
            "a stale per-agent target {manual_target} must not replace the forecast-derived \
             target {FORECAST_DERIVED_TARGET}, but computed {target}"
        );
    }
}
