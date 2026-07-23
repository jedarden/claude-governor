# BaselineBurnRates Construction Sites Audit

## Task
Identify and categorize all BaselineBurnRates construction sites in the codebase.

## Definitions
- **Warm site**: Location with ≥3 EMA samples (should use config-derived values)
- **Cold site**: Location with <3 EMA samples (cold-start, should keep BaselineBurnRates::default() but ensure it uses config as base)
- **MIN_SAMPLES_FOR_EMA**: 3 (defined in src/burn_rate.rs:149)

---

## Production Code Sites

### 1. `src/config.rs:109-114` - `BaselineBurnRateConfig::to_baseline_burn_rates()`
**Pattern**: Manual struct construction from config
```rust
pub fn to_baseline_burn_rates(&self) -> crate::burn_rate::BaselineBurnRates {
    crate::burn_rate::BaselineBurnRates {
        pct_per_worker_per_hour: self.pct_per_worker_per_hour,
        dollars_per_worker_per_hour: self.dollars_per_worker_per_hour,
    }
}
```
**Category**: N/A (conversion function, not direct construction)
**Status**: ✅ CORRECT - Converts config to BaselineBurnRates

---

### 2. `src/config.rs:152-156` - `AgentConfig::baseline_burn_rate_or_default()`
**Pattern**: BaselineBurnRates::default() as fallback
```rust
pub fn baseline_burn_rate_or_default(&self) -> crate::burn_rate::BaselineBurnRates {
    match &self.baseline_burn_rate {
        Some(config) => config.to_baseline_burn_rates(),
        None => crate::burn_rate::BaselineBurnRates::default(),
    }
}
```
**Category**: Cold (when no config)
**Status**: ✅ CORRECT - Already has path to use config

---

### 3. `src/governor.rs:843-867` - `get_sonnet_baseline_config()`
**Pattern**: Delegates to `cfg.baseline_burn_rate_or_default()`
```rust
fn get_sonnet_baseline_config(
    agents: &HashMap<String, AgentConfig>,
) -> crate::burn_rate::BaselineBurnRates {
    // First try to find a subscription agent with "sonnet" in its name
    for (name, cfg) in agents {
        if cfg.subscription && (name.contains("sonnet") || name.contains("needle")) {
            return cfg.baseline_burn_rate_or_default();
        }
    }
    // Fallback: use the first subscription agent's baseline, or default
    for (name, cfg) in agents {
        if cfg.subscription {
            return cfg.baseline_burn_rate_or_default();
        }
    }
    // No subscription agents at all - use default
    crate::burn_rate::BaselineBurnRates::default()
}
```
**Category**: Warm if agent has config; Cold (line 867) if no subscription agents
**Status**: ✅ CORRECT - Uses config path via baseline_burn_rate_or_default()
**Note**: Line 867 is a true cold-start fallback (no subscription agents configured)

---

### 4. `src/state.rs:753-756` - `load_baseline_burn_rates_from_config()`
**Pattern**: Manual construction from config
```rust
if let Some(baseline_config) = &agent_config.baseline_burn_rate {
    let baseline = BaselineBurnRates {
        pct_per_worker_per_hour: baseline_config.pct_per_worker_per_hour,
        dollars_per_worker_per_hour: baseline_config.dollars_per_worker_per_hour,
    };
    self.baseline_burn_rates.insert(agent_name.clone(), baseline);
}
```
**Category**: Warm (uses config values)
**Status**: ✅ CORRECT - Loads from config into state
**Note**: This is the primary method for populating GovernorState.baseline_burn_rates HashMap

---

### 5. `src/state.rs:766` - Comment reference
```rust
// The caller can use BaselineBurnRates::default() as a fallback
```
**Category**: N/A (documentation comment)
**Status**: ✅ CORRECT - Documents fallback behavior

---

## Test Code Sites (src/burn_rate.rs)

All test sites use `BaselineBurnRates::default()` or manual construction for test isolation. These are NOT production code and do not need config integration.

### 6. `src/burn_rate.rs:2187` - `estimate_with_two_workers_computes_fleet_stats`
**Pattern**: BaselineBurnRates::default()
**Category**: Test code

### 7. `src/burn_rate.rs:2264` - `estimate_with_changed_workers_skips`
**Pattern**: BaselineBurnRates::default()
**Category**: Test code

### 8. `src/burn_rate.rs:2301` - `estimate_with_no_valid_data_returns_empty`
**Pattern**: BaselineBurnRates::default()
**Category**: Test code

### 9. `src/burn_rate.rs:2339` - `binding_window_is_most_constrained`
**Pattern**: BaselineBurnRates::default()
**Category**: Test code

### 10. `src/burn_rate.rs:2390` - `ema_updates_over_multiple_cycles`
**Pattern**: BaselineBurnRates::default()
**Category**: Test code

### 11. `src/burn_rate.rs:2483-2486` - `baseline_used_until_min_samples`
**Pattern**: Manual construction (baseline: 99.0 pct/hr, 50.0 $/hr)
```rust
let baseline = BaselineBurnRates {
    pct_per_worker_per_hour: 99.0,
    dollars_per_worker_per_hour: 50.0,
};
```
**Category**: Test code (intentionally distinct from default to verify baseline usage)

### 12. `src/burn_rate.rs:2545-2548` - `ema_used_after_min_samples`
**Pattern**: Manual construction (same as #11)
**Category**: Test code

### 13. `src/burn_rate.rs:2608` - `each_window_independent_ema_sampling`
**Pattern**: BaselineBurnRates::default()
**Category**: Test code

---

## Test Fixtures (other files)

### 14-17. Test fixtures in status_display.rs, capacity_summary.rs, narrator.rs, alerts.rs
**Pattern**: HashMap::new() for baseline_burn_rates field
```rust
baseline_burn_rates: HashMap::new(),
```
**Category**: Test fixtures
**Status**: ✅ CORRECT - Empty HashMap is appropriate for tests

---

## Summary Table

| File | Line | Type | Pattern | Status | Action Needed |
|------|------|------|---------|--------|---------------|
| config.rs | 109-114 | Conversion | Manual from config | ✅ Correct | None |
| config.rs | 155 | Fallback | default() | ✅ Correct | None |
| governor.rs | 843-867 | Lookup | Delegates to config | ✅ Correct | None |
| governor.rs | 867 | Fallback | default() | ✅ Correct | None |
| state.rs | 753-756 | Config load | Manual from config | ✅ Correct | None |
| state.rs | 766 | Comment | N/A | ✅ Correct | None |
| burn_rate.rs | 2187,2264,2301,2339,2390,2608 | Tests | default() | ✅ OK | N/A (test code) |
| burn_rate.rs | 2483-2486,2545-2548 | Tests | Manual | ✅ OK | N/A (test code) |
| status_display.rs, etc. | Various | Fixtures | HashMap::new() | ✅ OK | N/A (test fixtures) |

---

## Key Findings

1. **All production sites are already correct** - Every production construction site either:
   - Converts from config (`to_baseline_burn_rates()`)
   - Delegates to config path (`baseline_burn_rate_or_default()`)
   - Manually constructs from config (`load_baseline_burn_rates_from_config()`)

2. **No code changes needed** - The audit found no instances of production code incorrectly using hardcoded defaults.

3. **Cold start handling is appropriate** - The only true cold-start fallback is at governor.rs:867, which correctly defaults when no subscription agents are configured at all.

4. **Config integration is complete** - The `baseline_burn_rate` field in agent configs flows through:
   - `AgentConfig::baseline_burn_rate_or_default()` (for immediate use)
   - `GovernorState::load_baseline_burn_rates_from_config()` (for persistence)

5. **EMA threshold logic** - MIN_SAMPLES_FOR_EMA = 3 is the threshold for warm vs cold. The `effective_burn_rate()` function (burn_rate.rs:980-989) correctly uses baseline when samples < 3.

---

## Conclusion

✅ **No code changes required**. All BaselineBurnRates construction sites in production code are properly integrated with the config system. The audit confirms that the baseline_burn_rate config field is being used appropriately throughout the codebase.
