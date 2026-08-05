# bf-5pl4o — Write consecutive snapshots test

**Date:** 2026-08-05
**Status:** re-dispatch of already-implemented work; tightened the surviving test.

## What was already there

The bead's acceptance criteria were met by earlier commits:

- `b049331 test: Add consecutive snapshots test for governor cycle` added
  `governor::window_delta_tests::test_consecutive_snapshots_governor_cycle`: two
  distinct `PrevUsageSnapshot`s (10.0/20.0/15.0 then 12.5/22.0/18.0, 120s apart),
  stored in sequence with the `current -> previous` shift between them, then
  differenced through `calculate_window_pct_delta`.
- `bc8da4c test(bf-4bzt9)` added
  `mock_poller_tests::test_second_cycle_repolls_and_computes_window_deltas`,
  which really does run `run_governor_cycle` twice — same `MockPoller` instance,
  10/20/30 then 14/25/33 — and asserts `poll_count == 2`, the snapshot rollover
  in the persisted state, and deltas `Some(4.0)/Some(5.0)/Some(3.0)`.

So "two distinct snapshots", "fed to the governor in sequence", and "demonstrates
consecutive polling" were all satisfied before this session, at both the
state-bookkeeping level and the full-cycle level. Verified: `cargo test --lib` →
734 passed, 0 failed.

## What this session changed

Two problems in `test_consecutive_snapshots_governor_cycle`:

1. The `last_fleet_aggregate.window_pct_deltas` preconditions asserted
   `>= 0.0` with the message "field should exist and be valid". On a fresh
   `GovernorState` those fields are `0.0`, so the assertions could never fail —
   and the invariant was wrong anyway, since deltas are signed (a window reset
   makes them negative). Replaced with `assert_eq!(.., 0.0)`, which is a real
   precondition and is what makes the later non-zero assertions mean something.

2. The doc comment claimed the test "simulates running the governor cycle
   twice". It touches no poller, database, or state file — it exercises the
   state + delta half only. It now says that, notes that the manual shift
   mirrors production ordering (`run_governor_cycle` shifts `current -> previous`
   at the top of the cycle and writes a new `current` only on a successful poll,
   so a failed poll keeps the prior reading in `previous_api_snapshot`), and
   points at `mock_poller_tests::test_second_cycle_repolls_and_computes_window_deltas`
   for the end-to-end coverage.

## Mutation check

Confirmed the test is load-bearing on `calculate_window_pct_delta`, not just on
values it assigned itself: flipping `src/governor.rs:1185` from
`current - previous` to `previous - current` fails it with

```
Computed 5h delta (-2.5) should match expected 5h delta (2.5) from formula: current (12.5) - previous (10)
```

The source was restored afterwards; the working tree carries no such change.

## Not changed

- Negative-delta (window reset) consecutive-snapshot coverage already exists at
  `src/governor.rs:8989` — not duplicated here.
- `cargo fmt --check` reports pre-existing diffs elsewhere in `src/governor.rs`
  (lines 155, 172, 288, 6046+) and in other files. None fall in the edited region
  (2517–2830); left alone rather than bundling an unrelated reformat.
