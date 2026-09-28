//! Table-driven coverage for the scaling policy documented in
//! `docs/hysteresis-and-smooth-scaling.md`.
//!
//! The older scaling tests are useful, focused regressions.  These tables are
//! the policy matrix: every row names the boundary or invariant it pins, so a
//! change to one branch cannot quietly leave an untested combination behind.

use claude_governor::config::{CompositeRiskConfig, ConeScalingConfig};
use claude_governor::governor::{
    apply_scaling, compute_target_workers, ScalingDecision, EMERGENCY_BRAKE_THRESHOLD,
};
use claude_governor::state::{CapacityForecast, GovernorState, WindowForecast, WorkerState};

#[derive(Debug)]
struct DecisionCase {
    name: &'static str,
    target: u32,
    current: u32,
    hysteresis_band: f64,
    max_up: u32,
    max_down: u32,
    emergency_brake_active: bool,
    expected: ScalingDecision,
}

#[test]
fn documented_scaling_decisions_are_exhaustive_table_rows() {
    let cases = [
        // A deficit is never hidden by the down-side hysteresis band.  The
        // one-worker rows are the convergence regression in its smallest form.
        DecisionCase {
            name: "one_worker_deficit_with_default_band",
            target: 6,
            current: 5,
            hysteresis_band: 1.0,
            max_up: 1,
            max_down: 1,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleUp(1),
        },
        DecisionCase {
            name: "one_worker_deficit_with_wide_band",
            target: 6,
            current: 5,
            hysteresis_band: 100.0,
            max_up: 3,
            max_down: 3,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleUp(1),
        },
        DecisionCase {
            name: "scale_up_cap_binds",
            target: 10,
            current: 5,
            hysteresis_band: 1.0,
            max_up: 2,
            max_down: 3,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleUp(2),
        },
        DecisionCase {
            name: "scale_up_gap_fits_under_cap",
            target: 7,
            current: 5,
            hysteresis_band: 1.0,
            max_up: 3,
            max_down: 3,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleUp(2),
        },
        // The band is inclusive on scale-down: equality holds, one worker
        // beyond it moves.  Fractional bands use their documented integer
        // truncation on this path.
        DecisionCase {
            name: "scale_down_exact_band_holds",
            target: 3,
            current: 5,
            hysteresis_band: 2.0,
            max_up: 3,
            max_down: 3,
            emergency_brake_active: false,
            expected: ScalingDecision::NoChange,
        },
        DecisionCase {
            name: "scale_down_one_beyond_band_moves",
            target: 2,
            current: 5,
            hysteresis_band: 2.0,
            max_up: 3,
            max_down: 3,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleDown(3),
        },
        DecisionCase {
            name: "fractional_band_truncates_at_two",
            target: 3,
            current: 5,
            hysteresis_band: 2.9,
            max_up: 3,
            max_down: 3,
            emergency_brake_active: false,
            expected: ScalingDecision::NoChange,
        },
        DecisionCase {
            name: "scale_down_cap_binds",
            target: 0,
            current: 5,
            hysteresis_band: 1.0,
            max_up: 3,
            max_down: 2,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleDown(2),
        },
        DecisionCase {
            name: "scale_down_gap_fits_under_cap",
            target: 3,
            current: 5,
            hysteresis_band: 1.0,
            max_up: 3,
            max_down: 3,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleDown(2),
        },
        // A computed zero without the real signal is still a graceful,
        // band-damped withdrawal.  The brake bypasses both band and cap only
        // when the caller has observed a near-limit window.
        DecisionCase {
            name: "computed_zero_inside_band_is_graceful_hold",
            target: 0,
            current: 1,
            hysteresis_band: 1.0,
            max_up: 3,
            max_down: 2,
            emergency_brake_active: false,
            expected: ScalingDecision::NoChange,
        },
        DecisionCase {
            name: "computed_zero_outside_band_is_graceful_and_capped",
            target: 0,
            current: 5,
            hysteresis_band: 1.0,
            max_up: 3,
            max_down: 2,
            emergency_brake_active: false,
            expected: ScalingDecision::ScaleDown(2),
        },
        DecisionCase {
            name: "real_brake_zeroes_live_workers",
            target: 0,
            current: 5,
            hysteresis_band: 100.0,
            max_up: 0,
            max_down: 0,
            emergency_brake_active: true,
            expected: ScalingDecision::EmergencyBrake,
        },
        DecisionCase {
            name: "brake_flag_does_not_change_nonzero_target",
            target: 6,
            current: 5,
            hysteresis_band: 1.0,
            max_up: 1,
            max_down: 1,
            emergency_brake_active: true,
            expected: ScalingDecision::ScaleUp(1),
        },
        DecisionCase {
            name: "brake_with_no_live_workers_is_no_change",
            target: 0,
            current: 0,
            hysteresis_band: 1.0,
            max_up: 1,
            max_down: 1,
            emergency_brake_active: true,
            expected: ScalingDecision::NoChange,
        },
    ];

    for case in cases {
        let actual = apply_scaling(
            case.target,
            case.current,
            case.hysteresis_band,
            case.max_up,
            case.max_down,
            case.emergency_brake_active,
        );
        assert_eq!(actual, case.expected, "scaling policy row `{}`", case.name);
    }
}

#[derive(Debug)]
struct ConvergenceCase {
    name: &'static str,
    current: u32,
    target: u32,
    hysteresis_band: f64,
    max_up: u32,
}

#[test]
fn every_scale_up_deficit_converges_exactly_to_target() {
    let cases = [
        ConvergenceCase {
            name: "one_worker_deficit",
            current: 4,
            target: 5,
            hysteresis_band: 1.0,
            max_up: 1,
        },
        ConvergenceCase {
            name: "wide_band_does_not_block_growth",
            current: 2,
            target: 7,
            hysteresis_band: 100.0,
            max_up: 1,
        },
        ConvergenceCase {
            name: "multi_worker_cap_and_large_gap",
            current: 3,
            target: 17,
            hysteresis_band: 2.0,
            max_up: 3,
        },
        ConvergenceCase {
            name: "target_one_above_current",
            current: 9,
            target: 10,
            hysteresis_band: 0.0,
            max_up: 10,
        },
    ];

    for case in cases {
        let mut current = case.current;
        let mut cycles = 0;
        while current < case.target {
            let decision = apply_scaling(
                case.target,
                current,
                case.hysteresis_band,
                case.max_up,
                1,
                false,
            );
            let ScalingDecision::ScaleUp(step) = decision else {
                panic!(
                    "row `{}` stopped before target: current={}, target={}, decision={:?}",
                    case.name, current, case.target, decision
                );
            };
            assert!(step > 0, "row `{}` made no progress", case.name);
            assert!(step <= case.max_up, "row `{}` exceeded its cap", case.name);
            assert!(
                current + step <= case.target,
                "row `{}` overshot",
                case.name
            );
            current += step;
            cycles += 1;
            assert!(cycles <= case.target - case.current + 1);
        }
        assert_eq!(current, case.target, "row `{}` did not converge", case.name);
        assert_eq!(
            apply_scaling(
                case.target,
                current,
                case.hysteresis_band,
                case.max_up,
                1,
                false
            ),
            ScalingDecision::NoChange,
            "row `{}` should hold after convergence",
            case.name
        );
    }
}

fn forecast_window(utilization: f64, safe_workers: u32, binding: bool) -> WindowForecast {
    WindowForecast {
        current_utilization: utilization,
        safe_worker_count: Some(safe_workers),
        safe_worker_count_p75: Some(safe_workers),
        margin_hrs: 50.0,
        hours_remaining: 100.0,
        binding,
        ..Default::default()
    }
}

fn state_with_usage(utilizations: [f64; 3]) -> GovernorState {
    let mut state = GovernorState::new();
    state.capacity_forecast = CapacityForecast {
        five_hour: forecast_window(utilizations[0], 0, true),
        seven_day: forecast_window(utilizations[1], 0, false),
        weekly_scoped: forecast_window(utilizations[2], 0, false),
        binding_window: "five_hour".to_string(),
        ..Default::default()
    };
    state.workers.insert(
        "pool".to_string(),
        WorkerState {
            current: 5,
            target: 5,
            min: 0,
            max: 10,
        },
    );
    state
}

#[derive(Debug)]
struct BrakeCase {
    name: &'static str,
    utilizations: [f64; 3],
    expected_brake: bool,
}

#[test]
fn emergency_brake_requires_an_actual_near_limit_window() {
    let cases = [
        BrakeCase {
            name: "just_below_threshold_is_graceful",
            utilizations: [97.99, 40.0, 40.0],
            expected_brake: false,
        },
        BrakeCase {
            name: "five_hour_at_threshold_is_brake",
            utilizations: [EMERGENCY_BRAKE_THRESHOLD, 40.0, 40.0],
            expected_brake: true,
        },
        BrakeCase {
            name: "seven_day_at_threshold_is_brake",
            utilizations: [40.0, EMERGENCY_BRAKE_THRESHOLD, 40.0],
            expected_brake: true,
        },
        BrakeCase {
            name: "weekly_scoped_at_threshold_is_brake",
            utilizations: [40.0, 40.0, EMERGENCY_BRAKE_THRESHOLD],
            expected_brake: true,
        },
        BrakeCase {
            name: "all_windows_below_threshold_are_graceful",
            utilizations: [97.99, 97.98, 97.97],
            expected_brake: false,
        },
    ];

    for case in cases {
        let state = state_with_usage(case.utilizations);
        let target = compute_target_workers(
            &state,
            90.0,
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default(),
        );
        assert_eq!(
            target, 0,
            "row `{}` must compute the zero target",
            case.name
        );

        let observed_brake = case
            .utilizations
            .into_iter()
            .any(|utilization| utilization >= EMERGENCY_BRAKE_THRESHOLD);
        assert_eq!(
            observed_brake, case.expected_brake,
            "row `{}` has the wrong actual-window classification",
            case.name
        );

        let decision = apply_scaling(target, 5, 1.0, 3, 2, observed_brake);
        let expected = if case.expected_brake {
            ScalingDecision::EmergencyBrake
        } else {
            ScalingDecision::ScaleDown(2)
        };
        assert_eq!(decision, expected, "row `{}` decision", case.name);
    }
}
