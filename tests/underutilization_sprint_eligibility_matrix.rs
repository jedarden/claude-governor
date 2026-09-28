//! Underutilization sprint eligibility matrix (bead claudego-9e4c4435).
//!
//! CLAUDE.md §4 pins two guarantees about the sprint: it is actually called in
//! the daemon cycle, and it "boosts an eligible subscription pool toward its
//! maximum" only when a window is under-used, resets soon, and nothing is at
//! cutoff risk. The *wiring* half — the sprint verifiably executing inside a
//! real cycle — is pinned by
//! `underutilization_sprint_wired_into_act_cycle_regression` in
//! `governor_scaling_fixes.rs`; this file pins the *eligibility* half as a
//! matrix driven through the real `run_act_cycle`:
//!
//! - a control with every condition satisfied boosts the fleet to exactly the
//!   pool max, and the boost reaches the executor;
//! - one row per failing condition, each proving that flipping that single
//!   condition off a still-eligible fixture suppresses the boost. Window
//!   conditions: utilization at/above the threshold, hours remaining at/beyond
//!   the limit, hours at/below zero (the window already reset), cutoff risk on
//!   the binding window or on any other window, safe mode active. Pool
//!   conditions: not a subscription pool, no max headroom, launch command
//!   without `--workspace` (nothing to count a backlog from). Backlog gate:
//!   no ready beads, and the boundary where the backlog merely equals the
//!   running census;
//! - the cap: a backlog far above the pool max still boosts to exactly
//!   `max_workers`, and a computed target already above the pool max is left
//!   alone — the sprint neither inflates nor deflates it.
//!
//! Two fixture families are needed because of what a cycle *records*. An
//! at-target hold (decision NoChange with nothing wanted) is deliberately not
//! written to the decision log, and `safe_worker_count_or_hold` computes a
//! hold at the current census when a window carries no safe count — so a
//! suppressed sprint on an empty fleet would leave no record to assert on.
//! The control therefore runs a parked fleet (census 0, safe count None:
//! computed target 0, the sprint the only possible lifter), while every
//! suppression row runs the [`Scenario::SUPPRESSED_BASE`] fixture: three
//! runners, a safe count of 1 (computed target 1), and a backlog above the
//! census. That fixture is sprint-eligible on its own —
//! `the_suppressed_fixture_base_would_boost` proves it — so each row's single
//! flip is what suppresses, and the suppressed cycle ends as a *wanted*
//! down-move (target 1, census 3) that the hysteresis band damps into a
//! recorded Hold carrying `sprint_boost: false`.
//!
//! Hermetic, same recipe as `governor_scaling_fixes.rs`: fake `tmux` (worker
//! census), fake `bf` (sprint backlog), a stub launcher, and the decision log
//! redirected into the temp dir. The pace-block sprint is disabled
//! (`pace_blocks: 0`) throughout so every boost observed here is
//! attributable to the underutilization sprint alone — that mechanism has its
//! own tests.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, ModelPricing,
    PricingConfig, SprintConfig,
};
use claude_governor::governor::{run_act_cycle, ScalingDecision};
use claude_governor::narrator::read_last_decisions_from_path;
use claude_governor::state::{self, GovernorState};

/// The control fleet's single pool. The name must stay clear of the
/// retired-component markers — this file pins the sprint, not the config
/// gate (`retired_sprint_target_config_gate.rs` does that).
const POOL_NAME: &str = "sprint-pool";

/// Serializes every test that swaps PATH / CGOV_DECISIONS_PATH.
static ENV_LOCK: Mutex<()> = Mutex::new(());
/// The PATH this process was launched with, captured before any test swaps it.
static ORIG_PATH: OnceLock<String> = OnceLock::new();

/// Restores the swapped environment when the scenario run ends.
struct EnvGuard {
    path: String,
    decisions: Option<String>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.path);
        match &self.decisions {
            Some(value) => std::env::set_var("CGOV_DECISIONS_PATH", value),
            None => std::env::remove_var("CGOV_DECISIONS_PATH"),
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One scenario's worth of inputs. [`Scenario::ELIGIBLE`] is the parked
/// control — every sprint condition satisfied; the matrix rows each flip
/// exactly one field off [`Scenario::SUPPRESSED_BASE`], the still-eligible
/// fixture they must suppress.
#[derive(Clone, Copy)]
struct Scenario {
    /// Binding (five_hour) window utilization. Sprint wants < 50%.
    utilization: f64,
    /// Binding window hours to reset. Sprint wants 0 < hours < 2.
    hours_remaining: f64,
    /// A window carrying `cutoff_risk` — the fleet-wide sprint veto.
    cutoff_risk_on: Option<&'static str>,
    /// Safe mode: predictions unreliable, so cross-window sprinting blocked.
    safe_mode: bool,
    /// Only a subscription pool may sprint.
    subscription: bool,
    /// The sprint target: the pool's max_workers.
    max_workers: u32,
    /// Whether the launch command carries `--workspace` (the backlog source).
    with_workspace: bool,
    /// Ready beads the fake `bf ready` reports.
    ready_beads: u32,
    /// Sessions the fake tmux census reports for the pool.
    running_sessions: u32,
    /// p75 safe count seeded on the binding window (see `seed_window` — the
    /// cone is wide, so this is what the computed target acts on). `None`
    /// computes a hold at the running census (`safe_worker_count_or_hold`),
    /// so on the parked control it keeps the base target at 0 and the sprint
    /// is the only thing that can lift the fleet.
    safe_target: Option<u32>,
}

impl Scenario {
    /// The fully-eligible control fleet: parked (no sessions), under-used
    /// binding window (20% < 50%) that resets soon (1.5h < 2h), no cutoff
    /// risk anywhere, safe mode off, a subscription pool with headroom
    /// (max 4), a workspace to count, and a backlog above the (zero) census.
    /// With no safe count anywhere the computed target holds at the census —
    /// 0 — so the sprint is the only possible lifter and whatever the cycle
    /// does is attributable to it alone.
    const ELIGIBLE: Self = Scenario {
        utilization: 20.0,
        hours_remaining: 1.5,
        cutoff_risk_on: None,
        safe_mode: false,
        subscription: true,
        max_workers: 4,
        with_workspace: true,
        ready_beads: 2,
        running_sessions: 0,
        safe_target: None,
    };

    /// The suppression rows' base: sprint-eligible by construction (see
    /// `the_suppressed_fixture_base_would_boost`) but shaped so a *suppressed*
    /// cycle still writes an auditable record. Three runners with a computed
    /// target of 1 leave a wanted down-move that the hysteresis band (2.0)
    /// damps into a recorded Hold — an at-target hold would write nothing —
    /// and a backlog strictly above the census so the backlog gate is never
    /// the reason a row suppresses (the rows that pin that gate flip
    /// `ready_beads` themselves).
    const SUPPRESSED_BASE: Self = Scenario {
        ready_beads: 4,
        running_sessions: 3,
        safe_target: Some(1),
        ..Scenario::ELIGIBLE
    };
}

/// Every single-condition failure the sprint must respect, one row each. The
/// driver asserts each row suppresses the boost the base fixture proves would
/// otherwise fire.
fn matrix() -> Vec<(&'static str, Scenario)> {
    vec![
        (
            "utilization exactly at the 50% threshold is not under-use",
            Scenario {
                utilization: 50.0,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "utilization above the threshold is not under-use",
            Scenario {
                utilization: 55.0,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "hours remaining exactly at the 2h limit is not soon enough",
            Scenario {
                hours_remaining: 2.0,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "hours remaining beyond the 2h limit resets too late to bother",
            Scenario {
                hours_remaining: 3.0,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "a window at its reset hour (0h remaining) cannot sprint",
            Scenario {
                hours_remaining: 0.0,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "a window past its reset (negative hours) cannot sprint",
            Scenario {
                hours_remaining: -1.0,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "cutoff risk on the binding window vetoes every sprint",
            Scenario {
                cutoff_risk_on: Some("five_hour"),
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "cutoff risk on any other window vetoes every sprint",
            Scenario {
                cutoff_risk_on: Some("seven_day"),
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "safe mode active blocks cross-window sprinting",
            Scenario {
                safe_mode: true,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "only a subscription pool may sprint",
            Scenario {
                subscription: false,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "a pool with no max headroom has nothing to boost toward",
            Scenario {
                max_workers: 0,
                // Fixture plumbing, not a second eligibility flip: with the
                // envelope max clamped to 0 the computed target collapses to
                // 0 regardless of the safe count, and a census of 3 would
                // turn the suppressed cycle into an executed shed (delta -3,
                // outside the band) instead of the recordable hold the
                // assertion reads. Two runners sit inside the band
                // (|0 - 2| = 2) while the backlog gate still passes
                // (4 ready > 2 running), so the max-0 pool skip is what
                // suppresses the boost.
                running_sessions: 2,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "a launch command without --workspace has no backlog to count",
            Scenario {
                with_workspace: false,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "no ready beads means nothing for an extra runner to do",
            Scenario {
                ready_beads: 0,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
        (
            "a backlog merely equal to the running census is not a backlog",
            Scenario {
                ready_beads: 3,
                ..Scenario::SUPPRESSED_BASE
            },
        ),
    ]
}

fn write_executable(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod script");
}

/// Build an `AgentConfig` from JSON rather than a struct literal: this file
/// must compile against both the committed tree and in-flight trees that add
/// optional fields, and serde tolerates both (same reason
/// `governor_scaling_fixes.rs` does this).
fn agent_config(
    scenario: Scenario,
    launch_stub: &Path,
    workspace: &Path,
    launch_log: &Path,
) -> AgentConfig {
    let launch_cmd = if scenario.with_workspace {
        format!(
            "{} --workspace {} sprint-pool {} --agent claude-sonnet",
            launch_stub.display(),
            workspace.display(),
            launch_log.display()
        )
    } else {
        format!(
            "{} sprint-pool {} --agent claude-sonnet",
            launch_stub.display(),
            launch_log.display()
        )
    };
    serde_json::from_value(serde_json::json!({
        "launch_cmd": launch_cmd,
        "session_pattern": "cgovtest-sprint-*",
        "heartbeat_dir": "/tmp/cgov-sprint-matrix-no-heartbeats",
        "min_workers": 0,
        "max_workers": scenario.max_workers,
        "subscription": scenario.subscription,
    }))
    .expect("agent config fixture should deserialize")
}

/// Wide-cone window fixture. `cone_ratio = 2.0` is at/above the default
/// `narrow_threshold` (1.5), so `compute_target_workers` acts on the p75
/// [`state::WindowForecast::safe_worker_count_p75`] — which is where
/// `safe_target` lands. The p50 `safe_worker_count` stays `None` on every
/// window, and that split is load-bearing: the executor's per-pool
/// window-affinity cap ([`pool_affinity_ceiling`] via
/// `distribute_workers_by_cost_priority`) reads the p50 safe counts of every
/// window the pool consumes, so seeding the p50 would cap the pool's
/// allocation at the same 1 the computed target holds at and the boosted
/// worker would never reach the executor. Leaving it unset keeps the ceiling
/// at `max_workers`, where the sprint's boost is observable.
fn seed_window(
    win: &mut state::WindowForecast,
    utilization: f64,
    hours_remaining: f64,
    cutoff_risk: bool,
    safe: Option<u32>,
) {
    win.current_utilization = utilization;
    win.hours_remaining = hours_remaining;
    win.cutoff_risk = cutoff_risk;
    win.safe_worker_count = None;
    win.safe_worker_count_p75 = safe;
    win.cone_ratio = 2.0;
}

/// The scenario's forecast: the binding five_hour window carries the row's
/// utilization/hours/cutoff (and the seeded p75 safe count), the other two
/// sit comfortably away from every trigger unless the row puts cutoff risk on
/// them and carry no safe count at all.
fn scenario_state(scenario: Scenario) -> GovernorState {
    let mut state = GovernorState::new();
    state.capacity_forecast.binding_window = "five_hour".to_string();
    seed_window(
        &mut state.capacity_forecast.five_hour,
        scenario.utilization,
        scenario.hours_remaining,
        scenario.cutoff_risk_on == Some("five_hour"),
        scenario.safe_target,
    );
    seed_window(
        &mut state.capacity_forecast.seven_day,
        60.0,
        40.0,
        scenario.cutoff_risk_on == Some("seven_day"),
        None,
    );
    seed_window(
        &mut state.capacity_forecast.weekly_scoped,
        55.0,
        100.0,
        scenario.cutoff_risk_on == Some("weekly_scoped"),
        None,
    );
    if scenario.safe_mode {
        state.safe_mode.active = true;
    }
    state
}

/// Opus priced above sonnet so cost-priority sorting is deterministic.
fn governor_config(agents: &HashMap<String, AgentConfig>) -> GovernorConfig {
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
        // pace_blocks: 0 disables the pace-block sprint (see
        // SprintConfig::pace_blocks) so every boost below is attributable to
        // the underutilization sprint alone.
        sprint: SprintConfig {
            pace_blocks: 0,
            ..Default::default()
        },
        daemon: Default::default(),
        alerts: AlertConfig {
            enabled: false,
            ..Default::default()
        },
        composite_risk: Default::default(),
        cone_scaling: Default::default(),
        agents: agents.clone(),
        credentials_path: None,
    }
}

/// What one scenario run produced. `_env` keeps the temp dir (and with it the
/// decision log the context was read from) alive for the assertions.
struct Outcome {
    decision: ScalingDecision,
    context: serde_json::Value,
    launches: Vec<String>,
    _env: TempDir,
}

/// Run one scenario through the real act cycle, hermetically. `label` only
/// decorates failures so a broken scenario names itself.
fn run_scenario(label: &str, scenario: Scenario) -> Outcome {
    let _env_lock = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let env = TempDir::new().expect("temp dir");
    let bin_dir = env.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("bin dir");

    // Census fixture: one tmux session per running worker.
    let sessions_path = env.path().join("sessions.txt");
    let census: String = (1..=scenario.running_sessions)
        .map(|i| format!("cgovtest-sprint-{i}\n"))
        .collect();
    std::fs::write(&sessions_path, census).expect("sessions fixture");
    write_executable(
        &bin_dir,
        "tmux",
        &format!(
            "case \"$1\" in\n  list-sessions) cat '{}';;\n  *) printf '%s\\n' \"$*\" >> '{}';;\nesac\nexit 0",
            sessions_path.display(),
            env.path().join("tmux-calls.log").display()
        ),
    );

    // Backlog fixture: one `bf-` line per ready bead — exactly what
    // count_ready_beads counts.
    let backlog = (1..=scenario.ready_beads)
        .map(|i| format!("bf-{i:06} ready"))
        .collect::<Vec<_>>()
        .join("\n");
    write_executable(&bin_dir, "bf", &format!("cat <<'BFEOD'\n{backlog}\nBFEOD"));

    // Launch stub: appends the pool tag once per worker actually launched.
    let launch_log = env.path().join("launches.log");
    write_executable(&bin_dir, "launch-stub", "echo \"$3\" >> \"$4\"");
    // The live executor checks disk before launch; keep this harness focused on
    // sprint eligibility rather than inheriting the host's filesystem usage.
    write_executable(
        &bin_dir,
        "df",
        "printf '%s\\n' 'Filesystem 1024-blocks Used Available Capacity Mounted on' 'fixture 100 10 90 10% /'",
    );

    let mut agents = HashMap::new();
    agents.insert(
        POOL_NAME.to_string(),
        agent_config(
            scenario,
            &bin_dir.join("launch-stub"),
            env.path(),
            &launch_log,
        ),
    );

    let state = scenario_state(scenario);
    let state_path = env.path().join("governor-state.json");
    state::save_state(&state, &state_path).expect("seed state");

    let decisions_path = env.path().join("decisions.jsonl");
    let old_path = ORIG_PATH
        .get_or_init(|| std::env::var("PATH").unwrap_or_default())
        .clone();
    let old_decisions = std::env::var("CGOV_DECISIONS_PATH").ok();
    std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), old_path));
    std::env::set_var("CGOV_DECISIONS_PATH", &decisions_path);
    let guard = EnvGuard {
        path: old_path,
        decisions: old_decisions,
    };

    let decision = run_act_cycle(
        &state_path,
        false, // dry_run — the sprint and launch arms only exist on the real path
        2.0,   // hysteresis_band
        10,    // max_up_per_cycle — never the binding constraint here
        10,    // max_down_per_cycle
        90.0,  // target_ceiling
        &AlertConfig {
            enabled: false,
            ..Default::default()
        },
        &agents,
        0, // pre_scale_minutes (disabled)
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &governor_config(&agents),
        Utc::now(),
    )
    .expect("act cycle should run");
    drop(guard);

    let context = read_last_decisions_from_path(1, &decisions_path)
        .unwrap_or_else(|e| {
            panic!(
                "sprint matrix [{label}]: decision log should be readable: {e}\nraw:\n{:?}",
                std::fs::read_to_string(&decisions_path).unwrap_or_default()
            )
        })
        .into_iter()
        .next()
        .and_then(|entry| entry.context)
        .unwrap_or_else(|| {
            panic!(
                "sprint matrix [{label}]: act cycle should record a decision context\nraw:\n{:?}",
                std::fs::read_to_string(&decisions_path).unwrap_or_default()
            )
        });
    let launches = std::fs::read_to_string(&launch_log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();

    Outcome {
        decision,
        context,
        launches,
        _env: env,
    }
}

/// A suppressed row shows the same four facts: the cycle holds (the wanted
/// down-move is band-damped, never executed), the cycle's own account records
/// no sprint boost, and no runner launched.
fn assert_suppressed(condition: &str, outcome: &Outcome) {
    assert_eq!(
        outcome.decision,
        ScalingDecision::NoChange,
        "sprint matrix [{condition}]: the boost must be suppressed into a hold, got {:?}",
        outcome.decision
    );
    assert_eq!(
        outcome.context["sprint_boost"],
        serde_json::json!(false),
        "sprint matrix [{condition}]: the cycle must report no sprint boost: {}",
        outcome.context
    );
    assert!(
        outcome.launches.is_empty(),
        "sprint matrix [{condition}]: no runner may launch on a suppressed sprint: {:?}",
        outcome.launches
    );
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

#[test]
fn control_with_every_condition_satisfied_boosts_to_pool_max() {
    let outcome = run_scenario("control", Scenario::ELIGIBLE);
    assert_eq!(
        outcome.decision,
        ScalingDecision::ScaleUp(4),
        "the fully-eligible control must boost the fleet to the pool max — \
         if this fails, the rows below prove nothing"
    );
    assert_eq!(
        outcome.context["sprint_boost"],
        serde_json::json!(true),
        "the cycle's own account must record the sprint boost: {}",
        outcome.context
    );
    assert_eq!(
        outcome.context["effective_target"],
        serde_json::json!(4),
        "the boost must bind at the pool max: {}",
        outcome.context
    );
    assert_eq!(
        outcome.launches.len(),
        4,
        "the boost must reach the executor, not just the decision log"
    );
    assert!(
        outcome.launches.iter().all(|tag| tag == POOL_NAME),
        "every sprint launch goes to the eligible pool: {:?}",
        outcome.launches
    );
}

// ---------------------------------------------------------------------------
// The counterfactual anchor for the suppression family
// ---------------------------------------------------------------------------

#[test]
fn the_suppressed_fixture_base_would_boost() {
    // The suppression rows each flip ONE field off this base. Run unflipped,
    // it must boost — a pool at 3 of 4 workers, an under-used soon-resetting
    // window, four ready beads behind three runners — or the rows built on
    // it would pass vacuously. The boost target is still the pool max (4);
    // with 3 runners already up the cycle asks for the one missing worker.
    let outcome = run_scenario("suppressed base, unflipped", Scenario::SUPPRESSED_BASE);
    assert_eq!(
        outcome.decision,
        ScalingDecision::ScaleUp(1),
        "the unflipped base fixture is sprint-eligible: 3 runners, target 4 — \
         the boost supplies the one missing worker"
    );
    assert_eq!(
        outcome.context["sprint_boost"],
        serde_json::json!(true),
        "the base fixture's boost must be recorded as a sprint boost: {}",
        outcome.context
    );
    assert_eq!(
        outcome.context["effective_target"],
        serde_json::json!(4),
        "the boost must bind at the pool max even from 3 runners: {}",
        outcome.context
    );
    assert_eq!(
        outcome.launches.len(),
        1,
        "exactly the missing worker launches: {:?}\ncontext: {}\ntmux calls: {:?}",
        outcome.launches,
        outcome.context,
        std::fs::read_to_string(outcome._env.path().join("tmux-calls.log")).unwrap_or_default()
    );
}

// ---------------------------------------------------------------------------
// The matrix: each single failing condition suppresses the boost
// ---------------------------------------------------------------------------

#[test]
fn each_single_failing_condition_suppresses_the_boost() {
    for (condition, scenario) in matrix() {
        let outcome = run_scenario(condition, scenario);
        assert_suppressed(condition, &outcome);
    }
}

// ---------------------------------------------------------------------------
// The cap: the boost lands at the pool max and never past it
// ---------------------------------------------------------------------------

#[test]
fn the_boost_caps_at_the_pool_max_even_with_a_huge_backlog() {
    let outcome = run_scenario(
        "cap: backlog far above the pool max",
        Scenario {
            ready_beads: 25,
            ..Scenario::ELIGIBLE
        },
    );
    assert_eq!(
        outcome.decision,
        ScalingDecision::ScaleUp(4),
        "25 ready beads cannot push the boost past max_workers=4 — the sprint \
         target is the pool max, not the backlog size"
    );
    assert_eq!(
        outcome.context["sprint_boost"],
        serde_json::json!(true),
        "the huge backlog must not suppress the boost either: {}",
        outcome.context
    );
    assert_eq!(
        outcome.launches.len(),
        4,
        "exactly max_workers runners launch, one per unit of boost"
    );
}

#[test]
fn a_computed_target_at_or_above_the_pool_max_records_no_sprint_boost() {
    // Every window's safe count is 6, so the computed base target already
    // meets the sprint's would-be destination (the pool max, 4). The sprint
    // arm is still *eligible* here — under-used window, resets soon, no
    // cutoff risk — but max(base, pool_max) == base, so it must raise
    // nothing and claim nothing: the fleet lands on the computed target as
    // the aggregate envelope clamps it (one 4-max pool), with
    // sprint_boost false in the cycle's own account.
    let outcome = run_scenario(
        "computed target already at the pool max",
        Scenario {
            safe_target: Some(6),
            ..Scenario::ELIGIBLE
        },
    );
    assert_eq!(
        outcome.decision,
        ScalingDecision::ScaleUp(4),
        "the fleet scales to the capacity-bound computed target — the sprint \
         adds nothing it cannot add"
    );
    assert_eq!(
        outcome.context["sprint_boost"],
        serde_json::json!(false),
        "a sprint that cannot raise the target must not claim one: {}",
        outcome.context
    );
    assert_eq!(
        outcome.context["effective_target"],
        serde_json::json!(4),
        "the effective target is the computed-and-clamped one, untouched by \
         the sprint: {}",
        outcome.context
    );
}
