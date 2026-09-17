//! End-to-end verification that an idle fleet plus an active operator session
//! does not scale to 0 (claudego-6d7a0e82, verification clause of
//! claudego-0ccbae3c).
//!
//! The 2026-09-07 incident this replays: zero NEEDLE workers, the operator's
//! own Opus session burning 12–14%/hr, and `cgov forecast` flagging BOTH
//! weekly-scale windows CUTOFF_RISK with margins of −26.4h/−27.8h — holding
//! the pool at 0 even though the 7d window could sustain ~0.97%/hr to reset.
//!
//! The fixture is the collector's shape: one synthetic token history per
//! session, the operator's carrying the raw CC session UUID in `session` and
//! `worker: None` (what attribution writes for a session no needle process
//! dispatched), replayed through `estimate_burn_rates` — the pipeline the
//! attribution fix (claudego-892a82b1) lives in — and through the two
//! `governor` seams `run_observe_cycle` uses to size and risk the pool while
//! idle.
//!
//! Verified clauses:
//! - no fleet CUTOFF_RISK anywhere under operator-only burn;
//! - `safe_worker_count` >= 1 wherever real headroom — net of the operator's
//!   reservation — supports one worker for the remaining window;
//! - the operator's burn is still respected as a constant budget reservation
//!   (the fix must not over-correct into ignoring it); and
//! - the fleet-attributable variant of the same history STILL trips
//!   CUTOFF_RISK.
//!
//! Unit-level pins for the classification primitive, the offset arithmetic
//! and the reserved `<exogenous>` key live in `src/burn_rate.rs`; the
//! semantics themselves are documented in
//! `docs/notes/burn-attribution-semantics.md`.

use std::collections::HashMap;

use claude_governor::burn_rate::{
    estimate_burn_rates, generate_window_forecast, BaselineBurnRates, InstanceRecord,
    ModelWindowEma, WindowUtilization,
};
use claude_governor::governor::{effective_fleet_pct_rate, per_worker_pct_for_sizing};
use claude_governor::state::EstimateQuality;

/// The operator's interactive model in the incident (their own Opus session).
const OPERATOR_MODEL: &str = "claude-opus-5";

/// A raw CC session UUID — what the collector writes to `sess` for ANY
/// session, operator or worker alike (claudego-a542d686). The operator's
/// record is distinguished by `worker: None`, not by the session id.
const OPERATOR_SESSION_UUID: &str = "c9594b9f-f9f3-4c1b-8f02-2f290a3db022";

/// The needle worker session name the collector stamps on a dispatched
/// session, and the governor.yaml glob that claims it as this pool's own.
const FLEET_WORKER: &str = "needle-claude-print-cgov-opus-0";
const FLEET_WORKER_PATTERN: &str = "needle-claude-print-cgov-*";

fn fleet_patterns() -> Vec<String> {
    vec![FLEET_WORKER_PATTERN.to_string()]
}

/// One collector interval for the operator's session. The headline incident
/// number is the 13%/hr the Opus session showed on the 5h window; the same
/// token stream is ~1/10 of that in the weekly windows' units (1% of a 168h
/// window is ~34x the capacity of 1% of the 5h window), which is the shape
/// the API actually reported on 2026-09-07.
fn operator_record() -> InstanceRecord {
    InstanceRecord {
        session: OPERATOR_SESSION_UUID.to_string(),
        worker: None,
        model: OPERATOR_MODEL.to_string(),
        total_usd: 4.5,
        total_tokens: 500_000,
        windows: vec![
            WindowUtilization::from_pct_delta("five_hour", Some(13.0), 60.0, 47.0),
            WindowUtilization::from_pct_delta("seven_day", Some(1.3), 30.0, 28.7),
            WindowUtilization::from_pct_delta("weekly_scoped", Some(1.4), 40.0, 38.6),
        ],
    }
}

/// The fleet-attributable variant of the same history: the identical deltas,
/// but stamped with a worker name the session_pattern globs match, so every
/// rate classifies FLEET.
fn fleet_record() -> InstanceRecord {
    let mut record = operator_record();
    record.session = "1e40ad47-6155-4c14-855a-c32f721d7cce".to_string();
    record.worker = Some(FLEET_WORKER.to_string());
    record.model = "claude-sonnet-5".to_string();
    record
}

/// Utilization / hours-remaining snapshots replaying the incident's account
/// state against a 90% target ceiling: the 5h window live at 60% with 2h to
/// reset, both weekly-scale windows well below ceiling with ~90h left.
fn snapshots() -> (HashMap<String, f64>, HashMap<String, f64>) {
    let mut utilization = HashMap::new();
    utilization.insert("five_hour".to_string(), 60.0);
    utilization.insert("seven_day".to_string(), 30.0);
    utilization.insert("weekly_scoped".to_string(), 40.0);
    let mut hours_remaining = HashMap::new();
    hours_remaining.insert("five_hour".to_string(), 2.0);
    hours_remaining.insert("seven_day".to_string(), 90.0);
    hours_remaining.insert("weekly_scoped".to_string(), 90.0);
    (utilization, hours_remaining)
}

fn window_names() -> [&'static str; 3] {
    ["five_hour", "seven_day", "weekly_scoped"]
}

/// THE VERIFICATION CLAUSE (claudego-0ccbae3c), replayed end to end: zero
/// workers, an unattributed operator session burning real percentages, two
/// cycles so the exogenous baseline is measured the way steady-state daemon
/// operation measures it.
///
/// Asserts, per window:
/// - no fleet CUTOFF_RISK, no phantom fleet rate, no finite fleet exhaustion;
/// - the operator's burn IS still respected — as a constant budget
///   reservation that reduces the net budget (exact arithmetic pinned);
/// - `safe_worker_count` >= 1 whenever the NET headroom supports one worker
///   for the remaining window (conditional, so a window the operator alone
///   exhausts may authorise 0 — it must simply never alarm).
#[test]
fn idle_fleet_plus_operator_session_never_flags_fleet_cutoff_risk_and_still_authorises_a_worker() {
    let baseline = BaselineBurnRates::default();
    let (utilization, hours_remaining) = snapshots();
    let operator = vec![operator_record()];

    // Cycle 1 measures the exogenous baseline from the operator's interval;
    // cycle 2 forecasts with the reservation armed, as the daemon does every
    // cycle after the first.
    let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();
    let _ = estimate_burn_rates(
        &operator,
        &fleet_patterns(),
        1.0, // elapsed hours
        0,   // current_workers — the incident's idle fleet
        0,   // prev_workers
        &mut ema_state,
        &baseline,
        &utilization,
        90.0,
        &hours_remaining,
    );
    let (estimate, forecast) = estimate_burn_rates(
        &operator,
        &fleet_patterns(),
        1.0,
        0,
        0,
        &mut ema_state,
        &baseline,
        &utilization,
        90.0,
        &hours_remaining,
    );

    // Exact reservation arithmetic, per window: gross headroom minus the
    // operator's measured rate over the hours that remain.
    //   five_hour:     (90 - 60) - 13.0 * 2h = 4
    //   seven_day:     (90 - 30) -  1.3 * 90h < 0 → floored at 0
    //   weekly_scoped: (90 - 40) -  1.4 * 90h < 0 → floored at 0
    let expected_remaining = [("five_hour", 4.0), ("seven_day", 0.0), ("weekly_scoped", 0.0)];

    for (name, expected_remaining_pct) in expected_remaining {
        let window = match name {
            "five_hour" => &forecast.five_hour,
            "seven_day" => &forecast.seven_day,
            _ => &forecast.weekly_scoped,
        };

        // Clause 1: operator burn must not flag FLEET CUTOFF_RISK — there is
        // no fleet burn to govern.
        assert!(
            !window.cutoff_risk,
            "{name}: operator-only burn must not flag fleet CUTOFF_RISK"
        );
        assert_eq!(
            window.fleet_pct_per_hour, 0.0,
            "{name}: the operator's 13%/hr-class burn must never surface as fleet burn"
        );
        assert!(
            window.predicted_exhaustion_hours.is_infinite(),
            "{name}: the fleet cannot exhaust a window it is not burning"
        );
        assert!(
            window.margin_hrs.is_infinite() && window.margin_hrs > 0.0,
            "{name}: margin must read unbounded-safe, got {}",
            window.margin_hrs
        );

        // The reservation: budget reduced by the operator's share, never
        // ignored (no over-correction into pretending the operator is not
        // there), and never mis-booked as fleet burn either.
        assert!(
            (window.remaining_pct - expected_remaining_pct).abs() < 1e-9,
            "{name}: net budget must be gross headroom minus the operator's \
             reservation, got {} (expected {})",
            window.remaining_pct,
            expected_remaining_pct
        );
        assert_eq!(
            window.current_utilization, utilization[name],
            "{name}: reported utilization stays the measured account fact"
        );

        // Clause 2, conditional form: wherever the NET headroom supports one
        // worker for the remaining window at the configured baseline, the
        // forecast must authorise at least one. five_hour satisfies the
        // antecedent (4% >= 1.5%/hr * 2h = 3%), so this is a live assertion
        // there, not a vacuous pass; on the weekly-scale windows the
        // operator's own measured rate already exceeds the window, the net
        // budget floors to 0 and authorising 0 is the correct yield — with
        // clause 1 guaranteeing it is not reported as fleet CUTOFF_RISK.
        let supports_one_worker = window.remaining_pct
            >= baseline.pct_per_worker_per_hour * window.hours_remaining;
        if supports_one_worker {
            assert!(
                window.safe_worker_count.unwrap_or(0) >= 1,
                "{name}: net headroom {:.2}% supports one worker over {:.1}h \
                 but safe_worker_count is {:?} — an idle fleet beside an \
                 active operator session must not scale to 0",
                window.remaining_pct,
                window.hours_remaining,
                window.safe_worker_count
            );
        }
    }

    // The concrete instance of clause 2 in this fixture: five_hour authorises
    // exactly one whole worker (4% / (1.5%/hr * 2h) = 1.33 -> floor 1).
    assert_eq!(
        forecast.five_hour.safe_worker_count,
        Some(1),
        "the operator is burning, the fleet is idle, and a worker is STILL \
         authorised on the window whose net headroom supports one"
    );

    // Attribution direction check: the operator's burn fed the exogenous
    // baseline (three window entries measured at exactly the operator's
    // rates — an EMA of a constant is that constant) and NEVER the
    // per-worker EMA (no fleet-model entry may exist).
    assert_eq!(
        estimate.ema_state.len(),
        3,
        "only the three exogenous window baselines may be recorded, got {:?}",
        estimate.ema_state.keys().collect::<Vec<_>>()
    );
    let exo_keys: Vec<&(String, String)> = estimate.ema_state.keys().collect();
    for (model, _window) in exo_keys {
        assert_ne!(
            model, OPERATOR_MODEL,
            "operator burn must never feed the per-worker EMA"
        );
    }
    for (window, expected) in window_names().into_iter().zip([13.0, 1.3, 1.4]) {
        let entry = estimate
            .ema_state
            .iter()
            .find(|((_, w), _)| w == window)
            .map(|(_, e)| e)
            .unwrap_or_else(|| panic!("{window}: exogenous baseline entry missing"));
        assert!(
            (entry.ema_pct - expected).abs() < 1e-9,
            "{window}: exogenous baseline must hold the operator's measured \
             {expected}%/hr, got {}",
            entry.ema_pct
        );
    }
}

/// Companion invariant: the SAME history, attributed to a fleet worker the
/// session_pattern globs match and running as 1 real worker, must STILL trip
/// CUTOFF_RISK. The split exists to stop operator burn masquerading as fleet
/// burn, not to deafen the governor to the pool's own overspend.
#[test]
fn fleet_attributable_burn_still_trips_cutoff_risk() {
    let baseline = BaselineBurnRates::default();
    // Account state under genuine fleet overspend: 85% utilized on every
    // window, so the worker's rates exhaust each one before reset.
    let mut utilization = HashMap::new();
    utilization.insert("five_hour".to_string(), 85.0);
    utilization.insert("seven_day".to_string(), 85.0);
    utilization.insert("weekly_scoped".to_string(), 85.0);
    let mut hours_remaining = HashMap::new();
    hours_remaining.insert("five_hour".to_string(), 2.0);
    hours_remaining.insert("seven_day".to_string(), 90.0);
    hours_remaining.insert("weekly_scoped".to_string(), 90.0);

    let fleet = vec![fleet_record()];
    let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();
    let _ = estimate_burn_rates(
        &fleet,
        &fleet_patterns(),
        1.0,
        1, // current_workers — one worker IS the fleet here
        1, // prev_workers — unchanged, so the EMA updates
        &mut ema_state,
        &baseline,
        &utilization,
        90.0,
        &hours_remaining,
    );
    let (estimate, forecast) = estimate_burn_rates(
        &fleet,
        &fleet_patterns(),
        1.0,
        1,
        1,
        &mut ema_state,
        &baseline,
        &utilization,
        90.0,
        &hours_remaining,
    );

    for (name, window, fleet_pct_hr) in [
        ("five_hour", &forecast.five_hour, 13.0),
        ("seven_day", &forecast.seven_day, 1.3),
        ("weekly_scoped", &forecast.weekly_scoped, 1.4),
    ] {
        assert!(
            window.cutoff_risk,
            "{name}: genuine fleet burn exhausting the window before reset \
             must flag CUTOFF_RISK"
        );
        assert!(
            (window.fleet_pct_per_hour - fleet_pct_hr).abs() < 1e-9,
            "{name}: fleet rate must be the worker's measured {fleet_pct_hr}%/hr, \
             got {}",
            window.fleet_pct_per_hour
        );
        assert!(
            window.margin_hrs < 0.0,
            "{name}: an exhausting window must carry a negative margin, got {}",
            window.margin_hrs
        );
        // No phantom reservation: with no exogenous burn the net budget is
        // the gross headroom (90 - 85 = 5).
        assert!(
            (window.remaining_pct - 5.0).abs() < 1e-9,
            "{name}: fleet burn must not be reserved against itself, got net {}",
            window.remaining_pct
        );
    }

    // The binding window is the one sounding the alarm.
    let binding = [
        ("five_hour", &forecast.five_hour),
        ("seven_day", &forecast.seven_day),
        ("weekly_scoped", &forecast.weekly_scoped),
    ]
    .into_iter()
    .find(|(_, w)| w.binding)
    .expect("some window must bind");
    assert!(
        binding.1.cutoff_risk,
        "{}: the binding window must carry the cutoff risk",
        binding.0
    );

    // And the fleet direction of attribution works: the worker's rate fed
    // the per-worker EMA (13%/hr over 1 worker), keyed by its real model.
    let worker_ema = estimate
        .ema_state
        .get(&("claude-sonnet-5".to_string(), "five_hour".to_string()))
        .expect("fleet burn must feed the per-worker EMA");
    assert!(
        (worker_ema.ema_pct - 13.0).abs() < 1e-9,
        "per-worker EMA must hold the worker's 13%/hr, got {}",
        worker_ema.ema_pct
    );
}

/// The seams the daemon's own cycle uses (`run_observe_cycle`), pinned at the
/// idle-fleet point `cgov forecast` actually reports through: whatever the
/// account is doing, an idle fleet's fleet rate is 0 (so no CUTOFF_RISK is
/// reachable) and sizing falls back to the configured baseline (so a worker
/// remains authorisable). Replayed with a POISONED EMA — exactly the
/// 2026-09-07 residue of operator burn measured while workers were last
/// active — to prove the idle pin does not depend on EMA hygiene.
#[test]
fn production_seams_idle_fleet_pins_fleet_rate_to_zero_and_sizes_from_baseline() {
    let baseline_pct_per_worker = 1.5;

    // Idle fleet: even a poisoned EMA (5 samples at 13%/hr) must not become
    // fleet burn while nothing is running.
    let fleet_pct_hr = effective_fleet_pct_rate(0, 5, 13.0, 6.0, 1.2, 3.33);
    assert_eq!(
        fleet_pct_hr, 0.0,
        "an idle fleet reports zero fleet burn regardless of the EMA"
    );

    // Sizing at zero workers falls back to the baseline, so the pool can
    // start again (the claudego-d64682d5 loop must stay open).
    assert_eq!(
        per_worker_pct_for_sizing(0, fleet_pct_hr, baseline_pct_per_worker),
        baseline_pct_per_worker,
        "sizing from zero workers uses the baseline, not the (zero) fleet rate"
    );

    // What run_observe_cycle then computes for the window: fleet rate 0 ->
    // no cutoff risk is reachable, and the baseline-sized worker is
    // authorised whenever the headroom supports it.
    let forecast = generate_window_forecast(
        "five_hour",
        fleet_pct_hr,
        60.0,
        90.0,
        2.0,
        baseline_pct_per_worker,
        0.0,
        EstimateQuality::ColdStart,
    );
    assert!(
        !forecast.cutoff_risk,
        "fleet rate 0 must never flag CUTOFF_RISK"
    );
    assert!(
        forecast.predicted_exhaustion_hours.is_infinite(),
        "no fleet burn -> no finite fleet exhaustion"
    );
    let supports_one_worker =
        forecast.remaining_pct >= baseline_pct_per_worker * forecast.hours_remaining;
    assert!(
        supports_one_worker,
        "fixture check: this window's headroom must support one worker for \
         the clause to be exercised"
    );
    assert!(
        forecast.safe_worker_count.unwrap_or(0) >= 1,
        "an idle fleet beside an active operator session must still be able \
         to authorise a worker, got {:?}",
        forecast.safe_worker_count
    );

    // The same seam with the fleet genuinely running (1 worker, calibrated
    // EMA at the worker's measured 13%/hr): the rate passes through and the
    // overspending window trips CUTOFF_RISK — the idle pin must not mute
    // real fleet burn.
    let active_fleet_pct_hr = effective_fleet_pct_rate(1, 5, 13.0, 6.0, 1.2, 3.33);
    assert!(
        (active_fleet_pct_hr - 13.0).abs() < 1e-9,
        "with workers running, the calibrated EMA is the fleet rate"
    );
    let active = generate_window_forecast(
        "five_hour",
        active_fleet_pct_hr,
        85.0,
        90.0,
        2.0,
        active_fleet_pct_hr / 1.0,
        0.0,
        EstimateQuality::Calibrated,
    );
    assert!(
        active.cutoff_risk,
        "genuine fleet burn must still trip CUTOFF_RISK at the production seam"
    );
}
