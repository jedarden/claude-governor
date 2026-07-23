# bf-29ql4: Verify baseline wiring through staleness_checked_fleet_dollar_rate

## Task
Wire baseline through `staleness_checked_fleet_dollar_rate` to ensure it receives config-derived values.

## Investigation

### Call sites found
All three call sites in `src/governor.rs`:

1. **Line 4065** - In `update_fleet_pct_ema`:
   ```rust
   let baseline = get_sonnet_baseline_config(&state, agents);
   let usd_per_worker = crate::burn_rate::staleness_checked_fleet_dollar_rate(
       &state.last_fleet_aggregate,
       &baseline,
   );
   ```

2. **Line 4387** - In `calculate_fleet_pct_per_hour`:
   ```rust
   let baseline = get_sonnet_baseline_config(&state, agents);
   let usd_per_worker = crate::burn_rate::staleness_checked_fleet_dollar_rate(
       &state.last_fleet_aggregate,
       &baseline,
   );
   ```

3. **Line 4781** - In `log_capacity_forecast`:
   ```rust
   let baseline = get_sonnet_baseline_config(&state, agents);
   let usd_per_worker = crate::burn_rate::staleness_checked_fleet_dollar_rate(
       &state.last_fleet_aggregate,
       &baseline,
   );
   ```

### Config-derived path verification

`get_sonnet_baseline_config()` (governor.rs:843) returns config-derived baselines via:

1. **Warm path**: State-stored baselines from `state.baseline_burn_rates` (loaded from config)
2. **Cold path**: Agent config lookup via `baseline_burn_rate_or_default()` which reads `self.baseline_burn_rate` from the agent config

`baseline_burn_rate_or_default()` (config.rs:152) implementation:
```rust
pub fn baseline_burn_rate_or_default(&self) -> crate::burn_rate::BaselineBurnRates {
    match &self.baseline_burn_rate {
        Some(config) => config.to_baseline_burn_rates(),
        None => crate::burn_rate::BaselineBurnRates::default(),
    }
}
```

## Conclusion
✅ **All acceptance criteria met**:
1. All call sites to `staleness_checked_fleet_dollar_rate` identified
2. All call sites pass config-derived `BaselineBurnRates` via `get_sonnet_baseline_config()`
3. Function signature already takes `&BaselineBurnRates` and is correctly wired

The wiring was already completed in prior commits:
- `c0a5775 feat(cold): Use config-derived defaults for BaselineBurnRates`
- `9649744 feat(warm): Update remaining warm sites to use config baselines`
- `ef51e55 feat(warm): Refactor get_sonnet_baseline_config to use state-loaded baselines`

No code changes required - the task was to verify the existing wiring is correct.
