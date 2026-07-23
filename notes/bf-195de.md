# Task bf-195de: Find all BaselineBurnRates construction sites

## Summary

This task searched the codebase for all locations where `BaselineBurnRates` is constructed or used to understand the current wiring before making changes.

## Key Finding: Two Separate `BaselineBurnRates` Structs

There are **two distinct** `BaselineBurnRates` structs in the codebase:

1. **`state::BaselineBurnRates`** (`src/state.rs:510-526`)
   - Lives in the governor state module
   - Stored in `GovernorState.baseline_burn_rates: HashMap<String, BaselineBurnRates>`
   - Used for persistence and warm-path lookups

2. **`burn_rate::BaselineBurnRates`** (`src/burn_rate.rs:881-897`)
   - Lives in the burn rate estimation module
   - Used by the adaptive estimator as a fallback when EMA is not ready
   - Conversion function exists between the two types

## Construction Sites

### 1. Config Module (`src/config.rs`)

**Source of truth for default values:**
```rust
pub fn default_baseline_pct() -> f64 { 1.5 }
pub fn default_baseline_dollars() -> f64 { 5.0 }
```

**Config-to-burn-rate conversion:**
```rust
// Line 109-114: BaselineBurnRateConfig::to_baseline_burn_rates()
pub fn to_baseline_burn_rates(&self) -> crate::burn_rate::BaselineBurnRates {
    crate::burn_rate::BaselineBurnRates {
        pct_per_worker_per_hour: self.pct_per_worker_per_hour,
        dollars_per_worker_per_hour: self.dollars_per_worker_per_hour,
    }
}
```

**Agent baseline retrieval with default fallback:**
```rust
// Line 152-157: AgentConfig::baseline_burn_rate_or_default()
pub fn baseline_burn_rate_or_default(&self) -> crate::burn_rate::BaselineBurnRates {
    match &self.baseline_burn_rate {
        Some(config) => config.to_baseline_burn_rates(),
        None => crate::burn_rate::BaselineBurnRates::default(),
    }
}
```

### 2. State Module (`src/state.rs`)

**Default implementation (config-derived):**
```rust
// Line 519-526: BaselineBurnRates::default()
impl Default for BaselineBurnRates {
    fn default() -> Self {
        Self {
            pct_per_worker_per_hour: crate::config::default_baseline_pct(),
            dollars_per_worker_per_hour: crate::config::default_baseline_dollars(),
        }
    }
}
```

**Population from agent configs (warm-start):**
```rust
// Line 748-769: GovernorState::load_baseline_burn_rates_from_config()
pub fn load_baseline_burn_rates_from_config(&mut self, agents_config: &HashMap<String, AgentConfig>) {
    for (agent_name, agent_config) in agents_config {
        if let Some(baseline_config) = &agent_config.baseline_burn_rate {
            let baseline = BaselineBurnRates { /* ... */ };
            self.baseline_burn_rates.insert(agent_name.clone(), baseline);
        }
    }
}
```

**Retrieval method:**
```rust
// Line 771-784: GovernorState::get_baseline_burn_rates()
pub fn get_baseline_burn_rates(&self, agent_name: &str) -> Option<&BaselineBurnRates> {
    self.baseline_burn_rates.get(agent_name)
}
```

### 3. Burn Rate Module (`src/burn_rate.rs`)

**Default implementation (config-derived):**
```rust
// Line 889-897: BaselineBurnRates::default()
impl Default for BaselineBurnRates {
    fn default() -> Self {
        Self {
            pct_per_worker_per_hour: crate::config::default_baseline_pct(),
            dollars_per_worker_per_hour: crate::config::default_baseline_dollars(),
        }
    }
}
```

**Staleness-checked fleet dollar rate:**
```rust
// Line 1463-1491: staleness_checked_fleet_dollar_rate()
pub fn staleness_checked_fleet_dollar_rate(
    aggregate: &crate::state::FleetAggregate,
    baseline: &crate::state::BaselineBurnRates,
) -> f64 {
    // Uses baseline.dollars_per_worker_per_hour when aggregate is stale
}
```

**Test construction:**
```rust
// Line 2229, 2306, 2343, 2381, 2431: Test BaselineBurnRates::default() calls
let baseline = BaselineBurnRates::default();
```

### 4. Governor Module (`src/governor.rs`)

**Warm-path baseline retrieval (line 843-906):**
```rust
fn get_sonnet_baseline_config(
    state: &state::GovernorState,
    agents: &HashMap<String, AgentConfig>,
) -> crate::state::BaselineBurnRates {
    // Priority 1: state.baseline_burn_rates (warm-start)
    // Priority 2: agent config lookup (cold-start)
    // Priority 3: BaselineBurnRates::default() (no config available)
}
```

**Conversion helper (line 876-881):**
```rust
let convert_baseline = |br: crate::burn_rate::BaselineBurnRates| -> crate::state::BaselineBurnRates {
    crate::state::BaselineBurnRates {
        pct_per_worker_per_hour: br.pct_per_worker_per_hour,
        dollars_per_worker_per_hour: br.dollars_per_worker_per_hour,
    }
};
```

**Called in daemon loop (line 3704):**
```rust
state.load_baseline_burn_rates_from_config(agents);
```

## Call Chain: Where Baseline Values Originate

```
Config File (governor.yaml)
    │
    ▼
GovernorConfig::load()
    │
    ▼
AgentConfig.baseline_burn_rate: Option<BaselineBurnRateConfig>
    │
    ├──> Some(config) ──> BaselineBurnRateConfig::to_baseline_burn_rates()
    │                      └─> BaselineBurnRates { pct, dollars }
    │
    └──> None ──> BaselineBurnRates::default()
                     └─> default_baseline_pct() ──> 1.5
                     └─> default_baseline_dollars() ──> 5.0
```

## Call Chain: Where Baseline Values Are Consumed

### Warm Path (daemon loop)

```
GovernorState::load_baseline_burn_rates_from_config(agents)
    │
    ▼
state.baseline_burn_rates: HashMap<String, BaselineBurnRates>
    │
    ▼
get_sonnet_baseline_config(state, agents)
    │
    ├──> Priority 1: Lookup in state.baseline_burn_rates
    ├──> Priority 2: AgentConfig::baseline_burn_rate_or_default()
    └──> Priority 3: BaselineBurnRates::default() (1.5%, $5.0/hr)
```

### Cold Path (burn rate estimation)

```
estimate_burn_rates() parameters
    │
    ├──> _baseline: &BaselineBurnRates (passed by caller)
    └──> effective_burn_rate() uses baseline when ema.samples < 3
```

### Staleness Check Path

```
staleness_checked_fleet_dollar_rate(aggregate, baseline)
    │
    ├──> Fresh aggregate: uses aggregate.sonnet_p75_usd_hr / sonnet_workers
    └──> Stale aggregate: uses baseline.dollars_per_worker_per_hour
```

## Test Construction Sites

All `BaselineBurnRates::default()` calls in tests are in `src/burn_rate.rs`:
- Line 2229: `estimate_with_two_workers_computes_fleet_stats`
- Line 2306: `estimate_with_changed_workers_skips`
- Line 2343: `estimate_with_no_valid_data_returns_empty`
- Line 2381: `binding_window_is_most_constrained`
- Line 2431: `ema_updates_over_multiple_cycles`

## Key Observations

1. **Config is the single source of truth**: Both `BaselineBurnRates::default()` implementations source from `default_baseline_pct()` and `default_baseline_dollars()` functions in `config.rs`.

2. **Two-phase loading**:
   - **Cold start**: `AgentConfig::baseline_burn_rate_or_default()` provides immediate fallback
   - **Warm start**: `GovernorState.load_baseline_burn_rates_from_config()` populates the HashMap for efficient lookups

3. **Three-tier fallback hierarchy** in `get_sonnet_baseline_config()`:
   - State-loaded baselines (warm)
   - Agent config lookup (cold)
   - Default values (no config)

4. **Struct conversion**: `burn_rate::BaselineBurnRates` and `state::BaselineBurnRates` are separate types with a conversion helper in governor.rs.

5. **All production `BaselineBurnRates::default()` calls use config-derived defaults** (1.5% per hour, $5.0 per hour). Test calls use the same defaults for consistency.
