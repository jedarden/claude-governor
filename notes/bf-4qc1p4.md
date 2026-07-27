# Bead bf-4qc1p4: Update is_structurally_inactive call sites

## Task
Update all code that calls `is_structurally_inactive` to pass only `window` and `state` (not `window_name`).

## Findings

### No existing call sites found
A thorough search of the codebase (`grep -rn "is_structurally_inactive(" src/`) revealed:
- Only the function definition exists at `src/governor.rs:127`
- **Zero actual call sites** in the current codebase

### Why this is correct
The function was refactored in bead bf-ax914v (commit d905d1f) from:
```rust
fn is_structurally_inactive(window_name: &str, window: &UsageWindow, state: &GovernorState) -> bool
```

To the new 2-parameter signature:
```rust
fn is_structurally_inactive(window: &UsageWindow, state: &GovernorState) -> bool
```

Since the function was just created/defined and hasn't been integrated into the binding-window selection logic yet (that's scheduled for future bead bf-1obif0), there are no legacy call sites to update.

### Future usage
Bead bf-1obif0 "Wire is_structurally_inactive filter into binding-window selection" will add call sites, and it's already planned to use the correct 2-parameter signature:
```
is_structurally_inactive(window, state)
```

## Acceptance criteria
- ✅ All call sites pass only 2 parameters: window and state (N/A - no call sites exist)
- ✅ Code compiles successfully
- ✅ No compiler warnings about unused parameters

## Conclusion
The bead acceptance criteria are satisfied. There were no call sites to update because the function was refactored before it was integrated into any calling code.
