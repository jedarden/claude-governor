# bf-9mtsa — Initialize delta fields for the first-poll case

**Date:** 2026-08-05

## What the code looked like going in

Both production cycle paths computed window deltas inside a bare
`if let (Some(prev), Some(curr))` with no `else`:

- `src/governor.rs` — `run_governor_cycle` (starts at ~4393)
- `src/governor.rs` — `run_observe_cycle_internal` (starts at ~6231)

The guard made the first poll graceful in the "nothing panics" sense (bf-3z0vo
verified that), but the delta fields were only ever *written* on the
`(Some, Some)` path. On any other path they kept whatever the previous cycle
left in them.

The `if let` sits inside the `Ok(usage_data)` arm, immediately after
`current_api_snapshot` is set to `Some(..)`. So `curr` is always `Some` there,
and the `else` branch means exactly one thing: **no previous snapshot to
subtract from**. Two ways to get there:

1. First poll after governor start or a state clear.
2. The poll *after* a failed one. Rotation happens before the poll
   (`state.previous_api_snapshot = state.current_api_snapshot.take()`), and the
   `Err` arm never writes `current_api_snapshot` — so a failure leaves `current`
   as `None`, and the next cycle rotates that `None` into `previous`. This is
   the staleness gap recorded at the end of `notes/bf-3z0vo.md`.

## Change

Added an explicit `else` at both sites setting all three delta fields, plus a
`log::debug!` naming the reason:

```rust
} else {
    state.p5h_delta = None;
    state.p7d_delta = None;
    state.p7ds_delta = None;
    log::debug!("[governor] no previous API snapshot; window deltas cleared ...");
}
```

**`None`, not `Some(0.0)`.** All three fields are `Option<f64>`, so the bead's
"Option<T> gets None" rule applies directly. It is also the honest value: `0.0`
asserts "the window did not move over this interval", which is a different claim
from "there was no interval to measure". bf-3z0vo's note makes the same point
about not fabricating deltas from a missing baseline.

Scope note: `last_fleet_aggregate.window_pct_deltas` (`f64` fields, so the
"f64 gets 0.0" half of the criterion) is deliberately **not** touched. It is
populated from the collector/db path (`db::annotate_window_pct_deltas`), not
from this poll block; zeroing it here would clobber unrelated data.

## Side effect: closes half the bf-3z0vo staleness gap

Case 2 above is now cleared — the cycle following a failed poll no longer
reports the pre-failure interval's deltas as if they were current. The remaining
half is untouched and still open: on the failing cycle itself the `Err` arm
returns without touching the delta fields, so they stay `Some(..)` for that one
cycle. Fixing that means writing to the delta fields from the `Err` arm, which
is outside this bead.

No production code reads `p5h/p7d/p7ds_delta` today (the only non-test writes
were `governor.rs:4536` and `:6370`; every other reference in the file is inside
`mod window_delta_tests` or `mod mock_poller_tests`), so nothing downstream
changes behavior from `Some(stale) -> None` yet.

## Test

Added `test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing`
(`mock_poller_tests`). Seeds a state file with deltas the poll cannot possibly
produce (9.9 / 8.8 / 7.7) and `current_api_snapshot = None` — the exact state a
failed poll leaves — runs a real cycle through `run_governor_cycle`, and asserts
all three fields come back `None`.

The existing `test_first_cycle_does_not_fabricate_deltas_without_a_previous_snapshot`
still passes unchanged: it asserts "absent or zero" precisely so this bead could
pick a representation.

### Mutation check

Deleting the three `= None` assignments from `run_governor_cycle` (leaving the
`else` and its log in place) fails the new test:

```
assertion `left == right` failed: 5h delta should be None with no baseline to subtract from, got Some(9.9)
```

Restored immediately; the mutation is not in the tree.

## Verification

- `cargo build` — clean (3 pre-existing warnings, untouched)
- `cargo test` — 737 lib tests + all integration and doc suites, 0 failures
  (736 before; the new test is the delta)
- `cargo fmt --check src/governor.rs` — 178 diffs, all pre-existing; none in the
  added regions
