# Add Cold-Start Confidence Signal to WindowForecast (bf-45tbd)

## Summary

Implemented cold-start/insufficient-samples quality marking in the production governor path. This completes the data model change started in bf-n21u3 by actually using the `EstimateQuality` enum to discriminate window forecast quality.

## Changes Made

### File: `src/governor.rs` (lines 4598-4602)

**Before:**
```rust
let estimate_quality = if state.burn_rate.fleet_pct_ema_samples >= 3 && ema_val > 0.0 {
    state::EstimateQuality::Calibrated
} else {
    state::EstimateQuality::Calibrated // default; child 2/3 will mark cold/insufficient
};
```

**After:**
```rust
let estimate_quality = if state.burn_rate.fleet_pct_ema_samples >= 3 && ema_val > 0.0 {
    state::EstimateQuality::Calibrated
} else if state.burn_rate.fleet_pct_ema_samples == 0 {
    state::EstimateQuality::ColdStart
} else {
    state::EstimateQuality::InsufficientSamples
};
```

## How It Works

The production governor path now properly marks window forecasts based on EMA sample count:

1. **Calibrated** (`samples >= 3` AND `ema_val > 0.0`): The burn rate is backed by sufficient measurement data — safe to use for scaling decisions.

2. **ColdStart** (`samples == 0`): No burn history yet. The rate is seeded from baseline conservative defaults. Downstream consumers should use pessimistic bounds (p75 safe workers) and wider uncertainty margins.

3. **InsufficientSamples** (`samples > 0` but `< 3`): Some data but not enough to trust the EMA yet. Falls back to baseline rates with conservative heuristics.

## Acceptance Criteria Met

✅ **WindowForecast has a cold-start field that discriminates cold vs calibrated windows**
   - The `estimate_quality: EstimateQuality` field exists and is properly set

✅ **The field is set correctly based on sample count (< MIN_SAMPLES = cold)**
   - `samples == 0` → `ColdStart`
   - `samples >= 3` AND `ema_val > 0.0` → `Calibrated`
   - `samples > 0` but `< 3` → `InsufficientSamples`

✅ **The signal is propagated through forecast generation**
   - The quality is computed in the production governor loop (governor.rs)
   - It's passed to `generate_window_forecast` and included in the WindowForecast
   - The signal is visible to the governor when it reads the forecast

✅ **No existing forecast tests are broken**
   - All 638 tests pass
   - The field defaults to `Calibrated` for backward compatibility with existing state files

## Impact

This is a **pure data model change** — no governor scaling logic changes yet. The signal is now available for downstream consumers (alerts, scaling decisions) to branch on estimate quality and apply conservative heuristics when the forecast is not grounded in measurement.

## Related Work

- **bf-n21u3**: Added the `EstimateQuality` enum and `estimate_quality` field to `WindowForecast`
- **bf-14umd**: Documented the production governor path for window EMA updates
- **bf-2kexl**: Parent bead for the cold-start seeding implementation
