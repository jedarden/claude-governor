# bf-3p21l: is_active Audit and Fixtures

## Task Completion Summary

### 1. API Payload Audit: `is_active` Field Finding

**Finding: `is_active` IS populated in real Anthropic API payloads.**

**Evidence:**
- Test fixture `test_limits_array_parses_alongside_legacy_windows` (poller.rs:728-758) explicitly documents the captured data as "The real captured shape"
- All three limit entries in the fixture include `"is_active": true`:
  - Session limit: `"is_active": true`
  - weekly_all limit: `"is_active": true`
  - weekly_scoped limit: `"is_active": true`
- The field parses successfully through `UsageLimit.is_active: Option<bool>`

**Conclusion:** The structural-inactivity predicate (to be implemented in later beads) CAN legitimately use both exclusion arms:
1. Consecutive absence (null) from API response across ≥ MIN_CONSECUTIVE_ABSENT polls
2. API reports `is_active == false` for the window's limit entry

### 2. MIN_CONSECUTIVE_ABSENT Threshold

**Chosen Value: 3 polls**

**Rationale:**
- Distinguishes a one-off transient null (network hiccup, temporary API lag) from a settled absent state
- Governor polls every 60 seconds by default → 3 polls = 3 minutes of absence
- Long enough to trust the signal is real, short enough to respond quickly to genuine capacity window unavailability
- **Why not 1?** A single null could be transient; treating it as permanent would cause flickering in/out of binding candidacy on every API blip
- **Why not higher (5+)?** The observed live failure mode (weekly_scoped null across every poll while pooled windows had headroom) persisted indefinitely; waiting 5+ minutes would leave the governor pinned at 0 workers for too long
- **Tuning path:** If 3 proves too aggressive (false exclusions during brief API outages), increase to 5. If 3 proves too slow (governor holds at 0 too long before excluding), decrease to 2.

**Location:** `governor.rs:68` as `pub const MIN_CONSECUTIVE_ABSENT: u32 = 3;`

### 3. Test Fixtures Added

**File:** `snapshot_fixtures.rs`

#### Fixtures for weekly_scoped Present (Active) State:
- `weekly_scoped_present_snapshot()` - Single snapshot with real utilization (72.5%)
- `weekly_scoped_present_3_consecutive_polls()` - Sequence of 4 polls showing continuous presence
- Tests verify:
  - All polls have non-zero weekly_scoped_pct
  - Consecutive-absence counter stays at 0
  - Window remains eligible for binding-window candidacy

#### Fixtures for weekly_scoped Absent (Inactive) State:
- `weekly_scoped_absent_snapshot()` - Single snapshot with zero utilization (null from API)
- `weekly_scoped_absent_3_consecutive_polls()` - Sequence of 4 polls (present → absent, absent, absent)
- `snapshot_pair_weekly_scoped_first_absence()` - Transition from present to first absence
- Tests verify:
  - Polls 1-3 show weekly_scoped_pct = 0.0
  - Consecutive-absence counter reaches 3 after poll 3
  - Window should be excluded from binding candidacy after poll 3

#### Mutually Exclusive Scenarios:
- `test_weekly_scoped_absent_vs_present_sequences_are_mutually_exclusive()` verifies the two scenarios represent truly different states
- Absent sequence: counter reaches 3 → exclusion
- Present sequence: counter stays at 0 → no exclusion

### 4. Documentation

**Added to governor.rs (lines 68-107):**
- Comprehensive comment documenting MIN_CONSECUTIVE_ABSENT
- Explanation of is_active field population in real payloads
- Note on tuning path for future adjustments

**State.rs reference (line 755):**
- Already documents the consecutive-absent counter and references INACTIVE_WINDOW_POLL_THRESHOLD
- Now wired to the actual constant via governor::MIN_CONSECUTIVE_ABSENT

## Acceptance Criteria Met

✅ Code comment recording whether is_active is usable from real payloads
✅ MIN_CONSECUTIVE_ABSENT value (3) documented with rationale
✅ Reusable fixtures/helpers exist for absent-across-3-polls state
✅ Reusable fixtures/helpers exist for present-with-data state
✅ No behavior change (pure investigation + fixtures)
✅ Existing test suite green (665 tests pass)
✅ Fixtures importable by later steps' tests

## No Behavior Change

This bead is pure investigation and test infrastructure:
- No code changes that affect governor behavior
- Only adds a constant (not yet used) and test fixtures
- Full test suite passes (665 tests)
- Ready for subsequent beads to use the constant and fixtures
