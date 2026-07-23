# Analysis of prev_snapshot Handling in run_governor_cycle

## Task: Examine governor.rs run_governor_cycle snapshot handling

### Findings

#### 1. Where prev_snapshot is used

**Line 3337** - Snapshot shift before poll:
```rust
state.previous_api_snapshot = state.current_api_snapshot.take();
```
- This shifts the current snapshot to previous before each new poll
- On first poll, `current_api_snapshot` is `None`, so `previous_api_snapshot` becomes `None` too

**Line 3368** - Delta computation guard:
```rust
if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot) {
```
- This is where `prev_snapshot` is actually used for computation
- Properly handles the `None` case with an `if let` pattern match

#### 2. Delta computation logic (Lines 3368-3403)

```rust
if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot) {
    // Both snapshots available: proceed with delta computation
    let prev_pct = crate::db::WindowPctSnapshot {
        five_hour: prev.five_hour_pct,
        seven_day: prev.seven_day_pct,
        seven_day_sonnet: prev.seven_day_sonnet_pct,
    };
    let curr_pct = crate::db::WindowPctSnapshot {
        five_hour: curr.five_hour_pct,
        seven_day: curr.seven_day_pct,
        seven_day_sonnet: curr.seven_day_sonnet_pct,
    };
    let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

    // Store computed deltas in governor state
    state.p5h_delta = Some(delta_5h);
    state.p7d_delta = Some(delta_7d);
    state.p7ds_delta = Some(delta_7ds);
} else {
    // No previous snapshot available (first poll)
    // Set delta fields to Some(0.0) to indicate no change from initial state
    state.p5h_delta = Some(0.0);
    state.p7d_delta = Some(0.0);
    state.p7ds_delta = Some(0.0);
    log::debug!(
        "[governor] window deltas: no previous snapshot (first poll), deltas initialized to 0.0",
    );
}
```

#### 3. Current panic risk points

**THERE IS NO PANIC RISK** in the actual `run_governor_cycle` function when `prev_snapshot` is `None`.

The code properly handles both cases:
- **Both snapshots `Some`**: Computes and stores real deltas
- **`previous_api_snapshot` is `None`** (first poll): Sets all deltas to `Some(0.0)`

The `.unwrap()` calls found on lines 2121, 2307, 2321, and 7560 are all in **TEST code**, not production code. They are safe because the tests ensure `previous_api_snapshot` is `Some` before calling `.unwrap()`.

### Conclusion

The snapshot handling in `run_governor_cycle` is **already panic-safe**. The use of `if let (Some(prev), Some(curr))` pattern matching ensures that:
1. Delta computation only happens when both snapshots exist
2. First poll gracefully initializes deltas to 0.0
3. No `.unwrap()` or `.expect()` calls on `prev_snapshot` in production code
