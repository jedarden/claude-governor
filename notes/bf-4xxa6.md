# bf-4xxa6 — Authoritative Some/Some window-pct delta block in `src/governor.rs`

**Provenance:** `src/governor.rs` @ 12560 lines, last touched in commit `13eb0db`.
Working tree clean for this file at the time of survey. All line numbers below are
current as of that state.

## Headline finding

**The authoritative production delta block does NOT contain an
`if let (Some(prev), Some(curr))`.** `run_governor_cycle` was refactored to call the
helper `window_deltas_from_snapshots(..)`, so grepping for the `if let` idiom misses
it entirely. That is why the parent bead's cited range (2585–2609) drifted onto test
code: the citation was pinned to a grep pattern that no longer marks the real site.

**Authoritative block: `src/governor.rs:4170-4206`**, inside
`pub fn run_governor_cycle` (4055–5782), in the `Ok(usage_data)` arm of the poll match.

It delegates the arithmetic to two top-level production helpers:

| Fn | Lines | Role |
| --- | --- | --- |
| `window_deltas_from_snapshots` | 1228–1255 | the `(Some(prev), Some(curr))` decision — returns `(Option<f64>, Option<f64>, Option<f64>)`; `_ => (None, None, None)` on no baseline |
| `calculate_window_pct_delta` | 1181–1189 | raw `current − previous` on the three windows |

Block shape at 4170–4206:
- 4170–4173 — comment
- 4174–4177 — `let (p5h_delta, p7d_delta, p7ds_delta) = window_deltas_from_snapshots(prev, curr)`
- 4179–4200 — `match` on the 5-tuple, **logging only** (info on both-present, debug on no-baseline)
- 4202–4206 — unconditional `state.p5h_delta / p7d_delta / p7ds_delta = ..` (so a stale `Some(..)` cannot survive a baseline-less cycle)

Downstream children should edit **`window_deltas_from_snapshots` (1228–1255)** for
behavior changes, and **4170–4206** for how `run_governor_cycle` logs/stores the result.

## Full enumeration of `(Some(prev), Some(curr))` sites touching window pct deltas

| Line | Form | Enclosing fn | Enclosing module | Class |
| --- | --- | --- | --- | --- |
| 1233 | `(Some(prev), Some(curr)) =>` match arm | `window_deltas_from_snapshots` (1228–1255) | top level | **PRODUCTION** — the single real decision |
| 2391 | `if let (Some(prev), Some(curr))` | `test_consecutive_snapshots_governor_cycle` (2249) | `#[cfg(test)] mod window_delta_tests` (1287–3451) | TEST — re-implements the old inline logic; comment at 2390 says "simulates the delta computation" |
| 3371 | commented-out `if let (Some(prev), Some(curr))` | `test_second_poll_with_both_snapshots` (3307) | `#[cfg(test)] mod window_delta_tests` (1287–3451) | TEST — comment text only, stale reference to the pre-refactor shape |
| 3373 | `(Some(prev), Some(curr)) =>` match arm | `test_second_poll_with_both_snapshots` (3307) | `#[cfg(test)] mod window_delta_tests` (1287–3451) | TEST |
| 6015 | `if let (Some(prev), Some(curr))` | `run_observe_cycle_internal` (5898–6534) | top level | PRODUCTION, but **observe path, not the governor polling path** — see divergence note |
| 10312 | `(Some(prev), Some(curr)) =>` match arm | `test_first_poll_and_second_poll_complete_flow` (10151) | `#[cfg(test)] mod mock_poller_tests` (9500–11425) | TEST |
| 10887 | `` `(Some(prev), Some(curr))` `` in a doc comment | doc comment on the following first-poll test | `#[cfg(test)] mod mock_poller_tests` (9500–11425) | TEST — prose; describes `run_governor_cycle` as still using a match guard, which is now only true via the helper |

Non-delta `if let (Some(..), Some(..))` sites, listed so future greps don't re-trip on
them — these parse fleet-record `t0`/`t1` timestamps and touch no window pct:
`4274` (in `run_governor_cycle`), `6111` (in `run_observe_cycle_internal`),
and `4638` `if let (Some(ref prev_snap), Ok(conn))` (record annotation, `run_governor_cycle`).

## Divergence worth flagging to downstream children

`run_observe_cycle_internal` at **6014–6052** still carries the *pre-refactor inline
copy* of the logic: it builds both `WindowPctSnapshot`s by hand, calls
`calculate_window_pct_delta`, and writes `Some(..)` / `None` in an if/else whose comment
reads "Mirrors run_governor_cycle."

It is behaviorally equivalent to `window_deltas_from_snapshots` today, but it is a
second place to change. Any child bead altering delta semantics must either update both
or collapse 6014–6052 onto the helper.

## Test coverage anchored on the helper

`test_first_poll_governor_state_no_panic_deltas_stay_none` (2695, in
`mod window_delta_tests`) already calls `window_deltas_from_snapshots` directly
(2754–2760) rather than re-implementing the match — the intended pattern for new tests.
