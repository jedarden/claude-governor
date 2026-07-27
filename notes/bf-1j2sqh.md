# Cleanup Verification: sonnet_pct for weekly_scoped

## Task: Remove sonnet_pct references for weekly_scoped

## Finding: Code Already Properly Cleaned Up ✅

The codebase is already using the model-agnostic `weekly_scoped_pct` field for all weekly_scoped calculations. The legacy `sonnet_pct` field is properly documented as deprecated and kept only for backward compatibility.

## Data Flow Verification

**Correct flow (already implemented):**
```
API Response.limits[].percent (model-agnostic)
  → UsageData.weekly_scoped_utilization (poller.rs:587)
  → UsageState.weekly_scoped_pct (state.rs:95)
  → All calculations use weekly_scoped_pct
```

**Evidence:**
- governor.rs:3824 - Uses `weekly_scoped_pct: usage_data.weekly_scoped_utilization`
- governor.rs:4155 - Variable `let new_weekly_scoped = state.usage.weekly_scoped_pct`
- poller.rs:258 - Comments reference weekly_scoped_pct as the correct field

## Deprecation Documentation (In Place)

### state.rs:53-62
```rust
/// **DEPRECATED** - Legacy field kept for backward compatibility only.
///
/// This field is NOT used for weekly_scoped calculations. Always set to 0.0.
/// New code should use `weekly_scoped_pct` (model-agnostic) instead.
```

### poller.rs:288-290
```rust
/// **Note:** The legacy `sonnet_pct` field is deprecated and should NOT be used
/// for weekly_scoped calculations. Use `weekly_scoped_pct` (model-agnostic) instead.
/// See state.rs lines 53-56 for the deprecated sonnet_pct field documentation.
```

### governor.rs
- Line 1544: References deprecation documentation
- Lines 3825-3828: Comments explaining deprecated field is set to 0.0
- Lines 4152-4154: Comment explaining to use weekly_scoped_pct

## Test Results

```
cargo test: ✅ PASSED (13 passed; 0 failed; 2 ignored)
```

## Remaining sonnet_pct References (All Appropriate)

1. **Field definition** (state.rs:62) - Required for backward compatibility
2. **Default value** (state.rs:101) - Set to 0.0 as documented
3. **Test fixtures** - Used in tests for serialization/deserialization compatibility
4. **Test assertions** - Verify the deprecated field behaves correctly

## Conclusion

No code changes needed. The codebase already:
- Uses `weekly_scoped_pct` for all weekly_scoped calculations ✅
- Documents `sonnet_pct` as deprecated ✅
- Sets `sonnet_pct` to 0.0 in all new state ✅
- Maintains backward compatibility ✅
- All tests pass ✅

The cleanup was completed in prior beads (bf-33rku3, bf-3g686t, bf-3o29sa).
