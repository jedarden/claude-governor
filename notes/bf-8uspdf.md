# Bead bf-8uspdf: Sonnet_pct Hard-coding for weekly_scoped

## Bug Location

**File**: `src/governor.rs`  
**Line**: 3818 (current code after fix)  
**Context**: Usage state update in `run_governor_cycle()`

## The Problem (Historical)

**Before fix (commit 082e400)**: The legacy `sonnet_pct` field was **hard-coded** to always use `weekly_scoped_utilization`, regardless of which model the `weekly_scoped` window was actually tracking.

```rust
// OLD CODE (BUGGY):
state.usage = state::UsageState {
    weekly_scoped_pct: usage_data.weekly_scoped_utilization,
    sonnet_pct: usage_data.weekly_scoped_utilization, // ❌ WRONG - always uses weekly_scoped value
    // ...
};
```

**Why this was wrong**:
- The `weekly_scoped` window tracks different models over time (Sonnet, Opus, Fable, etc.)
- When the model rotates to Opus/Fable, `sonnet_pct` was still being set to the `weekly_scoped_utilization` value
- This made the legacy `sonnet_pct` field reflect non-Sonnet utilization, breaking its semantic meaning
- The model-agnostic field `weekly_scoped_pct` should be used instead for all weekly_scoped calculations

## The Fix (Commit 082e400)

**Commit**: `082e400 fix(bf-5zk558): Make sonnet_pct model-specific`

**Fixed code**:
```rust
// NEW CODE (CORRECT):
state.usage = state::UsageState {
    weekly_scoped_pct: usage_data.weekly_scoped_utilization,
    // Only set sonnet_pct when weekly_scoped is actually tracking Sonnet;
    // otherwise set to 0.0 since the legacy field should not reflect other models
    sonnet_pct: if usage_data.is_weekly_scoped_sonnet() {
        usage_data.weekly_scoped_utilization
    } else {
        0.0
    },
    // ...
};
```

**How it works**:
- Uses the `is_weekly_scoped_sonnet()` helper to check if the weekly_scoped model is Sonnet
- If yes: sets `sonnet_pct` to `weekly_scoped_utilization` (legacy behavior maintained)
- If no: sets `sonnet_pct` to `0.0` (field correctly reflects "no Sonnet utilization")

## Current State (Post-Fix)

The code at line 3818 is now **correct**. The fix ensures:
1. `sonnet_pct` only reflects Sonnet utilization when weekly_scoped is actually tracking Sonnet
2. For other models (Opus, Fable), `sonnet_pct` is set to 0.0
3. All new code should use `weekly_scoped_pct` (model-agnostic) instead of `sonnet_pct`

## Documentation Added

Added comprehensive comment block at line 3816-3829 explaining:
- Historical bug context (bf-5zk558, commit 082e400)
- Why the hard-coding was problematic
- How the conditional fix works
- Reference to state.rs documentation for the deprecated field

## Related Documentation

- **state.rs lines 53-56**: Documents the deprecated `sonnet_pct` legacy field
- **state.rs lines 72-77**: Documents the model-agnostic `weekly_scoped_pct` field
- **governor.rs line 4145-4149**: Correct usage of `weekly_scoped_pct` in delta calculations
- **Test coverage**: `src/governor.rs mod sonnet_pct_tests` (lines 8976-9367)

## Verification

The fix is verified by comprehensive test suite:
- `test_sonnet_pct_when_model_is_sonnet()`: Verifies sonnet_pct equals weekly_scoped_utilization when model is Sonnet
- `test_sonnet_pct_when_model_is_opus()`: Verifies sonnet_pct is 0.0 (not 72.5) when model is Opus
- `test_sonnet_pct_when_model_is_none()`: Verifies sonnet_pct is 0.0 when model is None
- `test_sonnet_pct_when_model_is_fable()`: Verifies sonnet_pct is 0.0 when model is Fable
- `test_sonnet_pct_rotation_from_sonnet_to_opus()`: Verifies sonnet_pct clears on model rotation
- `test_sonnet_pct_case_insensitive()`: Verifies case-insensitive model matching

## What Needs to Change

**Nothing** - the bug is already fixed (commit 082e400). This bead only:
1. ✅ Located the specific line where the bug existed (line 3818)
2. ✅ Documented the historical problem and fix
3. ✅ Added inline code comments explaining the bugfix
4. ✅ Created comprehensive notes for posterity

The legacy `sonnet_pct` field is now correctly model-specific, and all new code should use the model-agnostic `weekly_scoped_pct` field instead.
