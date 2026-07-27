# bf-5vye04: Add first-startup test for brand-new state

## Task
Add first-startup test for brand-new state to verify the cold-start contract is properly enforced.

## Work Done

### Added Production Path Test
Added `first_startup_cold_start_production_path` test to `src/governor.rs` that validates:

1. **Cold-start detection**: First-startup (no persisted model, no samples) is flagged as `EstimateQuality::ColdStart`, NOT treated as "empty" or "absent"

2. **Seeded baseline rate**: The forecast uses a seeded (non-zero) baseline rate (4.5%/hr = 1.5 * 3 workers), NOT 0.0% which would give infinite headroom

3. **Uncertainty signaling**: The forecast signals uncertainty via:
   - Wide uncertainty cone (cone_ratio > 1.0)
   - Widened std_pct_hr_seeded = fleet_pct_hr_seeded
   - P75 safe worker count <= P50 (more conservative)

4. **Safe worker counts are computable**: Both safe_worker_count and safe_worker_count_p75 are Some (enforcing bounds, not unbounded scaling)

5. **Finite exhaustion**: Predicted exhaustion is finite (8.89h) from seeded rate, NOT infinite (which 0.0 rate would give)

6. **First-startup vs identity change parity**: Both paths produce ColdStart signal (None->Some and Some->Some model changes)

### Test Coverage
This test complements the existing:
- `cold_start_production_path_seeds_and_signals_uncertainty` - General cold-start production path test
- `test_first_startup_cold_start_behavior` - Integration test in weekly_scoped_model_rotation_test.rs

The new test specifically validates the **first-startup scenario** (None->Some model initialization) using the production path (inline EMA + generate_window_forecast).

### Acceptance Criteria Met
- ✅ Test creates a brand-new state with no prior history
- ✅ Asserts the forecast reports a seeded base rate (non-zero)
- ✅ Asserts the window is flagged as cold/uncertain
- ✅ Asserts it is NOT treated as confident-empty (cone_ratio > 1.0 signals uncertainty)
- ✅ Verifies the first-startup behavior matches the cold-start contract

## Files Modified
- `src/governor.rs`: Added `first_startup_cold_start_production_path` test

## Test Results
All 62 governor tests pass, including the new first-startup test.
