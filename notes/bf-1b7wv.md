# bf-1b7wv — Add delta value verification

**Date:** 2026-08-05
**Depends on:** [bf-4t780](bf-4t780.md) (delta population assertions)

## What was already there

The bead's three acceptance criteria were already literally satisfied inside
`governor::window_delta_tests::test_consecutive_snapshots_governor_cycle`
(`src/governor.rs`), the test this chain has been building up through bf-5vhv2 →
bf-5pl4o → bf-4t780:

- expected deltas are calculated manually from the snapshot fields
  (`expected_5h_delta = snapshot2.five_hour_pct - snapshot1.five_hour_pct`, …),
- each computed delta is asserted equal to its expected value, and
- the formula is documented in a comment block above the assertions.

bf-4t780 had already hoisted those expectations above the delta block and
mutation-checked them. Re-doing that work would have produced churn, so this bead
went after the place where the *same* verification was still weak.

## The gap: the production path

`mock_poller_tests::test_second_cycle_repolls_and_computes_window_deltas` is the
test that runs delta computation through the real `run_governor_cycle` and the
persisted state file. Its delta verification was weaker than the unit test's in
two ways:

1. **Deltas were anchored only to hardcoded literals** (`Some(4.0)`, `Some(5.0)`,
   `Some(3.0)`), not to the snapshot pair the cycle actually persisted.
2. **Two of the three input fields were never asserted.** The test checked
   `current.five_hour_pct` but not `current.seven_day_pct` or
   `current.weekly_scoped_pct` — so the 7d and 7ds delta literals sat on an input
   side the test never confirmed.

Both are now fixed: the expected deltas are derived from the persisted
`previous`/`current` snapshots, the literal checks are kept alongside them (they
pin the fixture arithmetic, and the three distinct values catch a crossed
window), and all three current-snapshot fields are asserted.

## New test: signed deltas through the cycle

The delta formula is signed, and the case that depends on the sign is a window
reset. Every existing reset test — `test_window_reset_boundary_transitions`,
`test_negative_deltas_window_reset`, `test_consecutive_snapshot_delta_with_window_reset`,
`test_calculate_window_pct_delta_negative_deltas` — calls
`calculate_window_pct_delta` directly. **No test drove a falling window through
`run_governor_cycle`**, so an `.abs()` or a flipped operand order in the cycle's
own wiring had no end-to-end coverage.

`mock_poller_tests::test_cycle_computes_negative_deltas_when_windows_reset` adds
it: cycle 1 polls 80/90/85, cycle 2 polls 5/15/8, and the persisted deltas are
asserted to be −75.0 / −75.0 / −77.0, both derived from the snapshots and as
literals (magnitudes alone would survive an `.abs()`).

## Formula documentation

Both the unit test and the end-to-end test now state the formula the same way:

> delta = current − previous, per window. The operands are percent-of-quota
> readings, so a delta is a signed difference in **percentage points** — not a
> ratio and not a relative percent change. 10.0% → 14.0% is a delta of 4.0
> points, not 40%.

The percentage-point-vs-relative-change distinction is the one a reader is most
likely to get wrong, and a relative-change implementation is a plausible mutation
(it would yield 40.0 where the test expects 4.0).

## Mutation checks

Both new assertion families were confirmed load-bearing; the source was restored
after each.

| Mutation | Result |
| --- | --- |
| `calculate_window_pct_delta` returns `.abs()` of each difference | 8 tests fail, including the new `test_cycle_computes_negative_deltas_when_windows_reset`: `5h delta should be 5.0 - 80.0 = -75.0` |
| `run_governor_cycle` crosswires `curr_pct.seven_day` / `weekly_scoped` | both end-to-end delta tests fail; the derived assertion reports `7d delta should be current (25) − previous (20)` |

In each case the newly added derived assertion fires before the literal one, so
the failure message names the snapshot operands rather than a bare number.

## Independent re-verification (2026-08-05, redispatch)

The bead was redispatched after its work had already been committed and pushed as
`c5150d7` — the commit landed but the bead was never closed. Rather than trust the
commit message, the claims above were re-checked from scratch:

- Both delta tests exist and execute (`test_second_cycle_repolls_and_computes_window_deltas`,
  `test_cycle_computes_negative_deltas_when_windows_reset`).
- The `.abs()` mutation was re-applied to `calculate_window_pct_delta`: the reset
  test fails with `left: Some(75.0)` / `right: Some(-75.0)`, and the **derived**
  assertion is the one that fires — confirming the negative test is not tautological
  despite deriving its expectations (the trailing literal pins `-75.0`/`-75.0`/`-77.0`
  hold the values down).
- The 7d/7ds crosswire mutation was re-applied: the positive test fails with
  `7d delta should be current (25) − previous (20)`, `left: Some(3.0)` / `right: Some(5.0)`.
- `src/governor.rs` was restored after each mutation and confirmed byte-identical
  to HEAD (`git diff` empty).

No code changes were needed; the acceptance criteria were already met.

## Verification

- `cargo test --lib` → 735 passed, 0 failed (734 before; +1 new test).
- `cargo clippy --lib --tests` → no warnings.
- `cargo fmt --check` → no diffs in either edited region. The pre-existing diffs
  elsewhere in `src/governor.rs` (lines 155, 172, 288, 6155+, 11977+) are
  untouched, same as under bf-5pl4o and bf-4t780.
