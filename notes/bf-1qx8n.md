# Governor Module Structure and Test Patterns

## Governor Module Overview

**File**: `src/governor.rs` (~5000 lines, 195KB)

The governor module is the core of the capacity management system, handling:
- Emergency brake detection (98% hard stop)
- Underutilization sprint triggering and management
- End-of-window capacity sprint
- Governor state management
- Agent scaling decisions
- Main daemon loop: poll → schedule → burn_rate → target → scale → alert → write_state

## Core Types and Methods

### 1. Data Structures

#### `UsageSnapshot` (lines 64-97)
- Holds utilization percentages for all tracked windows
- **Key methods**: `new()`, `from_windows()`, `get()`
- Stores data in `HashMap<String, f64>` with window names as keys

#### `EmergencyBrake` (lines 99-107)
- Records which window triggered the emergency brake
- Fields: `triggered_window`, `utilization_pct`

#### `Agent` (lines 109-120)
- Represents a worker pool for scaling
- Fields: `id`, `workers`, `is_idle`

#### `WindowContext` (lines 122-139)
- Context for sprint eligibility evaluation
- Fields: name, hours_remaining, headroom_pct, cutoff_risk, safe_worker_count, has_backlog, cone_ratio

#### `SprintState` (lines 141-156)
- Active sprint state tracking
- Fields: worker_id, target_workers, window, original_workers, sprint_expires_at, normal_max_workers

#### `GovernorState` (lines 158-562)
- Main state structure with comprehensive methods
- **Key methods**:
  - `new()` - Constructor
  - `add_agent()` - Add/update agent tracking
  - `scale_all_to_zero()` - Emergency brake action
  - `check_emergency_brake()` - Detect 98% threshold breach
  - `clear_emergency_brake()` - Reset when utilization drops
  - `update_emergency_brake()` - Combined check + clear
  - `apply_sprint()` - Boost workers for sprint
  - `clear_sprint()` - Restore original workers
  - `check_sprint_end()` - Test if sprint should end
  - `sprint_eligible()` - Test end-of-window sprint conditions
  - `compute_sprint_max_workers()` - Calculate effective max during sprint

### 2. Decision Types

#### `ScalingDecision` (lines 574-585)
Enum for scaling actions:
- `NoChange` - Within hysteresis band
- `ScaleUp(u32)` - Add N workers
- `ScaleDown(u32)` - Remove N workers gracefully
- `EmergencyBrake` - Scale all to zero immediately

### 3. Key Functions

#### Window Delta Calculation (lines 607-673)
- `calculate_window_pct_delta()` - Compute percentage changes between API poll snapshots
- `apportion_delta()` - Distribute fleet-wide delta to individual sessions by USD weight

#### Agent Cost Priority (lines 1217-1410)
- `extract_model_from_launch_cmd()` - Parse --agent flag from command
- `get_agent_cost_per_worker()` - Look up per-worker cost (burn_rate → pricing → default)
- `distribute_workers_by_cost_priority()` - Scale up/down by cost (low→high, high→low)

#### Target Computation (lines 1412-1559)
- `compute_target_workers()` - Main target calculation with cone-based scaling
- Uses binding window's safe_worker_count (p50 or p75 based on cone_ratio)
- Supports composite risk optimization
- Applies sprint boost if active

#### Scaling Decision (lines 1561-1616)
- `apply_scaling()` - Apply hysteresis band and rate limits
- Returns `ScalingDecision` based on delta vs hysteresis

#### Pre-scale Logic (lines 1618-1691)
- `compute_pre_scale_target()` - Conservative-only: pre-scale down before losing multiplier bonus
- Never pre-scales up (only scales down before off-peak→peak transition)

#### Safe Mode Calibration (lines 1693-1765)
- `update_safe_mode_from_calibration()` - Enter/exit safe mode based on prediction error
- Entry: median error > 15.0%
- Exit: median error < 8.0% (hysteresis)

#### Main Daemon Loop (lines 1807-3177)
- `run_governor_cycle()` - Complete cycle: poll → schedule → burn_rate → target → scale → alert → write_state
- Approximately 1300 lines of orchestration logic

## Existing Test Infrastructure in governor.rs

### Test Module: `window_delta_tests` (lines 674-1213)

**Location**: Lines 674-1213 (540 lines of tests)

**Test Pattern**: The module already contains comprehensive tests for window delta calculations:

#### Test Functions (18 tests):
1. `test_calculate_window_pct_delta_basic` - Basic delta calculation
2. `test_calculate_window_pct_delta_negative_deltas` - Window resets
3. `test_calculate_window_pct_delta_zero_previous` - First poll handling
4. `test_apportion_delta_basic` - Fleet delta apportionment
5. `test_apportion_delta_zero_total_usd` - Edge case handling
6. `test_apportion_delta_zero_session_usd` - Edge case handling
7. `test_apportion_delta_equal_weights` - Equal distribution
8. `test_apportion_delta_negative_total_delta` - Window reset case
9. `test_apportion_delta_fractional_weights` - Fractional USD weights
10. `test_consecutive_snapshots_non_zero_deltas` - Real poll cycle simulation
11. `test_identical_snapshots_zero_deltas` - No-change scenario
12. `test_first_poll_no_previous_snapshot` - First poll edge case
13. `test_delta_uses_correct_window_fields` - Field mapping verification
14. `test_negative_deltas_window_reset` - Reset detection
15. `test_mixed_deltas_increase_and_decrease` - Realistic mixed behavior
16. `test_delta_precision_small_changes` - Precision verification
17. `test_snapshot_helpers_create_valid_structs` - Helper function validation

#### Helper Functions (documented with rustdoc):
- `make_window_pct_snapshot(five_hour, seven_day, seven_day_sonnet)` - Create test snapshots
- `make_usage_snapshot(five_hour_pct, seven_day_pct, seven_day_sonnet_pct)` - Create usage snapshots with current timestamp
- `make_usage_snapshot_with_time(taken_at, ...)` - Create snapshots with custom timestamp

**Pattern characteristics**:
- Tests use `assert!` with floating-point tolerance (`f64::EPSILON` or custom `TOL`)
- Helper functions are documented with `///` comments including examples
- Tests cover normal cases, edge cases, and precision
- Uses `chrono::Utc` for deterministic timestamps

## Test Patterns Used Elsewhere in Codebase

### From `src/state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Helper function to build fully populated test state
    fn full_state() -> GovernorState {
        // ... constructs complex test data
    }
}
```

**Pattern**:
- Uses `#[cfg(test)]` module attribute
- Uses `tempfile::TempDir` for file-based tests
- Helper functions like `full_state()` to build complex test data
- Tests focus on serialization/deserialization roundtrips

### From `src/alerts.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use std::collections::HashMap;

    fn base_now() -> DateTime<Utc> {
        "2026-03-20T10:00:00Z".parse().unwrap()
    }

    fn make_window(cutoff_risk: bool, margin_hrs: f64, hrs_left: f64) -> WindowForecast {
        // ... creates test window
    }
}
```

**Pattern**:
- Uses `chrono::Utc` for deterministic time-based tests
- Helper functions like `base_now()`, `make_window()` for consistent test data
- Tests verify alert conditions, thresholds, and firing logic

### From `src/burn_rate.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_worker_count_basic() {
        // Basic functionality
    }

    #[test]
    fn test_safe_worker_count_no_burn_rate() {
        // Edge case: no data
    }
}
```

**Pattern**:
- Individual `#[test]` functions for specific scenarios
- Tests use descriptive names: `test_<function>_<scenario>`
- Focus on edge cases and boundary conditions

## Key Governor Methods to Test

Based on the module structure, these are the key methods that should have test coverage:

### Emergency Brake
- `GovernorState::check_emergency_brake()` - Already has implicit tests in main cycle
- `GovernorState::clear_emergency_brake()` - Tests would verify 98% threshold behavior
- `GovernorState::update_emergency_brake()` - Combined check + clear logic

### Sprint Management
- `GovernorState::apply_sprint()` - Worker boost application
- `GovernorState::clear_sprint()` - Worker restoration
- `GovernorState::check_sprint_end()` - Sprint termination conditions
- `GovernorState::sprint_eligible()` - Eligibility logic (multiple conditions)
- `GovernorState::compute_sprint_max_workers()` - Sprint ceiling calculation

### Scaling Decisions
- `compute_target_workers()` - Main target calculation (cone-based, composite risk)
- `apply_scaling()` - Hysteresis and rate limiting
- `compute_pre_scale_target()` - Conservative pre-scaling

### Safe Mode
- `update_safe_mode_from_calibration()` - Entry/exit thresholds

### Agent Distribution
- `distribute_workers_by_cost_priority()` - Cost-based worker distribution

## Testing Strategy Recommendations

1. **Follow existing patterns**: Use `#[cfg(test)]` modules with helper functions
2. **Use deterministic time**: `chrono::Utc` with fixed timestamps like `base_now()`
3. **Document helpers**: Use `///` comments with examples
4. **Test edge cases**: Zero values, negative deltas, boundary conditions
5. **Use floating-point tolerance**: `f64::EPSILON` or custom `TOL` for comparisons
6. **Test realistic scenarios**: Consecutive polls, window resets, mixed behavior

## Test Helper Patterns to Reuse

```rust
// From governor.rs window_delta_tests:
fn make_window_pct_snapshot(five_hour: f64, seven_day: f64, seven_day_sonnet: f64) -> WindowPctSnapshot
fn make_usage_snapshot(five_hour_pct: f64, seven_day_pct: f64, seven_day_sonnet_pct: f64) -> PrevUsageSnapshot

// From alerts.rs:
fn base_now() -> DateTime<Utc>
fn make_window(cutoff_risk: bool, margin_hrs: f64, hrs_left: f64) -> WindowForecast

// From state.rs:
fn full_state() -> GovernorState
```

## Summary

The governor module is well-structured with clear separation of concerns:
- **Data structures** capture state (UsageSnapshot, GovernorState, SprintState)
- **Decision types** enumerate actions (ScalingDecision)
- **Core functions** implement business logic (emergency brake, sprint, scaling)
- **Helper functions** support operations (window delta, cost priority, pre-scale)

The existing `window_delta_tests` module demonstrates excellent test patterns:
- Comprehensive coverage (18 tests for delta calculation)
- Helper functions for test data construction
- Edge case and precision testing
- Realistic scenario simulation

Other modules (`state.rs`, `alerts.rs`, `burn_rate.rs`) follow similar patterns with `#[cfg(test)]` modules, helper functions, and descriptive test names.
