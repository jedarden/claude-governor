# bf-31ktu: Add tests and verify three-tier fallback implementation

## Summary

Added comprehensive unit tests for the three-tier staleness fallback implementation in `src/burn_rate.rs` and verified all acceptance criteria are met.

## Tests Added

1. **`staleness_checked_fleet_dollar_rate_45_minutes_uses_baseline`**
   - Tests the specific 45-minute case from docs-y5p
   - Verifies very stale data (≥45 min) falls back to configured baseline
   - Confirms baseline is used instead of aggregate value

2. **`staleness_recovery_restores_ema_within_one_interval`**
   - Tests recovery after a stale period
   - Verifies three-phase scenario: fresh → stale → fresh
   - Confirms EMA-based rates restore immediately when data becomes fresh again
   - Ensures the system doesn't get stuck on baseline after recovery

3. **`staleness_tier_exactly_15_minutes_is_aging`**
   - Tests the specific 15-minute case from docs-y5p
   - Verifies staleness tier classification at exactly 15 minutes

## Test Results

- **Total tests**: 107 passed, 0 failed
- **Staleness-specific tests**: 15 passed
- All tests pass with `cargo test --release`

## Build Verification

- `cargo build --release` succeeded
- Binary size: 5.3M (target/release/cgov)

## Manual Verification

1. ✓ `grep -rn baseline_burn_rate src/` - Found matches in:
   - `src/config.rs`: Configuration struct and helper functions
   - Multiple test cases for baseline configuration

2. ✓ `grep -rn collector_data_age src/` - Found matches in:
   - `src/burn_rate.rs`: Function definition at line 858
   - Used in `staleness_tier()` at line 871
   - Multiple test cases

## Three-Tier Behavior Verified

The implementation correctly implements the three-tier staleness fallback:

1. **Fresh data (< 10 minutes)**: Use normally
   - Tests: `staleness_tier_fresh_under_10_minutes`, `staleness_checked_fleet_dollar_rate_fresh_uses_aggregate`

2. **Aging data (10-30 minutes)**: Use last EMA values with WARN log
   - Tests: `staleness_tier_aging_10_to_30_minutes`, `staleness_checked_fleet_dollar_rate_aging_uses_aggregate`, `staleness_tier_exactly_15_minutes_is_aging`

3. **Stale data (≥ 30 minutes)**: Fall back to configured baseline
   - Tests: `staleness_tier_stale_over_30_minutes`, `staleness_checked_fleet_dollar_rate_stale_uses_baseline`, `staleness_checked_fleet_dollar_rate_45_minutes_uses_baseline`

4. **Recovery**: Restores EMA-based rates within one interval
   - Test: `staleness_recovery_restores_ema_within_one_interval`

## Integration with Previous Beads

This bead validates that all three previous beads work correctly together:
- bf-31r8h: Baseline configuration wiring
- bf-29ql4: Baseline burn rates integration
- bf-5a4nz: Staleness detection and three-tier fallback

The implementation matches the documented three-tier behavior from the plan (docs-y5p).
