# Bead bf-1ii9m: Weekly Scoped Model Identity Change Detection

## Status: ✅ IMPLEMENTED

This bead implements detection of weekly_scoped model rotation by comparing persisted `weekly_scoped_model` against the newly-resolved model name from UsageData.

## Implementation Details

### Detection Function
**File:** `src/state.rs:106-150`
- Function: `reset_weekly_scoped_on_model_change(prev_model, new_model, burn_rate_state)`
- Compares persisted model with current API-resolved model
- Returns `bool` indicating whether a change was detected
- Handles all transition cases:
  - Model rotation: `Some("Fable") → Some("Opus")`
  - Model cleared: `Some("Fable") → None`
  - Model initialized: `None → Some("Fable")`
  - No change: Same values or both `None`

### Poll Reconciliation Integration
**File:** `src/governor.rs:3766-3784`
- Extracts `prev_model` from persisted `state.usage.weekly_scoped_model`
- Extracts `new_model` from current `usage_data.weekly_scoped_model` (resolved via `UsageData::scoped_weekly()`)
- Calls detection function and captures `model_changed` boolean
- Signals rotation via:
  - INFO-level logs for all transitions
  - EMA sample reset to cold/zero
  - Previous snapshot clearing (prevents delta computation against old model's utilization)

### Signal Paths
When model rotation is detected:
1. **Logging**: INFO message with old and new model names
2. **EMA Reset**: `burn_rate_state.fleet_pct_hr_ema.weekly_scoped = 0.0`
3. **Snapshot Reset**: `previous_api_snapshot.weekly_scoped_pct = 0.0`

### Additional Changes
- Added `weekly_scoped_pct` field to `UsageState` (model-agnostic utilization percentage)
- Updated test fixtures in `src/simulator.rs` and `src/status_display.rs`
- Updated logging to use `weekly_scoped_display_label()` for dynamic model naming

## Verification
- ✅ Comparison logic runs during each poll cycle
- ✅ Detects when `weekly_scoped_model` differs from resolved model
- ✅ Triggers appropriate handling (EMA reset, snapshot clearing, logging)
- ✅ All 736 cargo tests pass (2 ignored, 0 failed)
