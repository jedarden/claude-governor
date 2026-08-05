# bf-rkrd5 — Verify first poll handling with tests

**Date:** 2026-08-05
**Scope:** verification of the API-snapshot rotation and delta computation in
`run_governor_cycle`, plus warning cleanup in the code that covers it.

## What the production path actually does

`src/governor.rs`:

- `state.previous_api_snapshot = state.current_api_snapshot.take();` — the
  rotation, run at the top of the cycle before the poll (`governor.rs:4422`, and
  the same rotation in the second cycle entry point at `governor.rs:6266`).
- On a **successful** poll the new reading is written to
  `state.current_api_snapshot` (`governor.rs:4501`).
- Deltas are computed only inside
  `if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot)`
  (`governor.rs:4510`). The `else` arm explicitly writes `None` to
  `p5h_delta` / `p7d_delta` / `p7ds_delta` rather than leaving the previous
  cycle's values in place.
- On a **failed** poll the `Err` arm never touches `current_api_snapshot`, so it
  stays `None` for that cycle and the `None` rotates into `previous` on the next
  one.

`GovernorState::update_api_snapshot` (`state.rs:1048`) performs the same
`take()`-then-set rotation for callers outside the cycle.

## The three required cases, and where each is pinned

| Case | Behaviour | Test |
| --- | --- | --- |
| First poll — `previous` None, `current` Some | no panic, no delta invented | `governor.rs` `test_first_cycle_does_not_fabricate_deltas_without_a_previous_snapshot`, `test_cycle_polls_once_and_persists_polled_usage` |
| Second poll — both Some | deltas = current − previous, per window | `test_second_cycle_repolls_and_computes_window_deltas` (4.0 / 5.0 / 3.0, three distinct values so a crossed window fails), `test_cycle_computes_negative_deltas_when_windows_reset` |
| Poll failure — `current` stays None | no deltas; stale deltas cleared on the following cycle | `test_cycle_survives_poll_failure_and_keeps_previous_usage`, `test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing`, and the 4-cycle walk in `mock_poller_tests` (cycle 3 fails → `current_api_snapshot` is None; cycle 4 → all three deltas `None`) |

All three drive the real `run_governor_cycle` against `MockPoller` and assert on
the persisted state file, not on a re-implementation of the cycle.

## Results

- `cargo build --all-targets` / `cargo check --all-targets` — no errors.
- `cargo test` — **738 lib + all integration suites + doctests pass, 0 failed.**
- 13 `first_poll` tests pass, including the `state.rs` transition tests and the
  `governor.rs` first-poll → second-poll flow.
- 37 `mock_poller_tests` pass, which is where the three required cases live.

### The one failure, now fixed

`governor::mock_poller_tests::test_cycle_holds_emergency_brake_safe_mode_at_98_percent`
failed on the first pass of this bead. It was not a regression — it fails
identically at `b6c18b0`, the commit before this bead's work (verified in a
detached worktree, not by stashing).

Exact cause, traced rather than assumed:

- Step 5a of the cycle (`governor.rs:5285`) calls `calibrator::read_all_scores()`,
  which reads `~/.needle/state/prediction-accuracy.jsonl`
  (`calibrator.rs:292`) — the machine's real file, with no injection seam.
- That file held 99 scores with `|median_error| = 1.31`.
- `update_safe_mode_from_calibration` (`governor.rs:4290`) exits safe_mode when
  `median_error_abs < 8.0` **and** `predictions_since_entry >= 3` **and**
  `total_samples >= 5`. The seeded state left `scored_at_entry` at 0, so
  `predictions_since_entry` was `99 - 0 = 99`. All three held, and
  `*safe_mode = SafeModeState::default()` wiped both `active` and the
  `emergency_brake` trigger before the assertion ran.
- The earlier pass in the same session simply caught the file below 5 samples.
  Some of those 99 scores were appended by the test runs themselves.

Fix (`seed_braked_state`): pin `safe_mode.scored_at_entry = u32::MAX`. Because
`predictions_since_entry` is `total_samples.saturating_sub(scored_at_entry)`, it
stays 0 for any ambient score count, so the calibration exit cannot fire and the
two brake tests observe only the emergency-brake decision they are about. Both
now pass, and `test_cycle_clears_emergency_brake_safe_mode_below_98_percent` is
strengthened by it — its clear now provably comes from step 1b rather than from
ambient calibration data agreeing by luck.

That is a local fix for two tests. The underlying non-hermeticity is filed as
**bf-1p1gr** (every other cycle test is still exposed, and the cycle writes to
the developer's real `~/.needle` state). A second finding from the same trace is
filed as **bf-2wizx**: the calibration exit branches on `safe_mode.active` alone,
ignoring `trigger`, so it can release an emergency brake while utilization is
still ≥98% — and it runs at 5285, before the safe-mode-conditioned hysteresis,
composite-risk and ceiling overrides are chosen at 5303–5360. Behavioural
question, deliberately not answered inside this bead.

## Changes made

Warning cleanup, confined to the first-poll / snapshot code:

- `governor.rs` — `delta_computation_attempted` and `delta_computation_called`
  are now the value of their `match` instead of pre-seeded `mut` bindings, so
  every arm must state whether it computed a delta. Silences
  "value assigned is never read" and makes a future arm that forgets to answer a
  compile error rather than a free pass.
- `governor.rs` — `final_state`, `first_poll_state` and `second_poll_state` were
  loaded and never used. They now carry assertions that hold whether or not the
  real `Poller` reaches the API: the rotation invariant
  (`second.previous == first.current`) and "deltas exist exactly when both
  snapshots do, and equal current − previous".
- Dropped unused imports (`chrono::Duration` ×2 in the fixture tests,
  `Datelike` moved into the `snapshot_fixtures` test module where it is actually
  used, `baseline_snapshot` in `governor_cycle_snapshot_test.rs`) and simplified
  a `match` clippy flagged in the flow test.

Test-isolation fix, second pass:

- `seed_braked_state` pins `scored_at_entry` (see above), with the reasoning in a
  doc comment on the helper so the next reader does not "clean up" the sentinel.

### Warnings: the one acceptance criterion not fully met

"Compiles without errors or warnings" holds for the code this bead covers — the
snapshot rotation and delta block (`governor.rs` ~4420–4600) and
`mock_poller_tests` are warning-free. A clean `cargo check --all-targets` still
emits **28 warnings elsewhere**, all pre-existing: `burn_rate.rs`, `alerts.rs`,
`narrator.rs`, `capacity_summary.rs`, `poller.rs`, unrelated `governor.rs`
regions (5492–8233), and three integration test files.

They were not swept here, for two reasons. Several are loaded-then-never-asserted
bindings in *other* beads' test files — silencing them with `_` would hide a real
test gap, so each needs a judgement call. And this repo has parallel agents in
flight; a 9-file mechanical sweep invites conflicts for no gain to this bead.
Filed with the full per-file list as **bf-5qbwr**.
