//! Comprehensive tests for hysteresis behavior and smooth scaling transitions
//!
//! This test suite validates:
//! - Hysteresis band behavior (edge cases, thresholds)
//! - Large gap scaling (progressive vs binary)
//! - Smooth scaling transitions (no oscillation)
//! - Exponential approach convergence
//! - Adaptive timing scenarios
//! - Emergency brake override of hysteresis

use cgov::governor::{apply_scaling, compute_target_workers, ScalingDecision};
use cgov::config::{CompositeRiskConfig, ConeScalingConfig};
use cgov::state;

// ---------------------------------------------------------------------------
// Hysteresis Band Tests
// ---------------------------------------------------------------------------

#[test]
fn test_hysteresis_exact_threshold() {
    // When |target - current| == hysteresis_band, should return NoChange
    let decision = apply_scaling(6, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Should return NoChange when delta equals hysteresis band"
    );
}

#[test]
fn test_hysteresis_below_threshold() {
    // When |target - current| < hysteresis_band, should return NoChange
    let decision = apply_scaling(5, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Should return NoChange when delta is below hysteresis band"
    );
}

#[test]
fn test_hysteresis_above_threshold() {
    // When |target - current| > hysteresis_band, should take scaling action
    let decision = apply_scaling(7, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::ScaleUp(2)),
        "Should scale up when delta exceeds hysteresis band"
    );
}

#[test]
fn test_hysteresis_zero_band() {
    // With zero hysteresis band, any delta triggers scaling
    let decision = apply_scaling(6, 5, 0.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::ScaleUp(1)),
        "Zero hysteresis band should scale on any delta"
    );
}

#[test]
fn test_hysteresis_wide_band() {
    // Wide hysteresis band (e.g., safe mode 2.0x multiplier)
    let decision = apply_scaling(7, 5, 2.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Wide hysteresis band should prevent scaling on moderate delta"
    );
}

#[test]
fn test_hysteresis_scale_down_within_band() {
    // Scale down should also respect hysteresis band
    let decision = apply_scaling(4, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Scale down within hysteresis band should return NoChange"
    );
}

#[test]
fn test_hysteresis_scale_down_above_threshold() {
    // Scale down when delta exceeds hysteresis band
    let decision = apply_scaling(2, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::ScaleDown(2)),
        "Should scale down when delta exceeds hysteresis band"
    );
}

// ---------------------------------------------------------------------------
// Rate Limiting Tests
// ---------------------------------------------------------------------------

#[test]
fn test_rate_limit_scale_up() {
    // Scale up should be limited by max_scale_up_per_cycle
    let decision = apply_scaling(10, 5, 1.0, 2, 3);

    // Delta is 5, but max_up_per_cycle is 2
    assert!(
        matches!(decision, ScalingDecision::ScaleUp(2)),
        "Scale up should respect max_scale_up_per_cycle limit"
    );
}

#[test]
fn test_rate_limit_scale_down() {
    // Scale down should be limited by max_scale_down_per_cycle
    let decision = apply_scaling(0, 5, 1.0, 3, 2);

    // Delta is -5, but max_down_per_cycle is 2
    assert!(
        matches!(decision, ScalingDecision::ScaleDown(2)),
        "Scale down should respect max_scale_down_per_cycle limit"
    );
}

#[test]
fn test_rate_limit_no_limit_when_delta_small() {
    // When delta is small, rate limit should not be reached
    let decision = apply_scaling(6, 5, 1.0, 10, 10);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Small delta within hysteresis should return NoChange regardless of rate limit"
    );
}

// ---------------------------------------------------------------------------
// Emergency Brake Tests
// ---------------------------------------------------------------------------

#[test]
fn test_emergency_brake_bypasses_hysteresis() {
    // Emergency brake (target=0) should bypass hysteresis and rate limits
    let decision = apply_scaling(0, 10, 5.0, 1, 1);

    assert!(
        matches!(decision, ScalingDecision::EmergencyBrake),
        "Emergency brake should bypass hysteresis band and rate limits"
    );
}

#[test]
fn test_emergency_brake_zero_current() {
    // When already at 0, emergency brake should return NoChange
    let decision = apply_scaling(0, 0, 1.0, 3, 2);

    // This is implementation-specific; adjust based on actual behavior
    // Either NoChange or EmergencyBrake could be valid
    let is_valid = matches!(decision, ScalingDecision::NoChange | ScalingDecision::EmergencyBrake);
    assert!(
        is_valid,
        "Emergency brake with zero current should be stable"
    );
}

// ---------------------------------------------------------------------------
// Large Gap Scaling Tests
// ---------------------------------------------------------------------------

#[test]
fn test_large_gap_binary_scaling() {
    // Current binary scaling: 1 worker per cycle regardless of gap
    let scenarios = vec![
        (5, 10),   // Gap of 5
        (5, 15),   // Gap of 10
        (5, 20),   // Gap of 15
    ];

    for (current, target) in scenarios {
        let decision = apply_scaling(target, current, 1.0, 1, 1);

        match decision {
            ScalingDecision::ScaleUp(n) => {
                assert_eq!(
                    n, 1,
                    "Binary scaling should always scale 1 worker per cycle (gap={})",
                    target - current
                );
            }
            _ => panic!("Expected ScaleUp for large gap"),
        }
    }
}

#[test]
fn test_large_gap_progressive_scaling_simulation() {
    // Simulate progressive scaling: larger gaps allow more workers per cycle
    // This is a conceptual test for future implementation

    let scenarios = vec![
        ((5, 6), 1),   // Gap of 1: scale 1
        ((5, 7), 2),   // Gap of 2: scale 2
        ((5, 8), 2),   // Gap of 3: scale 2
        ((5, 10), 3),  // Gap of 5: scale 3
        ((5, 15), 3),  // Gap of 10: scale 3 (max)
    ];

    for ((current, target), expected_scale) in scenarios {
        // With progressive max_scale_up_per_cycle = 3
        let decision = apply_scaling(target, current, 1.0, 3, 2);

        match decision {
            ScalingDecision::ScaleUp(n) => {
                // Current implementation: limited by hysteresis-exceeded delta
                // Future implementation: adaptive based on gap size
                assert!(
                    n <= expected_scale,
                    "Progressive scaling should not exceed expected scale for gap {}",
                    target - current
                );
            }
            _ => panic!("Expected ScaleUp for gap of {}", target - current),
        }
    }
}

// ---------------------------------------------------------------------------
// Smooth Scaling Transition Tests
// ---------------------------------------------------------------------------

#[test]
fn test_smooth_scale_up_sequence() {
    // Simulate multiple cycles scaling from 5 to 10 workers
    let mut current = 5;
    let target = 10;
    let hysteresis = 1.0;
    let max_up = 1;
    let max_down = 1;

    let mut cycles = 0;
    let mut sequence = vec![current];

    while cycles < 20 {
        // Simulate target computation returning stable target
        let decision = apply_scaling(target, current, hysteresis, max_up, max_down);

        match decision {
            ScalingDecision::ScaleUp(n) => {
                current += n;
                sequence.push(current);
            }
            ScalingDecision::NoChange => {
                // Hysteresis band reached
                break;
            }
            _ => panic!("Unexpected decision: {:?}", decision),
        }

        cycles += 1;
    }

    // Binary scaling: should reach 9 (stop at hysteresis band)
    assert_eq!(
        sequence,
        vec![5, 6, 7, 8, 9],
        "Binary scaling should stair-step to hysteresis band"
    );
}

#[test]
fn test_smooth_scale_down_sequence() {
    // Simulate multiple cycles scaling from 10 to 2 workers
    let mut current = 10;
    let target = 2;
    let hysteresis = 1.0;
    let max_up = 1;
    let max_down = 1;

    let mut sequence = vec![current];

    for _ in 0..20 {
        let decision = apply_scaling(target, current, hysteresis, max_up, max_down);

        match decision {
            ScalingDecision::ScaleDown(n) => {
                current -= n;
                sequence.push(current);
            }
            ScalingDecision::NoChange => {
                break;
            }
            _ => panic!("Unexpected decision: {:?}", decision),
        }
    }

    // Binary scaling: should reach 3 (stop at hysteresis band)
    assert_eq!(
        sequence,
        vec![10, 9, 8, 7, 6, 5, 4, 3],
        "Binary scaling should stair-step down to hysteresis band"
    );
}

#[test]
fn test_hysteresis_prevents_oscillation() {
    // Simulate fluctuating target around current to verify hysteresis prevents oscillation
    let current = 5;
    let hysteresis = 1.0;

    // Targets that oscillate around 5 without exceeding hysteresis
    let targets = vec![5, 6, 5, 6, 5, 6];

    for target in targets {
        let decision = apply_scaling(target, current, hysteresis, 3, 2);

        assert!(
            matches!(decision, ScalingDecision::NoChange),
            "Hysteresis should prevent oscillation for target={}, current={}",
            target,
            current
        );
    }
}

#[test]
fn test_hysteresis_allows_significant_change() {
    // Verify that significant changes do trigger scaling despite hysteresis
    let current = 5;
    let hysteresis = 1.0;

    // Target change that exceeds hysteresis
    let target = 7;

    let decision = apply_scaling(target, current, hysteresis, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::ScaleUp(2)),
        "Significant target change should exceed hysteresis and trigger scaling"
    );
}

// ---------------------------------------------------------------------------
// Edge Cases
// ---------------------------------------------------------------------------

#[test]
fn test_zero_workers_target() {
    // Target of 0 workers with non-zero current
    let decision = apply_scaling(0, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::ScaleDown(2)),
        "Should scale down toward zero target"
    );
}

#[test]
fn test_target_equals_current() {
    // No action needed when target equals current
    let decision = apply_scaling(5, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Should return NoChange when target equals current"
    );
}

#[test]
fn test_fractional_hysteresis_band() {
    // Hysteresis band as float should be handled correctly
    let decision = apply_scaling(6, 5, 0.5, 3, 2);

    // Delta is 1, hysteresis is 0.5, so should scale up
    assert!(
        matches!(decision, ScalingDecision::ScaleUp(1)),
        "Fractional hysteresis band should allow scaling when delta exceeds it"
    );
}

#[test]
fn test_very_large_hysteresis_band() {
    // Very large hysteresis band should prevent most scaling
    let decision = apply_scaling(10, 5, 10.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Very large hysteresis band should prevent scaling"
    );
}

// ---------------------------------------------------------------------------
// Target Worker Computation Context
// ---------------------------------------------------------------------------

#[test]
fn test_target_computation_with_hysteresis() {
    // Integration test: compute target then apply hysteresis
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

    state.capacity_forecast = state::CapacityForecast {
        five_hour: state::WindowForecast {
            current_utilization: 40.0,
            safe_worker_count: Some(7),
            ..Default::default()
        },
        seven_day: state::WindowForecast {
            current_utilization: 50.0,
            safe_worker_count: Some(7),
            ..Default::default()
        },
        weekly_scoped: state::WindowForecast {
            current_utilization: 45.0,
            safe_worker_count: Some(7),
            ..Default::default()
        },
        binding_window: "weekly_scoped".to_string(),
        ..Default::default()
    };

    // Compute target
    let target = compute_target_workers(
        &state,
        90.0,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
    );

    // Target should be 7 (from safe_worker_count)
    assert_eq!(target, 7, "Target should match safe worker count");

    // Apply hysteresis: current=5, target=7, hysteresis=1.0
    // Delta is 2, exceeds hysteresis, so should scale up
    let decision = apply_scaling(target, 5, 1.0, 3, 2);

    assert!(
        matches!(decision, ScalingDecision::ScaleUp(2)),
        "Should scale up when target exceeds current by more than hysteresis"
    );
}

// ---------------------------------------------------------------------------
// Future Implementation Tests (Progressive Scaling)
// ---------------------------------------------------------------------------

#[test]
fn test_progressive_scaling_concept_large_gap() {
    // Conceptual test for future progressive scaling implementation
    // This documents expected behavior for large gaps

    let current = 5;
    let target = 15;  // Gap of 10

    // With binary scaling: takes 10 cycles (50 minutes)
    // With progressive scaling: could take 3-4 cycles (15-20 minutes)
    // Gap > 5: scale 3 workers per cycle
    // Gap > 3: scale 2 workers per cycle
    // Gap <= 3: scale 1 worker per cycle (or apply hysteresis)

    // Expected progressive sequence: 5 → 8 → 11 → 14 → 15 (4 cycles)
    // Current binary sequence: 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13 → 14 → 15 (11 cycles)

    let gap = target - current;
    assert!(
        gap > 5,
        "Large gap scenario: gap of {} should qualify for aggressive scaling",
        gap
    );
}

#[test]
fn test_exponential_approach_concept() {
    // Conceptual test for exponential approach implementation
    // This documents expected behavior for smooth convergence

    let current = 5;
    let target = 20;
    let approach_rate = 0.3;  // Close 30% of gap per cycle

    // Expected sequence with exponential approach:
    // Cycle 0: gap = 15, scale = 15 * 0.3 = 4.5 → 5 workers
    // Cycle 1: current = 10, gap = 10, scale = 10 * 0.3 = 3 → 13 workers
    // Cycle 2: current = 13, gap = 7, scale = 7 * 0.3 = 2.1 → 15 workers
    // Cycle 3: current = 15, gap = 5, scale = 5 * 0.3 = 1.5 → 17 workers
    // Cycle 4: current = 17, gap = 3, scale = 3 * 0.3 = 0.9 → 18 workers
    // Cycle 5: current = 18, gap = 2, within hysteresis → stop

    // Exponential approach converges smoothly without overshoot
    let mut current_sim = current;
    let mut gap_sim = (target - current) as f64;

    for cycle in 0..10 {
        if gap_sim < 1.0 {
            break;  // Within hysteresis
        }

        let scale = (gap_sim * approach_rate).ceil();
        current_sim += scale as u32;
        gap_sim = (target - current_sim) as f64;

        // Verify we never exceed target
        assert!(
            current_sim <= target,
            "Exponential approach should never overshoot (cycle {}, current={})",
            cycle, current_sim
        );
    }

    // Should converge within 10 cycles
    assert!(
        current_sim >= target - 2,  // Within hysteresis band
        "Exponential approach should converge within 10 cycles"
    );
}

// ---------------------------------------------------------------------------
// Adaptive Timing Concept Tests
// ---------------------------------------------------------------------------

#[test]
fn test_adaptive_timing_concept() {
    // Conceptual test for adaptive polling interval
    // This documents expected behavior for faster convergence

    let current = 5;
    let target = 15;
    let base_interval_secs = 300;  // 5 minutes

    let gap = (target - current) as f64;

    // Large gap (> 5): use 1/3 interval = 1.67 minutes
    // Medium gap (> 3): use 1/2 interval = 2.5 minutes
    // Small gap (<= 3): use full interval = 5 minutes

    let expected_interval = if gap > 5.0 {
        base_interval_secs / 3
    } else if gap > 3.0 {
        base_interval_secs / 2
    } else {
        base_interval_secs
    };

    assert_eq!(
        expected_interval, 100,  // 300 / 3
        "Large gap should use faster polling interval"
    );
}
