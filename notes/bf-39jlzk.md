# Verification: Rotated Model's Actual Pct Feeds the weekly_scoped EMA

## Summary
**VERIFIED**: When the weekly_scoped model rotates (e.g., from Sonnet to Opus), the EMA calculation correctly uses the NEW model's actual utilization percentage, not a stale value from the previous model.

## Data Flow Analysis

### 1. Data Source: Model-Agnostic `limits[]` Array
The weekly_scoped utilization percentage comes from the Anthropic API's model-agnostic `limits[]` array:

```rust
// poller.rs:263-281
pub fn scoped_weekly(&self) -> Option<(String, UsageWindow)> {
    self.limits.iter().find_map(|limit| {
        if limit.kind.as_deref() != Some("weekly_scoped") {
            return None;
        }
        let model_name = limit.scope.as_ref()
            .and_then(|s| s.model.as_ref())
            .and_then(|m| m.display_name.clone())
            .unwrap_or_else(|| "Scoped".to_string());
        let window = UsageWindow {
            utilization: limit.percent.unwrap_or(0.0),  // <-- ACTUAL PCT FROM ROTATED MODEL
            resets_at: limit.resets_at.clone().unwrap_or_default(),
        };
        Some((model_name, window))
    })
}
```

### 2. State Population
The polled value flows into `state.usage.weekly_scoped_pct`:

```rust
// governor.rs:3814-3815
state.usage = state::UsageState {
    weekly_scoped_pct: usage_data.weekly_scoped_utilization,  // <-- NEW MODEL'S PCT
    // ...
    weekly_scoped_model: usage_data.weekly_scoped_model.clone(),
};
```

### 3. EMA Calculation
The EMA reads the current (possibly rotated) model's pct:

```rust
// governor.rs:4153-4157
let new_weekly_scoped = state.usage.weekly_scoped_pct;  // <-- ROTATED MODEL'S PCT
```

### 4. Model Rotation Detection
When the model identity changes, the EMA is reset:

```rust
// governor.rs:3785-3792
let prev_model = state.usage.weekly_scoped_model.clone();
let new_model = usage_data.weekly_scoped_model.clone();
let model_changed = crate::state::reset_weekly_scoped_on_model_change(
    &prev_model,
    &new_model,
    &mut state.burn_rate,
);
```

```rust
// state.rs:126-162
pub fn reset_weekly_scoped_on_model_change(...) -> bool {
    match (prev_model.as_deref(), new_model.as_deref()) {
        (Some(old), Some(new)) if old != new => {
            log::info!("weekly_scoped model identity changed: '{}' -> '{}', resetting EMA samples", old, new);
            burn_rate_state.fleet_pct_hr_ema.weekly_scoped = 0.0;  // <-- RESET
            true
        }
        // ...
    }
}
```

## Changes Made

### 1. Added Debug Logging (governor.rs)
**At model change detection:**
```rust
log::info!(
    "[governor] weekly_scoped model change detection: prev_model={:?}, new_model={:?}, new_weekly_scoped_pct={:.2}%",
    prev_model, new_model, usage_data.weekly_scoped_utilization
);
```

**At EMA input preparation:**
```rust
log::info!(
    "[governor] EMA input: weekly_scoped_model={:?}, weekly_scoped_pct={:.2}% (this is the actual pct from the rotated model)",
    state.usage.weekly_scoped_model, new_weekly_scoped
);
```

**At weekly_scoped EMA update:**
```rust
log::info!(
    "[governor] updating weekly_scoped EMA: delta={:+.3}%, rate={:.4}%/hr, model={:?}, source_pct={:.2}%",
    delta_7ds, rate, state.usage.weekly_scoped_model, new_weekly_scoped
);
```

### 2. Added Verification Test (state.rs)
Created `verify_rotated_model_pct_feeds_ema` test that:
- Simulates rotation from Fable (72% utilization) to Opus (45% utilization)
- Verifies EMA resets on model change
- Confirms the new pct (45.0) is used, not the stale pct (72.0)
- **Test Result: PASSED ✓**

## Verification Steps Completed

1. ✅ **Traced data flow** from API → `limits[]` → `weekly_scoped_pct` → EMA
2. ✅ **Verified model-agnostic source** - the pct always comes from `limit.percent`, not a model-specific field
3. ✅ **Confirmed rotation detection** - `reset_weekly_scoped_on_model_change()` resets EMA on model change
4. ✅ **Added debug logging** - three key points log model identity and pct value
5. ✅ **Created test** - comprehensive test proves rotated model's pct feeds EMA
6. ✅ **All tests pass** - 692 tests passed including the new verification test

## Conclusion

The rotated model's actual pct **DOES feed the weekly_scoped EMA**. The architecture ensures this through:

1. **Model-agnostic data source**: `weekly_scoped_pct` is always populated from `limit.percent` (the current model's utilization)
2. **Model identity tracking**: `weekly_scoped_model` field tracks which model is active
3. **EMA reset on rotation**: When model changes, EMA resets to avoid using stale data
4. **Clear data flow**: The pct value flows directly from the API response into the EMA calculation

The debug logging added will now surface in production logs, making it visible which model's pct is being used for the EMA calculation, especially during model rotations.
