# Weekly Scoped Fix Verification - bf-1hqw9s

## Test Execution Summary

**Date:** 2026-07-27

### Overall Results
- **Total tests run:** 692 tests
- **Passed:** 692 (100%)
- **Failed:** 0
- **Ignored:** 0

### Weekly Scoped Tests (25 tests)
All weekly_scoped related tests passed successfully:

#### Model Rotation Tests (6 tests)
- ✅ `reset_weekly_scoped_on_model_change_handles_none_to_none`
- ✅ `reset_weekly_scoped_on_model_change_detects_fable_to_opus_rotation`
- ✅ `reset_weekly_scoped_on_model_change_handles_none_to_some`
- ✅ `reset_weekly_scoped_on_model_change_handles_some_to_none`
- ✅ `reset_weekly_scoped_on_model_change_realistic_rotation_scenario`
- ✅ `reset_weekly_scoped_on_model_change_returns_false_when_same_model`

#### Model-Agnostic Behavior Tests (7 tests)
- ✅ `test_is_weekly_scoped_sonnet_false_for_other_models`
- ✅ `test_is_weekly_scoped_sonnet_false_when_none`
- ✅ `test_is_weekly_scoped_sonnet_true_for_sonnet`
- ✅ `test_is_weekly_scoped_sonnet_true_for_sonnet_case_insensitive`
- ✅ `test_weekly_scoped_model_carries_resolved_display_name`
- ✅ `test_weekly_scoped_model_none_when_no_scoped_cap`
- ✅ `test_weekly_scoped_display_label`

#### Burn Rate Integration Tests (2 tests)
- ✅ `absent_weekly_scoped_is_immediately_non_binding`
- ✅ `weekly_scoped_becomes_binding_when_most_constrained`

#### Weekly Scoped State Tests (10 tests)
All snapshot and state transition tests passed, including:
- Consecutive poll tracking (present/absent)
- Zero utilization during absence
- Transition documentation
- Null roundtrip serialization

### EMA & Burn Rate Tests (135 tests)
All burn rate and EMA calculation tests passed with no regressions:
- ✅ EMA computation tests
- ✅ Validation tests (ratio, samples, noise handling)
- ✅ Window cost and reset boundary tests
- ✅ Empirical promo ratio tests

### Verification Results
1. **No regressions detected** in EMA calculations or burn rate logic
2. **Model-agnostic weekly_scoped_pct** works correctly for all models (Sonnet, Opus, Fable, None)
3. **Model rotation** correctly resets weekly_scoped state when models change
4. **All existing weekly_scoped tests** continue to pass
5. **No warnings or errors** in test output

## Conclusion
The weekly_scoped fix is working correctly. The model-agnostic behavior properly handles:
- Sonnet model (sets weekly_scoped_model)
- Other models (Opus, Fable, etc. - returns None)
- Model rotation (resets state when model changes)
- Null serialization (roundtrips correctly)

All acceptance criteria for the bead have been met.
