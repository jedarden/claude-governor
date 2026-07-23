# Task bf-5q5dk: Already Completed

## Task Description
Add BaselineBurnRates config field to GovernorState

## Finding
This task has **already been completed** in prior work. The implementation exists in `src/state.rs`:

### Implementation Summary

**1. Field Added (line 696)**
```rust
/// Per-agent baseline burn rates from config.
/// Used as fallback when token collector is offline or EMA is not yet ready.
/// Key is agent name (e.g., "needle-sonnet", "polish-opus").
#[serde(default)]
pub baseline_burn_rates: HashMap<String, BaselineBurnRates>,
```

**2. Load Method (lines 732-768)**
- `load_baseline_burn_rates_from_config()` iterates through agents_config
- Populates HashMap with configured values per agent
- Logs each loaded baseline at DEBUG level

**3. Accessor Method (lines 770-783)**
- `get_baseline_burn_rates(agent_name: &str) -> Option<&BaselineBurnRates>`
- Returns Some if configured, None if not found
- Documented that callers should use BaselineBurnRates::default() as fallback

**4. Tests**
All baseline burn rate tests pass:
- `load_baseline_burn_rates_from_config_populates_state`
- `get_baseline_burn_rates_returns_none_for_unknown_agent`
- `baseline_burn_rates_roundtrip_serialization`
- Plus related config tests

### Design Note
The implementation uses `HashMap<String, BaselineBurnRates>` instead of the task's suggested `Option<BaselineBurnRates>`. This is **better** because it supports multiple agents with different baseline configurations, which matches the multi-agent pool architecture.

### Related Completed Beads
- bf-3s3qg: Add baseline_burn_rates field to GovernorState (closed)
- bf-3brmm: Implement load_baseline_burn_rates_from_config method (closed)
- bf-5a74x: Add get_baseline_burn_rates accessor method (closed)

### Conclusion
Task bf-5q5dk is superseded by prior implementation. No additional work needed.
