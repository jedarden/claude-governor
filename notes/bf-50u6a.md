# Task bf-50u6a: Wire BaselineBurnRates Construction to Use Config

## Status: COMPLETE

This task was already complete. All `BaselineBurnRates` construction sites already use config-derived defaults.

## Implementation Verification

### 1. Default Implementations Source from Config

Both `BaselineBurnRates` structs (in `state.rs` and `burn_rate.rs`) have `Default` implementations that source from config:

```rust
// src/state.rs:519-526
impl Default for BaselineBurnRates {
    fn default() -> Self {
        Self {
            pct_per_worker_per_hour: crate::config::default_baseline_pct(),    // 1.5
            dollars_per_worker_per_hour: crate::config::default_baseline_dollars(), // 5.0
        }
    }
}
```

### 2. Production Code Paths Use Config

All production construction sites use config values:

| File | Line | Method | Config Source |
|------|------|--------|---------------|
| state.rs | 754 | `load_baseline_burn_rates_from_config()` | Direct from `AgentConfig` |
| config.rs | 110 | `to_baseline_burn_rates()` | Direct from `BaselineBurnRateConfig` |
| config.rs | 155 | `baseline_burn_rate_or_default()` | Config → `Default()` (config-based) |
| governor.rs | 877 | `get_sonnet_baseline_config()` helper | Uses `baseline_burn_rate_or_default()` |
| governor.rs | 905 | Ultimate fallback | `Default()` (config-based) |

### 3. Warm Sites Use EMA, Not Baseline

The `effective_burn_rate()` function (burn_rate.rs:980) correctly uses EMA values when ≥3 samples are available:

```rust
fn effective_burn_rate(ema: &ModelWindowEma, baseline: &BaselineBurnRates) -> (f64, f64) {
    if ema.samples >= MIN_SAMPLES_FOR_EMA {  // 3
        (ema.ema_pct, ema.ema_usd)
    } else {
        (
            baseline.pct_per_worker_per_hour,
            baseline.dollars_per_worker_per_hour,
        )
    }
}
```

### 4. Cold-Start Uses Config-Derived Defaults

When no config is available, `BaselineBurnRates::default()` provides config-derived fallback values (1.5%/$5.0 per hour).

## Test Results

All 606 tests pass, confirming that:
- Config loading works correctly
- Default values are sourced from config functions
- Warm sites use EMA, not baseline
- Cold-start fallback uses config-derived defaults

## Conclusion

No changes were required. The codebase already has complete config wiring for `BaselineBurnRates` construction.
