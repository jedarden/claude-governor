# Bead bf-kkwnp: Test Module Structure Analysis

## Task
Examine existing test module structure in `src/governor.rs` to understand test patterns, organization, and first poll test coverage.

---

## Test Module Organization

The test module in `src/governor.rs` is organized into four distinct test modules:

### 1. `governor_state_tests` (lines 571-710)
Tests for `GovernorState` struct methods:
- `test_governor_state_new` - Default state initialization
- `test_governor_state_add_agent` - Agent addition/update
- `test_governor_state_scale_all_to_zero` - Scaling behavior
- `test_emergency_brake_triggers_at_threshold` - Emergency brake at 98%
- `test_emergency_brake_no_trigger_below_threshold` - No brake below 98%
- `test_clear_emergency_brake` - Brake clearing logic
- `test_apply_sprint` - Sprint state activation
- `test_clear_sprint` - Sprint state clearing

### 2. `window_delta_tests` (lines 816-1598)
Tests for delta computation between consecutive API polls:

**Basic delta calculation:**
- `test_calculate_window_pct_delta_basic` - Basic delta computation
- `test_calculate_window_pct_delta_negative_deltas` - Window reset scenarios
- `test_calculate_window_pct_delta_zero_previous` - First poll edge case
- `test_apportion_delta_*` - Delta apportionment by USD weight (5 tests)

**First poll handling tests (lines 1011-1598):**
- `test_first_poll_no_previous_snapshot` (line 1012) - Graceful first poll handling
- `test_first_poll_delta_defaults_to_zero` (line 1367) - Deltas set to Some(0.0) on first poll
- `test_first_poll_zero_deltas_regardless_of_current_values` (line 1432) - Zero deltas across all utilization levels
- `test_consecutive_polls_after_first_poll_computes_deltas` (line 1509) - Transition from first to second poll

**Snapshot integration tests:**
- `test_consecutive_snapshots_non_zero_deltas` - Non-zero deltas from consecutive snapshots
- `test_identical_snapshots_zero_deltas` - Zero deltas from identical snapshots
- `test_delta_uses_correct_window_fields` - Field mapping verification
- `test_negative_deltas_window_reset` - Window reset detection
- `test_mixed_deltas_increase_and_decrease` - Mixed delta scenarios
- `test_delta_precision_small_changes` - Precision testing with small values

**Test helper functions (lines 1240-1355):**
- `make_window_pct_snapshot()` - Creates `WindowPctSnapshot` instances
- `make_usage_snapshot()` - Creates `PrevUsageSnapshot` with current time
- `make_usage_snapshot_with_time()` - Creates `PrevUsageSnapshot` with custom time
- `test_snapshot_helpers_create_valid_structs` - Helper validation

### 3. `tests` (lines 3716-5188)
Main integration test module with helper functions and subsystem tests:

**Helper functions (lines 3746-3808):**
- `make_usage_snapshot()` - Creates `UsageSnapshot` instances
- `make_usage_snapshot_from_map()` - Creates from custom HashMap
- `governor_with_agents()` - Creates test `GovernorState` with pre-configured agents

**Emergency brake tests:**
- `test_97_9_pct_no_brake` - Below threshold behavior
- `test_98_0_pct_brake_triggers` - Exact threshold trigger
- `test_brake_scales_all_agents_to_zero` - Brake scaling behavior
- `test_brake_overrides_hysteresis` - Brake overrides idle status
- `test_brake_clears_below_98_pct` - Brake clearing logic
- `test_brake_clears_on_window_reset` - Reset-based clearing
- `test_brake_triggers_on_any_window` - Multi-window trigger
- `test_brake_does_not_clear_if_still_above_threshold` - Persistent brake
- `test_update_combines_check_and_clear` - Combined behavior
- `test_empty_agents_still_sets_flag` - Empty agent handling

**Sprint tests:**
- `sprint_apply_boosts_agent_to_max` - Sprint application
- `sprint_clear_restores_original_workers` - Sprint clearing
- `sprint_clear_returns_false_when_no_sprint` - No-op clearing
- `sprint_blocked_during_emergency_brake` - Brake interaction
- `sprint_not_reapplied_when_already_active` - Idempotency
- `sprint_ends_when_utilization_exceeds_threshold` - Sprint completion
- `sprint_continues_when_utilization_below_threshold` - Sprint persistence
- `sprint_end_noop_when_no_sprint` - No-op handling
- `new_governor_has_no_sprint` - Initial state

**Pre-scale tests:**
- `pre_scale_triggers_before_losing_multiplier_bonus` - Transition detection
- `compute_pre_scale_target_triggers_at_07_35` - Core bead test
- `compute_pre_scale_target_no_trigger_outside_window` - Window boundary
- `compute_pre_scale_target_never_triggers_for_gaining_bonus` - Conservative-only behavior
- `compute_pre_scale_target_no_trigger_when_already_at_post_target` - Idempotency
- `compute_pre_scale_target_disabled_when_zero` - Configuration test
- `pre_scale_does_not_trigger_when_outside_window` - Boundary enforcement
- `pre_scale_never_triggers_for_gaining_bonus` - Conservative-only enforcement
- `schedule_state_per_window_applies_to_filtering` - Per-window filtering

**Safe mode calibration tests:**
- `safe_mode_enters_when_accuracy_degrades` - Entry conditions
- `safe_mode_does_not_enter_below_threshold` - Threshold enforcement
- `safe_mode_does_not_enter_with_insufficient_samples` - Sample count requirement
- `safe_mode_exits_when_accuracy_recovers` - Exit conditions
- `safe_mode_does_not_exit_with_insufficient_new_predictions` - Exit sample requirement
- `safe_mode_does_not_exit_when_error_still_high` - Hysteresis enforcement
- `safe_mode_syncs_calibration_state` - State synchronization
- `safe_mode_entry_uses_absolute_error` - Absolute error handling

**Baseline dollar fallback tests:**
- `baseline_dollar_fallback_produces_nonzero_pct_hr` - Fallback formula verification
- `safe_worker_count_none_uses_max_workers` - Fallback logic
- `safe_worker_count_some_zero_uses_current_total` - Zero handling
- `safe_worker_count_some_nonzero_uses_value` - Normal case
- `compute_target_workers_none_safe_count_falls_back_to_max` - Integration test

**Cost priority distribution tests:**
- `distribute_scale_down_reduces_highest_cost_first` - Scale-down priority
- `distribute_scale_up_adds_lowest_cost_first` - Scale-up priority
- `distribute_respects_max_workers_constraint` - Constraint enforcement
- `distribute_uses_burn_rate_when_available` - Empirical data priority

**Consecutive snapshot delta tests (lines 4976-5188):**
- `test_consecutive_snapshot_delta_computation` - Full delta computation flow
- `test_consecutive_snapshot_delta_with_window_reset` - Reset detection
- `test_consecutive_snapshot_delta_identical_snapshots` - Zero delta handling

### 4. `mock_poller_tests` (lines 5597-5822)
Tests for `MockPoller` testing infrastructure:
- `test_mock_poller_default_returns_usage_data` - Default behavior
- `test_mock_poller_returns_error` - Error simulation
- `test_mock_poller_returns_stale_data` - Stale data simulation
- `test_mock_poller_custom_utilization` - Custom values
- `test_mock_poller_emergency_brake` - Emergency brake scenario
- `test_mock_poller_low_utilization` - Low utilization scenario
- `test_mock_poller_high_utilization` - High utilization scenario
- `test_mock_poller_poll_count_tracking` - Invocation tracking
- `test_mock_poller_set_error` - Dynamic error configuration
- `test_mock_poller_set_usage_data` - Dynamic data configuration
- `test_mock_poller_reusability` - Cross-test reuse
- `test_mock_poller_multiple_calls_consistency` - Consistent behavior
- `test_mock_poller_extreme_values` - Boundary testing

---

## Test Patterns and Naming Conventions

### Naming Convention
- Test functions use snake_case with `test_` prefix
- Descriptive names that describe what is being tested
- Pattern: `test_[feature]_[scenario]_[expected_outcome]`

### Examples
```rust
test_emergency_brake_triggers_at_threshold
test_first_poll_delta_defaults_to_zero
test_consecutive_polls_after_first_poll_computes_deltas
test_distribute_scale_down_reduces_highest_cost_first
```

### Organization Patterns
1. **Test modules grouped by functionality**
   - `governor_state_tests` - State management
   - `window_delta_tests` - Delta computation
   - `tests` - Integration tests
   - `mock_poller_tests` - Testing infrastructure

2. **Helper functions defined within test modules**
   - Factory functions for creating test data
   - Setup functions for common test scenarios
   - All helpers are `pub fn` with doc comments

3. **Test structure within each test**
   - Setup: Create test data/state
   - Execute: Call the function under test
   - Verify: Assert expected behavior
   - Clear comments explaining test purpose

### Documentation Pattern
- Each test has a doc comment explaining its purpose
- Helper functions have detailed documentation with examples
- Comments explain complex assertions or edge cases

---

## First Poll Test Coverage

### Existing First Poll Tests (4 dedicated tests)

| Test Name | Line | Purpose |
|-----------|------|---------|
| `test_first_poll_no_previous_snapshot` | 1012 | Tests graceful handling when `previous_api_snapshot` is None, verifies delta computation is skipped, ensures no panic occurs |
| `test_first_poll_delta_defaults_to_zero` | 1367 | Tests that deltas are set to Some(0.0) on first poll, verifies the pattern match logic for (None, Some) case, confirms no delta computation occurs |
| `test_first_poll_zero_deltas_regardless_of_current_values` | 1432 | Tests that delta defaults to 0.0 for any current utilization, covers multiple scenarios: low (10%), medium (50%), high (95%), zero (0%), ensures consistent behavior across all utilization ranges |
| `test_consecutive_polls_after_first_poll_computes_deltas` | 1509 | Tests transition from first poll (deltas = 0) to second poll (deltas computed), verifies that first poll sets deltas to 0.0, verifies that second poll computes actual deltas (2.5, 2.0, 3.0) |

### Additional Related Tests

| Test Name | Line | Purpose |
|-----------|------|---------|
| `test_consecutive_snapshots_non_zero_deltas` | 926 | Baseline test for consecutive snapshot behavior |
| `test_identical_snapshots_zero_deltas` | 974 | Tests when consecutive polls have identical values |
| `test_delta_uses_correct_window_fields` | 1059 | Verifies field mapping (five_hour_pct → five_hour, etc.) |
| `test_negative_deltas_window_reset` | 1102 | Tests negative deltas when window resets |
| `test_mixed_deltas_increase_and_decrease` | 1150 | Tests mixed delta scenarios |
| `test_delta_precision_small_changes` | 1192 | Tests precision with small percentage changes (0.001%) |

### First Poll Implementation Details

**In `run_governor_cycle()` (lines 2222-2300):**

1. **Snapshot shift before poll (line 2223):**
```rust
state.previous_api_snapshot = state.current_api_snapshot.take();
// On first poll, current_api_snapshot is None, so previous becomes None too
```

2. **Pattern matching for delta computation (lines 2255-2300):**
```rust
match (&state.previous_api_snapshot, &state.current_api_snapshot) {
    (Some(prev), Some(curr)) => {
        // Both snapshots available: proceed with delta computation
        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
        state.p5h_delta = Some(delta_5h);
        state.p7d_delta = Some(delta_7d);
        state.p7ds_delta = Some(delta_7ds);
    }
    (None, Some(_curr)) => {
        // First poll: no previous snapshot available, cannot compute delta
        // Set delta fields to zero to indicate no change from initial state
        state.p5h_delta = Some(0.0);
        state.p7d_delta = Some(0.0);
        state.p7ds_delta = Some(0.0);
        log::info!("[governor] first poll detected (no previous snapshot), skipping delta computation; deltas set to zero");
    }
    (None, None) | (Some(_), None) => {
        // Neither snapshot available OR only previous available: handle gracefully
        log::warn!("[governor] unexpected snapshot state...");
    }
}
```

---

## Gaps in First Poll Test Coverage

### Missing Tests

1. **No integration test for first poll handling in `run_governor_cycle`**
   - While unit tests cover the delta computation logic, there's no test of the full cycle flow
   - The `test_governor_cycle_basic_flow` test uses a single snapshot but doesn't test first poll scenario
   - **Recommendation**: Add test using `MockPoller` to verify full first poll behavior

2. **No test for snapshot state transition**
   - Missing test for `state.previous_api_snapshot = state.current_api_snapshot.take()` behavior
   - No verification that current becomes previous on cycle start
   - **Recommendation**: Test the complete transition:
     - Start with both snapshots None
     - First poll → current becomes Some, previous remains None
     - Second poll → previous becomes Some (was current), current becomes new value
     - Verify deltas are computed on second poll

3. **No test for delta field population**
   - Missing test for `state.p5h_delta`, `state.p7d_delta`, `state.p7ds_delta` field population
   - No verification that state fields are correctly updated from delta computation
   - **Recommendation**: Add test verifying state field population from delta computation

4. **No test for edge cases with both snapshots None**
   - The pattern match handles `(None, None)` case but doesn't test it explicitly
   - No test for `(Some(_), None)` case (only previous exists, current poll failed)
   - **Recommendation**: Add explicit tests for these edge cases

5. **No test for logging behavior on first poll**
   - The code logs "first poll detected" message but this isn't tested
   - No verification that the correct log message is emitted
   - **Recommendation**: Add test verifying log message emission on first poll

6. **No test for interaction with burn_rate.prev_usage_snapshot**
   - First poll affects burn rate computation (lines 2566-2696)
   - No test verifying interaction between first poll handling and burn rate EMA
   - **Recommendation**: Add test for first poll interaction with burn rate EMA computation

---

## Coverage Strengths

1. **Comprehensive unit coverage** of delta computation logic
2. **Multiple first poll scenarios** tested (zero deltas, consecutive polls, various utilization levels)
3. **Helper functions** well-tested and documented
4. **Integration tests** for governor cycle flow (though first poll specifically is missing)
5. **Clear naming conventions** and test organization patterns
6. **Good documentation** with docstrings explaining test purpose

---

## Summary

### Current State
- **Total first poll tests**: 4 dedicated tests in `window_delta_tests`
- **Coverage**: Good coverage of delta computation logic for first poll
- **Patterns**: Clear naming conventions, good use of helper functions

### Strengths
1. First poll logic is well-tested at the delta computation level
2. Tests cover the key scenarios (None previous, zero deltas, transition to second poll)
3. Helper functions make tests readable and maintainable
4. Test organization follows clear patterns by functionality

### Gaps
1. No integration test with actual `run_governor_cycle()` behavior
2. No verification of logging behavior
3. Limited testing of snapshot state initialization/transitions
4. Missing edge case tests for (None, None) and (Some, None) patterns
5. No testing of interaction with burn rate EMA computation

### Test Organization Quality
- **High quality**: Tests are well-organized by functionality
- **Good documentation**: Tests have clear docstrings explaining purpose
- **Readable**: Helper functions and clear naming make tests easy to understand
