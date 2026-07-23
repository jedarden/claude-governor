# Analysis: Current if let Pattern Structure in run_governor_cycle

## Location
File: `src/governor.rs`, function: `run_governor_cycle` (starting at line 3607)

## Current Pattern Structure (lines 3664-3709)

The code currently uses a **`match` statement** (not `if let`) with three arms to handle snapshot availability:

```rust
match (&state.previous_api_snapshot, &state.current_api_snapshot) {
    (Some(prev), Some(curr)) => {
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

        // Log and store computed deltas
        state.p5h_delta = Some(delta_5h);
        state.p7d_delta = Some(delta_7d);
        state.p7ds_delta = Some(delta_7ds);
    }
    (None, Some(_curr)) => {
        // First poll: no previous snapshot available
        state.p5h_delta = Some(0.0);
        state.p7d_delta = Some(0.0);
        state.p7ds_delta = Some(0.0);
    }
    (None, None) | (Some(_), None) => {
        // Neither snapshot available OR only previous available
        // Leave deltas as None (no change)
    }
}
```

## Delta Computation Logic

### Function: `calculate_window_pct_delta` (line 864)
```rust
pub fn calculate_window_pct_delta(
    previous_snapshot: &crate::db::WindowPctSnapshot,
    current_snapshot: &crate::db::WindowPctSnapshot,
) -> (f64, f64, f64) {
    let delta_5h = current_snapshot.five_hour - previous_snapshot.five_hour;
    let delta_7d = current_snapshot.seven_day - previous_snapshot.seven_day;
    let delta_7ds = current_snapshot.seven_day_sonnet - previous_snapshot.seven_day_sonnet;
    (delta_5h, delta_7d, delta_7ds)
}
```

### Data Structures

**`PrevUsageSnapshot`** (`src/state.rs`, line 134):
```rust
pub struct PrevUsageSnapshot {
    pub taken_at: DateTime<Utc>,
    pub five_hour_pct: f64,
    pub seven_day_pct: f64,
    pub seven_day_sonnet_pct: f64,
}
```

**`WindowPctSnapshot`** (`src/db.rs`, line 690):
```rust
pub struct WindowPctSnapshot {
    pub five_hour: f64,         // 5-hour window utilization percentage
    pub seven_day: f64,         // 7-day all-models window utilization percentage
    pub seven_day_sonnet: f64,  // 7-day Sonnet window utilization percentage
}
```

## Snapshot State Management

**At cycle start (line 3632):**
```rust
// Shift snapshot state before poll: current becomes previous
state.previous_api_snapshot = state.current_api_snapshot.take();
```

**After successful poll (lines 3655-3660):**
```rust
state.current_api_snapshot = Some(state::PrevUsageSnapshot {
    taken_at: now,
    five_hour_pct: usage_data.five_hour_utilization,
    seven_day_pct: usage_data.seven_day_utilization,
    seven_day_sonnet_pct: usage_data.seven_day_sonnet_utilization,
});
```

## What Needs to Change

This analysis confirms the current implementation already properly handles:

1. ✅ **First poll scenario** - `(None, Some(_curr))` arm sets deltas to `Some(0.0)`
2. ✅ **Normal operation** - `(Some(prev), Some(curr))` arm computes actual deltas
3. ✅ **Error scenarios** - `(None, None) | (Some(_), None)` arms leave deltas as `None`

The pattern is **already well-structured** using exhaustive `match` rather than `if let`. The task description mentions "current if let pattern structure" but the actual implementation uses `match`, which is more appropriate for this multi-case scenario.

## Summary

- **Function**: `run_governor_cycle` starts at line 3607
- **Snapshot handling**: Lines 3664-3709 use `match` (not `if let`)
- **Delta computation**: Lines 3667-3677 call `calculate_window_pct_delta` (line 864)
- **Pattern**: Exhaustive three-arm match covering all snapshot availability cases
- **State management**: Snapshot shift happens before poll (line 3632), new snapshot stored after successful poll (lines 3655-3660)
