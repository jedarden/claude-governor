# Binding-Window Selection Context Documentation

## Overview

This document describes the binding-window selection logic in `governor.rs` (lines 4852-4878), including the current implementation and how to integrate `is_structurally_inactive` filtering.

## Function Context

**Function:** `run_governor_cycle` (line 3781)

**Signature:**
```rust
pub fn run_governor_cycle(
    poller: &mut Poller,
    state_path: &Path,
    dry_run: bool,
    loop_interval: u64,
    hysteresis_band: f64,
    max_up_per_cycle: u32,
    max_down_per_cycle: u32,
    target_ceiling: f64,
    alert_config: &AlertConfig,
    agents: &std::collections::HashMap<String, AgentConfig>,
    pre_scale_minutes: u64,
    promotions: &[Promotion],
    composite_risk_config: &CompositeRiskConfig,
    cone_scaling_config: &ConeScalingConfig,
    pricing_config: &crate::config::GovernorConfig,
) -> anyhow::Result<()>
```

**State availability:** The `state` variable is loaded at line 3802:
```rust
let mut state = state::load_state(state_path)?;
```

This `state` variable is mutable and available throughout the function scope, including at the binding-window selection point (line 4861).

## Current Implementation (Lines 4852-4878)

### Current Code Structure

```rust
// Identify binding window (highest risk_score)
// The risk_score combines margin urgency, duration weight, and volatility (cone_ratio).
// Higher risk_score = more urgent window that should drive scaling decisions.
let windows = [
    ("five_hour", &five_hour_forecast),
    ("seven_day", &seven_day_forecast),
    ("weekly_scoped", &weekly_scoped_forecast),
];

let binding_window = windows
    .iter()
    .max_by(|(_, a), (_, b)| {
        a.risk_score
            .partial_cmp(&b.risk_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
    .map(|(name, _)| name.to_string())
    .unwrap_or_default();
```

### Current Flow

1. **windows array:** Contains tuples of `(&str, &WindowForecast)` for all three windows
2. **max_by:** Selects the window with the highest `risk_score` value
3. **Result:** String name of the binding window (e.g., "five_hour")

## is_structurally_inactive Function (Line 127)

### Function Signature

```rust
fn is_structurally_inactive(
    window: &UsageWindow,
    state: &state::GovernorState,
) -> bool
```

### Purpose

Returns `true` if the window is structurally inactive (should be excluded from binding-window candidacy), `false` otherwise.

### Implementation Details

```rust
fn is_structurally_inactive(
    window: &UsageWindow,
    state: &state::GovernorState,
) -> bool {
    // Condition 1: Consecutive absence threshold reached
    let is_inactive_by_consecutive_absence = state.is_window_consecutively_absent(&window.name);

    // Condition 2: API reports is_active == false
    let is_inactive_by_api = window.is_active == Some(false);

    // The window is structurally inactive if EITHER condition is true.
    is_inactive_by_consecutive_absence || is_inactive_by_api
}
```

### Conditions for Structural Inactivity

A window is considered structurally inactive if **either** condition is true:

1. **Consecutive absence threshold:** The window has been absent (null) from API responses across ≥ `MIN_CONSECUTIVE_ABSENT` consecutive polls
2. **API explicit false:** The API returns `is_active == false` for the window

### UsageWindow Structure (from poller.rs)

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct UsageWindow {
    /// Name of the window (e.g., "five_hour", "seven_day", "weekly_scoped")
    #[serde(default)]
    pub name: String,
    pub utilization: f64,
    #[serde(rename = "resets_at")]
    pub resets_at: String,
    /// Whether this window's limit is currently active.
    #[serde(default)]
    pub is_active: Option<bool>,
}
```

**Note:** The `is_active` field is optional. When `None` or `true`, the window is considered active. Only an explicit `false` marks the window as structurally inactive.

## Integration Point: Where to Insert Filter

### Current Code Location (Lines 4861-4869)

```rust
let binding_window = windows
    .iter()
    // <!-- FILTER INSERTION POINT HERE -->
    .max_by(|(_, a), (_, b)| {
        a.risk_score
            .partial_cmp(&b.risk_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
    .map(|(name, _)| name.to_string())
    .unwrap_or_default();
```

### Proposed Integration

To filter out structurally inactive windows before binding-window selection, insert a `.filter()` call **before** `.max_by()`:

```rust
let binding_window = windows
    .iter()
    .filter(|(_, forecast)| {
        // Access the UsageWindow via state.usage.<window>_window
        // TODO: Need to map forecast name to the correct UsageWindow
        // Implementation depends on how UsageWindow objects are accessible
        false // Placeholder - actual implementation needed
    })
    .max_by(|(_, a), (_, b)| {
        a.risk_score
            .partial_cmp(&b.risk_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
    .map(|(name, _)| name.to_string())
    .unwrap_or_default();
```

## Expected Flow After Integration

```
1. windows array (3 windows)
   ↓
2. filter(is_structurally_inactive) → excludes inactive windows
   ↓
3. max_by(risk_score) → selects highest-risk window from remaining
   ↓
4. binding_window name (e.g., "five_hour")
```

## Key Considerations for Implementation

1. **Access to UsageWindow objects:** The filter needs access to the raw `UsageWindow` objects to check `is_active` and consecutive absence status. These are currently accessible via `state.usage.five_hour_window`, `state.usage.sonnet_window`, etc.

2. **Window name mapping:** The filter closure receives `(&str, &WindowForecast)` tuples. The string slice ("five_hour", "seven_day", "weekly_scoped") must be mapped to the correct `UsageWindow` field in `state.usage`.

3. **All windows filtered:** If all three windows are filtered out as inactive, the current implementation returns `String::default()` (empty string). This edge case should be handled.

4. **State mutability:** The `state` variable is available as `&mut GovernorState` in scope, so `is_structurally_inactive` can be called with `&state` reference.

## Related Test Coverage

Unit tests for `is_structurally_inactive` are located in the `is_structurally_inactive_tests` module starting at line 9651, covering various scenarios:
- Active windows with `is_active: Some(true)` or `None`
- Inactive windows with `is_active: Some(false)`
- Consecutive absence threshold behavior

## References

- Binding-window selection: `src/governor.rs:4852-4878`
- `is_structurally_inactive` function: `src/governor.rs:127-144`
- `UsageWindow` struct: `src/poller.rs:103-117`
- Unit tests: `src/governor.rs:9651-9950`
