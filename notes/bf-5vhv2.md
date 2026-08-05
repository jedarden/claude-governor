# bf-5vhv2 — Add basic governor cycle test infrastructure

**Date:** 2026-08-05
**Status:** re-dispatch of already-implemented work; tightened the surviving test.

## What was already there

The bead's acceptance criteria were met by earlier commits:

- `07ab260 Add basic governor cycle test infrastructure (bf-5vhv2)` added
  `governor::tests::test_governor_cycle_basic_flow` plus its two siblings
  (`..._emergency_brake`, `..._hysteresis_no_change`) — a single usage snapshot
  driven through `compute_target_workers` → `apply_scaling`.
- `aa264c4 test(bf-375k6)` added `MockPoller` and
  `mock_poller_tests::test_governor_cycle_smoke`, the first test to actually call
  `run_governor_cycle`.
- `bc8da4c test(bf-4bzt9)` extended that into the cycle behavior tests
  (poll-once, snapshot rollover/deltas, state persistence, emergency brake,
  poll failure, stale data) and the shared `run_cycle` helper.

So "test function exists and compiles", "can run a governor cycle", and
"infrastructure in place for more complex tests" were all satisfied before this
session. Verified: `cargo test --lib` → 734 passed, 0 failed.

## What this session changed

`test_governor_cycle_basic_flow` step 6 was tautological — it matched on the
`ScalingDecision` and accepted `NoChange`, `ScaleUp(1..=3)` or `ScaleDown(1..=2)`
alike, so it could only fail on `EmergencyBrake` or a panic. A single fixed
snapshot has exactly one correct outcome, so it is now pinned:

- `target == 7` (the binding weekly_scoped window's `safe_worker_count`)
- `decision == NoChange` (7 vs current 5 is inside the 2.0 hysteresis band)

Confirmed load-bearing: changing the expected target to 5 fails the test with
`left: 7, right: 5`.

The doc comment also claimed the test ran "a governor cycle". It runs the
decision half only — no poller, database, or tmux — so it now says that and
points at `mock_poller_tests::test_governor_cycle_smoke` for the end-to-end
`run_governor_cycle` coverage.

## Not changed

`cargo fmt --check` reports pre-existing diffs in `src/governor.rs` (lines 155,
172, 288, 6034–6536, 11744–11982), `src/main.rs`, `src/poller.rs`, and several
`tests/*.rs` files. None are in the edited region; left alone rather than
bundling an unrelated reformat into this commit.
