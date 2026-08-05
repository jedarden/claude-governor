# bf-37w5k — Unit test for consecutive snapshot delta computation

**Date:** 2026-08-05
**Outcome:** No code change. Every acceptance criterion was already satisfied;
this bead verified that the existing tests are *load-bearing* rather than merely
present, and recorded which test covers which failure mode.

## Why no new test was written

This bead is the tail of a chain — bf-5vhv2 → bf-5pl4o → bf-4t780 → bf-1b7wv —
that already built and then hardened exactly the test described here. Two tests
already match the description, both passing:

- `governor::tests::test_consecutive_snapshot_delta_computation`
  (`src/governor.rs:8976`) — literally in the `tests` module the bead names.
  Creates two consecutive snapshots with known values, computes deltas via
  `calculate_window_pct_delta`, verifies each against a manually derived
  expectation *and* against the literal arithmetic (2.5 / 2.0 / 3.0), then
  populates and asserts the `WindowPctDeltas` state fields.
- `governor::window_delta_tests::test_consecutive_snapshots_governor_cycle`
  (`src/governor.rs:2547`) — the longer version, adding the snapshot shift
  (`current → previous`), the `last_fleet_aggregate.window_pct_deltas` fields,
  and a serialization round trip that pins the persisted field names.

The criterion "run the governor cycle with both snapshots" is covered by
`mock_poller_tests::test_second_cycle_repolls_and_computes_window_deltas`, which
drives two real `run_governor_cycle` calls against a mock poller and a temp state
file, then reads the deltas back off the persisted state.

Writing a third near-identical test would have added churn and no coverage, so
this bead went after the question the acceptance criteria can't answer on their
own: do these tests actually fail when the delta computation breaks?

## Mutation verification

Two mutations were applied to production code, the suite run against each, then
the file restored (`git diff` clean, 735 passed / 0 failed).

**Mutation 1 — swap the operands** in `calculate_window_pct_delta`
(`previous - current` instead of `current - previous`):

> **29 tests failed**, including all three tests named above. The formula is
> comprehensively pinned; a sign inversion cannot land silently.

**Mutation 2 — cross-wire the windows** at the production assignment site
(`src/governor.rs:4536-4538`, `state.p7d_delta = Some(delta_7ds)` and vice versa):

> **Exactly 2 tests failed**, both on the production path:
> `test_second_cycle_repolls_and_computes_window_deltas` and
> `test_cycle_computes_negative_deltas_when_windows_reset`.

Mutation 2 is the informative one. The two unit tests did **not** catch it, and
that is by design rather than a defect: they compute the deltas and assign the
state fields themselves, so they pin the *formula* and the *state shape* but not
the cycle's field wiring. The mock-poller tests pin the wiring. The division of
labour is real and documented in the unit test's own doc comment — and mutation 2
confirms neither layer is redundant.

## Known gap (not closable here)

`state.last_fleet_aggregate.window_pct_deltas` is populated in production by a
database round trip: `db::annotate_window_pct_deltas` writes `p5h`/`p7d`/`p7ds`
onto the interval's `f` row (`src/governor.rs:5020`), and the next cycle reads
that row back into the aggregate (`src/governor.rs:4675`). The unit test
hand-assigns those three fields to stand in for the round trip, and says so.

That round trip is untested end-to-end. `db.rs` covers the write side; the
governor's read-back side has no coverage. It is **not** testable as things
stand: `run_governor_cycle` resolves its database through
`collector::default_db_path()`, which is hardcoded to
`$HOME/.needle/state/token-history.db` with no parameter and no env override, so
a test can only reach it by writing to the developer's real database. Closing
this needs an injectable db path — a production refactor, out of scope for a
test-writing bead. Worth filing separately if the aggregate wiring ever matters
enough.
