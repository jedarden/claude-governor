# Investigation: run_governor_cycle Pattern Matching (bf-3yv31)

## Task
Examine the pattern matching code at `src/governor.rs:2585` to understand the current structure used for Option types.

## Findings

### Primary Pattern (Line 2585)

**Exact Code:**
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

    log::info!(
        "[governor] {} computed window deltas: 5h={:+.3}% 7d={:+.3}% 7ds={:+.3}%",
        now.to_rfc3339(),
        delta_5h, delta_7d, delta_7ds
    );
}
```

### Pattern Structure Analysis

1. **Tuple Pattern Match**: `(Some(prev), Some(curr))` simultaneously destructures two `Option` values
2. **Borrowed References**: Uses `&` to avoid moving values (both are borrowed from `state`)
3. **Both-Some Guard**: The block only executes when **both** Options are `Some`
4. **No Else Branch**: If either Option is `None`, the code simply does nothing (no delta computed)
5. **Purpose**: Computes window deltas only when consecutive snapshots are available

### Context

- **Location**: Inside `run_governor_cycle` at line 2585
- **Purpose**: Calculate window deltas from consecutive API snapshots
- **State Fields Used**:
  - `state.previous_api_snapshot: Option<PrevUsageSnapshot>`
  - `state.current_api_snapshot: Option<PrevUsageSnapshot>`
  - Sets `state.p5h_delta`, `state.p7d_delta`, `state.p7ds_delta`

### Similar Patterns in Same File

1. **Line 2679**: Same pattern for JSON string extraction:
   ```rust
   if let (Some(t0_str), Some(t1_str)) = (
       fleet_json.get("t0").and_then(|v| v.as_str()),
       fleet_json.get("t1").and_then(|v| v.as_str()),
   ) {
   ```

2. **Line 1028** (commented): Test/example version showing the same pattern:
   ```rust
   // if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot)
   match (previous, current) {
       (Some(prev), Some(curr)) => { ... }
   ```

## Summary

The current pattern is a **tuple-based `if let` pattern match** that checks for `Some-Some` cases only. This is a concise Rust idiom for handling cases where multiple Option values must all be present before proceeding with computation.
