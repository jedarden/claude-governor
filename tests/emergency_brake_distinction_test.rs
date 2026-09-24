//! Boundary tests for the emergency brake and the hysteresis path it deliberately
//! does not share.
//!
//! The contract under test is the documented distinction between a **real usage
//! window at or above 98%** (`first_brake_window`, the only signal that selects
//! the violent arm) and an **ordinary computed target of 0**
//! (`safe_worker_count = Some(0)`, a duty-cycle withdrawal that travels the
//! graceful, band-damped, per-cycle-capped `ScaleDown` path) — see the
//! `apply_scaling_with_policy` and `safe_worker_count_or_hold` doc comments.
//!
//! Groups:
//! 1. The 98% boundary itself at the brake state machine (`>=`, not `>`), and
//!    the any-window clear semantics.
//! 2. The distinction as the act cycle wires it: same zero target, opposite
//!    decisions, decided solely by whether a window actually crossed.
//! 3. Exact hysteresis band boundaries, including the `as i32` truncation of a
//!    fractional band on the down side.
//! 4. Per-cycle caps on the graceful withdrawal — including that the brake is
//!    NOT weakened by the cushion holding the last worker.
//! 5. Manual override boundaries: expiry instant, suspension from any window,
//!    and a pin's band bypass that still honours the down cap.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{
    apply_scaling, compute_target_workers, resolve_manual_override, ManualOverrideResolution,
    ScalingDecision, UsageSnapshot, EMERGENCY_BRAKE_THRESHOLD,
};
use claude_governor::state::{self, ManualOverride, WorkerState};

// The brake state machine lives on `governor::GovernorState`; the persisted
// state the target computation and override resolution read is
// `state::GovernorState`. Alias so both can be named unambiguously.
use claude_governor::governor::GovernorState as BrakeState;

/// Mirror of the act cycle's brake-flag wiring (`first_brake_window` is
/// deliberately private): a real signal exists when ANY of the three windows
/// sits at or above the threshold. Keeping the scan visible here also pins
/// its any-window shape.
fn brake_window_present(forecast: &state::CapacityForecast) -> bool {
    [
        forecast.five_hour.current_utilization,
        forecast.seven_day.current_utilization,
        forecast.weekly_scoped.current_utilization,
    ]
    .iter()
    .any(|&util| util >= EMERGENCY_BRAKE_THRESHOLD)
}

// ---------------------------------------------------------------------------
// Group 1 — the 98% boundary at the brake state machine
// ---------------------------------------------------------------------------

/// The threshold is inclusive: a window AT `EMERGENCY_BRAKE_THRESHOLD` engages
/// the brake (`utilization >= THRESHOLD`). Pins the exact boundary — 97.99 must
/// not engage (next test), 98.0 must.
#[test]
fn brake_engages_at_exactly_98_percent() {
    let mut brake_state = BrakeState::new();
    brake_state.add_agent("pool", 5, false);

    let usage = UsageSnapshot::from_windows(EMERGENCY_BRAKE_THRESHOLD, 50.0, 50.0);
    let brake = brake_state.check_emergency_brake(&usage);

    let brake = brake.expect("a window at exactly the threshold must engage the brake");
    assert_eq!(brake.triggered_window, "five_hour");
    assert_eq!(brake.utilization_pct, EMERGENCY_BRAKE_THRESHOLD);
    assert!(brake_state.emergency_brake_active);
    // Engaging the brake sheds everything immediately — no ramp, no cap.
    assert_eq!(brake_state.agents["pool"].workers, 0);
}

/// Just below the threshold nothing happens: workers keep running and no brake
/// event is recorded.
#[test]
fn brake_stays_off_just_below_98_percent() {
    let mut brake_state = BrakeState::new();
    brake_state.add_agent("pool", 5, false);

    let usage = UsageSnapshot::from_windows(97.99, 50.0, 50.0);
    let brake = brake_state.check_emergency_brake(&usage);

    assert!(brake.is_none(), "97.99% is below the threshold");
    assert!(!brake_state.emergency_brake_active);
    assert_eq!(brake_state.agents["pool"].workers, 5);
}

/// Clearing requires EVERY window below the threshold: one window holding at
/// exactly 98% keeps the brake engaged even while the others cooled off.
#[test]
fn brake_holds_while_any_window_remains_at_threshold() {
    let mut brake_state = BrakeState::new();
    brake_state.add_agent("pool", 5, false);
    brake_state
        .check_emergency_brake(&UsageSnapshot::from_windows(98.5, 50.0, 50.0))
        .expect("engage first");

    // five_hour cooled, but weekly_scoped is now the one at the threshold.
    let cleared = brake_state
        .clear_emergency_brake(&UsageSnapshot::from_windows(
            50.0,
            50.0,
            EMERGENCY_BRAKE_THRESHOLD,
        ));

    assert!(
        !cleared,
        "any window at/above the threshold keeps the brake engaged"
    );
    assert!(brake_state.emergency_brake_active);
    assert!(brake_state.emergency_brake.is_some(), "event retained");
}

/// The release side of the same boundary: once every window is strictly below,
/// the brake clears and the event is dropped.
#[test]
fn brake_releases_only_when_every_window_drops_below_threshold() {
    let mut brake_state = BrakeState::new();
    brake_state.add_agent("pool", 0, false);
    brake_state
        .check_emergency_brake(&UsageSnapshot::from_windows(98.5, 60.0, 55.0))
        .expect("engage first");

    let cleared = brake_state
        .clear_emergency_brake(&UsageSnapshot::from_windows(50.0, 60.0, 97.99));

    assert!(cleared);
    assert!(!brake_state.emergency_brake_active);
    assert!(brake_state.emergency_brake.is_none());
}

/// `update_emergency_brake` across the boundary: engage at 98.5, hold while a
/// window sits at exactly 98.0, release only when all windows drop.
#[test]
fn update_emergency_brake_lifecycle_across_the_boundary() {
    let mut brake_state = BrakeState::new();
    brake_state.add_agent("pool", 4, false);

    // Engage.
    let engaged = brake_state.update_emergency_brake(&UsageSnapshot::from_windows(
        98.5, 50.0, 50.0,
    ));
    assert!(engaged.is_some());
    assert!(brake_state.emergency_brake_active);

    // A window at exactly the threshold: the attempted clear is refused and
    // the brake stays engaged (the original event is kept).
    let held = brake_state.update_emergency_brake(&UsageSnapshot::from_windows(
        50.0,
        50.0,
        EMERGENCY_BRAKE_THRESHOLD,
    ));
    assert!(held.is_some(), "98.0 on any window keeps the brake held");
    assert!(brake_state.emergency_brake_active);

    // All windows strictly below: released.
    let released = brake_state.update_emergency_brake(&UsageSnapshot::from_windows(
        50.0, 50.0, 97.99,
    ));
    assert!(released.is_none());
    assert!(!brake_state.emergency_brake_active);
    assert!(brake_state.emergency_brake.is_none());
}

// ---------------------------------------------------------------------------
// Group 2 — the documented distinction, wired as the act cycle wires it
// ---------------------------------------------------------------------------

/// A forecast state with the binding window at `util` reporting `safe` workers.
/// Worker entry min is 0 so a computed zero is not clamped back up to a floor.
fn forecast_state(util: f64, safe: u32) -> state::GovernorState {
    let mut st = state::GovernorState::new();
    let mut five = state::WindowForecast::default();
    five.current_utilization = util;
    five.safe_worker_count = Some(safe);
    let mut seven = state::WindowForecast::default();
    seven.current_utilization = 50.0;
    seven.safe_worker_count = Some(5);
    let mut weekly = state::WindowForecast::default();
    weekly.current_utilization = 50.0;
    weekly.safe_worker_count = Some(5);
    st.capacity_forecast = state::CapacityForecast {
        five_hour: five,
        seven_day: seven,
        weekly_scoped: weekly,
        binding_window: "five_hour".to_string(),
        ..Default::default()
    };
    st.workers.insert(
        "pool".to_string(),
        WorkerState {
            current: 5,
            target: 5,
            min: 0,
            max: 8,
        },
    );
    st
}

/// The brake scan covers ALL windows, not just the binding one: a 98.0 reading
/// on `weekly_scoped` alone forces the target to 0 even though the binding
/// window's own safe count is a comfortable 5.
#[test]
fn weekly_scoped_window_at_exactly_98_forces_zero_target() {
    let mut st = forecast_state(50.0, 5);
    st.capacity_forecast.weekly_scoped.current_utilization = EMERGENCY_BRAKE_THRESHOLD;

    let target = compute_target_workers(
        &st,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );

    assert_eq!(
        target, 0,
        "a >= 98% window anywhere drives the target to 0, regardless of the binding window"
    );
}

/// THE distinction, at the target-computation level: the binding window's
/// `safe_worker_count = Some(0)` — an honest "even one worker exhausts the
/// window" verdict with NO window at 98% — computes a zero target that the act
/// cycle's wiring (brake flag = `first_brake_window(...).is_some()`) turns into
/// a graceful, capped `ScaleDown`. Not the brake.
#[test]
fn computed_zero_without_a_brake_window_is_a_graceful_scale_down() {
    let st = forecast_state(50.0, 0);

    let target = compute_target_workers(
        &st,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );
    assert_eq!(target, 0, "Some(0) on the binding window targets zero");

    // Wired exactly as run_act_cycle wires it (brake flag from the forecast).
    let brake_active = brake_window_present(&st.capacity_forecast);
    let decision = apply_scaling(target, 5, 1.0, 3, 2, brake_active);

    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(2),
        "a computed zero with no >= 98% window is a duty-cycle withdrawal: \
         graceful ScaleDown capped by max_down_per_cycle"
    );
}

/// The complement, byte-for-byte the same zero target: with a window actually
/// AT the threshold the identical wiring selects the violent arm instead.
#[test]
fn same_zero_target_with_a_real_98_window_takes_the_brake() {
    let st = forecast_state(EMERGENCY_BRAKE_THRESHOLD, 0);

    let target = compute_target_workers(
        &st,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );
    assert_eq!(target, 0);

    let brake_active = brake_window_present(&st.capacity_forecast);
    assert!(brake_active, "the window at 98.0 is the real signal");

    let decision = apply_scaling(target, 5, 1.0, 3, 2, brake_active);
    assert_eq!(
        decision,
        ScalingDecision::EmergencyBrake,
        "identical computed target — only the real window selects kill-sessions"
    );
}

/// The brake arm's guard shape: the flag alone fires nothing. A nonzero target
/// with the flag set falls through to normal scaling, and a zero target with no
/// workers running has nothing to kill.
#[test]
fn brake_flag_without_a_zero_target_does_not_fire_the_violent_arm() {
    let decision = apply_scaling(5, 3, 1.0, 3, 2, true);
    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(2),
        "the violent arm requires target == 0; otherwise the flag is inert"
    );

    let decision = apply_scaling(0, 0, 1.0, 3, 2, true);
    assert_eq!(
        decision, ScalingDecision::NoChange,
        "nothing to brake at current 0"
    );
}

// ---------------------------------------------------------------------------
// Group 3 — exact hysteresis boundaries
// ---------------------------------------------------------------------------

/// Down-side boundary pair at band 2: a surplus of exactly the band holds (the
/// cushion), one worker beyond it moves (uncapped here, to isolate the band
/// from the per-cycle cap).
#[test]
fn scale_down_holds_at_exactly_the_band_and_moves_one_beyond() {
    let held = apply_scaling(3, 5, 2.0, 10, 10, false);
    assert_eq!(
        held, ScalingDecision::NoChange,
        "gap == band is the intended down-side cushion"
    );

    let moved = apply_scaling(2, 5, 2.0, 10, 10, false);
    assert_eq!(
        moved,
        ScalingDecision::ScaleDown(3),
        "gap == band + 1 must move, by the full gap when the cap allows"
    );
}

/// A fractional band truncates on the down side (`hysteresis_band as i32`):
/// band 1.9 behaves as band 1 — a 1-worker surplus holds, a 2-worker surplus
/// moves. Pins the integer truncation so a future f64 comparison is a
/// deliberate change, not an accident.
#[test]
fn fractional_band_truncates_on_the_down_side() {
    let held = apply_scaling(4, 5, 1.9, 10, 10, false);
    assert_eq!(held, ScalingDecision::NoChange, "1 <= trunc(1.9)");

    let moved = apply_scaling(3, 5, 1.9, 10, 10, false);
    assert_eq!(moved, ScalingDecision::ScaleDown(2), "2 > trunc(1.9)");
}

/// The asymmetry at its extreme: a band of 5 cannot hold even a 1-worker
/// deficit. Unused capacity is capacity that resets unused.
#[test]
fn any_deficit_closes_regardless_of_band() {
    let decision = apply_scaling(6, 5, 5.0, 10, 10, false);
    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(1),
        "the band damps scale-down only — any deficit closes"
    );
}

// ---------------------------------------------------------------------------
// Group 4 — per-cycle caps on the graceful withdrawal
// ---------------------------------------------------------------------------

/// A computed zero withdraws at `max_scale_down_per_cycle` per cycle and lands
/// exactly on zero — no overshoot, no stall.
#[test]
fn computed_zero_ramps_to_zero_within_the_down_cap() {
    let mut current = 7u32;
    let mut steps = Vec::new();
    for _ in 0..10 {
        match apply_scaling(0, current, 0.0, 3, 3, false) {
            ScalingDecision::ScaleDown(n) => {
                assert!(
                    n <= 3,
                    "every withdrawal step honours max_scale_down_per_cycle"
                );
                current -= n;
                steps.push(current);
            }
            ScalingDecision::NoChange => break,
            other => panic!("graceful withdrawal took the wrong arm: {:?}", other),
        }
    }

    assert_eq!(
        steps,
        vec![4, 1, 0],
        "7 workers exit 3-at-a-time and land exactly on zero"
    );
    assert_eq!(current, 0);
}

/// The cushion holds the LAST worker of a computed withdrawal — and the brake
/// is not weakened by that: the identical shape with a real 98% window takes
/// the violent arm instead of honouring the cushion.
#[test]
fn the_cushion_holds_the_last_worker_but_the_brake_takes_it() {
    let held = apply_scaling(0, 1, 1.0, 3, 2, false);
    assert_eq!(
        held, ScalingDecision::NoChange,
        "a 1-worker surplus inside the band is forecast-noise cushion, not a withdrawal"
    );

    let braked = apply_scaling(0, 1, 1.0, 3, 2, true);
    assert_eq!(
        braked,
        ScalingDecision::EmergencyBrake,
        "a real >= 98% window bypasses hysteresis, caps, and the cushion entirely"
    );
}

// ---------------------------------------------------------------------------
// Group 5 — manual override boundaries
// ---------------------------------------------------------------------------

fn pinned_state(target: u32, expires_at: Option<DateTime<Utc>>) -> state::GovernorState {
    let mut st = state::GovernorState::new();
    st.workers.insert(
        "pool".to_string(),
        WorkerState {
            current: 3,
            target: 3,
            min: 0,
            max: 8,
        },
    );
    st.manual_override = Some(ManualOverride {
        target,
        set_at: Utc::now() - Duration::minutes(1),
        expires_at,
        source: "cli".to_string(),
    });
    st
}

/// The expiry comparison is `expires_at <= now`: a pin expires AT its expiry
/// instant, not after it. One second earlier it still binds.
#[test]
fn override_expires_at_exactly_its_expiry_instant() {
    let now = Utc::now();

    let mut just_expired = pinned_state(3, Some(now));
    assert_eq!(
        resolve_manual_override(&mut just_expired, now),
        ManualOverrideResolution::ExpiredOrAbsent,
        "expires_at == now is expired"
    );
    assert!(
        just_expired.manual_override.is_none(),
        "expiry clears the field so the act-owned save persists the drop"
    );

    let mut still_binding = pinned_state(3, Some(now + Duration::seconds(1)));
    assert_eq!(
        resolve_manual_override(&mut still_binding, now),
        ManualOverrideResolution::Applied { applied_target: 3 }
    );
    assert!(still_binding.manual_override.is_some());
}

/// The brake suspends a pin from ANY window at the threshold — here
/// `weekly_scoped` alone — and the pin stays stored for resumption.
#[test]
fn override_is_suspended_by_a_brake_on_any_window() {
    let mut st = pinned_state(3, Some(Utc::now() + Duration::hours(1)));
    st.capacity_forecast.weekly_scoped.current_utilization = EMERGENCY_BRAKE_THRESHOLD;

    assert_eq!(
        resolve_manual_override(&mut st, Utc::now()),
        ManualOverrideResolution::SuspendedByBrake,
        "the brake scan is not five_hour-only"
    );
    assert!(
        st.manual_override.is_some(),
        "suspension keeps the pin; it resumes when the brake clears"
    );
}

// ---------------------------------------------------------------------------
// Group 5b — the same boundaries through the real act cycle
// ---------------------------------------------------------------------------

static ENV_LOCK: Mutex<()> = Mutex::new(());

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

/// Fake tmux/bf environment so `run_act_cycle` runs its real census and
/// executor without touching the operator's fleet. `sessions` is the literal
/// tmux `list-sessions` output — one `pool-N` line per live worker.
fn fake_environment(dir: &TempDir, sessions: &str) -> (EnvGuard, PathBuf, PathBuf) {
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let sessions_path = dir.path().join("sessions");
    std::fs::write(&sessions_path, sessions).unwrap();
    let decisions_path = dir.path().join("decisions.jsonl");

    let tmux = format!(
        "#!/bin/sh\ncase \"$1\" in\n  list-sessions) [ -f '{sessions}' ] && cat '{sessions}';;\n  has-session) [ -s '{sessions}' ];;\n  send-keys) : > '{sessions}';;\n  *) :;;\nesac\n",
        sessions = sessions_path.display()
    );
    write_executable(&bin.join("tmux"), &tmux);
    write_executable(&bin.join("bf"), "#!/bin/sh\necho 'bf-fake0001 ready'\n");
    write_executable(&bin.join("launch-stub"), "#!/bin/sh\nexit 0\n");

    let old_path = std::env::var("PATH").unwrap_or_default();
    let old_decisions = std::env::var("CGOV_DECISIONS_PATH").ok();
    std::env::set_var("PATH", format!("{}:{}", bin.display(), old_path));
    std::env::set_var("CGOV_DECISIONS_PATH", &decisions_path);

    (
        EnvGuard {
            path: old_path,
            decisions: old_decisions,
        },
        sessions_path,
        decisions_path,
    )
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn agent(name: &str, min_workers: u32, max_workers: u32) -> AgentConfig {
    serde_json::from_value(serde_json::json!({
        "launch_cmd": "launch-stub",
        "session_pattern": format!("{name}-*"),
        "heartbeat_dir": "/tmp/cgov-brake-distinction-no-heartbeats",
        "min_workers": min_workers,
        "max_workers": max_workers,
        "subscription": false,
    }))
    .unwrap()
}

fn governor_config(agents: &HashMap<String, AgentConfig>) -> GovernorConfig {
    GovernorConfig {
        pricing: PricingConfig {
            models: HashMap::new(),
        },
        sprint: Default::default(),
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

/// All three windows reporting `safe` workers at `util`% — comfortable unless
/// the caller overrides utilization.
fn act_state(safe: u32, five_util: f64) -> state::GovernorState {
    let mut st = state::GovernorState::new();
    let mut five = state::WindowForecast::default();
    five.current_utilization = five_util;
    five.hours_remaining = 100.0; // far from reset: no sprint, no pre-scale
    five.safe_worker_count = Some(safe);
    five.safe_worker_count_p75 = Some(safe);
    five.cone_ratio = 0.0;
    let seven = five.clone();
    let weekly = five.clone();
    st.capacity_forecast = state::CapacityForecast {
        five_hour: five,
        seven_day: seven,
        weekly_scoped: weekly,
        binding_window: "five_hour".to_string(),
        ..Default::default()
    };
    st
}

fn pool_worker(st: &mut state::GovernorState, current: u32) {
    st.workers.insert(
        "pool".to_string(),
        WorkerState {
            current,
            target: current,
            min: 0,
            max: 8,
        },
    );
}

fn five_sessions() -> &'static str {
    "pool-1\npool-2\npool-3\npool-4\npool-5\n"
}

fn run_act(
    path: &Path,
    dry_run: bool,
    hysteresis: f64,
    max_up: u32,
    max_down: u32,
    agents: &HashMap<String, AgentConfig>,
    config: &GovernorConfig,
) -> ScalingDecision {
    claude_governor::governor::run_act_cycle(
        path,
        dry_run,
        hysteresis,
        max_up,
        max_down,
        90.0,
        &AlertConfig {
            enabled: false,
            ..Default::default()
        },
        agents,
        0,
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        config,
        Utc::now(),
    )
    .unwrap()
}

/// End to end: a computed zero (Some(0) on the binding window, no window near
/// the threshold) runs the GRACEFUL path through the real cycle — capped
/// partial scale-down, target recorded, computed_target decision source. The
/// fleet is NOT zeroed and no brake is reported.
#[test]
fn a_computed_zero_runs_the_graceful_path_through_the_real_cycle() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, decisions) = fake_environment(&dir, five_sessions());

    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), agent("pool", 0, 8));
    let config = governor_config(&agents);
    let mut st = act_state(0, 10.0);
    pool_worker(&mut st, 5);
    let state_path = dir.path().join("governor-state.json");
    state::save_state(&st, &state_path).unwrap();

    let decision = run_act(&state_path, true, 2.0, 3, 2, &agents, &config);

    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(2),
        "computed zero + no brake window: capped graceful scale-down"
    );
    let after = state::load_state(&state_path).unwrap();
    assert_eq!(
        after.workers["pool"].target, 3,
        "in-flight work finishes: the move paces, it does not zero the fleet"
    );

    let context = read_first_decision_context(&decisions);
    assert_eq!(context["decision_source"], "computed_target");
    assert_eq!(context["effective_target"], 0);
    assert_eq!(context["sprint_boost"], false);
}

/// The same cycle with a window actually at 98%: the brake arm, audited as
/// such, with no override present to suspend.
#[test]
fn a_real_98_window_runs_the_brake_through_the_real_cycle() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, decisions) = fake_environment(&dir, five_sessions());

    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), agent("pool", 0, 8));
    let config = governor_config(&agents);
    let mut st = act_state(0, EMERGENCY_BRAKE_THRESHOLD);
    pool_worker(&mut st, 5);
    let state_path = dir.path().join("governor-state.json");
    state::save_state(&st, &state_path).unwrap();

    let decision = run_act(&state_path, true, 2.0, 3, 2, &agents, &config);

    assert_eq!(
        decision,
        ScalingDecision::EmergencyBrake,
        "identical computed zero — the real window alone selects the brake"
    );
    assert_eq!(
        read_first_decision_context(&decisions)["decision_source"],
        "emergency_brake"
    );
}

/// A pin's band bypass, end to end: the computed path holds a 1-worker surplus
/// inside the band, the pin to zero moves the same fleet — but still at the
/// per-cycle down cap, never the whole gap at once, and never via the brake.
#[test]
fn a_pin_bypasses_the_band_but_still_honours_the_down_cap() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, decisions) = fake_environment(&dir, five_sessions());

    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), agent("pool", 0, 8));
    let config = governor_config(&agents);

    // Control — no pin: computed target 4 vs current 5 is a 1-worker surplus,
    // inside the 5.0 band. The computed path holds.
    let mut held = act_state(4, 10.0);
    pool_worker(&mut held, 5);
    let held_path = dir.path().join("held-state.json");
    state::save_state(&held, &held_path).unwrap();
    assert_eq!(
        run_act(&held_path, true, 5.0, 10, 2, &agents, &config),
        ScalingDecision::NoChange,
        "the band damps the computed 1-worker surplus"
    );

    // The pin: same fleet, same band — the override bypasses it but the move
    // is still capped at 2 (not the full gap of 5), and it is not the brake.
    let mut pinned = act_state(4, 10.0);
    pool_worker(&mut pinned, 5);
    pinned.manual_override = Some(ManualOverride {
        target: 0,
        set_at: Utc::now() - Duration::minutes(1),
        expires_at: None,
        source: "cli".to_string(),
    });
    let pinned_path = dir.path().join("pinned-state.json");
    state::save_state(&pinned, &pinned_path).unwrap();

    let decision = run_act(&pinned_path, true, 5.0, 10, 2, &agents, &config);
    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(2),
        "override bypasses the band AND honours max_scale_down_per_cycle"
    );
    let after = state::load_state(&pinned_path).unwrap();
    assert_eq!(after.workers["pool"].target, 3);
    assert_eq!(
        after.manual_override.unwrap().target, 0,
        "a hold-until-clear pin survives the cycle it drove"
    );
    assert_eq!(
        read_first_decision_context(&decisions)["decision_source"],
        "manual_override"
    );
}

fn read_first_decision_context(decisions_path: &PathBuf) -> serde_json::Value {
    let entries =
        claude_governor::narrator::read_last_decisions_from_path(10, decisions_path).unwrap();
    entries
        .first()
        .and_then(|entry| entry.context.clone())
        .expect("act cycle should record a decision context")
}
