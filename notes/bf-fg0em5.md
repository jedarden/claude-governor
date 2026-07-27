# Verification of weekly_scoped_pct Usage (bf-fg0em5)

## Task
Search `src/governor.rs` for any remaining uses of `sonnet_pct` in the weekly_scoped calculation path that should instead use the model-agnostic `weekly_scoped_pct`.

## Findings

### 1. All weekly_scoped calculations use `weekly_scoped_pct` (✓)

**Line 4130** - Explicit comment and usage:
```rust
// NOTE: weekly_scoped_pct is the model-agnostic field for weekly_scoped utilization.
// The legacy sonnet_pct field is kept for backward compatibility but should not be used
// in new code. When model identity changes, reset logic above ensures stale samples
// are cleared.
let new_weekly_scoped = state.usage.weekly_scoped_pct;
```

**Lines 4137-4148** - Snapshot construction uses `weekly_scoped_pct`:
```rust
let old_pct = crate::db::WindowPctSnapshot {
    five_hour: snap.five_hour_pct,
    seven_day: snap.seven_day_pct,
    weekly_scoped: snap.weekly_scoped_pct,  // ← weekly_scoped_pct
};
let new_pct = crate::db::WindowPctSnapshot {
    five_hour: new_five_hour,
    seven_day: new_seven_day,
    weekly_scoped: new_weekly_scoped,  // ← derived from weekly_scoped_pct
};
let (delta_5h, delta_7d, delta_7ds) =
    calculate_window_pct_delta(&old_pct, &new_pct);
```

**Lines 4201-4222** - EMA calculation uses `delta_7ds` (derived from weekly_scoped):
```rust
if delta_7ds > 0.0 {
    let rate = delta_7ds / elapsed_hours_snap;
    // Updates state.burn_rate.fleet_pct_hr_ema.weekly_scoped
    // Updates state.burn_rate.usd_per_pct_ema_weekly_scoped
}
```

### 2. Legacy `sonnet_pct` is only for backward compatibility (✓)

**Lines 3796-3803** - State update sets both fields correctly:
```rust
state.usage = state::UsageState {
    weekly_scoped_pct: usage_data.weekly_scoped_utilization,  // ← model-agnostic
    // Only set sonnet_pct when weekly_scoped is actually tracking Sonnet;
    // otherwise set to 0.0 since the legacy field should not reflect other models
    sonnet_pct: if usage_data.is_weekly_scoped_sonnet() {
        usage_data.weekly_scoped_utilization
    } else {
        0.0
    },
    // ... other fields
};
```

### 3. Remaining `sonnet_pct` references are legitimate (✓)

All remaining `sonnet_pct` references fall into two categories:

1. **Line 3799** - Setting the legacy field in state (backward compatibility, model-specific)
2. **Lines 8942+** - All in test code (`sonnet_pct_tests` module) that verifies:
   - `sonnet_pct` equals `weekly_scoped_utilization` when model is Sonnet
   - `sonnet_pct` is 0.0 when model is Opus/Fable/None
   - Rotation from Sonnet to Opus clears `sonnet_pct` to 0.0
   - Case-insensitive model detection works correctly

### 4. No `sonnet_pct` usage in weekly_scoped delta/EMA calculations (✓)

Verified by searching for all usage patterns:
- `calculate_window_pct_delta` uses `current_snapshot.weekly_scoped - previous_snapshot.weekly_scoped`
- These snapshots are constructed from `weekly_scoped_pct`, not `sonnet_pct`
- EMA updates target `fleet_pct_hr_ema.weekly_scoped` and `usd_per_pct_ema_weekly_scoped`
- No code path uses `sonnet_pct` for rate calculations or delta computations

## Conclusion

✅ **No legacy `sonnet_pct` references remain in the weekly_scoped calculation path.**

The code correctly:
1. Uses `weekly_scoped_pct` (model-agnostic) for all weekly_scoped rate calculations
2. Sets `sonnet_pct` only for backward compatibility (when model is Sonnet, else 0.0)
3. Contains comprehensive test coverage for model-specific `sonnet_pct` behavior
4. Has explicit comments documenting the legacy status of `sonnet_pct`

All weekly_scoped utilization tracking, delta calculation, and EMA updates use the model-agnostic `weekly_scoped_pct` field, ensuring correctness when the rotated model is not Sonnet.
