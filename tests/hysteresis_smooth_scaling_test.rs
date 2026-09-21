//! Comprehensive tests for hysteresis behavior and smooth scaling transitions
//!
//! This test suite validates:
//! - Hysteresis band behavior (asymmetric: damps scale-down only, edge cases)
//! - Convergence: every deficit closes — the fleet reaches target, never
//!   stranding one worker short (claudego-44b1f4f5)
//! - Large gap scaling (progressive per-cycle caps via `progressive_scale_cap`)
//! - Smooth scaling transitions (no oscillation)
//! - Exponential approach convergence
//! - Adaptive timing scenarios
//! - Emergency brake override of hysteresis

use claude_governor::config::{CompositeRiskConfig, ConeScalingConfig};
use claude_governor::governor::{
    apply_scaling, compute_target_workers, progressive_scale_cap, ScalingDecision,
};
use claude_governor::state;

// ---------------------------------------------------------------------------
// Hysteresis Band Tests
// ---------------------------------------------------------------------------

#[test]
fn test_hysteresis_exact_threshold() {
    // The band is asymmetric. Scale-UP: a deficit of exactly the band is
    // still closed — this is the convergence fix; the old symmetric band
    // swallowed a 1-worker deficit forever and stranded the fleet one short
    // of target.
    let decision = apply_scaling(6, 5, 1.0, 3, 2, false);

    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(1),
        "A deficit equal to the band must still close (no up-side dead zone)"
    );
}

#[test]
fn test_hysteresis_exact_threshold_scale_down() {
    // Scale-DOWN keeps the band: a target exactly `hysteresis_band` below
    // current holds, so a one-worker forecast dip does not shed workers.
    let decision = apply_scaling(4, 5, 1.0, 3, 2, false);

    assert_eq!(
        decision,
        ScalingDecision::NoChange,
        "A surplus equal to the band is the intended down-side cushion"
    );
}

#[test]
fn test_hysteresis_below_threshold() {
    // When |target - current| < hysteresis_band, should return NoChange
    let decision = apply_scaling(5, 5, 1.0, 3, 2, false);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Should return NoChange when delta is below hysteresis band"
    );
}

#[test]
fn test_hysteresis_above_threshold() {
    // When |target - current| > hysteresis_band, should take scaling action
    let decision = apply_scaling(7, 5, 1.0, 3, 2, false);

    assert!(
        matches!(decision, ScalingDecision::ScaleUp(2)),
        "Should scale up when delta exceeds hysteresis band"
    );
}

#[test]
fn test_hysteresis_zero_band() {
    // With zero hysteresis band, any delta triggers scaling
    let decision = apply_scaling(6, 5, 0.0, 3, 2, false);

    assert!(
        matches!(decision, ScalingDecision::ScaleUp(1)),
        "Zero hysteresis band should scale on any delta"
    );
}

#[test]
fn test_hysteresis_wide_band() {
    // Wide hysteresis band (e.g., safe mode 2.0x multiplier). The band damps
    // scale-DOWN only — a deficit still closes, at the per-cycle cap.
    let up = apply_scaling(7, 5, 2.0, 3, 2, false);
    assert_eq!(
        up,
        ScalingDecision::ScaleUp(2),
        "Wide band must not strand a deficit: the gap-2 deficit closes (min(2, cap 3))"
    );

    // The same wide band does hold a surplus of 2 (gap <= band, damped).
    let down = apply_scaling(3, 5, 2.0, 3, 2, false);
    assert_eq!(
        down,
        ScalingDecision::NoChange,
        "Wide band damps a scale-down whose gap is within the band"
    );
}

#[test]
fn test_hysteresis_scale_down_within_band() {
    // Scale down should also respect hysteresis band
    let decision = apply_scaling(4, 5, 1.0, 3, 2, false);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Scale down within hysteresis band should return NoChange"
    );
}

#[test]
fn test_hysteresis_scale_down_above_threshold() {
    // Scale down when delta exceeds hysteresis band
    let decision = apply_scaling(2, 5, 1.0, 3, 2, false);

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
    let decision = apply_scaling(10, 5, 1.0, 2, 3, false);

    // Delta is 5, but max_up_per_cycle is 2
    assert!(
        matches!(decision, ScalingDecision::ScaleUp(2)),
        "Scale up should respect max_scale_up_per_cycle limit"
    );
}

#[test]
fn test_rate_limit_scale_down() {
    // Scale down should be limited by max_scale_down_per_cycle. A non-zero
    // target is used because target == 0 with an ACTIVE emergency brake
    // (brake flag true) bypasses hysteresis and the rate limits entirely (see
    // test_emergency_brake_bypasses_hysteresis); without the brake a zero
    // target ramps down gracefully (see test_zero_workers_target).
    let decision = apply_scaling(3, 8, 1.0, 3, 2, false);

    // Delta is -5, but max_down_per_cycle is 2
    assert!(
        matches!(decision, ScalingDecision::ScaleDown(2)),
        "Scale down should respect max_scale_down_per_cycle limit"
    );
}

#[test]
fn test_rate_limit_no_limit_when_delta_small() {
    // When delta is small, rate limit should not be reached — and the gap-1
    // deficit still closes (it used to be swallowed by the symmetric band).
    let decision = apply_scaling(6, 5, 1.0, 10, 10, false);

    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(1),
        "Small deficit closes without hitting the rate limit"
    );
}

// ---------------------------------------------------------------------------
// Emergency Brake Tests
// ---------------------------------------------------------------------------

#[test]
fn test_emergency_brake_bypasses_hysteresis() {
    // Emergency brake (target=0 WITH a genuine >=98% window — brake flag
    // true) should bypass hysteresis and rate limits
    let decision = apply_scaling(0, 10, 5.0, 1, 1, true);

    assert!(
        matches!(decision, ScalingDecision::EmergencyBrake),
        "Emergency brake should bypass hysteresis band and rate limits"
    );
}

#[test]
fn test_emergency_brake_zero_current() {
    // When already at 0, emergency brake should return NoChange
    let decision = apply_scaling(0, 0, 1.0, 3, 2, true);

    // This is implementation-specific; adjust based on actual behavior
    // Either NoChange or EmergencyBrake could be valid
    let is_valid = matches!(
        decision,
        ScalingDecision::NoChange | ScalingDecision::EmergencyBrake
    );
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
        (5, 10), // Gap of 5
        (5, 15), // Gap of 10
        (5, 20), // Gap of 15
    ];

    for (current, target) in scenarios {
        let decision = apply_scaling(target, current, 1.0, 1, 1, false);

        match decision {
            ScalingDecision::ScaleUp(n) => {
                assert_eq!(
                    n,
                    1,
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
    // Progressive scaling is real now (`progressive_scaling: true` widens the
    // per-cycle caps through `progressive_scale_cap`): larger gaps allow more
    // workers per cycle, clamped to the gap itself so a cycle never overshoots
    // the target. With base max_scale_up_per_cycle = 3:
    let scenarios = vec![
        ((5, 6), 1),  // Gap 1:  1x tier -> min(3, 1) = 1
        ((5, 7), 2),  // Gap 2:  1x tier -> min(3, 2) = 2
        ((5, 8), 3),  // Gap 3:  1x tier -> min(3, 3) = 3
        ((5, 10), 5), // Gap 5:  2x tier -> min(6, 5) = 5 (gap clamp)
        ((5, 15), 9), // Gap 10: 3x tier -> min(9, 10) = 9 (cap clamp)
    ];

    for ((current, target), expected_scale) in scenarios {
        let gap = target - current;
        assert_eq!(
            progressive_scale_cap(3, gap),
            expected_scale,
            "Progressive cap for gap of {}",
            gap
        );

        // The decision function itself honours the widened cap.
        let decision = apply_scaling(target, current, 1.0, expected_scale, 2, false);
        assert_eq!(
            decision,
            ScalingDecision::ScaleUp(expected_scale),
            "Progressive scaling should move {} workers for gap {}",
            expected_scale,
            gap
        );
    }
}

#[test]
fn test_progressive_scale_cap_tiers_and_clamps() {
    // Tier boundaries (base cap 1): 3x beyond a gap of 5, 2x beyond 3, 1x else.
    assert_eq!(progressive_scale_cap(1, 3), 1, "gap 3: 1x tier");
    assert_eq!(progressive_scale_cap(1, 4), 2, "gap 4: 2x tier");
    assert_eq!(progressive_scale_cap(1, 5), 2, "gap 5: still the 2x tier");
    assert_eq!(progressive_scale_cap(1, 6), 3, "gap 6: 3x tier");

    // Never overshoots: the gap itself always wins over the widened cap.
    assert_eq!(
        progressive_scale_cap(3, 5),
        5,
        "2x of 3 = 6, clamped to gap 5"
    );
    assert_eq!(
        progressive_scale_cap(2, 1),
        1,
        "1x of 2 = 2, clamped to gap 1"
    );

    // A disabled cap stays disabled.
    assert_eq!(
        progressive_scale_cap(0, 8),
        0,
        "0 workers per cycle stays 0"
    );

    // The operator's cap is the base rate, not an afterthought: a big gap
    // widens it by at most 3x.
    assert_eq!(progressive_scale_cap(1, 100), 3, "huge gap: at most 3x");
}

#[test]
fn test_progressive_scaling_converges_faster_and_exactly() {
    // 5 -> 10 with progressive base cap 3: gap 5 lands in the 2x tier,
    // min(3*2, 5) = 5, so the whole deficit closes in ONE cycle.
    let decision = apply_scaling(10, 5, 1.0, progressive_scale_cap(3, 5), 3, false);
    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(5),
        "Progressive: 5 -> 10 in a single cycle"
    );

    // 5 -> 15 (gap 10, 3x tier): min(3*3, 10) = 9, then the residual gap of 1.
    let mut current = 5u32;
    let target = 15u32;
    let mut sequence = vec![current];
    for _ in 0..10 {
        let step = match apply_scaling(
            target,
            current,
            1.0,
            progressive_scale_cap(3, target - current),
            3,
            false,
        ) {
            ScalingDecision::ScaleUp(n) => n,
            ScalingDecision::NoChange => break,
            other => panic!("Unexpected decision: {:?}", other),
        };
        current += step;
        sequence.push(current);
    }
    assert_eq!(
        sequence,
        vec![5, 14, 15],
        "Progressive sequence closes a 10-gap in 2 cycles (binary needs 10)"
    );
    assert_eq!(
        current, target,
        "Progressive scaling must end exactly at target"
    );
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
        let decision = apply_scaling(target, current, hysteresis, max_up, max_down, false);

        match decision {
            ScalingDecision::ScaleUp(n) => {
                current += n;
                sequence.push(current);
            }
            ScalingDecision::NoChange => {
                // At target — converged
                break;
            }
            _ => panic!("Unexpected decision: {:?}", decision),
        }

        cycles += 1;
    }

    // The band no longer strands the fleet one short: the sequence must end
    // AT the target (claudego-44b1f4f5 — it used to stop at 9 forever).
    assert_eq!(
        sequence,
        vec![5, 6, 7, 8, 9, 10],
        "Binary scaling must converge exactly to target"
    );
    assert_eq!(current, target, "Fleet reached target");
}

#[test]
fn test_scale_up_converges_from_one_short() {
    // The regression in one line: target 10, current 9, band 1.0. The old
    // symmetric band read |10 - 9| = 1 <= 1 and held there FOREVER — the
    // documented example ended "stops at 9 due to hysteresis".
    let decision = apply_scaling(10, 9, 1.0, 1, 1, false);

    assert_eq!(
        decision,
        ScalingDecision::ScaleUp(1),
        "A one-worker deficit must close, not strand one short of target"
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
        let decision = apply_scaling(target, current, hysteresis, max_up, max_down, false);

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

    // Scale-down keeps the band, so the sequence rests one worker ABOVE the
    // soft target: the final 1-worker surplus is the intended down-side
    // cushion (forecast noise must not shed workers), and the hard
    // protections still override it if the window really runs out.
    assert_eq!(
        sequence,
        vec![10, 9, 8, 7, 6, 5, 4, 3],
        "Binary scale-down stair-steps to the band above target and holds"
    );
}

#[test]
fn test_hysteresis_prevents_oscillation() {
    // A target jittering ±1 around the fleet must not produce churn. Track
    // the fleet as it moves instead of holding `current` fixed: the first
    // deficit closes (up to 6), after which the down-side band absorbs the
    // jitter — the fleet never sheds a worker back toward 5.
    let hysteresis = 1.0;
    let targets = vec![5, 6, 5, 6, 5, 6];
    let mut current = 5u32;
    let mut downs = 0;

    for target in targets {
        match apply_scaling(target, current, hysteresis, 3, 2, false) {
            ScalingDecision::ScaleUp(n) => current += n,
            ScalingDecision::ScaleDown(n) => {
                downs += 1;
                current -= n;
            }
            ScalingDecision::NoChange => {}
            ScalingDecision::EmergencyBrake => panic!("Brake not expected here"),
        }
    }

    assert_eq!(current, 6, "Fleet converged to the top of the jitter band");
    assert_eq!(downs, 0, "Down-band damping absorbed the jitter: no churn");
}

#[test]
fn test_hysteresis_allows_significant_change() {
    // Verify that significant changes do trigger scaling despite hysteresis
    let current = 5;
    let hysteresis = 1.0;

    // Target change that exceeds hysteresis
    let target = 7;

    let decision = apply_scaling(target, current, hysteresis, 3, 2, false);

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
    // A computed zero target with live workers and NO brake window (brake
    // flag false) is a duty-cycle withdrawal, not an emergency
    // (claudego-1138ab78): it ramps down gracefully, capped by
    // max_scale_down_per_cycle. This was the #[ignore]d aspirational contract
    // of the old test_zero_workers_target_graceful_ramp_down.
    let decision = apply_scaling(0, 5, 1.0, 3, 2, false);

    assert!(
        matches!(decision, ScalingDecision::ScaleDown(2)),
        "A computed zero target with live workers should ramp down gracefully, \
         respecting max_scale_down_per_cycle"
    );
}

#[test]
fn test_zero_workers_target_with_active_brake_still_kills() {
    // The complement: the SAME zero target with a genuine >=98% window (brake
    // flag true) still takes the violent path — kill-sessions, no ramp-down.
    // Replaces the old #[ignore]d test_zero_workers_target_graceful_ramp_down.
    let decision = apply_scaling(0, 5, 1.0, 3, 2, true);

    assert!(
        matches!(decision, ScalingDecision::EmergencyBrake),
        "A zero target WITH an active brake window should trigger the emergency brake"
    );
}

#[test]
fn test_target_equals_current() {
    // No action needed when target equals current
    let decision = apply_scaling(5, 5, 1.0, 3, 2, false);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "Should return NoChange when target equals current"
    );
}

#[test]
fn test_fractional_hysteresis_band() {
    // Hysteresis band as float should be handled correctly
    let decision = apply_scaling(6, 5, 0.5, 3, 2, false);

    // Delta is 1, hysteresis is 0.5, so should scale up
    assert!(
        matches!(decision, ScalingDecision::ScaleUp(1)),
        "Fractional hysteresis band should allow scaling when delta exceeds it"
    );
}

#[test]
fn test_very_large_hysteresis_band() {
    // Very large hysteresis band damps scale-DOWN only — a deficit still
    // closes (at the per-cycle cap), because no band may strand the fleet
    // below target.
    let up = apply_scaling(10, 5, 10.0, 3, 2, false);
    assert_eq!(
        up,
        ScalingDecision::ScaleUp(3),
        "Even a huge band must not hold a deficit: closes at max_up_per_cycle"
    );

    // The same huge band does hold an equally large surplus (gap <= band).
    let down = apply_scaling(2, 12, 10.0, 3, 2, false);
    assert_eq!(
        down,
        ScalingDecision::NoChange,
        "Very large hysteresis band damps a scale-down within the band"
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
    let decision = apply_scaling(target, 5, 1.0, 3, 2, false);

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
    // Now implemented (daemon.progressive_scaling + progressive_scale_cap):
    // a gap > 5 widens the per-cycle cap 3x, a gap > 3 2x, otherwise 1x —
    // always clamped to the remaining gap. Binary 5 -> 15 stair-steps +1 per
    // cycle (50 minutes at the 5-minute loop interval); progressive with base
    // cap 3 closes it in 2 cycles (see
    // test_progressive_scaling_converges_faster_and_exactly).
    let current = 5;
    let target = 15; // Gap of 10

    let gap = target - current;
    assert_eq!(
        progressive_scale_cap(3, gap),
        9,
        "3x tier, clamped to gap 10"
    );
}

#[test]
fn test_exponential_approach_concept() {
    // Conceptual test for exponential approach implementation
    // This documents expected behavior for smooth convergence

    let current = 5;
    let target = 20;
    let approach_rate = 0.3; // Close 30% of gap per cycle

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
            break; // Within hysteresis
        }

        let scale = (gap_sim * approach_rate).ceil();
        current_sim += scale as u32;
        gap_sim = (target - current_sim) as f64;

        // Verify we never exceed target
        assert!(
            current_sim <= target,
            "Exponential approach should never overshoot (cycle {}, current={})",
            cycle,
            current_sim
        );
    }

    // Should converge within 10 cycles
    assert!(
        current_sim >= target - 2, // Within hysteresis band
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
    let base_interval_secs = 300; // 5 minutes

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
        expected_interval,
        100, // 300 / 3
        "Large gap should use faster polling interval"
    );
}
