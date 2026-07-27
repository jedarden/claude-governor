# Bead bf-1v11sl: Add cold-start test for production path

## Status: COMPLETE - Test already exists

## Summary

The requested cold-start production path test already exists in the codebase at `src/governor.rs:6659` as the function `cold_start_production_path_seeds_and_signals_uncertainty()`.

This test was originally added in commit `d4b10ff test(bf-jwfh2m): Fix cold-start production path test` for a different bead, but it fully satisfies the requirements of bf-1v11sl.

## Acceptance Criteria Verification

All acceptance criteria are met:

### ✅ 1. Test exists and passes
```bash
$ cargo test cold_start_production_path_seeds_and_signals_uncertainty
test governor::tests::cold_start_production_path_seeds_and_signals_uncertainty ... ok
```

### ✅ 2. Test uses the production path
The test calls `generate_window_forecast()` (line 6713), which is the production forecasting function in governor.rs, NOT the test-only `estimate_burn_rates()` function.

### ✅ 3. Test asserts non-zero base rate for cold window
Lines 6689-6703 verify:
- `fleet_pct_hr_seeded > 0.0` - seeded fleet rate is non-zero
- `fleet_pct_hr_seeded == 1.5` - matches baseline (1.5% * 1 worker)
- `pct_per_worker_seeded > 0.0` - per-worker rate is non-zero

### ✅ 4. Test asserts cold/uncertain signal is present
Lines 6724-6730 verify:
```rust
assert_eq!(
    forecast.estimate_quality,
    EstimateQuality::ColdStart,
    "cold-start forecast must be flagged with EstimateQuality::ColdStart (Child-1 signal)"
);
```

### ✅ 5. Test would fail if Child-1 cold-start fix is reverted
The test explicitly validates the cold-start seeding logic and the `EstimateQuality::ColdStart` signal. If the Child-1 fix (cold-start signaling) were reverted, this test would fail because:
- The seeding logic produces non-zero rates from baseline
- The forecast preserves the `ColdStart` quality flag
- Without these, the assertions on lines 6689-6703 and 6724-6730 would fail

## Test Coverage

The test comprehensively validates:
- Cold-start seeding from baseline burn rate
- Non-zero base rates for cold windows
- Cold/uncertain signal presence via `EstimateQuality::ColdStart`
- Wide confidence cone (`cone_ratio > 1.0`) to signal uncertainty
- Finite, meaningful exhaustion estimates
- Proper `safe_worker_count` computation (conservative, can be 0 with tight margins)

## Regression Protection

This test serves as a critical regression guard. If cold-start logic is reverted to incorrectly treat "no data" as "definitely empty" (0.0 burn rate), this test would fail immediately, preventing dangerous over-scaling behavior.

## Conclusion

No code changes needed. The test is complete, passing, and provides full coverage of the required cold-start production path behavior.
