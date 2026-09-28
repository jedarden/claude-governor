//! A sprint target can never be a retired component (bead claudego-9e4c4435).
//!
//! CLAUDE.md §4: config validation at load (`RETIRED_REFERENCE_MARKERS` in
//! `src/config.rs`) rejects a governor.yaml referencing a retired pool or
//! queue, "so a sprint target can no longer be retired by construction". The
//! sprint target is the pool itself — an `agents:` entry the underutilization
//! sprint would boost toward its `max_workers` — so the pin here is that
//! shape: a pool that is otherwise a perfectly valid, sprint-eligible
//! subscription target (min 0, max headroom, a workspace to count a backlog
//! from) is still refused at load the moment its name is a retired component.
//!
//! The unit tests in `src/config.rs` already cover the marker positions
//! (top-level keys, pool names, `launch_cmd`, `session_pattern`,
//! `heartbeat_dir`) and the generator-pool spellings; `tests/
//! retired_component_surface_sweep.rs` covers the doctor/init surfaces. This
//! file adds what neither pins: the sprint-target shape specifically, the
//! full remediation text (removal directive and the do-not-recreate bar)
//! through the parsing entry the daemon actually starts from, and a control
//! proving the rejection is caused by the retired name alone — the identical
//! pool under a non-retired name loads with its sprint-eligible shape intact.

use claude_governor::config::GovernorConfig;

/// A retired pool wearing exactly the shape the underutilization sprint
/// boosts: a subscription pool with `max_workers` headroom, `min_workers` 0,
/// and a `--workspace` the backlog gate can count. Every field except the
/// pool name is marker-free, so the name is the sole violation — if this
/// config loads, the sprint could be armed against a retired component.
const SPRINT_SHAPED_POOL: &str = r#"
pricing:
  models: {}
agents:
  polish-opus:
    launch_cmd: "needle run --agent claude-print-opus --workspace /home/coding/claude-governor"
    session_pattern: "needle-claude-print-opus-*"
    heartbeat_dir: "~/.needle/state/heartbeats"
    min_workers: 0
    max_workers: 4
    subscription: true
"#;

/// Both retired naming families, spelled the way a pre-retirement
/// governor.yaml would have named the pool.
const RETIRED_POOL_NAMES: [&str; 2] = ["polish-opus", "generator-pool-fable"];

#[test]
fn sprint_target_shaped_retired_pool_fails_config_load() {
    for name in RETIRED_POOL_NAMES {
        let yaml = SPRINT_SHAPED_POOL.replace("polish-opus", name);
        let err = GovernorConfig::parse_and_validate(&yaml).err().unwrap_or_else(|| {
            panic!("a retired pool shaped as a sprint target ({name}) must be rejected at load")
        });
        let msg = format!("{err:#}");
        assert!(
            msg.contains("retired on 2026-09-16"),
            "the error must carry the retirement verdict for {name}: {msg}"
        );
        assert!(
            msg.contains(name),
            "the error must name the offending pool: {msg}"
        );
        assert!(
            msg.contains("Remove the retired entries"),
            "the error must carry the removal directive: {msg}"
        );
        assert!(
            msg.contains("do not recreate it"),
            "the error must bar recreation: {msg}"
        );
    }
}

#[test]
fn the_identical_pool_under_a_non_retired_name_loads() {
    let yaml = SPRINT_SHAPED_POOL.replace("polish-opus", "sprint-opus");
    let config = GovernorConfig::parse_and_validate(&yaml).expect(
        "the control pool differs from the rejected one only by its name — \
         it must load, proving the gate rejects the retired name and not the \
         sprint shape",
    );
    let agent = config
        .agents
        .get("sprint-opus")
        .expect("the control pool must parse into the agents map");
    assert!(
        agent.subscription,
        "the control must keep the sprint-eligible subscription flag"
    );
    assert_eq!(
        agent.max_workers, 4,
        "the control must keep the sprint target headroom"
    );
    assert_eq!(
        agent.min_workers, 0,
        "the control must keep the sprint-eligible min floor"
    );
}

#[test]
fn sprint_shaped_retired_pool_fails_load_from_path_naming_the_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("governor.yaml");
    std::fs::write(&path, SPRINT_SHAPED_POOL).expect("write poisoned config");

    let err = GovernorConfig::load_from_path(&path)
        .err()
        .expect("a config arming the sprint for a retired pool must be rejected at load");
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&path.display().to_string()),
        "the error must name the config file so the operator knows what to fix: {msg}"
    );
    assert!(
        msg.contains("retired on 2026-09-16"),
        "the error must carry the retirement verdict: {msg}"
    );
    assert!(
        msg.contains("polish-opus"),
        "the error must name the offending pool: {msg}"
    );
}
