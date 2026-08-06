# bf-1uqqx — Audit: duplicate delta computation in `run_observe_cycle_internal`

**Date:** 2026-08-05
**Scope:** report only, no refactor (per acceptance criteria)
**File:** `src/governor.rs` (12560 lines)

## Summary

The finding in the bead is **confirmed**. Production window-delta computation lives in two
places:

1. `window_deltas_from_snapshots` (`src/governor.rs:1228-1255`), called by
   `run_governor_cycle` (`src/governor.rs:4174-4177`).
2. An inline copy in `run_observe_cycle_internal` (`src/governor.rs:6015-6052`), which
   never calls the helper.

Both are reachable in production: `run_observe_cycle_internal` is called only by
`run_observe` (`src/governor.rs:5837`), which is called by
`run_internal_observe_command` in `src/main.rs:1655` (dispatched from `src/main.rs:1203`),
and its results are persisted via `state::save_state` at `src/governor.rs:6530`.

Verdict on equivalence: **behaviourally identical for state**, differing only in log detail.
Recommendation: **delegate to the helper** — the duplication is not justified.

## Both branches, precisely

### `if` branch — `src/governor.rs:6015-6039`

- `6015-6017` — `if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot,
  &state.current_api_snapshot)`. Note `state.current_api_snapshot` was assigned `Some(..)`
  unconditionally just above at `6007-6012`, so in practice this guard tests only whether a
  previous snapshot exists.
- `6018-6022` — builds `prev_pct: crate::db::WindowPctSnapshot` from
  `prev.five_hour_pct` / `prev.seven_day_pct` / `prev.weekly_scoped_pct`.
- `6023-6027` — builds `curr_pct` from the same three fields of `curr`.
- `6028-6029` — `calculate_window_pct_delta(&prev_pct, &curr_pct)` →
  `(delta_5h, delta_7d, delta_7ds)`.
- `6031-6034` — `log::info!` of the three deltas
  (`"[governor] window deltas: 5h={:+.2}%, 7d={:+.2}%, 7ds={:+.2}%"`).
- `6036-6039` — assigns `state.p5h_delta = Some(delta_5h)`, `state.p7d_delta = Some(delta_7d)`,
  `state.p7ds_delta = Some(delta_7ds)`.

### `else` branch — `src/governor.rs:6040-6052`

- `6041-6044` — comment: no previous snapshot (first poll, or the poll after a failed one);
  clear every delta field explicitly so a stale `Some(..)` cannot be read as a measurement of
  the current interval. Ends with *"Mirrors run_governor_cycle."*
- `6045-6047` — sets all three of `p5h_delta` / `p7d_delta` / `p7ds_delta` to `None`.
- `6049-6051` — `log::debug!`
  (`"[governor] no previous API snapshot; window deltas cleared (first poll or poll following a failure)"`).

## Behavioural equivalence with `window_deltas_from_snapshots`

**Verdict: equivalent.** For every reachable input the two produce the same values for
`p5h_delta` / `p7d_delta` / `p7ds_delta`.

Field-by-field, the inline `if` branch (`6018-6029`) is a character-level match for the
helper's `(Some(prev), Some(curr))` arm (`1234-1245`): the same `WindowPctSnapshot`
construction with the same field pairing (`five_hour_pct→five_hour`,
`seven_day_pct→seven_day`, `weekly_scoped_pct→weekly_scoped`), the same
`calculate_window_pct_delta` call, and the same `Some(..)` wrapping. The inline `else`
(`6045-6047`) produces `(None, None, None)`, matching the helper's `_ => (None, None, None)`
arm (`1253`).

The `if let` pattern and the helper's `match` also partition the input space identically:
`(Some, Some)` → computed deltas, everything else → all-`None`. The `(Some(prev), None)`
case is unreachable in both callers (both assign `current_api_snapshot = Some(..)` before
the branch: `4163-4168` and `6007-6012`).

The preconditions feeding the branch are the same in both functions, so identical inputs
reach identical code:

| Precondition | `run_governor_cycle` | `run_observe_cycle_internal` |
|---|---|---|
| snapshot rotation `previous = current.take()` before poll | `4084` | `5916` |
| model-change zeroing of `prev_snap.weekly_scoped_pct` | `4128-4134` | `5957-5963` |
| `fleet_pct_ema_samples = 0` on model change | `4136-4143` | `5965-5970` |
| `current_api_snapshot` assigned from poll result | `4163-4168` | `6007-6012` |
| delta code sits inside the `Ok(usage_data)` arm (poll failure leaves deltas untouched) | yes | yes |

**Differences, all non-behavioural:**

1. **Log detail.** `run_governor_cycle` logs the deltas *plus* the previous and current
   percentages (`4188-4192`); the observe copy logs only the three deltas (`6031-6034`).
   The `log::debug!` no-baseline string is byte-identical in both (`4196` / `6050`).
2. **Assignment shape.** `run_governor_cycle` computes once and assigns unconditionally
   (`4204-4206`), using its `match` purely for logging (`4179-4199`); the observe copy
   assigns inside each arm of the `if`/`else`. Same resulting values; the unconditional form
   is the more robust shape, since it cannot grow a path that forgets to clear.

Also worth recording: nothing inside `run_observe_cycle_internal` *reads* `p5h_delta` /
`p7d_delta` / `p7ds_delta` after they are written — grep over the rest of the function
(`6053-6534`) finds no further reference. Their only consumer is the persisted state written
at `6528-6530`.

## Is the duplication justified?

No. There is no observe-specific requirement the helper fails to meet:

- The helper is `pub` and in the same module, so there is no visibility or layering barrier.
- The inline copy computes nothing extra — its only additive behaviour is the `log::info!`
  line, which is at the call site in `run_governor_cycle` too and stays at the call site
  under delegation.
- The helper's doc comment (`1191-1227`) already states the first-poll contract as the single
  authority ("This is the whole first-poll contract in one place"), and its docs name
  `run_governor_cycle` as the caller — so the observe copy silently contradicts the
  documentation it is supposed to be mirroring.

The concrete cost is test coverage, not present-day behaviour. `window_deltas_from_snapshots`
is exercised by ~12 unit tests in `src/governor.rs` (e.g. `1523`, `1911`, `1990`, `2022`,
`2052`, `2190-2213`) and 7 assertions in `tests/governor_cycle_snapshot_test.rs`
(`317`, `371`, `426`, `463`, `530`, `612`). The inline copy has **zero** coverage: no test in
`tests/` calls `run_observe` or `run_observe_cycle_internal` at all. So every test that
"proves the first-poll contract" proves it for the helper only, and the observe path is
correct today purely by hand-maintained copy. The `Mirrors run_governor_cycle` comment at
`6044` is the only thing holding the invariant, and it is already partly stale: it claims to
mirror a function that has since moved to the helper and to unconditional assignment.

## Recommendation

**Delegate.** In a follow-up bead, replace `src/governor.rs:6014-6052` with the same shape
`run_governor_cycle` uses:

- call `window_deltas_from_snapshots(state.previous_api_snapshot.as_ref(),
  state.current_api_snapshot.as_ref())`;
- keep an `info`/`debug` log pair at the call site (adopting `run_governor_cycle`'s richer
  info line, which includes the prev/curr percentages, is a free improvement);
- assign the three fields unconditionally from the returned tuple, so no future edit can add
  a path that leaves a stale `Some(..)` behind.

This is a pure de-duplication: per the equivalence analysis above it changes no persisted
value, only which code produces it. It makes the existing helper tests cover the observe path
as well, and removes the second place that must be edited whenever the first-poll contract
changes.

Two follow-ups worth separate beads:

1. Add at least one integration test that drives `run_observe` end-to-end — currently the
   entire observe cycle, not just its delta block, is untested.
2. After delegation, drop the now-inaccurate `Mirrors run_governor_cycle` comment at `6044`
   in favour of pointing at the helper, matching the comment style at `4170-4173`.

## Acceptance criteria

- [x] Both branches described with line numbers — `6015-6039` (if), `6040-6052` (else).
- [x] Explicit verdict on behavioural equivalence — equivalent; differences are log detail
      and assignment shape only.
- [x] Recommendation with reasoning — delegate; unjustified duplication, zero test coverage
      on the copy, helper docs already claim sole authority.
- [x] No refactor performed in this bead.
