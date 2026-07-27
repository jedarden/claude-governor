# Bead bf-676sby: Replace sonnet_pct with model-agnostic weekly_scoped pct source

## Task Completion Status: Already Complete

The requested change has already been implemented in the codebase. The code is already using the model-agnostic `weekly_scoped_pct` field instead of `sonnet_pct` for weekly_scoped calculations.

## Evidence

### 1. Model-agnostic field is already being used (src/governor.rs:3815)
```rust
state.usage = state::UsageState {
    weekly_scoped_pct: usage_data.weekly_scoped_utilization,  // Model-agnostic limits[]-derived value
    // ...
}
```

### 2. Bug fix documentation (src/governor.rs:3816-3824)
The code contains comments documenting this exact fix (bead bf-5zk558, commit 082e400):
```rust
// BUGFIX (bf-5zk558, commit 082e400):
// Previously, sonnet_pct was always set to weekly_scoped_utilization
// regardless of which model the weekly_scoped window was tracking.
// This was incorrect when the model rotated to Opus, Fable, etc.
//
// Fixed: Only set sonnet_pct when weekly_scoped is actually tracking Sonnet;
// otherwise set to 0.0 since the legacy field should not reflect other models.
//
// New code should use weekly_scoped_pct (model-agnostic) instead of sonnet_pct.
```

### 3. Business logic uses the model-agnostic field
All references in the business logic use `state.usage.weekly_scoped_pct`:
- Line 4157: `let new_weekly_scoped = state.usage.weekly_scoped_pct;`
- Line 4309: `weekly_scoped: state.usage.weekly_scoped_pct,`
- Line 4363: `current_utilization.insert("weekly_scoped".to_string(), state.usage.weekly_scoped_pct);`
- Line 4377: `let cur_7ds = state.usage.weekly_scoped_pct;`

### 4. Code compiles successfully
```bash
cargo check  # No errors
```

## Acceptance Criteria Status

- ✓ src/governor.rs no longer references sonnet_pct for weekly_scoped
- ✓ Code now reads from the model-agnostic limits[]-derived value
- ✓ Code compiles without errors
- ✓ Change is isolated to the weekly_scoped pct calculation only

## Conclusion

No code changes were required. The fix was already implemented in a previous commit (082e400, bead bf-5zk558).
