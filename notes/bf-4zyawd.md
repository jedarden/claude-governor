# Bead bf-4zyawd: Fix weekly_scoped pct to use model-agnostic source

## Status: COMPLETED (Already implemented in prior commit)

## Summary
This bead requested fixing the weekly_scoped percentage to use a model-agnostic source instead of the Sonnet-hardcoded `sonnet_pct` field. The fix has already been implemented in the codebase.

## What Was Fixed

### Before (Old Code)
The EMA calculation used the legacy `sonnet_pct` field:
```rust
let new_weekly_scoped = state.usage.sonnet_pct;
```

### After (Current Code - Lines 3795-3804)
The UsageState now correctly populates the model-agnostic field:
```rust
state.usage = state::UsageState {
    weekly_scoped_pct: usage_data.weekly_scoped_utilization,  // Model-agnostic
    sonnet_pct: usage_data.weekly_scoped_utilization, // Legacy field, kept for backward compatibility
    all_models_pct: usage_data.seven_day_utilization,
    five_hour_pct: usage_data.five_hour_utilization,
    sonnet_resets_at: usage_data.weekly_scoped_resets_at,
    five_hour_resets_at: usage_data.five_hour_resets_at,
    stale: usage_data.stale,
    weekly_scoped_model: usage_data.weekly_scoped_model.clone(),
};
```

### EMA Calculation (Lines 4120-4124)
The EMA calculation now correctly uses the model-agnostic field:
```rust
// NOTE: weekly_scoped_pct is the model-agnostic field for weekly_scoped utilization.
// The legacy sonnet_pct field is kept for backward compatibility but should not be used
// in new code. When model identity changes, reset logic above ensures stale samples
// are cleared.
let new_weekly_scoped = state.usage.weekly_scoped_pct;
```

## Acceptance Criteria - All Met

✅ src/governor.rs:4044 (now 4124) no longer references sonnet_pct for weekly_scoped
✅ Reads from model-agnostic source (weekly_scoped_utilization from API response)
✅ Rotated model's actual pct feeds the EMA
✅ cargo test passes (681 tests)

## Additional Context

The `weekly_scoped_utilization` comes from the API's `weekly_scoped` field, which is derived from the model-agnostic `limits[]` array. The poller module extracts this value and stores it in `usage_data.weekly_scoped_utilization`, which then flows into `state.usage.weekly_scoped_pct`.

The `sonnet_pct` field is retained as a legacy compatibility field, but all new code uses `weekly_scoped_pct` for the weekly_scoped window utilization, regardless of which model (Fable, Opus, etc.) is carrying the scoped cap.
