# BaselineBurnRates Construction Sites Audit

**Bead:** bf-4ljx8  
**Date:** 2026-07-23  
**Task:** Identify and document all locations where BaselineBurnRates::default() is called

## Summary

Found **2 production** call sites and **7 test** call sites for `BaselineBurnRates::default()`.

## Production Call Sites

### 1. src/config.rs:155
**Location:** `AgentConfig::baseline_burn_rate_or_default()` method  
**Call chain:**
- Called by `get_sonnet_baseline_config()` in governor.rs:847, 858
- Which is called from governor.rs:4021, 4343, 4532, 4748

**Purpose:** Returns default baseline burn rate when an agent has no `baseline_burn_rate` configured in governor.yaml

**Code:**
```rust
pub fn baseline_burn_rate_or_default(&self) -> crate::burn_rate::BaselineBurnRates {
    match &self.baseline_burn_rate {
        Some(config) => config.to_baseline_burn_rates(),
        None => crate::burn_rate::BaselineBurnRates::default(),
    }
}
```

**Note:** This is the **primary production call site** - it provides the fallback when configuration is missing.

---

### 2. src/governor.rs:866
**Location:** `get_sonnet_baseline_config()` function  
**Call chain:**
- Called from governor.rs:4021, 4343, 4532, 4748 for dollar staleness checks

**Purpose:** Returns default baseline when NO subscription agents are configured at all (emergency fallback)

**Code:**
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
            log::debug!(...);
            return cfg.baseline_burn_rate_or_default();
        }
    }

    // No subscription agents at all - use default
    log::warn!(...);
    crate::burn_rate::BaselineBurnRates::default()
}
```

**Note:** This is a **last-resort fallback** when the governor has no subscription agents configured - should rarely be hit.

---

## Test Call Sites

All test calls are in `src/burn_rate.rs` and use the default for testing burn rate calculations:

| Line | Test Function | Purpose |
|------|--------------|---------|
| 2270 | `estimate_with_two_workers_computes_fleet_stats` | Tests fleet estimation with multiple workers |
| 2347 | `estimate_with_changed_workers_skips` | Tests behavior when worker set changes |
| 2384 | `estimate_with_no_valid_data_returns_empty` | Tests empty data handling |
| 2422 | `binding_window_is_most_constrained` | Tests window constraint logic |
| 2473 | `ema_updates_over_multiple_cycles` | Tests EMA state evolution |
| 2691 | `each_window_independent_ema_sampling` | Tests per-window EMA sampling |
| 3887 | `collector_data_age_negative_age_treated_as_zero` | Tests edge case: future timestamps |

**Note:** Test calls are appropriate to remain as `BaselineBurnRates::default()` - they need consistent, predictable values for test reproducibility.

---

## Default Implementation

**Location:** src/burn_rate.rs:889-897

```rust
impl Default for BaselineBurnRates {
    fn default() -> Self {
        // Conservative defaults: ~1.5%/hr per worker, ~$5/hr per worker
        Self {
            pct_per_worker_per_hour: 1.5,
            dollars_per_worker_per_hour: 5.0,
        }
    }
}
```

**Values:** 1.5% per worker per hour, $5.00 per worker per hour

---

## Comments in Code (Not Call Sites)

Two comments reference the default but are not actual calls:

- **src/config.rs:1017** - Test comment explaining that `baseline_burn_rate` field defaults to `None` (which then calls `BaselineBurnRates::default()` via `baseline_burn_rate_or_default()`)
- **src/config.rs:1076** - Test comment noting the assertion validates default values

---

## Call Chain Summary

```
governor.rs callers (4021, 4343, 4532, 4748)
  └─> get_sonnet_baseline_config()
       ├─> (for each subscription agent) cfg.baseline_burn_rate_or_default()
       │    └─> BaselineBurnRates::default()  [if no config]
       └─> BaselineBurnRates::default()        [if no subscription agents]
```

---

## Next Steps

For future work integrating per-agent baseline_burn_rate configuration:
1. The production call at **src/config.rs:155** is the primary site that would read from the config field
2. The fallback at **src/governor.rs:866** could be eliminated once all subscription agents have explicit baseline_burn_rate configured
3. Test calls should remain unchanged
