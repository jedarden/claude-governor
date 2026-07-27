# Bead bf-3g2dtr: Tests for model-specific sonnet_pct behavior

## Task Verification

The task requirements were to add tests for model-specific sonnet_pct behavior. Upon investigation, comprehensive tests already exist in the `sonnet_pct_tests` module at line 8946 in `src/governor.rs`.

## Existing Test Coverage

All required test scenarios are already implemented and passing:

### 1. Test: Sonnet model sets sonnet_pct correctly
- **Test**: `test_sonnet_pct_when_model_is_sonnet`
- **Coverage**: Verifies that when `weekly_scoped_model` is "Sonnet", `sonnet_pct` equals `weekly_scoped_utilization`
- **Status**: ✅ PASSING

### 2. Test: Opus model clears sonnet_pct
- **Test**: `test_sonnet_pct_when_model_is_opus`
- **Coverage**: Verifies that when `weekly_scoped_model` is "Opus", `sonnet_pct` is 0.0 (not the Opus utilization)
- **Status**: ✅ PASSING

### 3. Test: None model clears sonnet_pct
- **Test**: `test_sonnet_pct_when_model_is_none`
- **Coverage**: Verifies that when `weekly_scoped_model` is None, `sonnet_pct` is 0.0
- **Status**: ✅ PASSING

### 4. Test: Fable model clears sonnet_pct
- **Test**: `test_sonnet_pct_when_model_is_fable`
- **Coverage**: Verifies that when `weekly_scoped_model` is "Fable", `sonnet_pct` is 0.0
- **Status**: ✅ PASSING

### 5. Test: Rotation from Sonnet to Opus
- **Test**: `test_sonnet_pct_rotation_from_sonnet_to_opus`
- **Coverage**: Verifies that rotation from Sonnet to Opus correctly clears sonnet_pct to 0.0
- **Status**: ✅ PASSING

### 6. Bonus: Case-insensitive model matching
- **Test**: `test_sonnet_pct_case_insensitive`
- **Coverage**: Verifies case-insensitive matching for model names (e.g., "sonnet", "Sonnet", "SONNET")
- **Status**: ✅ PASSING

## Test Results

All 6 sonnet_pct tests pass:
```
test governor::sonnet_pct_tests::test_sonnet_pct_case_insensitive ... ok
test governor::sonnet_pct_tests::test_sonnet_pct_rotation_from_sonnet_to_opus ... ok
test governor::sonnet_pct_tests::test_sonnet_pct_when_model_is_none ... ok
test governor::sonnet_pct_tests::test_sonnet_pct_when_model_is_opus ... ok
test governor::sonnet_pct_tests::test_sonnet_pct_when_model_is_fable ... ok
test governor::sonnet_pct_tests::test_sonnet_pct_when_model_is_sonnet ... ok
```

Full test suite: 691 tests passed, 0 failed

## Acceptance Criteria Met

- ✅ All test cases pass
- ✅ Tests cover both Sonnet and non-Sonnet weekly_scoped models (Sonnet, Opus, None, Fable)
- ✅ Tests verify the rotation scenario
- ✅ cargo test passes (691 tests passed, 0 failed)

## Conclusion

No new tests were needed as comprehensive coverage already exists. All requirements from the task scope are satisfied by the existing test suite.
