# bf-100ol: Add cold-start and weekly_scoped identity-change tests for the production path

## Summary

Parent coordinating bead for comprehensive production-path test coverage. All work completed in child beads (bf-jwfh2m, bf-3zuklh, bf-5zwunw, bf-5vye04).

## Acceptance Criteria - ALL MET ✅

### 1. COLD-START Test
**Requirement:** Window with 0 prior samples reports seeded (non-zero) base rate AND is flagged cold/uncertain

**Implementation:**
- `test_cold_start_uses_baseline_not_zero()` (tests/weekly_scoped_model_rotation_test.rs:242)
  - Verifies cold-start windows use conservative baseline seeding (not 0.0)
  - Asserts `EstimateQuality::ColdStart` flag is set
  - Checks widened uncertainty cone (cone_ratio > 1.0)

- `test_weekly_scoped_cold_start_quality_flag()` (tests/weekly_scoped_model_rotation_test.rs:278)
  - Tests production path `generate_window_forecast()` directly
  - Asserts cold-start windows are flagged, not treated as confident-empty
  - Verifies safe worker counts are computable from seeded rates

**Child bead:** bf-jwfh2m (CLOSED)

### 2. IDENTITY CHANGE Test
**Requirement:** Seed weekly_scoped=Fable samples until calibrated, then rotate model → slot resets (samples→0, signal→cold, rate→seeded base)

**Implementation:**
- `test_production_path_identity_change_cold_start_flow()` (tests/weekly_scoped_model_rotation_test.rs:468)
  - Seeds weekly_scoped=Fable samples until calibrated (≥3 samples)
  - Rotates model identity to "Opus" mid-simulation
  - Asserts samples reset to 0, signal→ColdStart, rate→seeded baseline
  - Verifies NOT stale Fable rate, NOT 0.0

- `test_full_cycle_model_rotation_resets_calibrated_slot()` (tests/weekly_scoped_model_rotation_test.rs:723)
  - Comprehensive end-to-end simulation
  - Tests "Children 1-3" behaviors: cold-start signaling, baseline seeding, no infinite headroom claims

**Child bead:** bf-3zuklh (CLOSED)

### 3. REGRESSION Test
**Requirement:** Continuously-calibrated window's forecast numerically unchanged by Children 1-3

**Implementation:**
- Regression tests added to `src/governor.rs` (commit b919123, +233 lines)
  - `test_continuously_calibrated_window_bypasses_cold_start_seeding()`
  - `test_continuously_calibrated_window_preserves_ema_values()`
  - Boundary condition test at exactly 3 samples (MIN_SAMPLES_FOR_EMA)

**Guards:** "Only the cold path changes" invariant - hot path stable

**Child bead:** bf-5zwunw (CLOSED)

### 4. FIRST-STARTUP Test
**Requirement:** Brand-new state (no persisted weekly_scoped_model, no samples) cold-starts flagged cold/uncertain

**Implementation:**
- `test_first_startup_cold_start_behavior()` (tests/weekly_scoped_model_rotation_test.rs:612)
  - Simulates cgov first-time startup (weekly_scoped_model=None, samples=0, ema=0.0)
  - Asserts EstimateQuality::ColdStart (not "empty" or "absent")
  - Verifies conservative baseline seeding
  - Produces safe worker count (not unbounded)

**Child bead:** bf-5vye04 (CLOSED)

## Test Coverage Summary

| Test | Location | Production Path | Guards Against |
|------|----------|-----------------|-----------------|
| test_cold_start_uses_baseline_not_zero | tests/weekly_scoped_model_rotation_test.rs:242 | ✅ generate_window_forecast | Reverting to 0.0 burn rate |
| test_weekly_scoped_cold_start_quality_flag | tests/weekly_scoped_model_rotation_test.rs:278 | ✅ generate_window_forecast | Missing cold-start signal |
| test_production_path_identity_change_cold_start_flow | tests/weekly_scoped_model_rotation_test.rs:468 | ✅ Full cycle simulation | Stale model data on rotation |
| test_full_cycle_model_rotation_resets_calibrated_slot | tests/weekly_scoped_model_rotation_test.rs:723 | ✅ Full cycle simulation | Identity change not detected |
| test_first_startup_cold_start_behavior | tests/weekly_scoped_model_rotation_test.rs:612 | ✅ generate_window_forecast | First startup treated as confident-empty |
| Regression tests (continuously-calibrated) | src/governor.rs (b919123) | ✅ Inline EMA + governor cycle | Hot path changes from cold-start fixes |

## Test Results

```
cargo test: 691 tests passed, 0 failed
```

All tests verify the PRODUCTION path (`governor.rs` inline EMA + `generate_window_forecast`), NOT the test-only `estimate_burn_rates`.

## Dependencies

All child beads closed:
- ✅ bf-3flif (Reset weekly_scoped slot samples when model identity changes)
- ✅ bf-jwfh2m (Cold-start production path test)
- ✅ bf-3zuklh (Weekly_scoped model identity-change test)
- ✅ bf-5zwunw (Regression test for continuously-calibrated windows)
- ✅ bf-5vye04 (First-startup test)

## Integration

These tests complete the parent bead bf-12wx4's acceptance criteria for the "simulate Fable → other model" requirement by exercising the full production path, not just the test helper utilities.

## Completion

All four behaviors asserted; identity-change and cold-start tests FAIL if Children 1-3 are reverted; full cargo test suite green; explicit verification of parent's "simulate Fable → other model" acceptance criterion.

**Status:** COMPLETE - All work done in child beads
