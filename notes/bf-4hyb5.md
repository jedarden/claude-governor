# Bead bf-4hyb5: weekly_scoped_model persistence field

## Task Verification

The `weekly_scoped_model` field is already fully implemented in the codebase. All acceptance criteria are met:

### ✅ Acceptance Criteria Met

1. **weekly_scoped_model field exists in persisted state**
   - Location: `src/state.rs:71` in `UsageState` struct
   - Defined as: `pub weekly_scoped_model: Option<String>`
   - Marked with `#[serde(default)]` for persistence

2. **Field is initialized to the current scoped model name on first load**
   - Location: `src/poller.rs:572` and `src/poller.rs:724`
   - Initialized via: `data.weekly_scoped_model = data.scoped_weekly().map(|(model, _)| model);`
   - Extracts model name (e.g., "Fable", "Opus") from the scoped weekly cap

3. **Field is included in state serialization**
   - `UsageState` has `#[derive(Serialize, Deserialize)]`
   - Field is part of the persisted `governor-state.json`
   - Round-trip tested in `test_usage_state_weekly_scoped_model_null_roundtrip`

4. **cargo test passes**
   - All 61 state tests pass
   - Specific tests for this field:
     - `test_usage_state_weekly_scoped_model_null_roundtrip`
     - `test_weekly_scoped_display_label`
     - `test_weekly_scoped_model_carries_resolved_display_name`
     - `test_weekly_scoped_model_none_when_no_scoped_cap`

### Implementation Details

The field serves as the foundation for detecting model rotations:

- **Persistence**: Stores which model currently carries the weekly_scoped cap
- **Rotation Detection**: `reset_weekly_scoped_on_model_change()` in `state.rs` detects when the model identity changes and resets EMA samples
- **Display**: `weekly_scoped_display_label()` uses this field for human-readable labels in logs/status

### Test Coverage

```bash
cargo test --lib state::null_roundtrip_test
# test_usage_state_weekly_scoped_model_null_roundtrip ... ok
# test_weekly_scoped_display_label ... ok
```

## Conclusion

Task complete - no changes needed. The field was implemented in a prior commit and all functionality is working correctly.
