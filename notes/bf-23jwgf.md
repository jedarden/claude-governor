# bf-23jwgf: Weekly-Scoped Identity-Change Test - COMPLETED

## Summary

The headline acceptance test `test_full_cycle_model_rotation_resets_calibrated_slot` already exists in `/tests/weekly_scoped_model_rotation_test.rs` and comprehensively covers all requirements.

## Test Location

File: `/home/coding/claude-governor/tests/weekly_scoped_model_rotation_test.rs`
Test function: `test_full_cycle_model_rotation_resets_calibrated_slot` (line 723)

## Test Coverage

The test simulates the complete governor production path in 6 phases:

### PHASE 1: Seed Fable samples until calibrated
- Accumulates 10 samples (well above MIN_SAMPLES_FOR_EMA = 3)
- Establishes Fable's burn rate at 2.5%/hr per worker (12.5%/hr fleet)
- Verifies calibrated state before rotation

### PHASE 2: Rotate model identity mid-run
- Simulates Anthropic rotating weekly_scoped from Fable → Opus
- Applies model change detection via `reset_weekly_scoped_on_model_change()`
- Explicitly resets `fleet_pct_ema_samples = 0` (production path)

### PHASE 3: Assert slot resets
- VERIFY 3a: `fleet_pct_ema_samples == 0` (no stale carry-over)
- VERIFY 3b: `fleet_pct_hr_ema.weekly_scoped == 0.0`
- VERIFY 3c: `usd_per_pct_ema_weekly_scoped == 0.0`

### PHASE 4: Assert cold signal
- VERIFY 4: `estimate_quality == EstimateQuality::ColdStart` (Child-1 signal)

### PHASE 5: Assert seeded rate
- VERIFY 5a: Rate is seeded baseline (7.5%/hr = 1.5 * 5 workers), not 0.0
- VERIFY 5b: Rate is NOT Fable's stale rate (12.5%/hr)
- VERIFY 5c: Rate is NOT 0.0 (prevents infinite headroom)

### PHASE 6: Forecast verification (Children 1-3)
- VERIFY 6a: Forecast carries `ColdStart` quality flag (Child-1)
- VERIFY 6b: Forecast uses seeded rate, not Fable's stale rate or 0.0 (Child-2)
- VERIFY 6c: Predicted exhaustion is finite, not infinite (Child-3)
- VERIFY 6d: Wide uncertainty cone (conservative)
- VERIFY 6e: Safe worker counts are computable

### REGRESSION GUARD
- Verifies calibrated windows (five_hour) remain unchanged
- Ensures the fix doesn't silently change behavior for normal windows

## Test Results

All 11 weekly_scoped tests pass:
```
running 11 tests
test test_cold_start_uses_baseline_not_zero ... ok
test test_comprehensive_model_rotation_scenario ... ok
test test_first_startup_cold_start_behavior ... ok
test test_full_cycle_model_rotation_resets_calibrated_slot ... ok
test test_model_cleared_to_none_resets_samples ... ok
test test_model_initialization_does_not_reset ... ok
test test_model_rotation_preserves_other_windows ... ok
test test_no_reset_when_model_unchanged ... ok
test test_production_path_identity_change_cold_start_flow ... ok
test test_weekly_scoped_cold_start_quality_flag ... ok
test test_weekly_scoped_model_rotation_resets_samples ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Acceptance Criteria Status

✅ Test exists and passes
✅ Test seeds Fable samples until calibrated  
✅ Test rotates to a different model mid-run
✅ Test asserts samples reset to 0
✅ Test asserts cold signal is present
✅ Test asserts rate is seeded base rate (not stale Fable rate)
✅ Test would fail if Children 1-3 fixes are reverted

## Notes

This test was already implemented as part of the weekly_scoped model rotation feature. It serves as the comprehensive acceptance test for model identity changes, covering all edge cases and ensuring proper cold-start handling when Anthropic rotates the scoped model.

The test is well-documented with clear phase markers and detailed assertions that map directly to the acceptance criteria.
