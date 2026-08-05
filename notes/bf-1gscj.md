# First-poll test suite: run and verification — bf-1gscj

**Date:** 2026-08-05
**Bead:** bf-1gscj — "Run and verify first poll test suite"
**Depends on:** bf-1rbwd (added the first-poll test cases)

## Result

`cargo test` — **833 passed, 0 failed, 8 ignored** (738 lib + 81 across the 15
integration suites + 14 doctests). No panics, no flakes across repeated runs.
`cargo clippy --all-targets` clean; the 15 lib / 18 lib-test warnings are the
pre-existing ones tracked by bf-5qbwr, none in the code touched here.

Filtered first-poll run (`cargo test --lib first_poll`) — 12 passed:

| Test | Covers |
| --- | --- |
| `window_delta_tests::test_first_poll_no_previous_snapshot` | previous None, current Some → no delta |
| `window_delta_tests::test_first_poll_reports_no_deltas` | the same, asserted on the returned tuple |
| `window_delta_tests::test_first_poll_reports_no_deltas_regardless_of_current_values` | 4 utilization profiles, incl. 0% and 95/98/97% |
| `window_delta_tests::test_delta_computation_skipped_on_first_poll` | computation bypassed, not run against a stand-in baseline |
| `window_delta_tests::test_first_poll_governor_state_no_panic_deltas_stay_none` | the `GovernorState` fields, seeded stale first |
| `window_delta_tests::test_consecutive_polls_after_first_poll_computes_deltas` | first → second poll transition |
| `mock_poller_tests::test_first_poll_none_prev_snapshot_no_panic` | real `run_governor_cycle`, no panic |
| `mock_poller_tests::test_first_poll_and_second_poll_complete_flow` | both cycles end to end |
| `state::tests::*` (4) | snapshot rotation and bookkeeping |

Plus `mock_poller_tests::test_delta_fields_across_governor_cycles` (drives four
real cycles against `MockPoller` and asserts the persisted state file) and the
integration cases in `tests/governor_cycle_snapshot_test.rs`.

## What verification turned up

The suite was green on the first run, but the tests were not testing the
governor. Nine tests in `src/governor.rs` and four in
`tests/governor_cycle_snapshot_test.rs` each contained their **own copy** of the
`match (&previous_api_snapshot, &current_api_snapshot)` block, asserted against
that copy, and never called production code. Six of those copies encoded the
`(None, Some(_))` arm as:

```rust
p5h_delta = Some(0.0);   // "default value for first poll"
```

`run_governor_cycle` sets `None` there, and has since b6c18b0 — deliberately, per
the comment at the call site: *"None — not Some(0.0) — because 'no baseline' is
not the same claim as 'no change'."* One test even said `// Set default values
(Some(0.0)) as run_governor_cycle does`. So the suite passed while documenting
the opposite of the shipped contract, and `test_delta_fields_across_governor_cycles`
— the one test that drove the real cycle — asserted `(None, None, None)` a few
hundred lines away.

Two of the bead's acceptance criteria are "delta computation skip is verified"
and "default value usage is confirmed". Both were nominally met and actually
weren't: the value being confirmed was wrong, and the skip was confirmed on a
replica. Reporting a pass on that would have been reporting a pass on nothing.

## Fix

Extracted the decision into `governor::window_deltas_from_snapshots(previous,
current) -> (Option<f64>, Option<f64>, Option<f64>)` and made
`run_governor_cycle` assign its delta fields straight from it. Behaviour is
unchanged — same values, same info/debug log lines — the arithmetic just now has
a name a test can call.

Then repointed all thirteen simulation tests at it and dropped their inlined
matches (~500 lines). The expectations move from `Some(0.0)` to `None`, matching
production. Notable strengthenings while rewriting:

- `test_delta_computation_skipped_on_first_poll` uses non-zero current values
  (25/45/35), so a fabricated zero baseline would surface as `Some(25.0)` rather
  than hide inside a `Some(0.0)` that looks plausible.
- `test_first_poll_governor_state_no_panic_deltas_stay_none` seeds the delta
  fields with `Some(9.9)` first, so it proves they were *cleared*, not merely
  never written.
- `test_default_delta_value_specific_to_first_poll` → renamed
  `test_zero_delta_reported_only_when_a_baseline_exists`. `Some(0.0)` is a real
  and meaningful value — "measured, and the window did not move" — so the test
  now pins the one pairing that earns it (baseline present, readings identical)
  against the three that do not.

`window_deltas_from_snapshots` carries a doctest asserting the first-poll case,
so the contract is checked from the public API too.

## Not addressed

- bf-1p1gr (`run_governor_cycle` is not hermetic: reads the real accuracy log and
  collector DB, writes to the developer's `~/.needle`). The MockPoller tests here
  inherit that exposure.
- bf-5qbwr (28 pre-existing warnings elsewhere in the tree).

## Independent re-verification (2026-08-05, second session)

The bead was re-dispatched: 13eb0db was committed and pushed, but the bead was
never closed. Re-ran everything from the committed tree rather than trusting the
numbers above.

**Suite.** `cargo test` — **833 passed, 0 failed, 8 ignored**, matching the count
recorded above exactly (738 lib + 81 across 15 integration suites + 14 doctests).
No panics. `cargo test --lib first_poll` — 12 passed;
`cargo test --test governor_cycle_snapshot_test` — 9 passed.

**The suite is not vacuous.** A green run is what the first session found here
too, and it meant nothing then. So the arm under test was deliberately broken —
`window_deltas_from_snapshots`'s catch-all changed from `(None, None, None)` to
`(Some(0.0), Some(0.0), Some(0.0))`, i.e. exactly the contract the old replica
tests asserted — and the suite re-run:

```
11 failed:
  window_delta_tests::test_delta_computation_skipped_on_first_poll
  window_delta_tests::test_zero_delta_reported_only_when_a_baseline_exists
  window_delta_tests::test_first_poll_governor_state_no_panic_deltas_stay_none
  window_delta_tests::test_first_poll_no_previous_snapshot
  window_delta_tests::test_first_poll_reports_no_deltas
  window_delta_tests::test_first_poll_reports_no_deltas_regardless_of_current_values
  window_delta_tests::test_consecutive_polls_after_first_poll_computes_deltas
  window_delta_tests::test_no_snapshots_available_no_panic
  window_delta_tests::test_previous_snapshot_without_current_no_panic
  mock_poller_tests::test_delta_fields_across_governor_cycles
  mock_poller_tests::test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing
```

Both acceptance-criteria tests are in that list, and so are the two MockPoller
tests that drive real `run_governor_cycle` cycles — so the criteria are checked
against production behaviour, not against a copy of it. `src/governor.rs` was
restored (`git checkout --`) and the full suite re-run green: 833/0/8.

**Wiring confirmed by inspection.** `run_governor_cycle` (src/governor.rs:4055)
assigns its three delta fields from `window_deltas_from_snapshots`
(src/governor.rs:4174); `run_observe_cycle_internal` does the same at :2754.
`grep` for the old inlined `match (&previous_api_snapshot, ...)` blocks and for
first-poll `Some(0.0)` returns nothing in either `src/governor.rs` or
`tests/governor_cycle_snapshot_test.rs` — no replica survived the rewrite.

**Lints.** `cargo clippy --all-targets` exits 0: 198 warnings, **0 errors**. (The
"15 lib / 18 lib-test" figure above was the rustc-only count surfaced by
`cargo test`; 198 is the full count with `clippy::` lints included, still the
pre-existing set under bf-5qbwr.) Two land inside the regions this bead touched
and both predate it: `clippy::manual_range_contains` at :1874 (introduced by
2218165, bf-37wkv) and `clippy::too_many_arguments` on `run_governor_cycle`'s
signature. None are in `window_deltas_from_snapshots` or the rewritten tests.

Note for anyone re-running this: in this environment `cargo clippy`/`check`/
`build` emit nothing on the human-readable stderr path, which reads as "clean"
but is not evidence of anything. Use `--message-format=json`, or `-- -D warnings`
and check the exit code (101 here, since warnings exist).

All four acceptance criteria met.
