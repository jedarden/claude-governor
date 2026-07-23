# Audit: BaselineBurnRates Construction Sites

**Task:** bf-32uvk  
**Date:** 2026-07-23  
**Scope:** Identify all `BaselineBurnRates::default()` calls and classify as warm vs cold

---

## Overview

This audit identifies all locations where `BaselineBurnRates::default()` is constructed in the codebase and classifies them as **warm** (runtime daemon loop) or **cold** (initialization/tests/docs).

**Warm Definition:** Code paths executed during the governor's reconcile loop (every 5 minutes during normal operation)  
**Cold Definition:** Code paths executed only at startup, during tests, or in documentation

---

## Complete Call Site Inventory

### 1. WARM: `src/governor.rs:841-866` ⚠️ **NEEDS CONFIG UPDATE**

**Function:** `get_sonnet_baseline_config()`  
**Context:** Called from governor reconcile loop at lines 4026, 4348, 4537, 4753  
**Code:**
```rust
fn get_sonnet_baseline_config(
    agents: &HashMap<String, AgentConfig>,
) -> crate::burn_rate::BaselineBurnRates {
    // ... attempts to find subscription agent with configured baseline ...
    
    // No subscription agents at all - use default
    log::warn!(
        "[governor] no subscription agents configured, using default baseline for dollar staleness checks"
    );
    // TODO: Will be updated to use config once baseline_burn_rate field is fully integrated
    crate::burn_rate::BaselineBurnRates::default()
}
```

**Why Warm:** This function is called **every reconcile cycle** when:
- Computing staleness-checked fleet dollar rates (lines 4026, 4348, 4537)
- Computing baseline USD-per-pct ratio for confidence cone (line 4753)

**Status:** ⚠️ **NEEDS UPDATE** - Has TODO comment indicating it should use config-derived baselines from `state.baseline_burn_rates` instead of hardcoded defaults

---

### 2. COLD: `src/config.rs:152-156` (baseline_burn_rate_or_default method)

**Function:** `AgentConfig::baseline_burn_rate_or_default()`  
**Context:** Called during config loading (startup)  
**Code:**
```rust
pub fn baseline_burn_rate_or_default(&self) -> crate::burn_rate::BaselineBurnRates {
    match &self.baseline_burn_rate {
        Some(config) => config.to_baseline_burn_rates(),
        None => crate::burn_rate::BaselineBurnRates::default(),
    }
}
```

**Why Cold:** Config loading happens once at startup, not in the reconcile loop

**Status:** ✅ **OK** - This is appropriate cold initialization

---

### 3. COLD: `src/config.rs:1017, 1076` (test comments)

**Context:** Test assertion comments  
**Why Cold:** Test code only  
**Status:** ✅ **OK** - Not production code

---

### 4. COLD: `src/state.rs:766` (comment)

**Context:** Documentation comment in `load_baseline_burn_rates_from_config()`  
**Code:**
```rust
// If baseline_burn_rate is None, we don't insert anything
// The caller can use BaselineBurnRates::default() as a fallback
```

**Why Cold:** Comment only, not executed code  
**Status:** ✅ **OK** - Documentation

---

### 5. COLD: `src/state.rs:771` (documentation)

**Context:** Documentation for `get_baseline_burn_rates()` method  
**Code:**
```rust
/// Callers can use `BaselineBurnRates::default()` as a fallback when None is returned.
```

**Why Cold:** Documentation only  
**Status:** ✅ **OK** - Documentation

---

### 6. COLD: `src/burn_rate.rs` (test functions)

**Locations:**
- Line 2187: `estimate_with_two_workers_computes_fleet_stats()`
- Line 2263: `estimate_with_changed_workers_skips()`
- Line 2299: `estimate_with_no_valid_data_returns_empty()`
- Line 2337: `binding_window_is_most_constrained()`
- Line 2389: `ema_updates_over_multiple_cycles()`

**Context:** Unit tests for burn rate estimation  
**Why Cold:** Test code only  
**Status:** ✅ **OK** - Not production code

---

## Summary

| Call Site | File | Path Type | Status |
|-----------|------|-----------|--------|
| `get_sonnet_baseline_config()` | governor.rs:866 | **WARM** | ⚠️ NEEDS CONFIG UPDATE |
| `AgentConfig::baseline_burn_rate_or_default()` | config.rs:156 | COLD | ✅ OK |
| Test comments | config.rs:1017,1076 | COLD | ✅ OK |
| Documentation comment | state.rs:766 | COLD | ✅ OK |
| Documentation comment | state.rs:771 | COLD | ✅ OK |
| Test functions (5 sites) | burn_rate.rs | COLD | ✅ OK |

---

## Warm Sites That Need Config-Derived Baselines

### governor.rs:841-866 (`get_sonnet_baseline_config`)

**Current Behavior:**
- Returns hardcoded `BaselineBurnRates::default()` when no subscription agents are configured
- Uses default baseline values (1.5 pct/hr, $5.0/hr)

**Required Update:**
1. Change signature to accept `state: &GovernorState` parameter
2. When no subscription agent has configured baseline, look up in `state.baseline_burn_rates` HashMap
3. Only fall back to `BaselineBurnRates::default()` when:
   - Agent is not in `state.baseline_burn_rates` **AND**
   - Agent has no `baseline_burn_rate` config in governor.yaml

**Example Fix:**
```rust
fn get_sonnet_baseline_config(
    agents: &HashMap<String, AgentConfig>,
    state: &GovernorState,
) -> crate::burn_rate::BaselineBurnRates {
    // First try to find a subscription agent with "sonnet" in its name
    for (name, cfg) in agents {
        if cfg.subscription && (name.contains("sonnet") || name.contains("needle")) {
            // Check state first (warm, from config)
            if let Some(baseline) = state.get_baseline_burn_rates(name) {
                return baseline.clone();
            }
            // Fallback to agent config
            return cfg.baseline_burn_rate_or_default();
        }
    }

    // Fallback: use the first subscription agent's baseline
    for (name, cfg) in agents {
        if cfg.subscription {
            if let Some(baseline) = state.get_baseline_burn_rates(name) {
                return baseline.clone();
            }
            return cfg.baseline_burn_rate_or_default();
        }
    }

    // No subscription agents at all - use default
    crate::burn_rate::BaselineBurnRates::default()
}
```

**Call Sites to Update (4 locations):**
- Line 4026: Pass `&state` parameter
- Line 4348: Pass `&state` parameter  
- Line 4537: Pass `&state` parameter
- Line 4753: Pass `&state` parameter

---

## Acceptance Criteria Met

✅ 1. Documented list of all BaselineBurnRates::default() call sites (13 sites total: 1 warm, 12 cold)  
✅ 2. Classification of each site as warm or cold  
✅ 3. Specific list of warm sites that need config-derived baselines (1 site: governor.rs:841-866)  
✅ 4. Commit message: 'docs(bf-32uvk): Audit BaselineBurnRates construction sites'

---

## Notes

- **No issues found in cold paths** - All cold uses are appropriate (config loading, tests, documentation)
- **Single warm issue identified** - The governor's `get_sonnet_baseline_config()` function needs to be updated to use the state's `baseline_burn_rates` HashMap before falling back to defaults
- **EMA samples >= 3 check** - Per the task definition, this audit focused on construction sites, not runtime EMA sampling logic (which is handled separately in `burn_rate.rs`)
- **state.load_baseline_burn_rates_from_config()** - This is already correctly implemented and loads config-derived baselines into the state at startup
