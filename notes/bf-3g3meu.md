# bead bf-3g3meu: is_structurally_inactive predicate

## Task
Add is_structurally_inactive predicate to governor.rs

## Status
**ALREADY COMPLETE** - The function was implemented in a prior bead.

## Implementation Details
Located in `src/governor.rs` at lines 127-144.

### Function signature
```rust
fn is_structurally_inactive(window: &UsageWindow, state: &state::GovernorState) -> bool
```

### Implementation
```rust
fn is_structurally_inactive(window: &UsageWindow, state: &state::GovernorState) -> bool {
    let is_inactive_by_consecutive_absence = state.is_window_consecutively_absent(&window.name);
    let is_inactive_by_api = window.is_active == Some(false);
    is_inactive_by_consecutive_absence || is_inactive_by_api
}
```

### Acceptance criteria verification
✅ Compiles and returns correct bool based on conditions  
✅ Handles both conditions independently (either true = inactive)  
✅ MIN_CONSECUTIVE_ABSENT defined as 3  
✅ Gracefully handles missing is_active field (treats None/true as active)  

### Test coverage
14 unit tests in `governor::is_structurally_inactive_tests` all pass:
- Boundary cases (threshold, just below)
- Independent condition testing
- Both conditions true/false combinations
- is_active field variations (None, true, false)
- Window absent map handling
