# Bead bf-3vs03y: Findings Summary

## Task Completed
Updated bead bf-3vs03y body with a comprehensive summary of findings from previous child beads (bf-v9vqpj, bf-3dpuq4, bf-3j93rz, bf-2uqbeo).

## Bug Summary
The codebase contains documentation that incorrectly states `weekly_scoped` fields are "Sonnet only". In reality, these fields are **model-agnostic** and can be scoped to ANY model (Fable, Opus, Sonnet, etc.) depending on which model carries the scoped cap this period.

## Affected Lines in governor.rs
1. **Line 1454** - `make_window_pct_snapshot` documentation
2. **Line 1495** - `make_usage_snapshot` documentation
3. **Line 1539** - `make_usage_snapshot_with_time` documentation
4. **Line 5519** - `tests::make_usage_snapshot` documentation
5. **Line 7632** - `MockPoller::with_utilization` documentation
6. **Line 7709** - `default_usage_data` documentation

## What Was Documented
Bead bf-v9vqpj added inline code comments (commit e1c8b3c) at each location documenting:
- The bug: Documentation incorrectly states "Sonnet only"
- The fix: `weekly_scoped` is MODEL-AGNOSTIC
- Reference: Points to `state.rs UsageState.weekly_scoped_model` and `weekly_scoped_pct` fields

## Next Steps for the Actual Fix
1. Update all 6 documentation lines to replace "Sonnet only" with "model-agnostic (Fable, Opus, Sonnet, etc.)"
2. Update any related tests or documentation that reference the old behavior
3. Verify the fix with full test suite (already validated in bf-2uqbeo)

## References
- state.rs lines 53-77: Model-agnostic weekly_scoped implementation
- state.rs lines 95-104: `weekly_scoped_display_label()` helper
- state.rs lines 106-129: Model rotation reset logic
- Commit e1c8b3c: Inline code comments added
