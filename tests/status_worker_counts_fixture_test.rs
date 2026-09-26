//! claudego-f1e38b15: `cgov status` must report the worker counts the daemon
//! persisted — never a hard-coded zero.
//!
//! The bead suspected the status render carried a hard-coded
//! `current_total=0` seam (a `per_worker_pct_for_sizing(0, ...)` call)
//! sourced from the 2026-09-21 sizing-bug investigation notes. The audit that
//! opened this suite found no such seam at HEAD: every status surface —
//! `format_status_dashboard`'s Workers section, `format_status_json`'s
//! `workers` block, and `cgov workers` — sums `state.workers` from the
//! persisted `governor-state.json`, and both daemon cycles write the live
//! tmux census into that state before saving (`governor.rs`
//! `ws.current = current_workers_per_agent[...]`). The literal-zero
//! `per_worker_pct_for_sizing(0, ...)` calls that exist are the deliberate
//! zero-worker invariant tests (claudego-1942b4ea / claudego-6f45312e), not
//! production display code.
//!
//! What nothing pinned was that state-faithfulness itself. These tests load
//! a committed active-fleet fixture through the exact production path the
//! status command uses — `state::load_state` (file → serde, corrupt-file
//! fallback included) → `format_status_dashboard` / `format_status_json` —
//! and assert the rendered output carries the fixture's non-zero per-agent
//! counts and their cross-agent sums. Any regression that breaks
//! state-faithfulness (a hard-coded zero, a dropped or renamed `workers`
//! field, a deserialize-to-default) fails here instead of misleading an
//! operator mid-incident, which is the failure shape the deployment
//! verification runbook (docs/notes/deployment-verification.md step 3)
//! warns about.

use claude_governor::state::{self, GovernorState};
use claude_governor::status_display::{format_status_dashboard, format_status_json};

const ACTIVE_FLEET_JSON: &str = include_str!("fixtures/state/governor_state_active_fleet.json");

/// Fleet totals the fixture encodes: two agents, 3+1 running, 4+2 targeted.
/// The dashboard/JSON assertions below pin these sums exactly, so a render
/// that echoes any single agent's counts — or a constant — cannot pass.
const EXPECTED_FLEET_CURRENT: u32 = 4;
const EXPECTED_FLEET_TARGET: u32 = 6;

/// Guard that the fixture still exercises the case it exists for: a persisted
/// active fleet with non-zero per-agent counts and non-zero totals. If the
/// fixture is edited to zero them, the render assertions below would pass
/// vacuously against a hard-coded zero — this must fail loudly instead.
#[test]
fn fixture_carries_nonzero_worker_totals() {
    let v: serde_json::Value =
        serde_json::from_str(ACTIVE_FLEET_JSON).expect("fixture must be valid JSON");
    let workers = v["workers"].as_object().expect("workers object present");

    assert!(
        workers.len() >= 2,
        "fixture must carry at least two agents so the pinned totals are cross-agent sums"
    );
    let sum_current: u32 = workers
        .values()
        .map(|w| w["current"].as_u64().expect("current is a number") as u32)
        .sum();
    let sum_target: u32 = workers
        .values()
        .map(|w| w["target"].as_u64().expect("target is a number") as u32)
        .sum();

    assert_eq!(sum_current, EXPECTED_FLEET_CURRENT, "fixture fleet current");
    assert_eq!(sum_target, EXPECTED_FLEET_TARGET, "fixture fleet target");
    for (agent, w) in workers {
        assert!(
            w["current"].as_u64().unwrap() > 0 && w["target"].as_u64().unwrap() > 0,
            "agent {agent} must have non-zero current and target in the fixture"
        );
    }
}

/// Load the committed fixture through the production path: written to disk as
/// a governor-state.json and read back with `state::load_state` — the same
/// function the `cgov status` command handler calls. `load_state` swallows
/// deserialization failures into a fresh state, so the caller asserts the
/// workers actually arrived rather than silently rendering an empty fleet.
fn load_active_fleet_state() -> GovernorState {
    let dir = tempfile::tempdir().expect("tempdir for state file");
    let path = dir.path().join("governor-state.json");
    std::fs::write(&path, ACTIVE_FLEET_JSON).expect("write fixture to state path");

    let state = state::load_state(&path).expect("load_state must succeed");
    assert!(
        !state.workers.is_empty(),
        "fixture must deserialize through load_state — an empty workers map means \
         the load fell back to a fresh state and the render assertions prove nothing"
    );
    state
}

/// The dashboard's Workers section renders the persisted counts verbatim:
/// the fleet line carries the cross-agent sums, and each agent line carries
/// that agent's own current/target/range from the state file.
#[test]
fn dashboard_reports_persisted_worker_totals() {
    let state = load_active_fleet_state();
    let output = format_status_dashboard(&state, chrono::Utc::now());

    assert!(
        output.contains(&format!(
            "Fleet: {EXPECTED_FLEET_CURRENT} current / {EXPECTED_FLEET_TARGET} target"
        )),
        "dashboard fleet line must report the persisted sums, got:\n{output}"
    );
    assert!(
        output.contains("  claude-anthropic-sonnet: 3 current / 4 target (range: 0-8)"),
        "per-agent line must report the persisted counts, got:\n{output}"
    );
    assert!(
        output.contains("  needle-sonnet: 1 current / 2 target (range: 0-4)"),
        "per-agent line must report the persisted counts, got:\n{output}"
    );
}

/// The machine surface (`cgov status --json`, also what a non-TTY invocation
/// prints) carries the same persisted counts under `workers.current`,
/// `workers.target`, and `workers.by_agent`.
#[test]
fn status_json_reports_persisted_worker_totals() {
    let state = load_active_fleet_state();
    let json = format_status_json(&state, None);

    assert_eq!(
        json["workers"]["current"], EXPECTED_FLEET_CURRENT,
        "workers.current must be the persisted cross-agent sum"
    );
    assert_eq!(
        json["workers"]["target"], EXPECTED_FLEET_TARGET,
        "workers.target must be the persisted cross-agent sum"
    );
    assert_eq!(
        json["workers"]["by_agent"]["claude-anthropic-sonnet"]["current"], 3,
        "by_agent must carry the persisted per-agent counts"
    );
    assert_eq!(
        json["workers"]["by_agent"]["claude-anthropic-sonnet"]["target"],
        4
    );
    assert_eq!(json["workers"]["by_agent"]["needle-sonnet"]["current"], 1);
    assert_eq!(json["workers"]["by_agent"]["needle-sonnet"]["target"], 2);
}
