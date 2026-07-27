# Task bf-3dpuq4: Identify weekly_scoped context usages

## Summary
This document identifies which `sonnet_pct` usages are related to `weekly_scoped` code paths in the claude-governor codebase.

## Key Finding: `sonnet_pct` is a legacy field tied to `weekly_scoped`

The `sonnet_pct` field is **completely dependent** on the `weekly_scoped` utilization path. It is set conditionally based on whether the `weekly_scoped` window is currently tracking a Sonnet model.

## Core weekly_scoped-related sonnet_pct usages

### 1. Main assignment logic (governor.rs:3797-3803)

**Location**: `src/governor.rs:3797-3803`

```rust
// Only set sonnet_pct when weekly_scoped is actually tracking Sonnet;
// otherwise set to 0.0 since the legacy field should not reflect other models
sonnet_pct: if usage_data.is_weekly_scoped_sonnet() {
    usage_data.weekly_scoped_utilization
} else {
    0.0
},
```

**Context**: This is the **primary and most critical** usage where `sonnet_pct` is assigned from `weekly_scoped_utilization`. The conditional logic checks `is_weekly_scoped_sonnet()` which returns true only when `weekly_scoped_model` indicates a Sonnet model.

### 2. Documentation comment (governor.rs:4127)

**Location**: `src/governor.rs:4127`

```rust
// NOTE: weekly_scoped_pct is the model-agnostic field for weekly_scoped utilization.
// The legacy sonnet_pct field is kept for backward compatibility but should not be used
// in new code. When model identity changes, reset logic above ensures stale samples
// are cleared.
```

**Context**: Explains that `sonnet_pct` is a legacy field maintained for backward compatibility, while `weekly_scoped_pct` is the model-agnostic field.

### 3. Test module: sonnet_pct_tests (governor.rs:8942-9332)

**Location**: `src/governor.rs:8946-9332`

The entire `sonnet_pct_tests` module validates the `sonnet_pct` ↔ `weekly_scoped` relationship:

#### Test 1: `test_sonnet_pct_when_model_is_sonnet` (lines 8951-9002)
- **Purpose**: Verifies `sonnet_pct` equals `weekly_scoped_utilization` when `weekly_scoped_model` is "Sonnet"
- **Key assertion**: `sonnet_pct == weekly_scoped_utilization == 45.0`

#### Test 2: `test_sonnet_pct_when_model_is_opus` (lines 9006-9057)
- **Purpose**: Verifies `sonnet_pct` is 0.0 when `weekly_scoped_model` is "Opus" (NOT `weekly_scoped_utilization` which is 72.5)
- **Key assertion**: `sonnet_pct == 0.0` (not 72.5)

#### Test 3: `test_sonnet_pct_when_model_is_none` (lines 9061-9105)
- **Purpose**: Verifies `sonnet_pct` is 0.0 when `weekly_scoped_model` is None
- **Key assertion**: `sonnet_pct == 0.0`

#### Test 4: `test_sonnet_pct_when_model_is_fable` (lines 9109-9154)
- **Purpose**: Verifies `sonnet_pct` is 0.0 when `weekly_scoped_model` is "Fable"
- **Key assertion**: `sonnet_pct == 0.0`

#### Test 5: `test_sonnet_pct_rotation_from_sonnet_to_opus` (lines 9158-9257)
- **Purpose**: Verifies that when `weekly_scoped_model` rotates from Sonnet to Opus, `sonnet_pct` is cleared to 0.0
- **Key assertion**: `sonnet_pct` goes from 68.0 (Sonnet) → 0.0 (Opus)

#### Test 6: `test_sonnet_pct_case_insensitive` (lines 9261-9332)
- **Purpose**: Verifies case-insensitive matching of model names (e.g., "sonnet", "Sonnet", "SONNET")
- **Key assertion**: All Sonnet variants produce the same `sonnet_pct` value

## Supporting code paths

### Helper function: `is_weekly_scoped_sonnet()` (poller.rs:269-271)

**Location**: `src/poller.rs:269-271`

```rust
pub fn is_weekly_scoped_sonnet(&self) -> bool {
    match self.weekly_scoped_model.as_deref() {
        Some(model) if model.eq_ignore_ascii_case("sonnet") => true,
        _ => false,
    }
}
```

**Context**: This helper function is used by the conditional logic that determines whether to set `sonnet_pct`.

## Data flow diagram

```
┌─────────────────────────────┐
│ UsageData (from API poll)   │
│ - weekly_scoped_utilization│
│ - weekly_scoped_model       │
└──────────────┬──────────────┘
               │
               ▼
       ┌───────────────────┐
       │ is_weekly_scoped_  │
       │ sonnet()?          │
       └─────────┬─────────┘
                 │
         ┌───────┴───────┐
         │               │
      true            false
         │               │
         ▼               ▼
┌────────────────┐  ┌─────────┐
│ sonnet_pct =   │  │sonnet_% │
│ weekly_scoped_ │  │ = 0.0   │
│ utilization    │  └─────────┘
└────────────────┘
```

## Key code paths summary

| File | Lines | Description |
|------|-------|-------------|
| `governor.rs` | 3797-3803 | **Core logic**: Conditional assignment of `sonnet_pct` from `weekly_scoped_utilization` |
| `governor.rs` | 4127 | Documentation comment explaining legacy nature |
| `governor.rs` | 8946-9332 | **Test suite**: 6 tests validating `sonnet_pct` behavior across model changes |
| `poller.rs` | 269-271 | Helper function `is_weekly_scoped_sonnet()` |

## Conclusion

**All `sonnet_pct` usages are tied to the `weekly_scoped` code path.** The field is a legacy backward-compatibility field that is:

1. Set from `weekly_scoped_utilization` when `weekly_scoped_model` is Sonnet
2. Set to 0.0 when `weekly_scoped_model` is any other model or None
3. Automatically cleared on model rotation from Sonnet to another model

The relationship is extensively tested and documented in the `sonnet_pct_tests` module.
