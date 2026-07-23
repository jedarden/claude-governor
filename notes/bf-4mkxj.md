# Task bf-4mkxj: Warm Site BaselineBurnRates Update

**Status:** Already Completed

## Summary

The task to update warm sites to use config-derived BaselineBurnRates has already been completed in commit `9649744 feat(warm): Update remaining warm sites to use config baselines`.

## Investigation

### Audits Performed
1. **bf-32uvk** - Audit BaselineBurnRates construction sites
   - Identified ONE warm site needing update: `src/governor.rs:841-866` (`get_sonnet_baseline_config()`)

2. **bf-8jv66** - Document BaselineBurnRates construction sites audit
   - Concluded: "All production sites are already correct"
   - Concluded: "No code changes needed"

### Current Implementation

The `get_sonnet_baseline_config()` function in `src/governor.rs:843-906` now:

1. **Warm-start path** (lines 849-870):
   ```rust
   for (name, baseline) in &state.baseline_burn_rates {
       if name.contains("sonnet") || name.contains("needle") {
           log::debug!("[governor] using state-loaded baseline for {} (pct={:.2}/hr, ${:.2}/hr)", ...);
           return baseline.clone();
       }
   }
   ```

2. **Cold-start path** (lines 875-898):
   ```rust
   for (name, cfg) in agents {
       if cfg.subscription && (name.contains("sonnet") || name.contains("needle")) {
           return convert_baseline(cfg.baseline_burn_rate_or_default());
       }
   }
   ```

3. **True fallback** (line 905):
   ```rust
   // No subscription agents at all - use default
   log::warn!("[governor] no subscription agents configured, using default baseline for dollar staleness checks");
   crate::state::BaselineBurnRates::default()
   ```

### Call Sites Updated

All four call sites now pass the `&state` parameter:
- Line 4064: `let baseline = get_sonnet_baseline_config(&state, agents);`
- Line 4386: `let baseline = get_sonnet_baseline_config(&state, agents);`
- Line 4575: `let baseline = get_sonnet_baseline_config(&state, agents);`
- Line 4780: `let baseline = get_sonnet_baseline_config(&state, agents);`

### Test Results

All 606 library tests pass successfully.

## Conclusion

✅ **Warm sites already use config-derived baselines**
✅ **Remaining BaselineBurnRates::default() calls are in cold paths only** (config loading, tests, true fallback)
✅ **No further action required**

## Related Commits

- `9649744` feat(warm): Update remaining warm sites to use config baselines
- `491efd6` docs(bf-32uvk): Audit BaselineBurnRates construction sites
- `c44c126` docs(bf-8jv66): Document BaselineBurnRates construction sites audit
