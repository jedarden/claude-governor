# bf-59xbv: Located Some-Some block in governor.rs

## Task
Locate the Some-Some block in governor.rs that guards delta computation.

## Finding
The Some-Some block is located at **lines 2990-3025** in `src/governor.rs` (not at the lines 2585-2609 mentioned in the bead description - those lines contain the emergency brake and binding window selection code).

## Exact boundaries

- **if let pattern start:** Line 2990
  ```rust
  if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot) {
  ```

- **Some-Some arm end:** Line 3015 (closing `}` before `else`)
- **Full if-else block end:** Line 3025 (final closing `}`)

## Code structure

The Some-Some pattern ensures both `previous_api_snapshot` and `current_api_snapshot` exist before computing deltas:

1. **Some-Some arm (2990-3015):** When both snapshots are available
   - Constructs `WindowPctSnapshot` from previous and current data
   - Calls `calculate_window_pct_delta(&prev_pct, &curr_pct)`
   - Logs deltas with detailed context
   - Stores computed deltas in `state.p5h_delta`, `state.p7d_delta`, `state.p7ds_delta`

2. **else arm (3016-3024):** First poll case (no previous snapshot)
   - Initializes all deltas to `Some(0.0)`

## State delta assignments
Inside the Some-Some block (lines 3013-3015):
```rust
state.p5h_delta = Some(delta_5h);
state.p7d_delta = Some(delta_7d);
state.p7ds_delta = Some(delta_7ds);
```

These assignments are correctly placed within the Some-Some block, ensuring they only execute when both snapshots are available.
