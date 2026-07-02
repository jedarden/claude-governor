# Unit Tests for Snapshot Delta Computation (bf-18hwm)

## Summary

Verified that comprehensive unit tests for snapshot delta computation already exist in `src/governor.rs` within the `window_delta_tests` module (lines 674-1071).

## Test Coverage

### 1. Consecutive Snapshots with Non-Zero Deltas
**Test:** `test_consecutive_snapshots_non_zero_deltas` (lines 784-825)

Tests that consecutive API polls produce correct positive deltas:
- Previous snapshot: (5h: 10.0%, 7d: 20.0%, 7ds: 15.0%)
- Current snapshot: (5h: 12.5%, 7d: 22.0%, 7ds: 18.0%)
- Expected deltas: (5h: +2.5%, 7d: +2.0%, 7ds: +3.0%)

### 2. Identical Snapshots Produce Zero Deltas
**Test:** `test_identical_snapshots_zero_deltas` (lines 832-862)

Tests that when API percentages haven't changed between polls:
- All three delta values are exactly 0.0
- Handles the case where previous == current

### 3. First Poll Handling (No Previous Snapshot)
**Test:** `test_first_poll_no_previous_snapshot` (lines 870-908)

Tests the graceful handling when:
- `previous_api_snapshot` is `None` (governor start or state clear)
- `current_api_snapshot` is `Some(...)`
- Code correctly skips delta computation when previous snapshot doesn't exist

### 4. Delta Uses Correct Window Fields
**Test:** `test_delta_uses_correct_window_fields` (lines 917-954)

Verifies field pairing:
- `five_hour_pct` → `five_hour`
- `seven_day_pct` → `seven_day`
- `seven_day_sonnet_pct` → `seven_day_sonnet`

## Additional Tests

The test module also includes comprehensive edge cases:

- **`test_negative_deltas_window_reset`** - Negative deltas when windows reset
- **`test_mixed_deltas_increase_and_decrease`** - Mixed positive/negative deltas
- **`test_delta_precision_small_changes`** - Precision with small changes (0.001%)
- **`test_apportion_delta_*`** - Tests for `apportion_delta()` function

## Test Results

```
running 16 tests
test governor::window_delta_tests::test_apportion_delta_basic ... ok
test governor::window_delta_tests::test_apportion_delta_equal_weights ... ok
test governor::window_delta_tests::test_apportion_delta_fractional_weights ... ok
test governor::window_delta_tests::test_apportion_delta_negative_total_delta ... ok
test governor::window_delta_tests::test_apportion_delta_zero_session_usd ... ok
test governor::window_delta_tests::test_apportion_delta_zero_total_usd ... ok
test governor::window_delta_tests::test_calculate_window_pct_delta_basic ... ok
test governor::window_delta_tests::test_calculate_window_pct_delta_negative_deltas ... ok
test governor::window_delta_tests::test_calculate_window_pct_delta_zero_previous ... ok
test governor::window_delta_tests::test_delta_precision_small_changes ... ok
test governor::window_delta_tests::test_consecutive_snapshots_non_zero_deltas ... ok
test governor::window_delta_tests::test_delta_uses_correct_window_fields ... ok
test governor::window_delta_tests::test_first_poll_no_previous_snapshot ... ok
test governor::window_delta_tests::test_identical_snapshots_zero_deltas ... ok
test governor::window_delta_tests::test_mixed_deltas_increase_and_decrease ... ok
test governor::window_delta_tests::test_negative_deltas_window_reset ... ok

test result: ok. 16 passed; 0 failed; 0 ignored
```

## Implementation Details

**Delta computation function** (lines 634-642):
```rust
pub fn calculate_window_pct_delta(
    previous_snapshot: &crate::db::WindowPctSnapshot,
    current_snapshot: &crate::db::WindowPctSnapshot,
) -> (f64, f64, f64) {
    let delta_5h = current_snapshot.five_hour - previous_snapshot.five_hour;
    let delta_7d = current_snapshot.seven_day - previous_snapshot.seven_day;
    let delta_7ds = current_snapshot.seven_day_sonnet - previous_snapshot.seven_day_sonnet;
    (delta_5h, delta_7d, delta_7ds)
}
```

**Structs involved:**
- `WindowPctSnapshot` (db.rs): API snapshot with `five_hour`, `seven_day`, `seven_day_sonnet`
- `PrevUsageSnapshot` (state.rs): Governor state snapshot with `five_hour_pct`, `seven_day_pct`, `seven_day_sonnet_pct`
- `WindowPctDeltas` (state.rs): Computed deltas stored in governor state

## Conclusion

All acceptance criteria from bead bf-18hwm are met:
- ✅ Unit test for consecutive snapshots with expected non-zero deltas
- ✅ Unit test for identical snapshots producing zero deltas
- ✅ Unit test for first poll handling (no previous snapshot)
- ✅ All tests pass with `cargo test`
