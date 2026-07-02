//! Simple governor cycle test with snapshot
//!
//! This test demonstrates a basic governor cycle:
//! - Create a usage snapshot with utilization data
//! - Initialize governor state with worker configuration
//! - Compute target workers based on capacity forecast
//! - Apply scaling decision
//! - Verify state consistency after the cycle

use claude_governor::config::{CompositeRiskConfig, ConeScalingConfig};
use claude_governor::governor::{
    compute_target_workers, apply_scaling, ScalingDecision,
    WINDOW_FIVE_HOUR, WINDOW_SEVEN_DAY, WINDOW_SEVEN_DAY_SONNET,
    UsageSnapshot,
};
use claude_governor::state;

/// Simple helper to create a usage snapshot from window values
fn make_usage_snapshot(five_hour: f64, seven_day: f64, seven_day_sonnet: f64) -> UsageSnapshot {
    UsageSnapshot::from_windows(five_hour, seven_day, seven_day_sonnet)
}

#[test]
fn test_governor_cycle_with_snapshot() {
    // 1. Create a usage snapshot with moderate utilization
    // This represents the current state of all three windows
    let usage = make_usage_snapshot(50.0, 40.0, 35.0);

    // 2. Initialize governor state with worker configuration
    let mut state = state::GovernorState::new();
    state.workers.insert(
        "test-agent".to_string(),
        state::WorkerState {
            current: 5,
            target: 5,
            min: 1,
            max: 10,
        },
    );

    // 3. Build capacity forecast from the snapshot
    // In a real cycle, this would come from burn_rate analysis
    state.capacity_forecast = state::CapacityForecast {
        five_hour: state::WindowForecast {
            current_utilization: usage.get(WINDOW_FIVE_HOUR).unwrap_or(0.0),
            safe_worker_count: Some(5),
            safe_worker_count_p75: Some(4),
            ..Default::default()
        },
        seven_day: state::WindowForecast {
            current_utilization: usage.get(WINDOW_SEVEN_DAY).unwrap_or(0.0),
            safe_worker_count: Some(6),
            safe_worker_count_p75: Some(5),
            ..Default::default()
        },
        seven_day_sonnet: state::WindowForecast {
            current_utilization: usage.get(WINDOW_SEVEN_DAY_SONNET).unwrap_or(0.0),
            safe_worker_count: Some(7),
            safe_worker_count_p75: Some(6),
            ..Default::default()
        },
        binding_window: WINDOW_SEVEN_DAY_SONNET.to_string(),
        ..Default::default()
    };

    // 4. Compute target workers based on the forecast
    let target_ceiling = 90.0;
    let target = compute_target_workers(
        &state,
        target_ceiling,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );

    // 5. Apply scaling decision with hysteresis and rate limits
    let current_total = 5;
    let hysteresis_band = 2.0;
    let max_up_per_cycle = 3;
    let max_down_per_cycle = 2;

    let decision = apply_scaling(
        target,
        current_total,
        hysteresis_band,
        max_up_per_cycle,
        max_down_per_cycle,
    );

    // 6. Verify the cycle completed and decision is reasonable
    match decision {
        ScalingDecision::NoChange => {
            // Target should be within hysteresis band of current
            let target_f = target as f64;
            let current_f = current_total as f64;
            assert!(
                target_f >= current_f - hysteresis_band
                    && target_f <= current_f + hysteresis_band,
                "NoChange: target {} should be within hysteresis band of current {}",
                target,
                current_total
            );
        }
        ScalingDecision::ScaleUp(n) => {
            // Scale-up should be positive and within rate limit
            assert!(
                n > 0 && n <= max_up_per_cycle,
                "ScaleUp: n={} should be 1-{} workers",
                n,
                max_up_per_cycle
            );
        }
        ScalingDecision::ScaleDown(n) => {
            // Scale-down should be positive and within rate limit
            assert!(
                n > 0 && n <= max_down_per_cycle,
                "ScaleDown: n={} should be 1-{} workers",
                n,
                max_down_per_cycle
            );
        }
        ScalingDecision::EmergencyBrake => {
            // At moderate utilization (50%), should not trigger emergency brake
            panic!(
                "EmergencyBrake should not trigger at moderate utilization (snapshot: {:?})",
                usage
            );
        }
    }

    // 7. Verify state is consistent after the cycle
    assert!(!state.workers.is_empty(), "State should retain workers after cycle");
    assert_eq!(
        state.workers["test-agent"].current,
        5,
        "Current workers unchanged in state"
    );
    assert!(!state.safe_mode.active, "Safe mode should not be active");
}

#[test]
fn test_snapshot_high_utilization_emergency_brake() {
    // Test with high utilization that should trigger emergency brake
    let usage = make_usage_snapshot(99.0, 50.0, 50.0);

    let mut state = state::GovernorState::new();
    state.workers.insert(
        "high-load-agent".to_string(),
        state::WorkerState {
            current: 10,
            target: 10,
            min: 1,
            max: 10,
        },
    );

    state.capacity_forecast = state::CapacityForecast {
        five_hour: state::WindowForecast {
            current_utilization: usage.get(WINDOW_FIVE_HOUR).unwrap_or(0.0),
            safe_worker_count: Some(0),
            safe_worker_count_p75: Some(0),
            ..Default::default()
        },
        seven_day: state::WindowForecast {
            current_utilization: usage.get(WINDOW_SEVEN_DAY).unwrap_or(0.0),
            safe_worker_count: Some(5),
            safe_worker_count_p75: Some(4),
            ..Default::default()
        },
        seven_day_sonnet: state::WindowForecast {
            current_utilization: usage.get(WINDOW_SEVEN_DAY_SONNET).unwrap_or(0.0),
            safe_worker_count: Some(5),
            safe_worker_count_p75: Some(4),
            ..Default::default()
        },
        binding_window: WINDOW_FIVE_HOUR.to_string(),
        ..Default::default()
    };

    let target = compute_target_workers(
        &state,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );

    // At 99% utilization, target should be 0 (emergency brake)
    assert_eq!(target, 0, "Target should be 0 at 99% utilization");

    let decision = apply_scaling(target, 10, 2.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::EmergencyBrake),
        "Should trigger EmergencyBrake at 99% utilization, got {:?}",
        decision
    );
}

#[test]
fn test_snapshot_low_utilization_scale_down() {
    // Test with low utilization that should trigger scale-down
    let usage = make_usage_snapshot(10.0, 10.0, 10.0);

    let mut state = state::GovernorState::new();
    state.workers.insert(
        "low-load-agent".to_string(),
        state::WorkerState {
            current: 8,
            target: 8,
            min: 1,
            max: 10,
        },
    );

    state.capacity_forecast = state::CapacityForecast {
        five_hour: state::WindowForecast {
            current_utilization: usage.get(WINDOW_FIVE_HOUR).unwrap_or(0.0),
            safe_worker_count: Some(2),
            safe_worker_count_p75: Some(1),
            ..Default::default()
        },
        seven_day: state::WindowForecast {
            current_utilization: usage.get(WINDOW_SEVEN_DAY).unwrap_or(0.0),
            safe_worker_count: Some(2),
            safe_worker_count_p75: Some(1),
            ..Default::default()
        },
        seven_day_sonnet: state::WindowForecast {
            current_utilization: usage.get(WINDOW_SEVEN_DAY_SONNET).unwrap_or(0.0),
            safe_worker_count: Some(2),
            safe_worker_count_p75: Some(1),
            ..Default::default()
        },
        binding_window: WINDOW_SEVEN_DAY_SONNET.to_string(),
        ..Default::default()
    };

    let target = compute_target_workers(
        &state,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );

    // At low utilization, target should be lower than current
    assert!(
        target < 8,
        "Target {} should be lower than current 8 at low utilization",
        target
    );

    let decision = apply_scaling(target, 8, 2.0, 3, 2);

    // With target=2, current=8, hysteresis=2: should scale down by max 2
    match decision {
        ScalingDecision::ScaleDown(n) => {
            assert!(
                n > 0 && n <= 2,
                "Should scale down by 1-2 workers, got {}",
                n
            );
        }
        other => {
            panic!("Expected ScaleDown at low utilization, got {:?}", other);
        }
    }
}
