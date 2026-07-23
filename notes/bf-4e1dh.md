# Bead bf-4e1dh: Implementation Already Complete

## Finding

The bead description claimed that the three-tier fallback to `baseline_burn_rate` for token collector offline scenarios was **not implemented**, citing:
- "grep -rn baseline_burn_rate src/ config/governor.yaml returns zero matches"
- "grep -rn collector_data_age src/ returns zero matches"

**This evidence is incorrect.** The implementation is **complete and working**.

## Verification

```bash
# All implementations exist
grep -rn "baseline_burn_rate" src/
# Returns 45+ matches across config.rs, state.rs, governor.rs, etc.

grep -rn "collector_data_age" src/
# Returns matches at burn_rate.rs:858, 871, 3828, 3835

grep -rn "staleness_checked" src/
# Returns matches at governor.rs:4065, 4387, 4781 and burn_rate.rs:1564

# All tests pass
cargo test --release staleness        # 15/15 pass
cargo test --release baseline_burn_rate  # 7/7 pass
cargo test --release --lib            # 623/623 pass
```

## Implementation Status

### 1. Config Field (✅ Complete)
- `src/config.rs:80`: `pub baseline_burn_rate: Option<BaselineBurnRateConfig>`
- `src/config.rs:84-93`: `BaselineBurnRateConfig` struct with `pct_per_worker_per_hour` and `dollars_per_worker_per_hour`
- Defaults: 1.5 pct/hr, $5.0/hr

### 2. Governor Integration (✅ Complete)
- `src/governor.rs:840-900`: `get_sonnet_baseline_config()` loads from `state.baseline_burn_rates`
- `src/governor.rs:4065, 4387, 4781`: Calls `staleness_checked_fleet_dollar_rate()` with baseline
- `src/state.rs:748-766`: `load_baseline_burn_rates_from_config()` populates state from agent configs

### 3. Three-Tier Staleness Check (✅ Complete)
- `src/burn_rate.rs:858`: `collector_data_age()` computes age in seconds
- `src/burn_rate.rs:870-884`: `staleness_tier()` returns `Fresh` / `Aging` / `Stale`
- `src/burn_rate.rs:898-922`: `check_staleness()` logs appropriate warnings
- `src/burn_rate.rs:1564-1606`: `staleness_checked_fleet_dollar_rate()` enforces three-tier behavior

### 4. Test Coverage (✅ Complete)
- 15 staleness tests cover all tiers and boundary conditions
- 7 baseline_burn_rate tests cover config loading and state management
- `staleness_recovery_restores_ema_within_one_interval` test covers recovery behavior

### 5. Runtime Behavior (✅ Complete)
The three-tier fallback is **actively enforced** at runtime:
- Lines 4065, 4387, 4781 in governor.rs call `staleness_checked_fleet_dollar_rate()`
- Dollar-denominated fields (`sonnet_p75_usd_hr`, cache efficiency stats) use baseline when stale
- The fallback does NOT affect percentage-based scaling (uses direct poller snapshots)

## Conclusion

The previous bead (docs-y5p) that claimed this feature was implemented was **correct**. The current bead (bf-4e1dh) was based on **outdated or incorrect grep evidence**.

**No implementation work was needed.** The feature has been complete and working since docs-y5p was closed.

## Verification Date

2026-07-23 - All 623 lib tests pass, staleness enforcement confirmed in code paths.
