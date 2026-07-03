# Bead bf-1f6lx: Option Pattern Matching for Snapshot Deltas

## Task
Implement proper Option pattern matching structure in `run_governor_cycle` for delta computation between consecutive API snapshots.

## Implementation
The code at `src/governor.rs:2011` already has the correct pattern:

```rust
if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot) {
    // Delta computation ONLY inside this Some-Some block
    let prev_pct = crate::db::WindowPctSnapshot { ... };
    let curr_pct = crate::db::WindowPctSnapshot { ... };
    let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
    // Store deltas in state
} else {
    // First poll case: prev is None
}
```

## Verification
- Pattern matches on `&Option<PrevUsageSnapshot>` and `&Option<PrevUsageSnapshot>`
- Explicitly checks for `Some` on BOTH snapshots before computing deltas
- Delta computation (lines 2012-2033) is ONLY inside the Some-Some block
- Code compiles successfully
- All 17 window delta tests pass

## Acceptance Criteria Met
✓ Pattern matches on Option<PreviousSnapshot> and Option<CurrentSnapshot>
✓ Code compiles with the pattern structure
✓ Delta computation is ONLY inside the Some-Some block
