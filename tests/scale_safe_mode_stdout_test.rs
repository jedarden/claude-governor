//! Integration test for the safe-mode stdout notification emitted by `cgov scale`.
//!
//! When an operator manually scales the fleet while the governor is in safe mode, the
//! scale still applies, but the governor will recompute (and likely override) the target
//! on its next cycle. `run_scale_command` therefore prints a notification to stdout:
//!
//! ```text
//! NOTE: Safe mode remains active and will reassert its target on the next cycle
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

use claude_governor::state::{self, GovernorState, WorkerState};
use tempfile::TempDir;

/// The exact notification text under test. Kept as a constant so the assertions and the
/// failure messages can never drift apart.
const SAFE_MODE_NOTICE: &str =
    "NOTE: Safe mode remains active and will reassert its target on the next cycle";

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

    let confirmation = "Target worker count set to 4";
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
/// after the manual scale, and the requested target was actually written.
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
    assert_eq!(
        reloaded.workers["test-agent"].target, 4,
        "the manual scale target should have been persisted"
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
        stdout.contains("Target worker count set to 3"),
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

/// The notification promises the governor "will reassert its target on the next cycle".
/// This test pins the mechanism that makes that promise true.
///
/// `compute_target_workers` derives the next target from each worker's `min`/`max`/`current`
/// and the capacity forecast — it never reads `worker.target`. So a manually scaled target is
/// not an input to the next cycle and gets recomputed away. Asserting the invariant (the
/// manual target has *no* influence) rather than a specific number keeps this test meaningful
/// without pinning it to whatever the forecast heuristics currently return.
#[test]
fn manual_scale_target_does_not_influence_next_cycle_target() {
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
        "the manual scale target leaked into the next cycle's computation; safe mode would \
         not reassert as the notification claims"
    );
}
