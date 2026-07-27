# All `sonnet_pct` Usages in governor.rs

## Summary
Found **64 occurrences** of `sonnet_pct` across 3 main locations:
1. **Production code** (lines 3797-3803, 4127) - 2 occurrences
2. **Test module declaration** (line 8946) - 1 occurrence  
3. **Test functions** (lines 8949-9333) - 61 occurrences

---

## 1. Production Code Usage

### Line 3797-3803: Conditional assignment logic
```rust
// Only set sonnet_pct when weekly_scoped is actually tracking Sonnet;
// otherwise set to 0.0 since the legacy field should not reflect other models
sonnet_pct: if usage_data.is_weekly_scoped_sonnet() {
    usage_data.weekly_scoped_utilization
} else {
    0.0
},
```

**Context**: This is in the main polling update logic where `state.usage` is being updated from `usage_data`. The code conditionally sets `sonnet_pct` to equal `weekly_scoped_utilization` only when the weekly_scoped model is Sonnet; otherwise it's set to 0.0.

### Line 4127: Legacy deprecation comment
```rust
// NOTE: weekly_scoped_pct is the model-agnostic field for weekly_scoped utilization.
// The legacy sonnet_pct field is kept for backward compatibility but should not be used
// in new code. When model identity changes, reset logic above ensures stale samples
// are cleared.
```

**Context**: This is a comment explaining that `sonnet_pct` is legacy code kept for backward compatibility, and new code should use the model-agnostic `weekly_scoped_pct` field instead.

---

## 2. Test Module Declaration

### Line 8946: Test module name
```rust
mod sonnet_pct_tests {
```

**Context**: Declares a test module specifically for testing `sonnet_pct` behavior.

---

## 3. Test Functions

The test module contains 7 comprehensive test functions that verify `sonnet_pct` behavior:

### Test 1: `test_sonnet_pct_when_model_is_sonnet` (lines 8951-9002)
**Purpose**: Verifies `sonnet_pct` equals `weekly_scoped_utilization` when the model is Sonnet
**Key assertions**:
- Line 8996-8997: `sonnet_pct` should equal `weekly_scoped_utilization`
- Line 9000-9001: `sonnet_pct` should be 45.0 (the actual utilization value)

### Test 2: `test_sonnet_pct_when_model_is_opus` (lines 9006-9057)
**Purpose**: Verifies `sonnet_pct` is 0.0 when the model is Opus (not the weekly_scoped_utilization value of 72.5)
**Key assertions**:
- Line 9048-9049: `sonnet_pct` should be 0.0
- Line 9054-9055: `sonnet_pct` should NOT equal `weekly_scoped_utilization`

### Test 3: `test_sonnet_pct_when_model_is_none` (lines 9061-9104)
**Purpose**: Verifies `sonnet_pct` is 0.0 when there is no weekly_scoped_model
**Key assertions**:
- Line 9102-9103: `sonnet_pct` should be 0.0

### Test 4: `test_sonnet_pct_when_model_is_fable` (lines 9109-9153)
**Purpose**: Verifies `sonnet_pct` is 0.0 when the model is Fable
**Key assertions**:
- Line 9151-9152: `sonnet_pct` should be 0.0

### Test 5: `test_sonnet_pct_rotation_from_sonnet_to_opus` (lines 9158-9257)
**Purpose**: Verifies that when the weekly_scoped_model rotates from Sonnet to Opus, `sonnet_pct` is cleared to 0.0
**Key assertions**:
- Line 9189-9190: Initial `sonnet_pct` should be 68.0 with Sonnet model
- Line 9242-9243: After rotation to Opus, `sonnet_pct` should be 0.0
- Line 9254-9255: `sonnet_pct` should NOT equal the new `weekly_scoped_utilization` (75.0)

### Test 6: `test_sonnet_pct_case_insensitive` (lines 9261-9333)
**Purpose**: Verifies case-insensitive matching for model names ("sonnet", "Sonnet", "SONNET")
**Key assertions**:
- Line 9326-9327: Case should not affect `sonnet_pct` value
- Line 9330-9331: `sonnet_pct` should be 42.5 regardless of case

---

## Pattern Summary

All `sonnet_pct` usage follows this pattern:

```rust
sonnet_pct: if usage_data.is_weekly_scoped_sonnet() {
    usage_data.weekly_scoped_utilization
} else {
    0.0
},
```

This conditional expression appears:
- Once in production code (line 3799)
- 6 times in test code (lines 8981, 9033, 9087, 9136, 9174, 9215, 9297, 9312)

The field is set to the weekly_scoped_utilization value **only** when `is_weekly_scoped_sonnet()` returns true (i.e., the weekly_scoped model is Sonnet, case-insensitive). Otherwise, it's set to 0.0.
