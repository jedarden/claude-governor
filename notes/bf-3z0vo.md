# bf-3z0vo — Window delta computation in the governor cycle

**Date:** 2026-08-05

## What was already in place

The delta computation is implemented on both production cycle paths. It was not
added by this bead — it landed with earlier work in this series and survives
today at:

- `src/governor.rs:4503-4539` — `run_governor_cycle`
- `src/governor.rs:6347-6373` — `run_observe_cycle_internal`

Both follow the same shape: shift `current_api_snapshot` into
`previous_api_snapshot` before the poll, record the new reading as current, then
compute deltas only inside an `if let (Some(prev), Some(curr))` match and store
them in `state.p5h_delta` / `p7d_delta` / `p7ds_delta`.

## Acceptance criteria audit

| Criterion | Status |
| --- | --- |
| Delta computation runs after each poll | Met — inside the `Ok(usage_data)` arm of both cycle paths |
| Deltas stored in governor memory | Met — `state.p{5h,7d,7ds}_delta`, persisted to the state file |
| First poll (no prev snapshot) handled gracefully | Met — the `(Some, Some)` guard skips computation; nothing panics |
| Code compiles | Met — `cargo test` builds clean; the pre-existing warnings are untouched |
| Unit test from consecutive snapshots | Met — `test_second_cycle_repolls_and_computes_window_deltas` drives two real cycles and pins 4.0 / 5.0 / 3.0 |

## What this bead added

The one gap was on the first-poll side: `test_cycle_polls_once_and_persists_polled_usage`
asserted that cycle 1 leaves `previous_api_snapshot` as `None`, but nothing on the
production path asserted what the *deltas* look like when there is no baseline to
subtract from. A guard regression there is silent and expensive — subtracting
against an implicit 0.0 writes the entire current reading in as if the fleet had
burned it in a single interval, feeding a fabricated spike into burn-rate inputs.

Added `test_first_cycle_does_not_fabricate_deltas_without_a_previous_snapshot`
(`src/governor.rs`, `mock_poller_tests`): runs one cycle against a fresh state
path with distinctive utilizations (42.5 / 63.25 / 57.75) and asserts each delta
is absent-or-zero and specifically *not* the current reading.

The assertion is "absent or zero" rather than `is_none()` on purpose — both are a
graceful first poll, and bf-9mtsa ("Initialize delta fields for first poll case")
is open to make these fields explicitly initialized. Pinning the representation
here would have handed that bead a false failure.

## Mutation check

The test is load-bearing. Replacing the guard with
`state.previous_api_snapshot.clone().unwrap_or(<zeroed snapshot>)` fails it:

```
assertion `left == right` failed: 5h delta should be absent or zero on the first poll, got Some(42.5)
```

The production code was restored immediately after; the mutation is not in the tree.

## Divergence from the bead text

The bead says "Do NOT add logging yet". Logging of the computed deltas is already
present at both call sites, added by later beads in the same series. Removing it
now would regress that work, so it stays.

## Verification

- `cargo test` — all suites pass (736 lib tests + integration/doc suites, 0 failures)
- `cargo fmt --check` — no diffs in the added region; the remaining diffs are pre-existing elsewhere in the file

## Independent re-verification (2026-08-05, later session)

Re-checked the above from scratch rather than trusting it. All of it holds:

- `cargo build` clean; `cargo test` green — 736 lib tests + every integration
  suite, 0 failures. The 27 `window_delta_tests` and
  `mock_poller_tests::test_second_cycle_repolls_and_computes_window_deltas` all pass.
- Both call sites are genuinely production code, not test-module code. The
  `#[cfg(test)]` blocks in this file interleave, so this was worth confirming by
  enclosing function rather than by line number: `run_governor_cycle` starts at
  `governor.rs:4393` and `run_observe_cycle_internal` at `governor.rs:6231`; the
  delta blocks sit inside their `Ok(usage_data)` arms. The large span
  `governor.rs:1222-3789` is `mod window_delta_tests`, which is why grep shows
  dozens of `calculate_window_pct_delta` hits that are not call sites.
- The only production writes to the delta fields are `governor.rs:4536` and
  `governor.rs:6370`. Every other `p5h_delta = ...` in the file is inside the
  test module.
- Rotation-before-poll confirmed at `governor.rs:4422` with the first-poll
  comment intact, so the `(Some, Some)` guard is what makes first poll graceful.

### New finding: deltas go stale for two cycles after a failed poll

Not covered by this bead's acceptance criteria and **not fixed here**, but it
falls out of the rotation ordering and is worth recording.

Rotation happens *before* the poll, and the `Err` arm never touches
`current_api_snapshot` or the delta fields. So a failed poll leaves:

- cycle N (poll fails): `previous = Some(last good)`, `current = None` → guard
  does not match, and the `Some(..)` deltas from cycle N−1 are **retained, not
  cleared**.
- cycle N+1 (poll succeeds): rotation sets `previous = current.take() = None`,
  so the guard still does not match — deltas remain the stale cycle N−1 values
  even though a fresh reading is now in hand.

Net effect: one poll failure produces a two-cycle window in which
`p5h/p7d/p7ds_delta` are non-`None` but describe an interval that has already
scrolled past. Nothing panics, so this is graceful in the sense the bead
required — but any downstream consumer that reads the deltas without checking
snapshot freshness will silently use them. Worth a follow-up bead; interacts with
bf-9mtsa ("Initialize delta fields for first poll case"), since a decision to
initialize these fields explicitly should cover the failure path too, not just
the first poll.
