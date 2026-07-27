# Bug Report: weekly_scoped sonnet_pct Hard-coding

## Summary

Identified multiple instances where `sonnet_pct` is hard-coded to equal `weekly_scoped_utilization` without checking whether the weekly_scoped window is actually tracking a Sonnet model.

## Root Cause

The correct behavior (implemented in `src/governor.rs`) is:

```rust
sonnet_pct: if usage_data.is_weekly_scoped_sonnet() {
    usage_data.weekly_scoped_utilization
} else {
    0.0
}
```

The `is_weekly_scoped_sonnet()` method checks if `weekly_scoped_model` equals "Sonnet" (case-insensitive). This is necessary because the weekly_scoped window can track any model (Sonnet, Opus, Fable, etc.), and the legacy `sonnet_pct` field should only reflect utilization when it's actually Sonnet.

## Bugs Found

### 1. tests/governor_cycle_behavior_test.rs (6 instances)

The cycle behavior tests unconditionally assign:

```rust
state.usage.sonnet_pct = poll_result.weekly_scoped_utilization;
```

This is incorrect because:
- The default test data in `SimpleMockPoller::default_usage_data()` sets `weekly_scoped_model: None`
- When `weekly_scoped_model` is None (or any non-Sonnet model), `sonnet_pct` should be 0.0
- The hard-coded assignment doesn't respect the model-agnostic nature of the weekly_scoped window

**Locations:**
- Line 337: `test_emergency_brake_at_98_percent()`
- Line 380: `test_no_emergency_brake_below_98_percent()`
- Line 417: `test_state_updated_after_cycle()`
- Line 570: `test_stale_data_handling()`
- Line 608: `test_complete_governor_cycle()`
- Line 687: `test_emergency_brake_exact_threshold()`

### 2. tests/fixtures.rs (2 instances - potentially OK)

Helper functions `create_state_file_with_utilization()` and `create_full_state_file()` both set:

```rust
state.usage.sonnet_pct = weekly_scoped_pct;
```

These may be acceptable since:
- The parameter is documented as "7-day Sonnet window utilization percentage"
- These are fixture helpers where the caller explicitly controls the semantic meaning
- However, they should still validate that the test setup is consistent

## Impact

- Tests may pass with incorrect assumptions about the relationship between `weekly_scoped_utilization` and `sonnet_pct`
- If the weekly_scoped window is tracking Opus or Fable, `sonnet_pct` would incorrectly show that model's utilization
- This violates the model-agnostic design principle where `weekly_scoped_pct` (the new field) should be used instead of the legacy `sonnet_pct`

## Recommended Fix

For the cycle behavior tests, either:
1. Set `weekly_scoped_model: Some("Sonnet".to_string())` in the test data to make the assignment semantically correct
2. Use `weekly_scoped_pct` instead of `sonnet_pct` in test assertions (the new model-agnostic field)
3. Add a helper method that implements the correct conditional logic:
   ```rust
   fn correct_sonnet_pct(usage_data: &UsageData) -> f64 {
       if usage_data.is_weekly_scoped_sonnet() {
           usage_data.weekly_scoped_utilization
       } else {
           0.0
       }
   }
   ```

## Related Code

- `src/poller.rs`: `is_weekly_scoped_sonnet()` method (line 269)
- `src/governor.rs`: Correct conditional assignment (around line 3817)
- `src/state.rs`: `sonnet_pct` field documentation (deprecated, use `weekly_scoped_pct` instead)
