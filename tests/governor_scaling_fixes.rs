//! Regression tests for the four governor.rs scaling fixes documented in
//! `CLAUDE.md` §4 ("The cgov code fixes behind this"). Each test names the fix
//! it guards and fails on the pre-fix behavior:
//!
//! 1. `distribute_workers_by_cost_priority` min_workers floor — an expensive
//!    pool's guaranteed slot is not swallowed by the cost sort
//!    (`min_workers_floor_expensive_pool_wins_slot_regression`).
//! 2. The `NoChange` arm reconciling the per-agent allocation even when the
//!    aggregate total is unchanged
//!    (`no_change_still_reconciles_per_agent_allocation_regression`).
//! 3. `safe_worker_count_or_max` (`safe_worker_count_or_hold`) mapping
//!    `Some(0)` → 0 instead of holding at `current_total`
//!    (`safe_worker_count_some_zero_scales_to_zero_regression`).
//! 4. `apply_underutilization_sprint` actually being wired into the act cycle
//!    (it was defined but never called)
//!    (`underutilization_sprint_wired_into_act_cycle_regression`).
//!
//! The act-cycle tests drive `run_act_cycle` with `dry_run = false`, because
//! both the reconcile arm and the launch arms are skipped entirely in dry-run
//! mode. They run hermetically: `tmux` and `bf` resolve to fake scripts on a
//! per-test `PATH` (worker census and sprint backlog), launches go to a stub
//! that appends a tag to a log file, and `CGOV_DECISIONS_PATH` redirects the
//! audit log into the temp dir. Every test that swaps the environment holds
//! `ENV_LOCK` for its whole body so the swaps cannot race.
//!
//! Regression property (mutation-checked at HEAD 2c02a55): reverting each fix
//! in `governor.rs` — floor pass disabled, NoChange reconcile loops disabled,
//! `Some(0)` re-held at `current_total`, sprint call unwired — flips exactly the
//! corresponding test to failure on the assertion that names the fix, so these
//! four tests pin the four CLAUDE.md §4 bullets one-to-one.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, ModelPricing,
    PricingConfig,
};
use claude_governor::governor::{compute_target_workers, run_act_cycle, ScalingDecision};
use claude_governor::state::{self, GovernorState};

/// Serializes every test that swaps PATH / CGOV_DECISIONS_PATH.
static ENV_LOCK: Mutex<()> = Mutex::new(());
/// The PATH this process was launched with, captured before any test swaps it.
static ORIG_PATH: OnceLock<String> = OnceLock::new();

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Build an `AgentConfig` from JSON rather than a struct literal: this file
/// must compile against both the committed tree and in-flight trees that add
/// optional fields (e.g. `AgentConfig::windows`), and serde tolerates both.
fn agent_config(
    launch_cmd: &str,
    session_pattern: &str,
    heartbeat_dir: &Path,
    min_workers: u32,
    max_workers: u32,
    subscription: bool,
) -> AgentConfig {
    serde_json::from_value(serde_json::json!({
        "launch_cmd": launch_cmd,
        "session_pattern": session_pattern,
        "heartbeat_dir": heartbeat_dir.to_string_lossy(),
        "min_workers": min_workers,
        "max_workers": max_workers,
        "subscription": subscription,
    }))
    .expect("agent config fixture should deserialize")
}

fn write_executable(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{}\n", body)).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod script");
}

/// `tmux` stands in for the live census: `list-sessions` prints the given
/// session file, every other subcommand (send-keys, kill-session, …) is
/// recorded to a log so tests can prove which sessions were touched.
fn install_fake_tmux(bin_dir: &Path, sessions_file: &Path, calls_log: &Path) {
    write_executable(
        bin_dir,
        "tmux",
        &format!(
            "case \"$1\" in\n  list-sessions) cat '{}';;\n  *) printf '%s\\n' \"$*\" >> '{}';;\nesac\nexit 0",
            sessions_file.display(),
            calls_log.display()
        ),
    );
}

/// `bf ready` reporting one ready bead passes the sprint backlog gate.
fn install_fake_bf(bin_dir: &Path) {
    write_executable(bin_dir, "bf", "echo \"bf-fake0001 ready\"");
}

/// Launch stub: `<stub> --workspace <dir> <tag> <log>` appends `<tag>`.
/// Carries `--workspace` so `workspace_from_launch_cmd` can parse it, and
/// optionally `--agent <model>` so cost-priority pricing resolves.
fn install_launch_stub(bin_dir: &Path) {
    write_executable(bin_dir, "launch-stub", "echo \"$3\" >> \"$4\"");
}

fn launch_cmd_for(bin_dir: &Path, workspace: &Path, tag: &str, log: &Path, model: &str) -> String {
    format!(
        "{} --workspace {} {} {} --agent {}",
        bin_dir.join("launch-stub").display(),
        workspace.display(),
        tag,
        log.display(),
        model
    )
}

fn launch_tags(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Point subprocess resolution at this test's fakes and keep the audit log
/// out of the operator's home. Caller must hold [`ENV_LOCK`].
fn activate_env(bin_dir: &Path, decisions_path: &Path) {
    let orig = ORIG_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), orig));
    std::env::set_var("CGOV_DECISIONS_PATH", decisions_path);
}

/// Narrow-cone window fixture: `cone_ratio = 0.0` selects the p50
/// `safe_worker_count`, so tests seed exactly one safe-count per window.
fn seed_window(
    win: &mut state::WindowForecast,
    utilization: f64,
    hours_remaining: f64,
    safe: Option<u32>,
) {
    win.current_utilization = utilization;
    win.hours_remaining = hours_remaining;
    win.cutoff_risk = false;
    win.safe_worker_count = safe;
    win.safe_worker_count_p75 = safe;
    win.cone_ratio = 0.0;
}

/// Fresh state with a five_hour binding window and no cutoff risk anywhere —
/// far from the emergency brake and safe mode on every axis.
fn seeded_state(
    binding_utilization: f64,
    binding_hours: f64,
    binding_safe: Option<u32>,
) -> GovernorState {
    let mut state = GovernorState::new();
    state.capacity_forecast.binding_window = "five_hour".to_string();
    seed_window(
        &mut state.capacity_forecast.five_hour,
        binding_utilization,
        binding_hours,
        binding_safe,
    );
    seed_window(&mut state.capacity_forecast.seven_day, 60.0, 40.0, None);
    seed_window(
        &mut state.capacity_forecast.weekly_scoped,
        55.0,
        100.0,
        None,
    );
    state
}

fn disabled_alerts() -> AlertConfig {
    AlertConfig {
        enabled: false,
        ..Default::default()
    }
}

/// Opus priced above sonnet so cost-priority sorting is deterministic.
fn priced_governor_config() -> GovernorConfig {
    let mut models = HashMap::new();
    models.insert(
        "claude-opus".to_string(),
        ModelPricing {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_write_5m_per_mtok: 18.75,
            cache_write_1h_per_mtok: 30.0,
            cache_read_per_mtok: 1.50,
        },
    );
    models.insert(
        "claude-sonnet".to_string(),
        ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_5m_per_mtok: 3.75,
            cache_write_1h_per_mtok: 6.0,
            cache_read_per_mtok: 0.30,
        },
    );
    GovernorConfig {
        pricing: PricingConfig { models },
        sprint: Default::default(),
        daemon: Default::default(),
        alerts: disabled_alerts(),
        composite_risk: Default::default(),
        cone_scaling: Default::default(),
        agents: Default::default(),
        credentials_path: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_real_act_cycle(
    state_path: &Path,
    agents: &HashMap<String, AgentConfig>,
    pricing_config: &GovernorConfig,
) -> ScalingDecision {
    run_act_cycle(
        state_path,
        false, // dry_run — the reconcile and launch arms only exist on the real path
        2.0,   // hysteresis_band
        10,    // max_up_per_cycle — never the binding constraint in these scenarios
        10,    // max_down_per_cycle
        90.0,  // target_ceiling
        &disabled_alerts(),
        agents,
        0, // pre_scale_minutes (disabled)
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        pricing_config,
        Utc::now(),
    )
    .expect("act cycle should run")
}

// ---------------------------------------------------------------------------
// Fix 3 — safe_worker_count_or_max: Some(0) → 0 (was → current_total)
// ---------------------------------------------------------------------------

#[test]
fn safe_worker_count_some_zero_scales_to_zero_regression() {
    // CLAUDE.md §4: "when the binding window can't afford even one worker, cgov
    // now actually scales to 0 instead of holding capacity". Three workers are
    // running but the binding window affords none of them: the pre-fix held at
    // current_total (3), driving the shared window to a platform cutoff.
    let mut braked = seeded_state(60.0, 30.0, Some(0));
    braked.workers.insert(
        "pool".to_string(),
        state::WorkerState {
            current: 3,
            target: 3,
            min: 0,
            max: 10,
        },
    );

    let target = compute_target_workers(
        &braked,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );
    assert_eq!(
        target, 0,
        "Some(0) safe worker count must scale to 0, not hold at current_total=3"
    );

    // Control: the same fleet with real headroom is not forced to zero — the 0
    // above came from the Some(0) binding, not from a brake, clamp or hold.
    let mut headroom = seeded_state(60.0, 30.0, Some(2));
    headroom.workers.insert(
        "pool".to_string(),
        state::WorkerState {
            current: 3,
            target: 3,
            min: 0,
            max: 10,
        },
    );
    let control = compute_target_workers(
        &headroom,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );
    assert_eq!(control, 2, "control case must scale to the safe count");
}

// ---------------------------------------------------------------------------
// Fix 1 — distribute_workers_by_cost_priority: min_workers floor
// ---------------------------------------------------------------------------

#[test]
fn min_workers_floor_expensive_pool_wins_slot_regression() {
    let env = TempDir::new().expect("temp dir");
    let bin_dir = env.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("bin dir");

    // Census: one sonnet worker running, opus none. Target total 3 → ScaleUp(2).
    let sessions = env.path().join("sessions.txt");
    std::fs::write(&sessions, "cgovtest-sonnet-a\n").expect("sessions fixture");
    install_fake_tmux(&bin_dir, &sessions, &env.path().join("tmux-calls.log"));
    install_launch_stub(&bin_dir);
    let launch_log = env.path().join("launches.log");

    let mut agents = HashMap::new();
    agents.insert(
        "opus".to_string(),
        agent_config(
            &launch_cmd_for(&bin_dir, env.path(), "opus", &launch_log, "claude-opus"),
            "cgovtest-opus-*",
            &env.path().join("hb-opus"),
            1, // min_workers: the expensive pool's guaranteed slot
            1,
            false,
        ),
    );
    agents.insert(
        "sonnet".to_string(),
        agent_config(
            &launch_cmd_for(&bin_dir, env.path(), "sonnet", &launch_log, "claude-sonnet"),
            "cgovtest-sonnet-*",
            &env.path().join("hb-sonnet"),
            0,
            8,
            false,
        ),
    );

    let state = seeded_state(50.0, 8.0, Some(3)); // binding affords exactly 3
    let state_path = env.path().join("governor-state.json");
    state::save_state(&state, &state_path).expect("seed state");

    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    activate_env(&bin_dir, &env.path().join("decisions.jsonl"));

    let decision = run_real_act_cycle(&state_path, &agents, &priced_governor_config());

    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(2),
        "premise: the fleet grows 1 → 3 against a 3-worker budget"
    );
    let mut tags = launch_tags(&launch_log);
    tags.sort();
    assert_eq!(
        tags,
        vec!["opus".to_string(), "sonnet".to_string()],
        "the expensive pool must get its min_workers slot while the cheap agent \
         gets the remainder — pre-fix the cost sort gave the whole delta to sonnet \
         and opus never launched"
    );
}

// ---------------------------------------------------------------------------
// Fix 2 — NoChange arm reconciles the per-agent allocation
// ---------------------------------------------------------------------------

#[test]
fn no_change_still_reconciles_per_agent_allocation_regression() {
    let env = TempDir::new().expect("temp dir");
    let bin_dir = env.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("bin dir");

    // Census: two sonnet workers, zero opus → total 2. The binding window
    // affords exactly 2, so the aggregate is at target and the decision is
    // NoChange — but the allocation violates opus's min_workers floor.
    let sessions = env.path().join("sessions.txt");
    std::fs::write(&sessions, "cgovtest-sonnet-a\ncgovtest-sonnet-b\n").expect("sessions fixture");
    install_fake_tmux(&bin_dir, &sessions, &env.path().join("tmux-calls.log"));
    install_launch_stub(&bin_dir);
    let launch_log = env.path().join("launches.log");

    let mut agents = HashMap::new();
    agents.insert(
        "opus".to_string(),
        agent_config(
            &launch_cmd_for(&bin_dir, env.path(), "opus", &launch_log, "claude-opus"),
            "cgovtest-opus-*",
            &env.path().join("hb-opus"),
            1, // min_workers: violated by the census (opus runs 0), total unchanged
            1,
            false,
        ),
    );
    agents.insert(
        "sonnet".to_string(),
        agent_config(
            &launch_cmd_for(&bin_dir, env.path(), "sonnet", &launch_log, "claude-sonnet"),
            "cgovtest-sonnet-*",
            &env.path().join("hb-sonnet"),
            0,
            8,
            false,
        ),
    );

    let state = seeded_state(50.0, 8.0, Some(2)); // binding affords exactly the current 2
    let state_path = env.path().join("governor-state.json");
    state::save_state(&state, &state_path).expect("seed state");

    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    activate_env(&bin_dir, &env.path().join("decisions.jsonl"));

    let decision = run_real_act_cycle(&state_path, &agents, &priced_governor_config());

    assert_eq!(
        decision,
        ScalingDecision::NoChange,
        "premise: the aggregate total is unchanged (2 running, 2 affordable)"
    );
    assert_eq!(
        launch_tags(&launch_log),
        vec!["opus".to_string()],
        "at a steady total the NoChange arm must still reconcile the allocation \
         and launch the below-floor pool — pre-fix NoChange meant no per-agent \
         action, so a pinned pool never launched until the total moved"
    );
}

// ---------------------------------------------------------------------------
// Fix 4 — apply_underutilization_sprint is wired into the act cycle
// ---------------------------------------------------------------------------

#[test]
fn underutilization_sprint_wired_into_act_cycle_regression() {
    let env = TempDir::new().expect("temp dir");
    let bin_dir = env.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("bin dir");

    // Census: nothing running. Fake `bf` reports one ready bead so the sprint
    // backlog gate passes. Forecast: the binding window is under-used (20%)
    // and resets soon (1.5h < the 2h default), with no cutoff risk anywhere
    // and no safe worker count — so the base target is 0 and ONLY the sprint
    // can lift it.
    let sessions = env.path().join("sessions.txt");
    std::fs::write(&sessions, "").expect("sessions fixture");
    install_fake_tmux(&bin_dir, &sessions, &env.path().join("tmux-calls.log"));
    install_fake_bf(&bin_dir);
    install_launch_stub(&bin_dir);
    let launch_log = env.path().join("launches.log");

    let mut agents = HashMap::new();
    agents.insert(
        "gen".to_string(),
        agent_config(
            &launch_cmd_for(&bin_dir, env.path(), "gen", &launch_log, "claude-sonnet"),
            "cgovtest-gen-*",
            &env.path().join("hb-gen"),
            0,
            4,    // sprint target: max_workers
            true, // subscription pool — the only kind the sprint boosts
        ),
    );

    let state = seeded_state(20.0, 1.5, None); // under-used, resets soon, base target 0
    let state_path = env.path().join("governor-state.json");
    state::save_state(&state, &state_path).expect("seed state");

    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    activate_env(&bin_dir, &env.path().join("decisions.jsonl"));

    let decision = run_real_act_cycle(&state_path, &agents, &priced_governor_config());

    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(4),
        "the under-used, soon-resetting, cutoff-free window must trigger the \
         sprint and boost the target 0 → max_workers=4 — pre-fix \
         check_underutilization_sprint was defined but never called, so the \
         cycle held at NoChange with 0 workers"
    );
    let tags = launch_tags(&launch_log);
    assert_eq!(
        tags.len(),
        4,
        "the sprint boost must reach the executor, not just the log"
    );
    assert!(
        tags.iter().all(|t| t == "gen"),
        "all sprint launches go to the eligible pool"
    );
}
