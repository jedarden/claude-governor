# Governor Module Examination - BF-1qx8n

## Task Summary
Examine src/governor.rs to understand:
- Current module structure
- Existing governor types and methods
- Test patterns used elsewhere in the codebase

## Governor Module Structure

### File Overview
- **Location**: `src/governor.rs`
- **Size**: 5000+ lines
- **Purpose**: Core governor logic for capacity management and scaling decisions
- **Main Loop**: `poll -> schedule -> burn_rate -> target -> scale -> alert -> write_state`

### Key Constants
```rust
const EMERGENCY_BRAKE_THRESHOLD: f64 = 98.0;
const SAFE_MODE_ENTRY_ERROR_THRESHOLD: f64 = 15.0;
const SAFE_MODE_EXIT_ERROR_THRESHOLD: f64 = 8.0;
const SAFE_MODE_MIN_SAMPLES: u32 = 5;
const SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT: u32 = 3;
const SAFE_MODE_CEILING_REDUCTION: f64 = 5.0;
const SAFE_MODE_HYSTERESIS_MULTIPLIER: f64 = 2.0;

pub const WINDOW_FIVE_HOUR: &str = "five_hour";
pub const WINDOW_SEVEN_DAY: &str = "seven_day";
pub const WINDOW_SEVEN_DAY_SONNET: &str = "seven_day_sonnet";
```

## Governor Types and Key Methods

### Core Data Structures

#### 1. UsageSnapshot (lines 64-97)
**Purpose**: Snapshot of usage data for all windows

**Fields**:
- `windows: HashMap<String, f64>` - Per-window utilization percentages

**Key Methods**:
- `new()` - Create empty snapshot
- `from_windows(five_hour, seven_day, seven_day_sonnet)` - Create from individual values
- `get(window: &str) -> Option<f64>` - Get utilization for specific window

#### 2. EmergencyBrake (lines 99-107)
**Purpose**: Emergency brake event when utilization >= 98%

**Fields**:
- `triggered_window: String` - Which window triggered the brake
- `utilization_pct: f64` - Utilization percentage that triggered

#### 3. Agent (lines 109-120)
**Purpose**: Agent representation for scaling decisions

**Fields**:
- `id: String` - Agent identifier
- `workers: u32` - Current worker count
- `is_idle: bool` - Whether agent has no active work

#### 4. WindowContext (lines 122-139)
**Purpose**: Context for sprint eligibility evaluation

**Fields**:
- `name: String` - Window name
- `hours_remaining: f64` - Hours until window reset
- `headroom_pct: f64` - Remaining headroom as percentage
- `cutoff_risk: bool` - Whether window has cutoff risk
- `safe_worker_count: Option<u32>` - Safe worker count for this window
- `has_backlog: bool` - Whether there's bead backlog
- `cone_ratio: Option<f64>` - Confidence cone ratio

#### 5. SprintState (lines 141-156)
**Purpose**: Active sprint state for underutilization recovery

**Fields**:
- `worker_id: String` - Which agent/worker pool is sprinting
- `target_workers: u32` - Target worker count during sprint
- `window: String` - Window that triggered sprint
- `original_workers: u32` - Worker count before sprint (for restoration)
- `sprint_expires_at: Option<DateTime<Utc>>` - When sprint ends
- `normal_max_workers: u32` - Normal max workers before sprint boost

#### 6. GovernorState (lines 158-562)
**Purpose**: Main governor state structure

**Fields**:
- `emergency_brake_active: bool` - Whether emergency brake is active
- `agents: HashMap<String, Agent>` - Tracked agents
- `emergency_brake: Option<EmergencyBrake>` - Emergency brake event if active
- `sprint: Option<SprintState>` - Active sprint state

**Key Methods**:
- `new()` - Create new governor state
- `add_agent(id, workers, is_idle)` - Add or update an agent
- `scale_all_to_zero()` - Scale all agents to 0 workers
- `check_emergency_brake(usage) -> Option<EmergencyBrake>` - Check if brake should apply
- `clear_emergency_brake(usage) -> bool` - Clear brake if utilization dropped
- `update_emergency_brake(usage) -> Option<EmergencyBrake>` - Combined check and clear
- `apply_sprint(trigger)` - Apply sprint trigger
- `clear_sprint() -> bool` - Clear active sprint
- `check_sprint_end(usage, sprint_config) -> bool` - Check if sprint should end
- `is_sprint_active() -> bool` - Check if sprint is active
- `sprint_eligible(window_ctx, other_windows, config) -> bool` - Check sprint eligibility
- `check_eow_sprint_end(window_ctx, config, now) -> bool` - Check end-of-window sprint end
- `compute_sprint_max_workers(normal_max, other_windows, config) -> u32` - Compute effective max during sprint

#### 7. ScalingDecision (lines 574-585)
**Purpose**: Result of scaling decision in one cycle

**Variants**:
- `NoChange` - Within hysteresis band or already at target
- `ScaleUp(u32)` - Scale up by N workers
- `ScaleDown(u32)` - Scale down by N workers (graceful)
- `EmergencyBrake` - Scale all to zero

#### 8. WindowPctDeltas
**Purpose**: Per-window percentage deltas (from state.rs)

**Fields**:
- `five_hour: f64` - 5-hour window delta
- `seven_day: f64` - 7-day window delta
- `seven_day_sonnet: f64` - 7-day Sonnet window delta

#### 9. PrevUsageSnapshot (from state.rs, lines 128-151)
**Purpose**: Previous API usage snapshot for computing percentage deltas across cycles

**Fields**:
- `taken_at: DateTime<Utc>` - When snapshot was taken
- `five_hour_pct: f64` - 5-hour window utilization
- `seven_day_pct: f64` - 7-day window utilization
- `seven_day_sonnet_pct: f64` - 7-day Sonnet window utilization

### Key Functions

#### Delta Computation
- `calculate_window_pct_delta(previous_snapshot, current_snapshot) -> (f64, f64, f64)` (line 634) - Calculate percentage deltas between consecutive API poll snapshots
- `apportion_delta(total_delta, total_usd, session_total_usd) -> f64` (line 666) - Apportion total delta to specific session by USD weight

#### Scaling Decision Functions
- `safe_worker_count_or_max(safe, max_workers, current_total) -> u32` (line 595) - Resolve safe_worker_count to concrete target with fallback
- `compute_target_workers(state, target_ceiling, composite_risk_config, cone_scaling_config) -> u32` (line 1435) - Compute target worker count from capacity forecast
- `apply_scaling(target, current, hysteresis_band, max_up_per_cycle, max_down_per_cycle) -> ScalingDecision` (line 1569) - Apply scaling with hysteresis
- `compute_pre_scale_target(now, pre_scale_minutes, promotions, reset_time, target, current_total, window) -> Option<u32>` (line 1638) - Compute effective target accounting for multiplier transition

#### Safe Mode
- `update_safe_mode_from_calibration(safe_mode, calibration, stats, now) -> bool` (line 1707) - Update safe mode state based on calibration accuracy

#### Agent Cost Priority
- `extract_model_from_launch_cmd(launch_cmd) -> Option<String>` (line 1227) - Extract model name from launch command
- `get_agent_cost_per_worker(agent_name, agent_config, burn_rate_by_model, pricing_config) -> f64` (line 1249) - Get per-worker dollar cost
- `distribute_workers_by_cost_priority(agents, current_workers, target_total, ...) -> HashMap<String, u32>` (line 1313) - Distribute workers by cost priority

#### Alert FP Telemetry
- `is_true_positive_alert(alert_type, state) -> bool` (line 1776) - Classify alert as true/false positive

#### Main Loop
- `run_governor_cycle(poller, state_path, dry_run, ...) -> anyhow::Result<()>` (line 1813) - Run one governor cycle

## Test Patterns in the Codebase

### Pattern 1: Module-Level Test Organization

Tests are organized in `#[cfg(test)]` modules within the source file:

```rust
#[cfg(test)]
mod window_delta_tests {
    use super::*;
    
    #[test]
    fn test_calculate_window_pct_delta_basic() {
        // Test implementation
    }
}
```

**Example from governor.rs** (lines 674-1213):
- Dedicated `window_delta_tests` module for window delta computation tests
- Tests grouped by functionality (delta calculation, apportionment, snapshot helpers)
- Uses descriptive test names like `test_calculate_window_pct_delta_basic`

### Pattern 2: Helper Functions for Test Data

**Example from state.rs** (lines 899-1068):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a fully populated GovernorState for round-trip testing
    fn full_state() -> GovernorState {
        // Constructs a complete state with all fields populated
    }
}
```

This pattern provides:
- Reusable test data builders
- Consistent state across multiple tests
- Easy maintenance (single place to update test data structure)

### Pattern 3: Property-Based Testing

Tests cover various scenarios:
- Basic functionality
- Edge cases (zeros, negatives)
- First-time scenarios (no previous data)
- Window resets (negative deltas)
- Mixed scenarios (some windows increase, some decrease)

**Example from governor.rs**:
```rust
#[test]
fn test_negative_deltas_window_reset() {
    // Simulates window reset where utilization drops
    let prev = PrevUsageSnapshot { five_hour_pct: 80.0, ... };
    let curr = PrevUsageSnapshot { five_hour_pct: 5.0, ... };
    // Verify negative deltas
}
```

### Pattern 4: Round-Trip Serialization Tests

**Example from state.rs**:
```rust
#[test]
fn round_trip_full_state() {
    let state = full_state();
    let json = serde_json::to_string(&state).unwrap();
    let loaded: GovernorState = serde_json::from_str(&json).unwrap();
    // Verify all fields survived the round-trip
}
```

### Pattern 5: TempDir for File-Based Tests

**Example from state.rs**:
```rust
use tempfile::TempDir;

#[test]
fn save_state_creates_file() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("governor-state.json");
    // Test file operations
}
```

### Pattern 6: State Transition Tests

Tests verify state machine transitions:

**Example from state.rs**:
```rust
#[test]
fn update_api_snapshot_first_poll_sets_current_only() {
    let mut state = GovernorState::new();
    state.update_api_snapshot(now, 10.0, 20.0, 15.0);
    assert!(state.previous_api_snapshot.is_none());
    assert!(state.current_api_snapshot.is_some());
}
```

### Pattern 7: Precision and Tolerance Testing

For floating-point values:

**Example from governor.rs**:
```rust
#[test]
fn test_delta_precision_small_changes() {
    const TOL: f64 = 1e-9;
    assert!((delta_5h - 0.1).abs() < TOL);
}
```

### Pattern 8: Documentation Examples in Tests

Tests serve as usage documentation:

**Example from governor.rs** (lines 624-633):
```rust
/// # Example
/// ```
/// use claude_governor::db::WindowPctSnapshot;
/// use claude_governor::governor::calculate_window_pct_delta;
/// let prev = WindowPctSnapshot { five_hour: 10.0, ... };
/// let curr = WindowPctSnapshot { five_hour: 12.5, ... };
/// let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);
/// assert_eq!(d5h, 2.5);
/// ```
```

## Existing Test Infrastructure in governor.rs

### Test Module 1: window_delta_tests (lines 674-1213)

**Test Helper Functions:**
- `make_window_pct_snapshot(five_hour, seven_day, seven_day_sonnet)` (line 1098)
- `make_usage_snapshot(five_hour_pct, seven_day_pct, seven_day_sonnet_pct)` (line 1133)
- `make_usage_snapshot_with_time(taken_at, ...)` (line 1170)

**Test Coverage:**
- `test_calculate_window_pct_delta_basic` (line 679) - basic delta computation
- `test_calculate_window_pct_delta_negative_deltas` (line 697) - window reset scenarios
- `test_calculate_window_pct_delta_zero_previous` (line 715) - first poll case
- `test_apportion_delta_basic` (line 733) - USD-weighted delta apportionment
- `test_apportion_delta_zero_total_usd` (line 740) - edge case handling
- `test_apportion_delta_zero_session_usd` (line 746) - edge case handling
- `test_apportion_delta_equal_weights` (line 752) - equal weight distribution
- `test_apportion_delta_negative_total_delta` (line 762) - window reset case
- `test_apportion_delta_fractional_weights` (line 769) - fractional weight handling
- `test_consecutive_snapshots_non_zero_deltas` (line 784) - consecutive API poll simulation
- `test_identical_snapshots_zero_deltas` (line 832) - no-change scenario
- `test_first_poll_no_previous_snapshot` (line 870) - first poll handling
- `test_delta_uses_correct_window_fields` (line 917) - field pairing verification
- `test_negative_deltas_window_reset` (line 960) - window reset negative deltas
- `test_mixed_deltas_increase_and_decrease` (line 1008) - mixed delta scenarios
- `test_delta_precision_small_changes` (line 1050) - precision testing
- `test_snapshot_helpers_create_valid_structs` (line 1189) - helper validation

### Test Module 2: tests (from state.rs, lines 894-1654)

**Test Helper Functions:**
- `full_state()` (line 899) - Build fully populated GovernorState for round-trip testing

**Test Coverage:**
- Round-trip serialization/deserialization
- Load from missing/corrupt files
- Atomic write operations
- Previous state management
- Default values for optional fields
- Delta computation
- Snapshot state tracking
- JSON field name matching

### Test Naming Convention

Tests use descriptive `test_<function>_<scenario>` naming:
- `test_calculate_window_pct_delta_basic`
- `test_negative_deltas_window_reset`
- `test_first_poll_no_previous_snapshot`
- `test_snapshot_helpers_create_valid_structs`

### Assertion Patterns

- `assert_eq!` for exact value matching
- `assert!` for boolean conditions
- Floating-point epsilon comparisons: `assert!((value - expected).abs() < f64::EPSILON)`
- Custom tolerances: `const TOL: f64 = 1e-9; assert!((delta - expected).abs() < TOL);`

## Key Observations

1. **Comprehensive test infrastructure already exists** in governor.rs with multiple test modules
2. **Helper functions are preferred** for creating test data (make_usage_snapshot, make_window_pct_snapshot, etc.)
3. **tempfile crate** is used extensively for temp file/directory management
4. **Floating-point comparisons** use epsilon-based comparisons for f64 values
5. **Test modules use #[cfg(test)]** attribute to exclude from production builds
6. **Tests are co-located with source code** (not in separate tests/ directory)
7. **Governor module is large** (5000+ lines) with extensive test coverage already in place
8. **Tests serve as documentation** - many tests include doc comments explaining behavior
9. **State transition testing** - emphasis on testing state machine transitions
10. **Test isolation** - each test is independent and can run in any order

## Summary

The governor module is well-structured with:
- Clear separation of concerns (state management, scaling decisions, emergency brake, sprint logic)
- Comprehensive type system for capacity management
- Existing test infrastructure following Rust conventions
- Good test coverage for core delta computation logic
- Patterns ready for extension to new functionality

### Key Takeaways for New Tests

1. Use `#[cfg(test)]` modules for test organization
2. Create helper functions to build test data
3. Use `tempfile::TempDir` for file-based tests
4. Test state transitions, not just static states
5. Use appropriate tolerances for floating-point comparisons
6. Include doc examples that double as tests
7. Group related tests in sub-modules
8. Use descriptive test names that explain what is being tested
9. Focus on testing behaviors and edge cases
10. Maintain test isolation and independence