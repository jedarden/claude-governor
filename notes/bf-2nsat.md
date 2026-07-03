# Bead bf-2nsat: Snapshot Handling Analysis

## Overview
Examination of `run_governor_cycle` snapshot handling (lines 1978-2040 in `src/governor.rs`).

## Snapshot Shift Logic (Line 1980)

```rust
// 1a-pre. Shift snapshot state before poll: current becomes previous.
// On first poll, current_api_snapshot is None, so previous becomes None too.
state.previous_api_snapshot = state.current_api_snapshot.take();
```

- Before each poll, `current_api_snapshot` is shifted to `previous_api_snapshot` using `take()`
- `take()` moves the value out, leaving `current_api_snapshot` as `None`
- On first poll: `current_api_snapshot` is `None` → `previous_api_snapshot` becomes `None`

## Delta Computation Logic (Lines 2011-2040)

```rust
if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot) {
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

    log::info!(...);
} else {
    // First poll: prev_snapshot is None, cannot compute delta
    // Ensure delta fields remain at default (0.0) - no update needed
    log::debug!(...);
}
```

### Pattern Matching
- Only computes deltas when **both** `previous_api_snapshot` AND `current_api_snapshot` are `Some`
- Creates `WindowPctSnapshot` structs from the snapshot data
- Calls `calculate_window_pct_delta(&prev_pct, &curr_pct)`
- Stores results as `Some(delta)` in the state fields

### Delta Calculation Function
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
- Simple subtraction: `current - previous` for each window

## First Poll Behavior

When `previous_api_snapshot` is `None` (first poll):
1. The `if let (Some(prev), Some(curr))` condition fails
2. Falls through to the `else` block
3. Logs a debug message: "first poll detected (no previous snapshot), skipping delta computation"
4. **Leaves delta fields unchanged** (stays at default value)

## State Initialization

In `src/state.rs`, the delta fields are defined as:

```rust
/// 5-hour window percentage delta (current - previous).
#[serde(default)]
pub p5h_delta: Option<f64>,

/// 7-day window percentage delta (current - previous).
#[serde(default)]
pub p7d_delta: Option<f64>,

/// 7-day Sonnet window percentage delta (current - previous).
#[serde(default)]
pub p7ds_delta: Option<f64>,
```

Default implementation sets all to `None`:

```rust
impl Default for GovernorState {
    fn default() -> Self {
        Self {
            // ... other fields ...
            p5h_delta: None,
            p7d_delta: None,
            p7ds_delta: None,
        }
    }
}
```

## Notable Comment Discrepancy

Line 2036 comment says:
> "Ensure delta fields remain at default (0.0) - no update needed"

But the actual default is `None`, not `0.0`. The comment is misleading - on first poll, the delta fields remain `None`, not `0.0`. Only after the **second** poll (when both prev and curr exist) do they get set to `Some(f64_value)`.

## PrevUsageSnapshot Type

From `src/state.rs`:

```rust
/// Persisted in state so that the governor can compute pct/hr from consecutive
/// API readings even across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrevUsageSnapshot {
    /// When this snapshot was taken (wall-clock time of the API poll)
    pub taken_at: DateTime<Utc>,
    pub five_hour_pct: f64,
    pub seven_day_pct: f64,
    pub seven_day_sonnet_pct: f64,
}
```

## Lifecycle Summary

| Phase | previous_api_snapshot | current_api_snapshot | Delta Fields | Notes |
|-------|----------------------|----------------------|--------------|-------|
| Initial state | `None` | `None` | `None`, `None`, `None` | Before first poll |
| After shift (1st poll) | `None` | `None` | `None`, `None`, `None` | take() on None = None |
| After 1st successful poll | `None` | `Some(snapshot1)` | `None`, `None`, `None` | Delta computation skipped |
| After shift (2nd poll) | `Some(snapshot1)` | `None` | `None`, `None`, `None` | Deltas still from prior cycle |
| After 2nd successful poll | `Some(snapshot1)` | `Some(snapshot2)` | `Some(d5h)`, `Some(d7d)`, `Some(d7ds)` | First delta computed |
| After shift (3rd poll) | `Some(snapshot2)` | `None` | Preserved | Deltas persist |
| After 3rd successful poll | `Some(snapshot2)` | `Some(snapshot3)` | Updated | New deltas computed |

**Key insight:** Delta fields persist in state between cycles. They are only updated when both snapshots exist (after the second poll and onward).

## Acceptance Criteria Status

- [x] Understand the snapshot shift logic (line 1980)
- [x] Understand current pattern matching for delta computation (lines 2011-2040)
- [x] Identify what happens on first poll when prev_snapshot is None
- [x] Understand state initialization of p5h_delta, p7d_delta, p7ds_delta fields
