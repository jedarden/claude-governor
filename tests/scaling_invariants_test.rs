//! Named unit tests for the documented scaling invariants.
//!
//! Source of truth: `docs/hysteresis-and-smooth-scaling.md` ("Design Notes —
//! The Band Is a Down-Side Cushion, Not a Dead Zone") and CLAUDE.md §4 ("The
//! cgov code fixes"). Each invariant documented there gets a named test here:
//!
//! 1. **Every deficit closes** — the hysteresis band damps scale-DOWN only; a
//!    deficit of any size, including 1 worker, always produces `ScaleUp`.
//! 2. **Emergency brake overrides everything** — `target == 0` short-circuits
//!    band and caps (`EmergencyBrake`), and `compute_target_workers` returns 0
//!    when any window sits at the brake threshold.
//! 3. **Safe mode widens the band 2.0x** (entry 15% / exit 8% median absolute
//!    error) without ever holding back a deficit.
//! 4. **The min_workers floor is guaranteed** before the remainder is
//!    cost-distributed across pools.
//! 5. **`safe_worker_count = Some(0)` maps to target 0** — the fleet actually
//!    scales to zero rather than holding capacity.
//! 6. **The NoChange arm still reconciles per-agent allocation** when the
//!    aggregate total is unchanged.
//! 7. **Progressive caps converge to target without overshoot.**
//!
//! This file is the enumerative layer — one named test per invariant, each
//! sweeping the parameter space the invariant quantifies over. Edge-case and
//! sequence coverage lives where it already was: `hysteresis_smooth_scaling_
//! test.rs` (invariants 1, 2, 7), the `#[cfg(test)]` module in
//! `src/governor.rs` (`distribute_enforces_min_workers_for_expensive_pool`,
//! `safe_worker_count_some_zero_scales_to_zero`), and
//! `explain_decisions_test.rs` (act-cycle wiring). New substance here: the
//! safe-mode entry/exit thresholds and band-multiplier wiring (3), the
//! pub-surface floor and steady-total reconcile through `run_act_cycle`
//! (4, 6), and the `Some(0)` → 0 target computation (5).
//!
//! Agent configs are built by deserializing only the long-stable fields
//! (serde fills the rest with their defaults) so this file does not depend
//! on any in-flight `AgentConfig` field additions.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use tempfile::TempDir;

use claude_governor::calibrator::CalibrationStats;
use claude_governor::config::{
    AgentConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{
    apply_scaling, compute_target_workers, progressive_scale_cap, run_act_cycle,
    update_safe_mode_from_calibration, ScalingDecision,
};
use claude_governor::narrator::read_last_decisions_from_path;
use claude_governor::state;

/// The decision-log path override is process-global; tests that run the act
/// cycle (which consults it) serialize on this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A window forecast with safe counts and comfortable margins — nothing here
/// can fire the emergency brake or cutoff alerts.
fn window(utilization: f64, safe: Option<u32>, safe_p75: Option<u32>) -> state::WindowForecast {
    state::WindowForecast {
        current_utilization: utilization,
        margin_hrs: 50.0,
        hours_remaining: 100.0,
        safe_worker_count: safe,
        safe_worker_count_p75: safe_p75,
        binding: true,
        ..Default::default()
    }
}

/// A state whose weekly_scoped window binds with the given safe counts; the
/// other two windows are strictly roomier so they cannot bind over it.
fn state_with_weekly_binding(safe: Option<u32>, safe_p75: Option<u32>) -> state::GovernorState {
    let mut s = state::GovernorState::new();
    s.capacity_forecast = state::CapacityForecast {
        five_hour: window(30.0, safe.map(|w| w + 2), safe_p75.map(|w| w + 2)),
        seven_day: window(25.0, safe.map(|w| w + 1), safe_p75.map(|w| w + 1)),
        weekly_scoped: window(20.0, safe, safe_p75),
        binding_window: "weekly_scoped".to_string(),
        ..Default::default()
    };
    s
}

/// An agent config whose session pattern cannot collide with a real tmux
/// session on the host, so the act-cycle census (tmux-based) always counts
/// zero workers for it. Built by deserialization so only stable fields are
/// named (see the module doc).
fn inert_agent(name: &str, min_workers: u32, max_workers: u32, model: &str) -> AgentConfig {
    let json = format!(
        r#"{{"launch_cmd": "needle run --agent {model} --workspace /tmp/cgov-invariants/{name}",
             "session_pattern": "cgov-invariants-{name}-*",
             "heartbeat_dir": "/tmp/cgov-invariants-heartbeats/{name}",
             "min_workers": {min_workers}, "max_workers": {max_workers}}}"#
    );
    serde_json::from_str(&json).expect("valid agent config")
}

/// A minimal pricing config (empty model table — tests that need a cost
/// ordering supply it through `state.burn_rate`, which takes priority).
fn config_with_agents(agents: &HashMap<String, AgentConfig>) -> GovernorConfig {
    GovernorConfig {
        pricing: PricingConfig {
            models: HashMap::new(),
        },
        sprint: Default::default(),
        daemon: Default::default(),
        alerts: Default::default(),
        composite_risk: Default::default(),
        cone_scaling: Default::default(),
        agents: agents.clone(),
        credentials_path: None,
    }
}

/// A one-agent map keyed the way the governor keys pools (agent name).
fn single_agent(name: &str, min: u32, max: u32, model: &str) -> HashMap<String, AgentConfig> {
    let mut agents = HashMap::new();
    agents.insert(name.to_string(), inert_agent(name, min, max, model));
    agents
}

/// Run one dry act cycle (hermetic: nothing launched or killed) against a
/// fixture state, with the decision log redirected into the temp dir.
/// Returns the decision; the caller re-loads the state file from `temp_dir`
/// to observe what the cycle persisted.
fn run_dry_act_cycle(
    temp_dir: &Path,
    state_to_write: &state::GovernorState,
    agents: &HashMap<String, AgentConfig>,
    hysteresis: f64,
    max_up: u32,
    max_down: u32,
) -> ScalingDecision {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let decisions_path = temp_dir.join("decisions.jsonl");
    std::env::set_var("CGOV_DECISIONS_PATH", &decisions_path);

    let state_path = temp_dir.join("governor-state.json");
    std::fs::write(
        &state_path,
        serde_json::to_string_pretty(state_to_write).unwrap(),
    )
    .expect("Failed to write state fixture");

    let pricing_config = config_with_agents(agents);
    let decision = run_act_cycle(
        &state_path,
        true, // dry_run — nothing is launched or killed
        hysteresis,
        max_up,
        max_down,
        90.0, // target ceiling
        &Default::default(),
        agents,
        30,  // pre-scale minutes
        &[], // promotions
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &pricing_config,
        Utc::now(),
    )
    .expect("act cycle should succeed");

    std::env::remove_var("CGOV_DECISIONS_PATH");
    decision
}

fn decisions_of(temp_dir: &Path) -> Vec<claude_governor::narrator::DecisionEntry> {
    read_last_decisions_from_path(10, &temp_dir.join("decisions.jsonl")).unwrap()
}

// ---------------------------------------------------------------------------
// Invariant 1: every deficit closes; the band damps scale-down only
// ---------------------------------------------------------------------------

/// `target > current` ⇒ `ScaleUp(min(gap, max_up))` for ANY band value — the
/// up-side has no dead zone, so no band can strand the fleet below target.
#[test]
fn invariant_1_every_deficit_closes_for_any_band_and_gap() {
    let bands = [0.0f64, 0.5, 1.0, 2.0, 3.7, 10.0];
    let cap = 3;

    for band in bands {
        for current in 0u32..=6 {
            for gap in 1u32..=6 {
                let target = current + gap;
                let decision = apply_scaling(target, current, band, cap, 2);
                assert_eq!(
                    decision,
                    ScalingDecision::ScaleUp(gap.min(cap)),
                    "deficit of {} from current {} must close under band {} (deficits are never band-damped)",
                    gap,
                    current,
                    band
                );
            }
        }
    }
}

/// The complement: the band DOES apply below current. A surplus within
/// `hysteresis_band` (integer part) holds; beyond it the fleet sheds, capped
/// by `max_down_per_cycle`. This asymmetry is the whole design — the band is
/// a down-side cushion, not a dead zone.
#[test]
fn invariant_1_band_cushions_scale_down_only() {
    let band = 2.0f64;
    let cushion = band as u32; // apply_scaling floors the band for the down-side compare

    // current 10, surpluses 1..=6 → targets 9..=4: never zero, so the
    // emergency brake (a separate invariant) cannot fire here.
    for surplus in 1u32..=6 {
        let decision = apply_scaling(10 - surplus, 10, band, 3, 2);
        if surplus <= cushion {
            assert_eq!(
                decision,
                ScalingDecision::NoChange,
                "surplus of {} within band {} is the down-side cushion — it holds",
                surplus,
                band
            );
        } else {
            assert_eq!(
                decision,
                ScalingDecision::ScaleDown(surplus.min(2)),
                "surplus of {} beyond band {} sheds (capped by max_down)",
                surplus,
                band
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Invariant 2: the emergency brake overrides everything
// ---------------------------------------------------------------------------

/// `target == 0` with live workers is `EmergencyBrake` regardless of band or
/// per-cycle caps — the brake check runs before the hysteresis and
/// rate-limit logic entirely.
#[test]
fn invariant_2_zero_target_brakes_regardless_of_band_and_caps() {
    for band in [0.0f64, 1.0, 5.0, 100.0] {
        for current in 1u32..=10 {
            for (max_up, max_down) in [(0, 0), (1, 1), (10, 10)] {
                assert_eq!(
                    apply_scaling(0, current, band, max_up, max_down),
                    ScalingDecision::EmergencyBrake,
                    "target 0 vs current {} must brake (band {}, caps {}/{})",
                    current,
                    band,
                    max_up,
                    max_down
                );
            }
        }
    }
}

/// `compute_target_workers` returns 0 the moment ANY window (not just the
/// binding one) reaches the 98% brake threshold, even when the binding
/// window's own safe count is generous.
#[test]
fn invariant_2_compute_target_brakes_on_any_window_at_threshold() {
    let mut s = state_with_weekly_binding(Some(2), Some(2));
    s.workers.insert(
        "w1".to_string(),
        state::WorkerState {
            current: 4,
            target: 4,
            min: 0,
            max: 8,
        },
    );

    // seven_day (not the binding window) slams into the brake threshold.
    s.capacity_forecast.seven_day.current_utilization = 98.0;
    assert_eq!(
        compute_target_workers(
            &s,
            90.0,
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default()
        ),
        0,
        "any window at 98% forces target 0 — the brake overrides the binding window's headroom"
    );

    // Just below the threshold there is no brake and the binding window's
    // safe count governs again.
    s.capacity_forecast.seven_day.current_utilization = 97.9;
    assert_eq!(
        compute_target_workers(
            &s,
            90.0,
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default()
        ),
        2,
        "below 98% the binding window's safe_worker_count is the target"
    );
}

// ---------------------------------------------------------------------------
// Invariant 3: safe mode — entry 15%, exit 8%, band widened 2.0x,
// deficits still never held back
// ---------------------------------------------------------------------------

fn stats(total_samples: u32, median_error: f64) -> CalibrationStats {
    CalibrationStats {
        total_samples,
        median_error,
        median_error_7ds: median_error,
        ..Default::default()
    }
}

/// Entry: median ABSOLUTE error strictly above 15 pct points with at least
/// SAFE_MODE_MIN_SAMPLES (5) samples. Below either condition there is no
/// entry.
#[test]
fn invariant_3a_safe_mode_entry_threshold() {
    let now = Utc::now();

    // Just above the threshold, with enough samples: enters.
    let mut safe_mode = state::SafeModeState::default();
    let mut calibration = state::CalibrationState::default();
    assert!(
        update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(10, 15.1), now),
        "error 15.1 > 15 with 10 samples must enter safe mode"
    );
    assert!(safe_mode.active);
    assert_eq!(safe_mode.trigger.as_deref(), Some("median_error"));
    assert_eq!(
        safe_mode.scored_at_entry, 10,
        "entry stamps the sample count"
    );

    // The error is |median_error| — a large negative error is just as bad.
    let mut safe_mode = state::SafeModeState::default();
    let mut calibration = state::CalibrationState::default();
    assert!(
        update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(10, -16.0), now),
        "median absolute error 16.0 must enter safe mode"
    );

    // Exactly 15.0 does not enter (strict >).
    let mut safe_mode = state::SafeModeState::default();
    let mut calibration = state::CalibrationState::default();
    assert!(
        !update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(10, 15.0), now),
        "error exactly at the 15.0 threshold must not enter"
    );
    assert!(!safe_mode.active);

    // Too few samples (< 5) does not enter, however bad the error.
    let mut safe_mode = state::SafeModeState::default();
    let mut calibration = state::CalibrationState::default();
    assert!(
        !update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(4, 40.0), now),
        "4 samples is below SAFE_MODE_MIN_SAMPLES — no entry"
    );
    assert!(!safe_mode.active);
}

/// Exit: median absolute error below 8 (the hysteresis gap below entry) AND
/// at least SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT (3) predictions scored since
/// entry. The 8–15 gap holds safe mode on; a brake-triggered safe mode is
/// never released by calibration at all.
#[test]
fn invariant_3b_safe_mode_exit_threshold_and_gates() {
    let now = Utc::now();
    let active_median_error = || state::SafeModeState {
        active: true,
        entered_at: Some(now),
        trigger: Some("median_error".to_string()),
        median_error_at_entry: Some(16.0),
        predictions_since_entry: 0,
        scored_at_entry: 10,
    };

    // Recovered past the exit threshold with enough new predictions: exits.
    let mut safe_mode = active_median_error();
    let mut calibration = state::CalibrationState::default();
    assert!(
        update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(15, 7.9), now),
        "error 7.9 < 8 with 5 predictions since entry must exit"
    );
    assert!(!safe_mode.active, "exit resets the state");

    // Inside the 8–15 hysteresis gap: holds — this is the anti-flap window.
    let mut safe_mode = active_median_error();
    let mut calibration = state::CalibrationState::default();
    assert!(
        !update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(15, 12.0), now),
        "error 12.0 sits in the 8–15 exit hysteresis gap — must stay in safe mode"
    );
    assert!(safe_mode.active);

    // Error recovered but not enough new predictions since entry: holds.
    let mut safe_mode = active_median_error();
    let mut calibration = state::CalibrationState::default();
    assert!(
        !update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(12, 7.0), now),
        "only 2 predictions since entry (< 3) — no exit on stale evidence"
    );
    assert!(safe_mode.active);
    assert_eq!(
        safe_mode.predictions_since_entry, 2,
        "the since-entry counter tracks the shortfall"
    );

    // A brake-triggered safe mode is released by utilization recovery, not by
    // calibration accuracy — however good the forecast looks.
    let mut safe_mode = state::SafeModeState {
        trigger: Some(state::EMERGENCY_BRAKE_TRIGGER.to_string()),
        ..active_median_error()
    };
    let mut calibration = state::CalibrationState::default();
    assert!(
        !update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats(15, 7.9), now),
        "emergency-brake safe mode must not exit via the calibration path"
    );
    assert!(safe_mode.active);
}

/// The widened band itself still damps scale-down only: with the band doubled
/// (base 1.0 → 2.0), a 2-worker surplus now sits inside the cushion and
/// holds, while deficits still close. Pure-function half of the invariant
/// (see 3d for the act-cycle wiring).
#[test]
fn invariant_3c_widened_band_still_only_damps_scale_down() {
    let base = 1.0;
    let widened = base * 2.0;

    // Deficits close under the widened band...
    for gap in 1u32..=3 {
        assert_eq!(
            apply_scaling(gap, 0, widened, 3, 2),
            ScalingDecision::ScaleUp(gap.min(3)),
            "deficit {} closes under the widened band",
            gap
        );
    }

    // ...but the widened cushion now also absorbs a 2-worker surplus that the
    // base band would have shed.
    assert_eq!(
        apply_scaling(3, 5, base, 3, 2),
        ScalingDecision::ScaleDown(2),
        "base band 1.0: a 2-worker surplus sheds"
    );
    assert_eq!(
        apply_scaling(3, 5, widened, 3, 2),
        ScalingDecision::NoChange,
        "widened band 2.0: the same 2-worker surplus is inside the cushion and holds"
    );
}

/// Act-cycle wiring: with `safe_mode.active` in the loaded state, the cycle
/// multiplies the configured band by 2.0 (capped at 10) and uses the widened
/// value in `apply_scaling` — recorded in the decision log's context — while
/// a deficit still closes. Safe mode also forces the conservative p75
/// estimate, so the computed target comes from `safe_worker_count_p75`.
#[test]
fn invariant_3d_act_cycle_widens_band_under_safe_mode() {
    let agents = single_agent("agent", 0, 8, "claude-sonnet");

    let mut s = state_with_weekly_binding(Some(2), Some(1));
    s.safe_mode = state::SafeModeState {
        active: true,
        entered_at: Some(Utc::now()),
        trigger: Some("median_error".to_string()),
        median_error_at_entry: Some(16.0),
        predictions_since_entry: 4,
        scored_at_entry: 10,
    };
    s.workers.insert(
        "agent".to_string(),
        state::WorkerState {
            current: 0,
            target: 0,
            min: 0,
            max: 8,
        },
    );

    let temp = TempDir::new().unwrap();
    let decision = run_dry_act_cycle(temp.path(), &s, &agents, 1.0, 10, 10);

    // The widened band never holds back the deficit: safe mode forced the
    // conservative p75 target of 1, and the 1-worker deficit from census-zero
    // still closes even though the effective band (2.0) exceeds the gap.
    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(1),
        "safe mode widens the band, not the deficit handling"
    );

    let entries = decisions_of(temp.path());
    assert_eq!(entries.len(), 1, "the convergence is recorded");
    let ctx = entries[0].context.as_ref().expect("decision context");
    assert_eq!(
        ctx["hysteresis_band"],
        serde_json::json!(2.0),
        "configured band 1.0 widened to 2.0 under safe mode"
    );
    assert_eq!(ctx["safe_mode"], serde_json::json!(true));
    assert_eq!(
        ctx["computed_target"],
        serde_json::json!(1),
        "safe mode forces the p75 conservative estimate (1), not the p50 (2)"
    );

    // The widening is capped at 10: a configured band of 6.0 would double to
    // 12.0 but records — and applies — 10.0.
    let temp = TempDir::new().unwrap();
    let decision = run_dry_act_cycle(temp.path(), &s, &agents, 6.0, 10, 10);
    assert_eq!(decision, ScalingDecision::ScaleUp(1), "still closes");
    let entries = decisions_of(temp.path());
    let ctx = entries[0].context.as_ref().expect("decision context");
    assert_eq!(
        ctx["hysteresis_band"],
        serde_json::json!(10.0),
        "6.0 * 2.0 = 12.0 clamps to the 10.0 cap"
    );
}

// ---------------------------------------------------------------------------
// Invariant 4: each agent's min_workers floor is guaranteed before the
// remainder is cost-distributed
// ---------------------------------------------------------------------------

/// End-to-end through `run_act_cycle`: an expensive dedicated pool with
/// `min_workers = 1, max_workers = 1` must be allocated its guaranteed
/// worker even though the cheap pool wins the cost sort outright. The pure
/// cost pass fills the cheap pool first; the floor pass then funds the
/// expensive pool's minimum from it — the distribution the act cycle
/// persists as per-agent targets. (Mirrors
/// `distribute_enforces_min_workers_for_expensive_pool` in the private
/// internal test module, through the public surface.)
#[test]
fn invariant_4_min_workers_floor_survives_cost_priority() {
    let mut agents = HashMap::new();
    agents.insert("opus".to_string(), inert_agent("opus", 1, 1, "claude-opus"));
    agents.insert(
        "sonnet".to_string(),
        inert_agent("sonnet", 0, 8, "claude-sonnet"),
    );

    // Binding window affords 2 workers total; census sees 0 for both pools.
    let mut s = state_with_weekly_binding(Some(2), Some(2));
    // Distinct per-worker costs so the cost sort is unambiguous: empirical
    // burn rates take priority over the (empty) pricing table.
    s.burn_rate.by_model.insert(
        "claude-opus".to_string(),
        state::ModelBurnRate {
            pct_per_worker_per_hour: 0.0,
            dollars_per_worker_per_hour: 60.0,
            samples: 100,
        },
    );
    s.burn_rate.by_model.insert(
        "claude-sonnet".to_string(),
        state::ModelBurnRate {
            pct_per_worker_per_hour: 0.0,
            dollars_per_worker_per_hour: 4.0,
            samples: 100,
        },
    );
    for name in agents.keys() {
        s.workers.insert(
            name.clone(),
            state::WorkerState {
                current: 0,
                target: 0,
                min: 0,
                max: 8,
            },
        );
    }

    let temp = TempDir::new().unwrap();
    let decision = run_dry_act_cycle(temp.path(), &s, &agents, 1.0, 10, 10);

    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(2),
        "target 2 vs census 0 scales the aggregate up by 2"
    );

    // The persisted per-agent targets carry the floor: the expensive pool
    // gets exactly its guaranteed worker, funded out of the cheap pool's
    // cost-priority allocation.
    let persisted = state::load_state(&temp.path().join("governor-state.json")).unwrap();
    let expensive = persisted.workers.get("opus").expect("opus pool present");
    let cheap = persisted
        .workers
        .get("sonnet")
        .expect("sonnet pool present");
    assert_eq!(
        expensive.target, 1,
        "the min_workers=1 floor is guaranteed despite losing the cost sort"
    );
    assert_eq!(
        cheap.target, 1,
        "the remainder is cost-distributed to the cheap pool"
    );
    assert_eq!(expensive.target + cheap.target, 2, "total matches target");
}

// ---------------------------------------------------------------------------
// Invariant 5: safe_worker_count Some(0) → target 0 — actually scales to zero
// ---------------------------------------------------------------------------

/// `Some(0)` on the binding window means even one worker exhausts it before
/// reset: the target is 0 (was `current_total` before the fix — the fleet
/// held capacity that drove the window to a platform cutoff). A configured
/// floor still claims its workers; `Some(0)` → 0 holds where no floor
/// contradicts it.
#[test]
fn invariant_5_safe_count_zero_targets_zero_workers() {
    let mut s = state_with_weekly_binding(Some(0), Some(0));
    s.workers.insert(
        "w1".to_string(),
        state::WorkerState {
            current: 3,
            target: 3,
            min: 0,
            max: 8,
        },
    );

    assert_eq!(
        compute_target_workers(
            &s,
            90.0,
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default()
        ),
        0,
        "Some(0) maps to target 0 — scale to zero and let the window recover"
    );

    // With a configured floor the clamp claims it: min of mins = 2.
    s.workers.insert(
        "w1".to_string(),
        state::WorkerState {
            current: 3,
            target: 3,
            min: 2,
            max: 8,
        },
    );
    assert_eq!(
        compute_target_workers(
            &s,
            90.0,
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default()
        ),
        2,
        "per-agent min bounds still apply on top of the safe count"
    );

    // And a target of 0 against live workers is the emergency brake, not a
    // ramp-down — the executor takes the fleet straight to zero.
    assert_eq!(
        apply_scaling(0, 3, 1.0, 2, 2),
        ScalingDecision::EmergencyBrake
    );
}

// ---------------------------------------------------------------------------
// Invariant 6: the NoChange arm still reconciles per-agent allocation
// ---------------------------------------------------------------------------

/// At a steady total the cycle must still re-derive each pool's target from
/// the priority distribution — a NoChange decision must not leave stale
/// per-agent targets in the persisted state. (The executor half of the
/// invariant — actually moving a worker between live pools at a steady
/// nonzero total — is the same `distribute_workers_by_cost_priority` call
/// invariant 4 exercises; running it against live sessions is not hermetic
/// and is covered by the internal-module distribution tests.)
#[test]
fn invariant_6_nochange_cycle_still_rewrites_per_agent_targets() {
    let agents = single_agent("agent", 0, 8, "claude-sonnet");

    // Target 0 (safe_worker_count Some(0)), census 0: the aggregate decision
    // is NoChange at target. The fixture carries a STALE target of 5 — if the
    // NoChange arm skipped the distribution, that 5 would survive the cycle.
    let mut s = state_with_weekly_binding(Some(0), Some(0));
    s.workers.insert(
        "agent".to_string(),
        state::WorkerState {
            current: 0,
            target: 5,
            min: 0,
            max: 8,
        },
    );

    let temp = TempDir::new().unwrap();
    let decision = run_dry_act_cycle(temp.path(), &s, &agents, 1.0, 10, 10);

    assert_eq!(decision, ScalingDecision::NoChange, "already at target 0");

    // At-target holds are suppressed in the decision log...
    let entries = decisions_of(temp.path());
    assert!(entries.is_empty(), "at-target NoChange records nothing");

    // ...but the per-agent targets in the persisted state were still
    // reconciled against the distribution — the stale 5 is gone.
    let persisted = state::load_state(&temp.path().join("governor-state.json")).unwrap();
    assert_eq!(
        persisted.workers["agent"].target, 0,
        "the NoChange arm re-derived the per-agent target instead of leaving the stale value"
    );
    assert_eq!(persisted.workers["agent"].current, 0);
}

// ---------------------------------------------------------------------------
// Invariant 7: progressive caps converge to target without overshoot
// ---------------------------------------------------------------------------

/// `progressive_scale_cap` never exceeds the remaining gap (no overshoot)
/// and never widens the operator's cap by more than 3x; iterating
/// `apply_scaling` with the widened cap converges EXACTLY to target for
/// every start/target/base-cap combination.
#[test]
fn invariant_7_progressive_caps_converge_without_overshoot() {
    // Structural: cap bounds, over a grid.
    for base in 0u32..=4 {
        for gap in 0u32..=12 {
            let cap = progressive_scale_cap(base, gap);
            assert!(
                cap <= gap,
                "base {} gap {}: cap {} overshoots the gap",
                base,
                gap,
                cap
            );
            assert!(
                cap <= base.saturating_mul(3),
                "base {} gap {}: cap {} exceeds 3x the operator cap",
                base,
                gap,
                cap
            );
        }
    }

    // Behavioural: simulated cycles converge exactly, never past target.
    let scenarios = [(0u32, 7u32), (5, 15), (3, 12), (9, 10), (0, 1)];
    for base_cap in 1u32..=4 {
        for &(start, target) in &scenarios {
            let mut current = start;
            let mut steps = 0;
            while current < target {
                let cap = progressive_scale_cap(base_cap, target - current);
                match apply_scaling(target, current, 1.0, cap, cap) {
                    ScalingDecision::ScaleUp(n) => {
                        current += n;
                        assert!(
                            current <= target,
                            "base {} start {} target {}: overshot to {}",
                            base_cap,
                            start,
                            target,
                            current
                        );
                    }
                    other => panic!(
                        "deficit must close (base {} start {} target {}): {:?}",
                        base_cap, start, target, other
                    ),
                }
                steps += 1;
                assert!(
                    steps < 50,
                    "did not converge: base {} start {} target {}",
                    base_cap,
                    start,
                    target
                );
            }
            assert_eq!(current, target, "converges exactly to target");
            assert_eq!(
                apply_scaling(target, current, 1.0, base_cap, base_cap),
                ScalingDecision::NoChange,
                "at target the decision settles"
            );
        }
    }
}
