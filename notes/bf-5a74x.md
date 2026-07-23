# Bead bf-5a74x: Add get_baseline_burn_rates accessor method

## Task Status

**COMPLETE** - Method was already implemented in `src/state.rs` (lines 770-783).

## Implementation Details

The `get_baseline_burn_rates` method on `GovernorState` already exists and meets all acceptance criteria:

### Method Signature
```rust
pub fn get_baseline_burn_rates(&self, agent_name: &str) -> Option<&BaselineBurnRates>
```

### Acceptance Criteria Met

1. ✅ Method added to `GovernorState` impl block (line 781)
2. ✅ Accepts parameter: `agent_name: &str`
3. ✅ Returns `Option<&BaselineBurnRates>` (Some if configured, None if not found)
4. ✅ Documentation explains None means cold-start or not configured:
   - "Returns the configured baseline burn rates for the agent, or None if not configured."
   - Return docs: "None if the agent is not in the state (cold-start or not configured)"
5. ✅ Documents that callers should use `BaselineBurnRates::default()` as fallback:
   - "Callers can use `BaselineBurnRates::default()` as a fallback when None is returned."

### Existing Tests

The method has comprehensive test coverage:
- `get_baseline_burn_rates_returns_none_for_unknown_agent` - Verifies None is returned for unknown agents
- `load_baseline_burn_rates_from_config_populates_state` - Tests successful retrieval of configured baselines

### Compilation

Code compiles successfully with `cargo check` and all tests pass.
