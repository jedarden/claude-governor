# Hysteresis and Smooth Scaling Transitions in Claude Governor

## Overview

This document describes the hysteresis implementation and scaling behavior in Claude Governor (cgov), including current behavior, identified gaps, and proposed improvements for smoother transitions.

## Current Implementation

### Hysteresis Band

**Location**: `src/governor.rs`, function `apply_scaling()` (lines 4836-4883)

The hysteresis band prevents oscillation between scale-up and scale-down decisions by requiring a minimum delta before taking action:

```rust
pub fn apply_scaling(
    target: u32,
    current: u32,
    hysteresis_band: f64,
    max_up_per_cycle: u32,
    max_down_per_cycle: u32,
) -> ScalingDecision {
    let delta = target as i32 - current as i32;
    let hysteresis = hysteresis_band as i32;

    // No change if within hysteresis band
    if delta.abs() <= hysteresis {
        return ScalingDecision::NoChange;
    }

    // Scale up or down, limited by per-cycle maximums
    if delta > 0 {
        let scale = (delta as u32).min(max_up_per_cycle);
        return ScalingDecision::ScaleUp(scale);
    }

    let scale = (delta.abs() as u32).min(max_down_per_cycle);
    ScalingDecision::ScaleDown(scale)
}
```

**Configuration**: `config/governor.yaml`
```yaml
daemon:
  hysteresis_band: 1.0          # Minimum delta before scaling
  max_scale_up_per_cycle: 1     # Maximum workers to add per cycle
  max_scale_down_per_cycle: 1   # Maximum workers to remove per cycle
  min_scale_interval_secs: 60   # Minimum time between scale operations
  loop_interval_secs: 300        # 5-minute polling cycle
```

### Safe Mode Hysteresis

Safe mode activation also uses hysteresis to prevent flapping:

**Entry threshold**: 15% median absolute error
**Exit threshold**: 8% median absolute error (hysteresis gap)
**Hysteresis multiplier**: 2.0x (widens the hysteresis band during safe mode)

```rust
// Lines 46-59 in governor.rs
const SAFE_MODE_ENTRY_ERROR_THRESHOLD: f64 = 15.0;
const SAFE_MODE_EXIT_ERROR_THRESHOLD: f64 = 8.0;  // Hysteresis gap
const SAFE_MODE_HYSTERESIS_MULTIPLIER: f64 = 2.0;
```

### Target Worker Computation

**Location**: `src/governor.rs`, function `compute_target_workers()` (lines 4702-4826)

The target worker count is computed from:

1. **Emergency brake check**: Any window ≥ 98% → target = 0
2. **Binding window selection**: Five-hour, seven-day, or weekly-scoped
3. **Cone-based scaling**: 
   - Narrow cone (low uncertainty) → use p50 median estimate
   - Wide cone (high uncertainty) → use p75 conservative estimate
4. **Composite risk optimization** (optional): Balances risk across all windows
5. **Min/max bounds**: Respects per-agent configured limits

```rust
pub fn compute_target_workers(
    state: &state::GovernorState,
    _target_ceiling: f64,
    composite_risk_config: &CompositeRiskConfig,
    cone_scaling_config: &ConeScalingConfig,
) -> u32 {
    // 1. Emergency brake
    // 2. Select binding window
    // 3. Apply cone-based scaling (p50 vs p75)
    // 4. Try composite risk optimization
    // 5. Apply global min/max bounds
}
```

## Current Behavior Analysis

### Scaling Rate Limits

The current implementation uses **binary, per-cycle rate limiting**:

- **Maximum scale-up**: 1 worker per 5-minute cycle
- **Maximum scale-down**: 1 worker per 5-minute cycle
- **Result**: Large adjustments take many cycles to complete

### Example: Large Scale-Up

**Scenario**: Current = 5 workers, Target = 10 workers

| Cycle | Workers | Delta | Action |
|-------|---------|-------|--------|
| 0     | 5       | +5    | Scale up 1 → 6 |
| 1     | 6       | +4    | Scale up 1 → 7 |
| 2     | 7       | +3    | Scale up 1 → 8 |
| 3     | 8       | +2    | Scale up 1 → 9 |
| 4     | 9       | +1    | Within hysteresis → NoChange |
| 5     | 9       | +1    | NoChange (hysteresis) |

**Result**: Takes 25 minutes to reach 10, then stops at 9 due to hysteresis.

### Example: Gradual Scale-Down

**Scenario**: Current = 10 workers, Target = 2 workers

| Cycle | Workers | Delta | Action |
|-------|---------|-------|--------|
| 0     | 10      | -8    | Scale down 1 → 9 |
| 1     | 9       | -7    | Scale down 1 → 8 |
| ...   | ...     | ...   | ... |
| 7     | 3       | -1    | Within hysteresis → NoChange |
| 8     | 3       | -1    | NoChange (hysteresis) |

**Result**: Takes 40 minutes to reach 3, then stops due to hysteresis.

## Identified Gaps

### 1. Binary Scaling (Coarse Granularity)

**Problem**: Only scales 0 or 1 workers per cycle, regardless of the gap magnitude.

**Impact**:
- Large adjustments (e.g., 5 → 10 workers) take many cycles
- No acceleration for large gaps
- Fixed rate regardless of how far off the target is

### 2. No Progressive Ramping

**Problem**: Scaling rate doesn't adapt to the distance from target.

**Expected behavior**:
- Far from target (e.g., 5 → 10): Scale up faster (2-3 workers per cycle)
- Close to target (e.g., 9 → 10): Scale up slower (1 worker, or wait for hysteresis)

### 3. No Exponential Approach

**Problem**: Linear approach doesn't smooth the transition curve.

**Expected behavior**:
- Rapid initial scaling when far from target
- Gradual deceleration as target approaches
- Smooth asymptotic approach to prevent overshoot

### 4. Fixed Rate Limiting

**Problem**: `max_scale_up_per_cycle` and `max_scale_down_per_cycle` are static values.

**Expected behavior**:
- Adaptive limits based on:
  - Distance from target
  - Rate of change in utilization
  - Time until window reset

## Proposed Improvements

### Option A: Progressive Scaling (Recommended)

Implement adaptive scaling rates based on distance from target:

```rust
fn compute_scale_amount(
    target: u32,
    current: u32,
    max_per_cycle: u32,
) -> u32 {
    let gap = (target as i32 - current as i32).abs() as u32;
    
    // Scale more aggressively when far from target
    let scale_factor = if gap > 5 {
        3.0  // Large gap: scale up to 3 workers
    } else if gap > 3 {
        2.0  // Medium gap: scale up to 2 workers
    } else {
        1.0  // Small gap: scale 1 worker
    };
    
    ((max_per_cycle as f64) * scale_factor).min(gap) as u32
}
```

**Benefits**:
- Faster response to large changes
- Smoother transitions for small changes
- Maintains safety caps

### Option B: Exponential Decay

Use exponential decay to smoothly approach the target:

```rust
fn compute_exponential_scale(
    target: u32,
    current: u32,
    max_per_cycle: u32,
) -> u32 {
    let gap = (target as i32 - current as i32).abs() as f64;
    
    // Exponential approach: each cycle closes a fraction of the gap
    let approach_rate = 0.3;  // Close 30% of remaining gap per cycle
    let desired_scale = (gap * approach_rate).ceil() as u32;
    
    desired_scale.min(max_per_cycle)
}
```

**Benefits**:
- Rapid initial scaling when gap is large
- Smooth deceleration as target approaches
- Never overshoots the target

### Option C: Adaptive Timing

Reduce the polling interval during large adjustments:

```rust
fn compute_poll_interval(
    target: u32,
    current: u32,
    base_interval_secs: u64,
) -> Duration {
    let gap = (target as i32 - current as i32).abs() as u32;
    
    // Poll more frequently when far from target
    if gap > 5 {
        Duration::from_secs(base_interval_secs / 3)  // 1/3 interval
    } else if gap > 3 {
        Duration::from_secs(base_interval_secs / 2)  // 1/2 interval
    } else {
        Duration::from_secs(base_interval_secs)      // Normal interval
    }
}
```

**Benefits**:
- Faster convergence without changing per-cycle limits
- Maintains hysteresis safety
- Easy to implement

## Implementation Priority

### Phase 1: Document Current Behavior (This Document)

✅ **Status**: Complete

This document provides a comprehensive reference for:
- How hysteresis currently works
- What the scaling behavior is
- What gaps exist
- What improvements are possible

### Phase 2: Add Comprehensive Tests

**Needed tests**:
1. Hysteresis band edge cases (exactly at threshold)
2. Large gap scaling behavior (5 → 10 workers)
3. Progressive scaling scenarios
4. Exponential approach validation
5. Adaptive timing behavior

**Test file**: `tests/hysteresis_smooth_scaling_test.rs`

### Phase 3: Implement Progressive Scaling

**Implementation**:
1. Add `progressive_scaling` config option to `governor.yaml`
2. Modify `apply_scaling()` to use adaptive scale amounts
3. Add integration tests
4. Document behavior in CLAUDE.md

### Phase 4: Monitor and Tune

**Metrics to collect**:
- Time to reach target for various gap sizes
- Oscillation frequency (should remain low with hysteresis)
- Utilization smoothness (rate of change in worker count)

## Configuration Examples

### Current Configuration (Binary Scaling)

```yaml
daemon:
  hysteresis_band: 1.0
  max_scale_up_per_cycle: 1
  max_scale_down_per_cycle: 1
  loop_interval_secs: 300  # 5 minutes
```

### Progressive Scaling Configuration

```yaml
daemon:
  hysteresis_band: 1.0
  max_scale_up_per_cycle: 3      # Allow up to 3
  max_scale_down_per_cycle: 3    # Allow up to 3
  progressive_scaling: true       # Enable adaptive rates
  loop_interval_secs: 300
```

### Exponential Approach Configuration

```yaml
daemon:
  hysteresis_band: 1.0
  max_scale_up_per_cycle: 5
  max_scale_down_per_cycle: 5
  scaling_mode: exponential       # New mode
  approach_rate: 0.3              # Close 30% of gap per cycle
  loop_interval_secs: 300
```

## Testing Strategy

### Unit Tests

Test the scaling decision logic in isolation:

```rust
#[test]
fn test_progressive_scaling_large_gap() {
    let decision = apply_scaling(
        10,  // target
        5,   // current
        1.0, // hysteresis
        3,   // max_up (progressive)
        2,   // max_down
    );
    
    // Should scale up by 3 (max), not just 1
    assert!(matches!(decision, ScalingDecision::ScaleUp(3)));
}

#[test]
fn test_exponential_approach_converges() {
    let mut current = 5;
    let target = 10;
    
    for cycle in 0..10 {
        let decision = apply_scaling_exponential(
            target,
            current,
            1.0,
            5,
            5,
            0.3,  // approach_rate
        );
        
        match decision {
            ScalingDecision::ScaleUp(n) => current += n,
            ScalingDecision::NoChange => break,
            _ => panic!("Unexpected decision"),
        };
        
        if current >= target { break; }
    }
    
    // Should reach target within 10 cycles
    assert!(current >= target);
}
```

### Integration Tests

Test end-to-end behavior with realistic scenarios:

```rust
#[test]
fn test_hysteresis_prevents_oscillation() {
    // Simulate fluctuating utilization around threshold
    // Verify hysteresis prevents rapid scale-up/scale-down
}

#[test]
fn test_smooth_scaling_transition() {
    // Start at 5 workers, target 10
    // Verify smooth progression: 5 → 7 → 9 → 10
    // Not: 5 → 6 → 7 → 8 → 9 → 10
}
```

## Safety Considerations

### Hysteresis Must Remain

Even with progressive/exponential scaling, the hysteresis band must remain to prevent oscillation:

```rust
// Always apply hysteresis FIRST
if delta.abs() <= hysteresis {
    return ScalingDecision::NoChange;
}

// THEN compute adaptive scale amount
let scale = compute_adaptive_scale(delta, max_per_cycle);
```

### Emergency Brake Override

The emergency brake (≥98% utilization) must always bypass hysteresis and rate limits:

```rust
if target == 0 && current > 0 {
    return ScalingDecision::EmergencyBrake;  // Immediate scale to 0
}
```

### Min/Max Bounds

Per-agent min/max bounds must always be respected:

```rust
let new_count = (current as i32 + scale_delta)
    .max(agent.min as i32)
    .min(agent.max as i32) as u32;
```

## References

- **Source code**: `src/governor.rs` lines 4702-4883
- **Configuration**: `config/governor.yaml`
- **Tests**: `tests/governor_cycle_snapshot_test.rs`
- **Related modules**: `src/burn_rate.rs`, `src/worker.rs`, `src/calibrator.rs`

## Version History

- **2026-08-23**: Initial documentation of hysteresis and scaling behavior
- Identified gaps in smooth scaling transitions
- Proposed progressive, exponential, and adaptive improvements
