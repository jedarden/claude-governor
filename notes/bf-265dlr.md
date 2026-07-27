# Verification: weekly_scoped_pct usage in governor.rs

**Bead:** bf-265dlr
**Date:** 2026-07-27
**Status:** ✅ PASSED

## Verification Results

### 1. ✅ Correct source field used (Line 4130)
```rust
let new_weekly_scoped = state.usage.weekly_scoped_pct;
```
Confirmed: The EMA calculation reads from `state.usage.weekly_scoped_pct`, not the legacy `sonnet_pct` field.

### 2. ✅ Clear comment explaining model-agnostic behavior (Lines 4126-4129)
```rust
// NOTE: weekly_scoped_pct is the model-agnostic field for weekly_scoped utilization.
// The legacy sonnet_pct field is kept for backward compatibility but should not be used
// in new code. When model identity changes, reset logic above ensures stale samples
// are cleared.
```

### 3. ✅ No sonnet_pct references in weekly_scoped EMA path
- The weekly_scoped delta calculation (line 4147) returns `(delta_5h, delta_7d, delta_7ds)` where `delta_7ds` is computed from `weekly_scoped` fields only
- The weekly_scoped EMA update (lines 4201-4222) uses `delta_7ds` and updates `fleet_pct_hr_ema.weekly_scoped` and `usd_per_pct_ema_weekly_scoped`
- No sonnet_pct references in this calculation path

### 4. ✅ Data source is model-agnostic limits[]-derived
From `src/poller.rs`:
```rust
// Populate weekly_scoped from the model-agnostic limits[] array.
// This is the authoritative source for weekly_scoped utilization regardless
// of which model (Fable, Opus, Sonnet, etc.) is carrying the scoped cap.
if let Some((model_name, window)) = data.scoped_weekly() {
    data.weekly_scoped_utilization = window.utilization;
    // ...
}
```

From `src/governor.rs` (lines 3796, 3800, 3818):
```rust
state.usage = state::UsageState {
    weekly_scoped_pct: usage_data.weekly_scoped_utilization,
    // ...
}
```

## Conclusion
The EMA calculation in `src/governor.rs` correctly uses the model-agnostic `weekly_scoped_pct` field throughout. The legacy `sonnet_pct` field is only maintained for backward compatibility and is not used in any new weekly_scoped calculation paths.
