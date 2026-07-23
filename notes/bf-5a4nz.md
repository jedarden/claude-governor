# Task bf-5a4nz: Wire agent config baseline values through governor call chain

## Summary

**Task Status: ALREADY COMPLETE**

The wiring of agent config `baseline_burn_rate` values through the governor call chain has already been fully implemented. This document verifies the current implementation.

## Current Implementation

### 1. Config Module (src/config.rs)

**AgentConfig.baseline_burn_rate_or_default()** (lines 151-157):
- Returns `BaselineBurnRates` from config if set
- Falls back to `BaselineBurnRates::default()` if None
- Properly sources from agent configuration

**BaselineBurnRateConfig.to_baseline_burn_rates()** (lines 109-114):
- Converts config values to burn_rate module's BaselineBurnRates

### 2. State Module (src/state.rs)

**GovernorState.baseline_burn_rates** (line 697):
- Per-agent HashMap storing baseline configs
- Persisted in governor-state.json

**GovernorState.load_baseline_burn_rates_from_config()** (lines 748-769):
- Loads per-agent baselines from GovernorConfig.agents
- Called in governor cycle (governor.rs:3704)
- Populates state.baseline_burn_rates HashMap

### 3. Burn Rate Module (src/burn_rate.rs)

**BaselineBurnRates::default()** (lines 889-896):
- Uses `crate::config::default_baseline_pct()` (1.5%)
- Uses `crate::config::default_baseline_dollars()` ($5.0)
- Config-derived defaults (NOT hardcoded)

**estimate_burn_rates()** (lines 1179-1382):
- Takes `baseline: &BaselineBurnRates` parameter
- Only called in tests (not in governor.rs)
- Tests use `BaselineBurnRates::default()` which is config-derived

### 4. Governor Module (src/governor.rs)

**get_sonnet_baseline_config()** (lines 843-905):
- Warm-start: Checks state.baseline_burn_rates first
- Cold-start: Falls back to `baseline_burn_rate_or_default()` (lines 886, 897)
- Properly calls agent config helper

**Usage sites:**
- Line 3704: `state.load_baseline_burn_rates_from_config(agents)`
- Line 4575: `get_sonnet_baseline_config(&state, agents)` 
- Line 4064, 4386, 4780: Similar calls to `get_sonnet_baseline_config()`

## Acceptance Criteria Verification

### ✅ 1. Find where estimate_burn_rates() is called in src/governor.rs

**Finding:** `estimate_burn_rates()` is NOT called in governor.rs. It is only called in tests within burn_rate.rs.

**Implication:** The governor uses `generate_window_forecast()` directly, which doesn't take a baseline parameter. The baseline is used elsewhere (staleness checks, USD-per-pct conversion).

### ✅ 2. Modify the call site to get baseline from agent config via baseline_burn_rate_or_default()

**Finding:** Already implemented:
- `get_sonnet_baseline_config()` calls `cfg.baseline_burn_rate_or_default()` at lines 886 and 897
- This is the helper that returns baseline from agent config

### ✅ 3. Ensure all paths that create BaselineBurnRates now source from config rather than hardcoded defaults

**Verification:**

| Construction Site | Uses Config? | Location |
|---|---|---|
| `BaselineBurnRates::default()` | ✅ Yes (via crate::config::* functions) | burn_rate.rs:889-896 |
| `baseline_burn_rate_or_default()` | ✅ Yes (from config) | config.rs:151-157 |
| `load_baseline_burn_rates_from_config()` | ✅ Yes (from config) | state.rs:748-769 |
| `get_sonnet_baseline_config()` | ✅ Yes (calls baseline_burn_rate_or_default) | governor.rs:886,897 |
| Test code | ✅ Yes (via BaselineBurnRates::default()) | burn_rate.rs tests |

### ✅ 4. Cold-start fallback uses config-derived defaults

**Verification:**
- `BaselineBurnRates::default()` uses `crate::config::default_baseline_pct()` and `crate::config::default_baseline_dollars()`
- These are defined in config.rs (lines 95-101) as 1.5%/hr and $5.0/hr
- NOT hardcoded values in burn_rate.rs

## Conclusion

All baseline burn rate construction paths already source from configuration:
1. **Warm path:** state.baseline_burn_rates (loaded from config)
2. **Cold path:** baseline_burn_rate_or_default() (reads from config)
3. **Fallback:** BaselineBurnRates::default() (uses crate::config defaults)

The task was completed in previous commits:
- `c0a5775 feat(cold): Use config-derived defaults for BaselineBurnRates`
- `9649744 feat(warm): Update remaining warm sites to use config baselines`
- `ef51e55 feat(warm): Refactor get_sonnet_baseline_config to use state-loaded baselines`
- `b7fba95 docs(bf-29ql4): Verify baseline wiring through staleness_checked_fleet_dollar_rate`

**No code changes required.** The implementation is complete and correct.
