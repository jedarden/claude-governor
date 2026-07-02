# Governor Module Structure and Test Patterns

## Governor Module Types

### Core Types

1. **UsageSnapshot** - Snapshot of usage data for all windows
   - Fields: `windows: HashMap<String, f64>` (utilization percentages per window)
   - Key methods: `new()`, `from_windows()`, `get()`
   - Purpose: Captures utilization state for three windows (5h, 7d, 7d-sonnet)

2. **EmergencyBrake** - Emergency brake event
   - Fields: `triggered_window: String`, `utilization_pct: f64`
   - Purpose: Tracks when any window hits 98% utilization

3. **Agent** - Agent representation for scaling
   - Fields: `id: String`, `workers: u32`, `is_idle: bool`
   - Purpose: Represents a worker pool/agent in the fleet

4. **WindowContext** - Window context for sprint eligibility
   - Fields: `name`, `hours_remaining`, `headroom_pct`, `cutoff_risk`, `safe_worker_count`, `has_backlog`, `cone_ratio`
   - Purpose: Provides decision context for sprint triggers

5. **SprintState** - Active sprint state tracking
   - Fields: `worker_id`, `target_workers`, `window`, `original_workers`, `sprint_expires_at`, `normal_max_workers`
   - Purpose: Tracks underutilization recovery sprint state

6. **GovernorState** - Main governor state container
   - Fields: `emergency_brake_active`, `agents: HashMap<String, Agent>`, `emergency_brake: Option<EmergencyBrake>`, `sprint: Option<SprintState>`
   - Key methods:
     - `new()`, `add_agent()`, `scale_all_to_zero()`
     - `check_emergency_brake()`, `clear_emergency_brake()`, `update_emergency_brake()`
     - `apply_sprint()`, `clear_sprint()`, `check_sprint_end()`
     - `sprint_eligible()`, `check_eow_sprint_end()`, `compute_sprint_max_workers()`

7. **ScalingDecision** - Result of scaling decision
   - Variants: `NoChange`, `ScaleUp(u32)`, `ScaleDown(u32)`, `EmergencyBrake`
   - Purpose: Represents the action to take each cycle

### Key Public Functions

1. **Window Delta Calculation**
   - `calculate_window_pct_delta()` - Computes percentage deltas between API poll snapshots
   - `apportion_delta()` - Distributes total delta to specific session by USD weight

2. **Target Computation**
   - `compute_target_workers()` - Main target worker count from capacity forecast and schedule
   - `compute_pre_scale_target()` - Pre-scaling for upcoming multiplier transitions

3. **Scaling Actions**
   - `apply_scaling()` - Apply scaling decision with hysteresis band

4. **Safe Mode**
   - `update_safe_mode_from_calibration()` - Update safe mode based on calibration accuracy

5. **Main Loop**
   - `run_governor_cycle()` - Single governor daemon cycle (poll -> schedule -> burn_rate -> target -> scale -> alert -> write_state)
   - `run_daemon()` - Main daemon loop

### Constants

- `EMERGENCY_BRAKE_THRESHOLD: f64 = 98.0` - 98% hard stop threshold
- `SAFE_MODE_ENTRY_ERROR_THRESHOLD: f64 = 15.0` - Enter safe mode when median error exceeds this
- `SAFE_MODE_EXIT_ERROR_THRESHOLD: f64 = 8.0` - Exit safe mode when median error drops below this (hysteresis)
- `SAFE_MODE_MIN_SAMPLES: u32 = 5` - Minimum prediction samples before safe mode can trigger
- `SAFE_MODE_CEILING_REDUCTION: f64 = 5.0` - Target ceiling reduction during safe mode
- `SAFE_MODE_HYSTERESIS_MULTIPLIER: f64 = 2.0` - Hysteresis band multiplier during safe mode

## Test Patterns in Codebase

### Pattern 1: Unit Tests in `#[cfg(test)]` modules

Tests are organized in `#[cfg(test)]` modules at the bottom of files:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        // test implementation
    }
}
```

### Pattern 2: Helper Functions for Test Data Creation

Common pattern of factory/helper functions to create test fixtures:

```rust
/// Create a test forecast with configurable values
fn make_forecast(
    margin_hrs: f64,
    hours_remaining: f64,
    cutoff_risk: bool,
    binding: bool,
) -> WindowForecast {
    WindowForecast {
        margin_hrs,
        hours_remaining,
        cutoff_risk,
        // ...
        ..WindowForecast::default()
    }
}

/// Create a minimal governor state for testing
fn make_state(forecast: CapacityForecast) -> GovernorState {
    GovernorState {
        capacity_forecast: forecast,
        usage: UsageState::default(),
        // ...
    }
}
```

### Pattern 3: Descriptive Test Names

Tests use long, descriptive names following `test_<function>_<scenario>` pattern:

- `test_calculate_window_pct_delta_basic`
- `test_apportion_delta_zero_total_usd`
- `test_governor_cycle_emergency_brake`
- `test_governor_cycle_hysteresis_no_change`

### Pattern 4: Documentation Comments

Tests include doc comments explaining what they verify:

```rust
/// Test governor cycle with scaling decision within hysteresis band.
///
/// Verifies that when the target is within the hysteresis band of current,
/// the governor correctly decides to make no change.
#[test]
fn test_governor_cycle_hysteresis_no_change() {
```

### Pattern 5: Assertion Style

Uses standard assert macros with descriptive messages:

```rust
assert_eq!(target, 0, "Target should be 0 at 99% utilization");
assert!(matches!(decision, ScalingDecision::EmergencyBrake),
    "Should trigger EmergencyBrake decision at 99% utilization");
```

## Existing Test Infrastructure in governor.rs

### Test Module: `window_delta_tests`

Located at lines 674-1213, this module provides comprehensive testing for:

1. **Window percentage delta calculation** (`calculate_window_pct_delta`)
   - Basic delta computation
   - Negative deltas (window resets)
   - Zero previous values (first poll)
   - Mixed deltas (some windows increase, some decrease)
   - Small changes precision testing

2. **Delta apportionment** (`apportion_delta`)
   - Basic apportionment by weight
   - Zero total USD handling
   - Zero session USD handling
   - Equal weights
   - Negative total deltas
   - Fractional weights

3. **Consecutive API poll scenarios**
   - Non-zero deltas from consecutive snapshots
   - Zero deltas from identical snapshots
   - First poll handling (no previous snapshot)
   - Correct field mapping (five_hour_pct -> five_hour)
   - Negative deltas (window resets)
   - Mixed deltas

4. **Test helper functions**
   - `make_window_pct_snapshot()` - Create WindowPctSnapshot with custom values
   - `make_usage_snapshot()` - Create PrevUsageSnapshot with current timestamp
   - `make_usage_snapshot_with_time()` - Create PrevUsageSnapshot with custom timestamp

### Test Module: Main governor tests (lines 4895+)

Located near end of file, tests core governor cycle behavior:

1. **`test_governor_cycle_emergency_brake`** (lines 4900-4948)
   - Tests emergency brake activation at 99% utilization
   - Verifies target becomes 0 and EmergencyBrake decision is returned

2. **`test_governor_cycle_hysteresis_no_change`** (lines 4954-5001)
   - Tests hysteresis band logic
   - Verifies NoChange decision when target equals current

### Key Test Infrastructure Features

1. **Factory Functions**: Reusable functions to create test fixtures with defaults
2. **Default Trait Usage**: Uses `..Default::default()` for unspecified fields
3. **State Builders**: Pattern of building complex state incrementally
4. **Snapshot Testing**: Tests snapshot state transitions (current -> previous)
5. **Float Comparison**: Uses `f64::EPSILON` tolerance for floating point comparisons
6. **Time Handling**: Uses `chrono::Utc::now()` and explicit time parameters for deterministic testing

## Summary

The governor module is well-structured with:
- Clear separation of concerns (emergency brake, sprint, scaling decision)
- Comprehensive public API for state management and decision-making
- Strong test coverage following consistent patterns
- Good use of Rust idioms (defaults, builder pattern, Result types)
- Thorough documentation of behavior in doc comments

Test patterns emphasize:
- Factory functions for test fixture creation
- Descriptive test names and documentation
- Edge case coverage (zero values, negative values, resets)
- Time-based scenario testing
- Snapshot state transition testing
