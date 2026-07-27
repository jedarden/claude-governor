# bf-3p21l: Audit `is_active` and Choose MIN_CONSECUTIVE_ABSENT

## Investigation Complete

This task documented design inputs for the split-off bead bf-5x0lf (umbrella). All requirements were already satisfied by existing code from prior beads (bf-oeotj, bf-3a3x7).

## Finding 1: `is_active` Field Population

**✅ CONFIRMED: `is_active` IS populated in real Anthropic API payloads**

### Evidence

1. **Code Definition** (`src/poller.rs:148`)
   ```rust
   pub struct UsageLimit {
       // ...
       pub is_active: Option<bool>,
   }
   ```

2. **Test Fixture with Real Captured Shape** (`src/poller.rs:729-758`)
   ```rust
   #[test]
   fn test_limits_array_parses_alongside_legacy_windows() {
       // The real captured shape: legacy top-level windows coexist with
       // the generic limits[] array. Both must parse from a single response.
       let json = r#"{
           "limits": [
               {"kind": "session", ..., "is_active": true},
               {"kind": "weekly_all", ..., "is_active": true},
               {"kind": "weekly_scoped", ..., "is_active": true}
           ]
       }"#;
   ```

### Conclusion

The structural-inactivity predicate CAN legitimately use both exclusion arms:
1. Consecutive absence (null) across ≥ MIN_CONSECUTIVE_ABSENT polls
2. API reports `is_active == false` for the window's limit entry

---

## Finding 2: MIN_CONSECUTIVE_ABSENT Threshold

**✅ Already chosen and documented: Value is 3**

### Location

`src/governor.rs:102`:
```rust
pub const MIN_CONSECUTIVE_ABSENT: u32 = 3;
```

### Rationale (from governor.rs:64-84)

- **Distinguishes transient vs settled**: 3 consecutive polls (3 minutes at default 60s interval) distinguishes a one-off network hiccup from a real absence
- **Why not 1?** Single null could be transient; treating as permanent causes flicker on every API blip
- **Why not higher (5+)?** Observed live failure mode (weekly_scoped null across every poll) persisted indefinitely; waiting 5+ minutes leaves governor pinned at 0 too long
- **Tuning path**: If 3 proves too aggressive (false exclusions), increase to 5. If too slow, decrease to 2.

---

## Finding 3: Fixtures Already Exist

**✅ All required fixtures in `src/snapshot_fixtures.rs`**

### Absent State Fixtures

1. **`weekly_scoped_absent_snapshot()`** (line 385)
   - Single snapshot with weekly_scoped = 0.0% (absent from API)
   - Timing: 5 minutes after present snapshot

2. **`weekly_scoped_absent_3_consecutive_polls()`** (line 454)
   - Returns Vec<PrevUsageSnapshot> of 4 polls
   - Pattern: present → absent×1 → absent×2 → absent×3
   - After poll 3, consecutive_absent_count reaches MIN_CONSECUTIVE_ABSENT (3)
   - Each poll is 60 seconds apart

### Present State Fixtures

1. **`weekly_scoped_present_snapshot()`** (line 347)
   - Single snapshot with weekly_scoped = 72.5% (active with real utilization)

2. **`weekly_scoped_present_3_consecutive_polls()`** (line 516)
   - Returns Vec<PrevUsageSnapshot> of 4 polls
   - Pattern: present → present → present → present
   - Consecutive-absence counter stays at 0 throughout

### Test Coverage

All fixtures have corresponding tests in `snapshot_fixtures.rs`:
- `test_weekly_scoped_present_snapshot_has_real_utilization` (line 1135)
- `test_weekly_scoped_absent_snapshot_has_zero_utilization` (line 1154)
- `test_weekly_scoped_absent_3_consecutive_polls_has_correct_structure` (line 1192)
- `test_weekly_scoped_present_3_consecutive_polls_has_correct_structure` (line 1225)
- `test_weekly_scoped_absent_reaches_min_consecutive_threshold` (line 1282)
- `test_weekly_scoped_present_never_reaches_absent_threshold` (line 1309)

All 665 tests pass ✅

---

## Acceptance Criteria Met

- ✅ Code comment recorded: `is_active` IS usable from real payloads (governor.rs:86-92)
- ✅ MIN_CONSECUTIVE_ABSENT value chosen with rationale (governor.rs:102, lines 64-84)
- ✅ Reusable fixtures exist for both states (snapshot_fixtures.rs)
- ✅ No behavior change (pure investigation + existing fixtures)
- ✅ Existing test suite green (665 passed)

**Status: COMPLETE** - All design inputs settled; later steps can proceed with implementing the structural-inactivity predicate using these foundations.
