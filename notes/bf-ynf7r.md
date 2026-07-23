# Verification: calculate_window_pct_delta call location in Some-Some block

## Task
Verify that the `calculate_window_pct_delta` call is inside the Some-Some block.

## Findings

### Primary Some-Some block (lines 2990-3025)

**Location:** `src/governor.rs:2990-3015`

```rust
if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot) {
    // Both snapshots available: proceed with delta computation
    let prev_pct = crate::db::WindowPctSnapshot {
        five_hour: prev.five_hour_pct,
        seven_day: prev.seven_day_pct,
        seven_day_sonnet: prev.seven_day_sonnet_pct,
    };
    let curr_pct = crate::db::WindowPctSnapshot {
        five_hour: curr.five_hour_pct,
        seven_day: curr.seven_day_pct,
        seven_day_sonnet: curr.seven_day_sonnet_pct,
    };
    let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);  // Line 3002
    ...
}
```

**Status:** ✅ **CONFIRMED** - The `calculate_window_pct_delta` call at line 3002 is **inside** the Some-Some pattern guard.

### Other production code calls

**Note:** There is another `calculate_window_pct_delta` call in production code at line 3321, but it is inside a **single Some** pattern guard, not a Some-Some pattern:

```rust
if let Some(snap) = old_snapshot.clone() {  // Line 3304 - single Some pattern
    ...
    let (delta_5h, delta_7d, delta_7ds) =
        calculate_window_pct_delta(&old_pct, &new_pct);  // Line 3320-3321
    ...
}
```

## Acceptance Criteria

- ✅ `calculate_window_pct_delta` call is confirmed inside the Some-Some block (line 3002)
- ✅ No `calculate_window_pct_delta` call exists outside the Some-Some pattern guard in this context
- ✅ Location documented with line numbers (line 3002 inside block at lines 2990-3015)

## Additional Context

The Some-Some pattern guard ensures that delta computation only proceeds when **both** `previous_api_snapshot` AND `current_api_snapshot` are `Some`. This is the correct behavior for computing window deltas from consecutive API snapshots.

When the pattern doesn't match (i.e., one or both snapshots are `None`), the code enters the `else` branch (lines 3016-3025) and initializes deltas to `0.0` for the first poll.
