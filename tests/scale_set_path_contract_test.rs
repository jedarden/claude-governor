//! End-to-end contract tests for the `cgov scale` set path (README
//! "Manual scale override").
//!
//! `manual_override_lifecycle.rs` mirrors `manual_override_record` with a
//! local helper because the real builder is private to the binary; the tests
//! here drive the actual `cgov` binary instead, so the record builder,
//! `validate_scale_count`, the locked save, and the `--ttl` flag flow are all
//! exercised exactly as shipped. That is what lets them pin things no other
//! test can:
//!
//! - the out-of-range rejection names the aggregate bounds in its message
//!   (unit tests only assert `is_err()`), and a rejected count writes nothing;
//! - a stored record is read back *by name* by a fresh `cgov` process — and
//!   the per-agent `worker.target` field is left alone (the old bare
//!   `worker.target` write is the regression this guards against);
//! - `--ttl 0` and the 2-hour default reach the persisted `expires_at`
//!   through the real flag flow, not just through `manual_override_record`.
//!
//! Isolation matches `scale_safe_mode_stdout_test.rs`: every child runs with
//! `HOME` and the XDG variables pointed at a fresh `TempDir`, so nothing in
//! the developer's real `~/.config/claude-governor` is read or written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use claude_governor::state::{self, GovernorState, WorkerState};
use tempfile::TempDir;

/// Path `cgov` will resolve as its state file, given `XDG_CONFIG_HOME` = `root/config`.
fn state_path_in(root: &Path) -> PathBuf {
    root.join("config")
        .join("claude-governor")
        .join("governor-state.json")
}

/// One agent with an aggregate envelope of [1, 10]: both a count above the
/// max (11) and below the min (0) are set-time rejections.
fn make_state() -> GovernorState {
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
    state
}

/// A fresh isolated environment for `cgov` child processes.
fn new_env_root() -> TempDir {
    TempDir::new().expect("failed to create temp dir")
}

/// Write `state` as the state file `cgov` will resolve inside `root`.
fn write_state(root: &Path, state: &GovernorState) {
    state::save_state(state, &state_path_in(root)).expect("failed to write test state");
}

/// Run the real `cgov` binary against `root`'s isolated environment.
fn run_cgov_in(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cgov"))
        .args(args)
        // Point every path-resolution mechanism at the temp dir. `HOME` covers
        // the `dirs` fallbacks; the XDG vars cover the primary lookups.
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .expect("failed to run cgov binary")
}

/// Decode captured stdout, failing loudly (with stderr) if the command failed.
fn stdout_of(output: Output) -> String {
    assert!(
        output.status.success(),
        "cgov exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("cgov stdout was not valid UTF-8")
}

/// A rejected `cgov scale N` must fail with a message naming the aggregate
/// bounds — the same envelope `resolve_manual_override` clamps against — and
/// must not store anything.
#[test]
fn scale_rejection_names_the_aggregate_bounds_and_stores_nothing() {
    for count in ["11", "0"] {
        let root = new_env_root();
        write_state(root.path(), &make_state());

        let output = run_cgov_in(root.path(), &["scale", count]);
        assert!(
            !output.status.success(),
            "cgov scale {count} is outside [1, 10] and must fail\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout),
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected =
            format!("Worker count {count} is outside the fleet's aggregate bounds (1 - 10)");
        assert!(
            stderr.contains(&expected),
            "the rejection must name the requested count and the aggregate bounds \
             {expected:?}, got stderr:\n{stderr}"
        );

        let reloaded = state::load_state(&state_path_in(root.path())).unwrap();
        assert!(
            reloaded.manual_override.is_none(),
            "a rejected count must not store an override"
        );
    }
}

/// `cgov scale N` persists an identifiable manual-override record — not a
/// bare `worker.target` write — and a NEW `cgov` process reads that record
/// back by name: `cgov scale --clear` in a second process reports the stored
/// pin's target before removing it.
#[test]
fn scale_persists_a_record_a_new_process_reads_back_by_name() {
    let root = new_env_root();
    write_state(root.path(), &make_state());

    // First process: the set. The confirmation names the aggregate envelope
    // the count validated against.
    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "4"]));
    assert!(
        stdout.contains("Manual override stored: fleet target 4 (aggregate bounds 1 - 10)"),
        "expected the scale confirmation naming the bounds, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Source: cli"),
        "the record must be identifiable by its source, got:\n{stdout}"
    );

    // The persisted record is the named `manual_override` field with a "cli"
    // source — and the per-agent `worker.target` is untouched (the old write
    // path put the count there instead).
    let reloaded = state::load_state(&state_path_in(root.path())).unwrap();
    let stored = reloaded
        .manual_override
        .as_ref()
        .expect("the override must be persisted under manual_override");
    assert_eq!(stored.target, 4);
    assert_eq!(stored.source, "cli");
    assert_eq!(
        reloaded.workers["test-agent"].target, 2,
        "the pin belongs in manual_override; worker.target must stay computed"
    );

    // The JSON itself carries the named field, so any reader (daemon, CLI,
    // operator) finds the record rather than a scattered worker.target.
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_path_in(root.path())).unwrap())
            .expect("state file is valid JSON");
    assert_eq!(
        raw["manual_override"]["source"], "cli",
        "the record must be persisted under the named manual_override field"
    );

    // Second process: reads the record back by name — the clear report
    // echoes the stored target, which it could only know from the file the
    // first process wrote.
    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "--clear"]));
    assert!(
        stdout.contains("Manual override cleared (target 4, set "),
        "the second cgov process must read the stored record back by name, got:\n{stdout}"
    );
    assert!(
        state::load_state(&state_path_in(root.path()))
            .unwrap()
            .manual_override
            .is_none(),
        "the second process's clear must remove the record"
    );
}

/// `--ttl 0` must persist `expires_at: null` through the real flag flow: the
/// hold-until-clear semantics live in the stored record, not just in the
/// resolution logic.
#[test]
fn scale_ttl_zero_stores_a_record_with_no_expiry() {
    let root = new_env_root();
    write_state(root.path(), &make_state());

    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "3", "--ttl", "0"]));
    assert!(
        stdout.contains("binding until `cgov scale --clear`"),
        "the confirmation must say the pin holds until cleared, got:\n{stdout}"
    );

    let stored = state::load_state(&state_path_in(root.path()))
        .unwrap()
        .manual_override
        .expect("the override must be persisted");
    assert_eq!(stored.target, 3);
    assert_eq!(
        stored.expires_at, None,
        "--ttl 0 must store no expiry: only cgov scale --clear ends the pin"
    );
}

/// The default (no `--ttl`) must persist a two-hour binding —
/// `MANUAL_OVERRIDE_DEFAULT_TTL_HOURS` — measured on the persisted record,
/// so a constant drift fails here rather than only in a unit test's mirror.
#[test]
fn scale_default_ttl_binds_two_hours() {
    let root = new_env_root();
    write_state(root.path(), &make_state());

    stdout_of(run_cgov_in(root.path(), &["scale", "3"]));

    let stored = state::load_state(&state_path_in(root.path()))
        .unwrap()
        .manual_override
        .expect("the override must be persisted");
    assert_eq!(stored.source, "cli");
    let expires_at = stored.expires_at.expect("the default TTL must set one");
    assert_eq!(
        (expires_at - stored.set_at).num_seconds(),
        2 * 60 * 60,
        "the default binding must be MANUAL_OVERRIDE_DEFAULT_TTL_HOURS (2h)"
    );
}

/// `cgov scale --clear` is idempotent. A clear with nothing stored — never
/// had an override, or after a previous clear — still exits 0 and prints
/// exactly "No manual override stored; nothing to clear.", because an
/// operator's retry loop must never treat the second pass as a failure.
/// Driven through the real binary: the exit code and the exact message are
/// the contract, and `manual_override_lifecycle.rs` can only mirror the
/// take(), not the print.
#[test]
fn clear_is_idempotent_and_a_second_pass_reports_nothing_to_clear() {
    // Empty state from the start.
    let root = new_env_root();
    write_state(root.path(), &make_state());

    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "--clear"]));
    assert_eq!(
        stdout.trim(),
        "No manual override stored; nothing to clear.",
        "a clear with nothing stored must report exactly this, got:\n{stdout}"
    );

    // And the same no-op after a real pin was stored and cleared.
    stdout_of(run_cgov_in(root.path(), &["scale", "4"]));
    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "--clear"]));
    assert!(
        stdout.contains("Manual override cleared (target 4, set "),
        "the first clear must report the record it removed, got:\n{stdout}"
    );

    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "--clear"]));
    assert_eq!(
        stdout.trim(),
        "No manual override stored; nothing to clear.",
        "the second clear must be the same no-op, got:\n{stdout}"
    );
    assert!(
        state::load_state(&state_path_in(root.path()))
            .unwrap()
            .manual_override
            .is_none(),
        "the idempotent pass must leave state without an override"
    );
}

/// `cgov scale --clear` takes no COUNT. A count alongside the flag is a
/// rejection naming that rule — in either argument order — and the stored
/// pin survives it: the guard short-circuits before the take, so a mistyped
/// retry must not silently consume the operator's override.
#[test]
fn clear_with_a_count_is_rejected_and_consumes_nothing() {
    let root = new_env_root();
    write_state(root.path(), &make_state());
    stdout_of(run_cgov_in(root.path(), &["scale", "4"]));

    for args in [["scale", "--clear", "4"], ["scale", "4", "--clear"]] {
        let output = run_cgov_in(root.path(), &args);
        assert!(
            !output.status.success(),
            "cgov scale with --clear and a COUNT ({args:?}) must be rejected\nstdout:\n{}",
            String::from_utf8_lossy(&output.stdout),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cgov scale --clear takes no COUNT"),
            "the rejection must name the rule, got stderr:\n{stderr}"
        );
        let stored = state::load_state(&state_path_in(root.path()))
            .unwrap()
            .manual_override
            .expect("the rejected clear must not consume the stored pin");
        assert_eq!(stored.target, 4, "the stored pin must be untouched");
    }
}

/// `--dry-run` on the clear path prints the intent — naming the stored
/// record it would remove — and mutates nothing: the pin is still stored
/// afterwards and a real clear still finds and removes it.
#[test]
fn clear_dry_run_prints_intent_without_mutating_state() {
    let root = new_env_root();
    write_state(root.path(), &make_state());
    stdout_of(run_cgov_in(root.path(), &["scale", "4"]));

    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "--clear", "--dry-run"]));
    assert!(
        stdout.contains("DRY RUN: Would clear the manual override (target 4, set "),
        "the dry run must print the intent naming the stored record, got:\n{stdout}"
    );

    let still_stored = state::load_state(&state_path_in(root.path()))
        .unwrap()
        .manual_override
        .expect("a dry-run clear must not remove the stored override");
    assert_eq!(still_stored.target, 4, "the stored pin must be untouched");

    // The real clear still finds the record the dry run left alone.
    let stdout = stdout_of(run_cgov_in(root.path(), &["scale", "--clear"]));
    assert!(
        stdout.contains("Manual override cleared (target 4, set "),
        "the real clear after the dry run must remove the record, got:\n{stdout}"
    );
}
