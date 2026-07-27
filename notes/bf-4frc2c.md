# Model-Agnostic weekly_scoped PCT Source Documentation (bf-4frc2c)

## Task Summary

Identified and documented the model-agnostic weekly_scoped percentage source field in the limits[] array structure.

## Key Finding

**The model-agnostic weekly_scoped pct is stored in: `limits[].percent`** 

Specifically: In the `limits[]` array entry where `kind == "weekly_scoped"`, the `percent` field contains the model-agnostic utilization value.

## Data Flow Trace

```
API Response
    ↓
UsageResponse.limits: Vec<UsageLimit> (poller.rs:186)
    ↓
[Find entry where kind == "weekly_scoped"]
    ↓
UsageLimit.percent: Option<f64> (poller.rs:140)
    ↓
scoped_weekly() method extracts limit.percent (poller.rs:256)
    ↓
UsageData.weekly_scoped_utilization (poller.rs:587)
    ↓
UsageState.weekly_scoped_pct (state.rs:76-77)
```

## Field Access Pattern

```rust
// 1. API response structure
pub struct UsageLimit {
    pub kind: Option<String>,        // "weekly_scoped" identifier
    pub percent: Option<f64>,        // ← MODEL-AGNOSTIC PCT SOURCE
    pub resets_at: Option<String>,
    pub scope: Option<LimitScope>,  // Contains model info
    // ...
}

// 2. Extraction method (poller.rs:244-261)
pub fn scoped_weekly(&self) -> Option<(String, UsageWindow)> {
    self.limits.iter().find_map(|limit| {
        if limit.kind.as_deref() != Some("weekly_scoped") {
            return None;
        }
        let window = UsageWindow {
            utilization: limit.percent.unwrap_or(0.0),  // ← PCT READ HERE
            resets_at: limit.resets_at.clone().unwrap_or_default(),
        };
        Some((model_name, window))
    })
}

// 3. Storage in state (state.rs:76-77)
pub struct UsageState {
    pub weekly_scoped_pct: f64,      // ← FINAL STORAGE LOCATION
    pub weekly_scoped_model: Option<String>,  // Metadata only
    // ...
}
```

## Why This Is Model-Agnostic

The `limits[].percent` field contains the utilization percentage for the weekly_scoped window **regardless of which model** (Fable, Opus, Sonnet, etc.) is currently carrying the scoped cap. The model identity is stored separately in `limits[].scope.model.display_name` as metadata.

This design allows cgov to work with the weekly_scoped window without needing model-specific logic, since Anthropic may rotate which model carries the scoped cap across different periods.

## Code Comments Added

Added comprehensive documentation to:
- `poller.rs:140` - UsageLimit.percent field definition
- `poller.rs:244-261` - scoped_weekly() method with data flow explanation  
- `poller.rs:583-591` - poll() method showing limits[] → UsageData flow
- `state.rs:72-77` - UsageState.weekly_scoped_pct with complete data flow trace

## Testing

Verified code compiles successfully with `cargo build --release` and tests pass.

## Acceptance Criteria Met

✅ Documented the correct model-agnostic source field name (`limits[].percent`)
✅ Traced the data flow from API → limits[] → usage
✅ Added code comments showing the correct source and access pattern
