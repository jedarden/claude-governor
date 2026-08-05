# bf-9mtsa — Re-dispatch verification

**Date:** 2026-08-05
**Bead:** bf-9mtsa — "Initialize delta fields for first poll case"
**Prior resolution:** commit `946220a` (pushed), closed at 18:51 UTC by `5501f6c`
**Re-dispatch:** bead reopened to `in_progress` at 19:24 UTC with labels
`deferred`, `failure-count:2`

## Why this note exists

The bead was dispatched to a fresh session after it had already been resolved,
committed, and pushed. `origin/main` contains both `946220a` (the fix) and
`5501f6c` (the close), so there was no lost work to redo. This session
re-verified the shipped state against the acceptance criteria instead of
re-implementing, and found nothing outstanding.

## Verification against the acceptance criteria

**"All delta fields explicitly initialized in else block"** — the `else` is
present at both production cycle sites, `src/governor.rs:4539`
(`run_governor_cycle`) and `src/governor.rs:6390` (`run_observe_cycle_internal`),
each setting `p5h_delta` / `p7d_delta` / `p7ds_delta` and emitting the
"no previous API snapshot; window deltas cleared" debug line (`:4554`, `:6400`).

**"Types match (Option<T> gets None, f64 gets 0.0)"** — both halves hold, via
two different paths:

- The three `state.p*_delta` fields are `Option<f64>` and get `None`. Rationale
  for `None` over `Some(0.0)` is in `notes/bf-9mtsa.md`: with no baseline there
  is no interval to measure, which is a different claim from "the window did not
  move".
- The `f64` half is `state.last_fleet_aggregate.window_pct_deltas`
  (`WindowPctDeltas`, three plain `f64`). It is not written from the snapshot
  block at all — the whole `FleetAggregate` is reassigned later in the same cycle
  from the collector/db record (`:4685`, mirrored at `:6491`), where the values
  come from `fleet_json.get("p5h"/"p7d"/"p7ds")…unwrap_or(0.0)` (`:4648-4658`,
  `:6483-6485`). So those `f64` fields already land on `0.0` explicitly every
  cycle when the db has no delta to report. Zeroing them from the snapshot
  `else` would clobber collector data with a value the poll block has no
  authority over; the `unwrap_or(0.0)` is the correct initialization site and it
  is already there.

**"Code compiles without errors"** — `cargo test --lib`: 737 passed, 0 failed.

**"First poll case handled gracefully with no uninitialized data"** — covered by
`test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing`
(added in `946220a`) and the pre-existing
`test_first_cycle_does_not_fabricate_deltas_without_a_previous_snapshot`.

## Not re-litigated

The known-remaining gap is unchanged and still out of scope for this bead: on
the cycle where the poll *itself* fails, the `Err` arm returns without touching
the delta fields, so they stay `Some(..)` for that one cycle. The cycle *after*
a failure is fixed (rotation puts `None` into `previous`, so the new `else`
fires). See the tail of `notes/bf-3z0vo.md` and the "Side effect" section of
`notes/bf-9mtsa.md`.

## Conclusion

No code change required. The re-dispatch is a tracking artifact, not a signal
that the fix regressed or was incomplete.
