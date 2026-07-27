# Binding-Window max_by Selection Analysis

## Task: bf-2zo43m

## Location Found

**File**: `src/governor.rs`
**Lines**: 4861-4869 (max_by selection)

## Current Selection Logic

```rust
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

### How it works

1. **Creates array** of tuples: `(window_name, &forecast_struct)`
2. **Selects max_by** comparing `risk_score` field on WindowForecast structs
3. **Returns** the window name with highest risk_score

### WindowForecast Structure (src/state.rs:326-365)

Key fields relevant to selection:
- `risk_score: f64` - Composite risk score (higher = riskier)
- `binding: bool` - Set after selection (lines 4872-4878)
- Other fields: margin_hrs, cone_ratio, predicted_exhaustion_hours, etc.

## Where to Insert Filter

**Before line 4861**: Add a filter step to exclude structurally inactive windows before `max_by` selection.

Current flow:
```rust
let windows = [
    ("five_hour", &five_hour_forecast),
    ("seven_day", &seven_day_forecast),
    ("weekly_scoped", &weekly_scoped_forecast),
];

// FILTER SHOULD BE INSERTED HERE
let binding_window = windows
    .iter()
    .max_by(...)
```

## is_structurally_inactive Function

**Location**: `src/governor.rs:127-144`

### Function signature
```rust
fn is_structurally_inactive(
    window: &UsageWindow,
    state: &state::GovernorState,
) -> bool
```

### Inactivity conditions (OR logic)
1. **Consecutive absence threshold reached**: Window has been absent (null) from API responses for >= MIN_CONSECUTIVE_ABSENT consecutive polls
2. **API reports is_active == false**: Explicit `is_active: false` in UsageWindow

### Note on is_active field
- `None` or `true` → window is active
- Only explicit `false` → window is structurally inactive

## Challenge for Filter Implementation

The `is_structurally_inactive` function takes a `UsageWindow` reference, but the max_by selection operates on `WindowForecast` structs. Need to either:

1. Map back to UsageWindow data during filter
2. Store is_structurally_inactive state in WindowForecast
3. Pass UsageWindow array alongside forecast array for filter

## Related Tests

**Location**: `src/governor.rs:9651-9957` - Comprehensive unit tests for `is_structurally_inactive`
