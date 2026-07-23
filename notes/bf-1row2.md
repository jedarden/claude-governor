# Bead bf-1row2: Verify calculate_window_pct_delta location

## Task
Verify that `calculate_window_pct_delta` call is inside the Some-Some block.

## Verification

**Location:** src/governor.rs:2990-3025

### The Some-Some block (line 2990)
```rust
if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot) {
```

### calculate_window_pct_delta call (line 3002)
```rust
let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
```

### Confirmation
- ✅ The `calculate_window_pct_delta` call is inside the Some-Some block
- ✅ Delta computation only executes when both previous_api_snapshot and current_api_snapshot are Some
- ✅ State assignments (p5h_delta, p7d_delta, p7ds_delta) at lines 3013-3015 are within the block

## Acceptance Criteria Met
1. Verified calculate_window_pct_delta call is inside the Some-Some block
2. Delta computation happens within the if let pattern
3. Ready to verify state assignments in next bead
