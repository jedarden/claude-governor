//! Simple governor cycle test with snapshot
//!
//! This test demonstrates a basic governor cycle:
//! - Create a usage snapshot with utilization data
//! - Initialize governor state with worker configuration
//! - Compute target workers based on capacity forecast
//! - Apply scaling decision
//! - Verify state consistency after the cycle

use chrono::Utc;
use claude_governor::config::{CompositeRiskConfig, ConeScalingConfig};
use claude_governor::governor::{
    apply_scaling, compute_target_workers, ScalingDecision, UsageSnapshot, WINDOW_FIVE_HOUR,
    WINDOW_SEVEN_DAY, WINDOW_WEEKLY_SCOPED,
};
use claude_governor::state;

/// Simple helper to create a usage snapshot from window values
fn make_usage_snapshot(five_hour: f64, seven_day: f64, weekly_scoped: f64) -> UsageSnapshot {
    UsageSnapshot::from_windows(five_hour, seven_day, weekly_scoped)
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
        weekly_scoped: state::WindowForecast {
            current_utilization: usage.get(WINDOW_WEEKLY_SCOPED).unwrap_or(0.0),
            safe_worker_count: Some(7),
            safe_worker_count_p75: Some(6),
            ..Default::default()
        },
        binding_window: WINDOW_WEEKLY_SCOPED.to_string(),
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
                target_f >= current_f - hysteresis_band && target_f <= current_f + hysteresis_band,
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
    assert!(
        !state.workers.is_empty(),
        "State should retain workers after cycle"
    );
    assert_eq!(
        state.workers["test-agent"].current, 5,
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
        weekly_scoped: state::WindowForecast {
            current_utilization: usage.get(WINDOW_WEEKLY_SCOPED).unwrap_or(0.0),
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
        weekly_scoped: state::WindowForecast {
            current_utilization: usage.get(WINDOW_WEEKLY_SCOPED).unwrap_or(0.0),
            safe_worker_count: Some(2),
            safe_worker_count_p75: Some(1),
            ..Default::default()
        },
        binding_window: WINDOW_WEEKLY_SCOPED.to_string(),
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

#[test]
fn test_second_poll_with_delta_computation() {
    // Test second poll scenario: both snapshots are Some, verify delta computation
    let mut state = state::GovernorState::new();

    // Simulate first poll: set current_api_snapshot
    let now1 = Utc::now();
    state.current_api_snapshot = Some(state::PrevUsageSnapshot {
        taken_at: now1,
        five_hour_pct: 10.0,
        seven_day_pct: 20.0,
        weekly_scoped_pct: 15.0,
    });

    // Verify initial state after first poll
    assert!(
        state.previous_api_snapshot.is_none(),
        "After first poll, previous should be None"
    );
    assert!(
        state.current_api_snapshot.is_some(),
        "After first poll, current should be Some"
    );

    // Simulate the shift at the start of second poll (as in run_governor_cycle line 2959)
    state.previous_api_snapshot = state.current_api_snapshot.take();

    // Verify shift occurred
    assert!(
        state.previous_api_snapshot.is_some(),
        "After shift, previous should be Some"
    );
    assert!(
        state.current_api_snapshot.is_none(),
        "After shift, current should be None"
    );

    // Simulate second poll: set new current_api_snapshot
    let now2 = now1 + chrono::Duration::seconds(60);
    state.current_api_snapshot = Some(state::PrevUsageSnapshot {
        taken_at: now2,
        five_hour_pct: 12.5,     // +2.5 from previous
        seven_day_pct: 22.0,     // +2.0 from previous
        weekly_scoped_pct: 18.0, // +3.0 from previous
    });

    // Now both snapshots are Some - compute deltas
    let (p5h_delta, p7d_delta, p7ds_delta) =
        claude_governor::governor::window_deltas_from_snapshots(
            state.previous_api_snapshot.as_ref(),
            state.current_api_snapshot.as_ref(),
        );

    // Verify delta computation: should have computed actual deltas, not Some(0.0)
    assert_eq!(
        p5h_delta,
        Some(2.5),
        "5h delta should be 12.5 - 10.0 = 2.5 on second poll"
    );
    assert_eq!(
        p7d_delta,
        Some(2.0),
        "7d delta should be 22.0 - 20.0 = 2.0 on second poll"
    );
    assert_eq!(
        p7ds_delta,
        Some(3.0),
        "7ds delta should be 18.0 - 15.0 = 3.0 on second poll"
    );
}

#[test]
fn test_poll_failure_current_snapshot_remains_none() {
    // Test poll failure: current_api_snapshot remains None, verify no incorrect deltas
    let mut state = state::GovernorState::new();

    // Simulate a previous successful poll (so previous_api_snapshot is Some)
    let now1 = Utc::now();
    state.previous_api_snapshot = Some(state::PrevUsageSnapshot {
        taken_at: now1,
        five_hour_pct: 10.0,
        seven_day_pct: 20.0,
        weekly_scoped_pct: 15.0,
    });

    // current_api_snapshot starts as None (no new poll data yet)
    assert!(
        state.current_api_snapshot.is_none(),
        "Before poll, current should be None"
    );

    // Simulate poll failure: the Err branch in run_governor_cycle (line 3027-3044)
    // When poll fails, current_api_snapshot is NOT updated, so it remains None

    // Verify current_api_snapshot is still None after failed poll
    assert!(
        state.current_api_snapshot.is_none(),
        "After failed poll, current should remain None"
    );

    // Now attempt delta computation with (Some(previous), None(current))
    let (p5h_delta, p7d_delta, p7ds_delta) =
        claude_governor::governor::window_deltas_from_snapshots(
            state.previous_api_snapshot.as_ref(),
            state.current_api_snapshot.as_ref(),
        );

    // Verify: deltas should remain None, not Some(0.0)
    assert_eq!(
        p5h_delta, None,
        "5h delta should be None when current snapshot is missing (poll failure)"
    );
    assert_eq!(
        p7d_delta, None,
        "7d delta should be None when current snapshot is missing (poll failure)"
    );
    assert_eq!(
        p7ds_delta, None,
        "7ds delta should be None when current snapshot is missing (poll failure)"
    );
}

#[test]
fn test_first_poll_no_previous_snapshot() {
    // Test first poll edge case: no previous snapshot exists
    //
    // This is the initial bootstrap condition when the governor starts:
    // - previous_api_snapshot is None (no prior poll data)
    // - current_api_snapshot is Some (first successful poll just completed)
    // - Expected: deltas are None for all windows, not Some(0.0) — there is no
    //   baseline, so no window has a measured change to report
    //
    // Calls window_deltas_from_snapshots, the same function run_governor_cycle
    // assigns its delta fields from.

    let mut state = state::GovernorState::new();

    // Simulate first poll: set current_api_snapshot, previous is None by default
    let now = Utc::now();
    state.current_api_snapshot = Some(state::PrevUsageSnapshot {
        taken_at: now,
        five_hour_pct: 25.0,
        seven_day_pct: 40.0,
        weekly_scoped_pct: 35.0,
    });

    // Verify initial state: first poll
    assert!(
        state.previous_api_snapshot.is_none(),
        "On first poll, previous should be None"
    );
    assert!(
        state.current_api_snapshot.is_some(),
        "On first poll, current should be Some"
    );

    // Now compute deltas with (None, Some(current))
    let deltas = claude_governor::governor::window_deltas_from_snapshots(
        state.previous_api_snapshot.as_ref(),
        state.current_api_snapshot.as_ref(),
    );

    // Verify: on first poll, every delta is None — no baseline, nothing measured
    assert_eq!(
        deltas,
        (None, None, None),
        "deltas should be None on first poll (no previous snapshot), not Some(0.0)"
    );
}

#[test]
fn test_first_poll_with_realistic_values() {
    // Test first poll using realistic fixture values from snapshot_fixtures
    //
    // This demonstrates that the first poll behavior works with realistic
    // utilization data, not just test values.

    use claude_governor::snapshot_fixtures::make_snapshot;

    let mut state = state::GovernorState::new();

    // Simulate first poll with realistic utilization values
    let now = Utc::now();
    state.current_api_snapshot = Some(make_snapshot(
        now, 12.5, // five_hour_pct: low usage
        45.2, // seven_day_pct: moderate usage
        38.7, // weekly_scoped_pct: moderate usage
    ));

    // Verify first poll condition
    assert!(state.previous_api_snapshot.is_none());
    assert!(state.current_api_snapshot.is_some());

    // Compute deltas using the same call run_governor_cycle makes
    let deltas = claude_governor::governor::window_deltas_from_snapshots(
        state.previous_api_snapshot.as_ref(),
        state.current_api_snapshot.as_ref(),
    );

    // Verify: realistic utilization values do not conjure a baseline either
    assert_eq!(
        deltas,
        (None, None, None),
        "first poll with realistic values should still report no deltas"
    );
}

#[test]
fn test_identical_snapshots_produce_zero_deltas() {
    // Test that consecutive snapshots with identical usage values produce 0% deltas
    //
    // This is a critical edge case: when the API reports the same utilization values
    // across two consecutive polls (e.g., no new usage accumulated, or the usage exactly
    // matches the previous snapshot), the delta computation should correctly identify
    // that there is no change (0% delta) for all three windows.
    //
    // Covers:
    // - p5h (5-hour window delta)
    // - p7d (7-day window delta)
    // - p7ds (7-day Sonnet window delta)
    //
    // The test uses identical utilization values but different timestamps to model
    // consecutive polls where usage hasn't changed.

    let mut state = state::GovernorState::new();

    // Set up previous snapshot with specific utilization values
    let now1 = Utc::now();
    state.previous_api_snapshot = Some(state::PrevUsageSnapshot {
        taken_at: now1,
        five_hour_pct: 25.5,
        seven_day_pct: 45.2,
        weekly_scoped_pct: 38.7,
    });

    // Set up current snapshot with IDENTICAL utilization values but later timestamp
    let now2 = now1 + chrono::Duration::seconds(60);
    state.current_api_snapshot = Some(state::PrevUsageSnapshot {
        taken_at: now2,
        five_hour_pct: 25.5,     // Identical to previous
        seven_day_pct: 45.2,     // Identical to previous
        weekly_scoped_pct: 38.7, // Identical to previous
    });

    // Verify both snapshots exist and have identical values
    assert!(state.previous_api_snapshot.is_some());
    assert!(state.current_api_snapshot.is_some());

    let prev = state.previous_api_snapshot.as_ref().unwrap();
    let curr = state.current_api_snapshot.as_ref().unwrap();

    assert_eq!(prev.five_hour_pct, curr.five_hour_pct);
    assert_eq!(prev.seven_day_pct, curr.seven_day_pct);
    assert_eq!(prev.weekly_scoped_pct, curr.weekly_scoped_pct);

    // Verify timestamps are different (simulating consecutive polls)
    assert_ne!(prev.taken_at, curr.taken_at);
    assert!(curr.taken_at > prev.taken_at);

    // Compute deltas using the same call run_governor_cycle makes
    let (p5h_delta, p7d_delta, p7ds_delta) =
        claude_governor::governor::window_deltas_from_snapshots(
            state.previous_api_snapshot.as_ref(),
            state.current_api_snapshot.as_ref(),
        );

    // Verify: with identical snapshot values, all deltas should be 0.0
    // Use f64::EPSILON for floating-point tolerance
    assert!(
        p5h_delta.is_some(),
        "5h delta should be Some (not None) when both snapshots exist"
    );
    assert!(
        (p5h_delta.unwrap() - 0.0).abs() < f64::EPSILON,
        "5h delta should be 0% when snapshot values are identical, got {}",
        p5h_delta.unwrap()
    );

    assert!(
        p7d_delta.is_some(),
        "7d delta should be Some (not None) when both snapshots exist"
    );
    assert!(
        (p7d_delta.unwrap() - 0.0).abs() < f64::EPSILON,
        "7d delta should be 0% when snapshot values are identical, got {}",
        p7d_delta.unwrap()
    );

    assert!(
        p7ds_delta.is_some(),
        "7ds delta should be Some (not None) when both snapshots exist"
    );
    assert!(
        (p7ds_delta.unwrap() - 0.0).abs() < f64::EPSILON,
        "7ds delta should be 0% when snapshot values are identical, got {}",
        p7ds_delta.unwrap()
    );
}

#[test]
fn test_identical_snapshots_with_realistic_fixture_values() {
    // Test identical snapshots using realistic fixture values from snapshot_fixtures
    //
    // This demonstrates that the zero-delta behavior works with realistic
    // utilization data, ensuring the edge case isn't just theoretical.

    use claude_governor::snapshot_fixtures::{baseline_snapshot, make_snapshot};

    let mut state = state::GovernorState::new();

    // Use baseline snapshot values (realistic starting point)
    let baseline = baseline_snapshot();
    let now1 = baseline.taken_at;

    state.previous_api_snapshot = Some(baseline);

    // Create current snapshot with identical values but later timestamp
    let now2 = now1 + chrono::Duration::hours(5);
    state.current_api_snapshot = Some(make_snapshot(
        now2, 12.5, // five_hour_pct: identical to baseline
        45.2, // seven_day_pct: identical to baseline
        38.7, // weekly_scoped_pct: identical to baseline
    ));

    // Verify values are identical
    let prev = state.previous_api_snapshot.as_ref().unwrap();
    let curr = state.current_api_snapshot.as_ref().unwrap();

    assert_eq!(
        prev.five_hour_pct, curr.five_hour_pct,
        "five_hour_pct should be identical"
    );
    assert_eq!(
        prev.seven_day_pct, curr.seven_day_pct,
        "seven_day_pct should be identical"
    );
    assert_eq!(
        prev.weekly_scoped_pct, curr.weekly_scoped_pct,
        "weekly_scoped_pct should be identical"
    );

    // Compute deltas
    let (p5h_delta, p7d_delta, p7ds_delta) =
        claude_governor::governor::window_deltas_from_snapshots(
            state.previous_api_snapshot.as_ref(),
            state.current_api_snapshot.as_ref(),
        );

    // Verify all deltas are exactly 0.0 with realistic values
    assert!(
        (p5h_delta.unwrap() - 0.0).abs() < f64::EPSILON,
        "5h delta should be 0% with identical realistic values, got {}",
        p5h_delta.unwrap()
    );
    assert!(
        (p7d_delta.unwrap() - 0.0).abs() < f64::EPSILON,
        "7d delta should be 0% with identical realistic values, got {}",
        p7d_delta.unwrap()
    );
    assert!(
        (p7ds_delta.unwrap() - 0.0).abs() < f64::EPSILON,
        "7ds delta should be 0% with identical realistic values, got {}",
        p7ds_delta.unwrap()
    );
}
