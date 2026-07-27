# Regression Test for Continuously-Calibrated Windows (bf-5zwunw)

## Summary

Added comprehensive regression tests to verify that continuously-calibrated windows are unaffected by the cold-start fixes from Children 1-3 (bf-3ebgd, bf-3zuklh, and related beads).

## Changes Made

### 1. Main Test: `continuously_calibrated_window_bypasses_cold_start_logic`

Located in `src/governor.rs` (around line 9185)

**What it tests:**
- A continuously-calibrated window with 12 EMA samples (well above the 3-sample threshold)
- Non-zero burn rate (2.5 %/hr from real measurements)
- EstimateQuality is Calibrated (not ColdStart or InsufficientSamples)
- Current utilization 65% with 2 workers

**Key assertions:**
1. Cold-start seeding logic does NOT trigger (wrong quality)
2. Original EMA values are preserved (not seeded with baseline)
3. Forecast uses calibrated EMA values (not seeded baseline)
4. Forecast is flagged as Calibrated
5. Forecast produces meaningful exhaustion prediction
6. Forecasts are numerically identical with and without cold-start logic

### 2. Boundary Test: `continuously_calibrated_window_at_threshold_bypasses_cold_start`

**What it tests:**
- A window at exactly the calibration threshold (3 samples = MIN_SAMPLES_FOR_EMA)
- Verifies the boundary condition is handled correctly
- Ensures windows at the threshold still bypass cold-start logic

**Key assertions:**
1. Window at threshold bypasses seeding
2. Forecasts are identical with and without cold-start logic
3. Forecast is correctly flagged as Calibrated

## Why This Matters

The cold-start fixes (Children 1-3) added seeding logic to prevent dangerous behavior when windows have no data. However, this should only affect the cold path (ColdStart/InsufficientSamples quality), not the hot path (Calibrated windows).

This regression test guards against accidental changes to hot-path behavior by verifying that:
1. Continuously-calibrated windows bypass the seeding logic entirely
2. The forecast is numerically unchanged by the cold-start fixes
3. The production path (governor.rs inline EMA + generate_window_forecast) is stable

## Test Results

All tests pass:
- `continuously_calibrated_window_bypasses_cold_start_logic` - ✓ PASS
- `continuously_calibrated_window_at_threshold_bypasses_cold_start` - ✓ PASS
- All existing cold-start tests continue to pass (10/10)

## Related Beads

- Parent: bf-100ol
- Depends on: bf-3zuklh (cold-start production path fixes)
- Related: bf-3ebgd (cold-start base-rate seeding)
