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

- `cargo build --all-targets` — no errors.
- `cargo test` — 737 lib + 71 integration + 13 doctests pass.
- 13 `first_poll` tests pass, including the `state.rs` transition tests and the
  `governor.rs` first-poll → second-poll flow.

### One pre-existing failure, not from this work

`governor::mock_poller_tests::test_cycle_holds_emergency_brake_safe_mode_at_98_percent`
fails. It fails identically on the unmodified tree (verified by stashing this
bead's changes), and it passed earlier in the same session — it is
environment-dependent, not a regression here.

Cause: `run_governor_cycle` is not hermetic. It calls
`collector::run_collection_pass()` and reads `collector::default_db_path()`
unconditionally, so the live governor database participates in the test. The
prediction-accuracy exit path (`governor.rs:4290`, `check_safe_mode_exit`) clears
`safe_mode` once `stats.total_samples` and the median error from that live DB
cross their thresholds — which is what happened between the two runs. The test
seeds `safe_mode.active = true` and expects it to survive the cycle.

This is a test-isolation defect in the emergency-brake test, unrelated to
snapshot handling; it deserves its own bead rather than a fix smuggled in here.

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

The remaining warnings in `governor.rs` (unused variables around lines
5492–8233, unnecessary parentheses in the forecast helpers) are pre-existing and
sit in unrelated code; they were left alone. Likewise the repo's existing
`cargo fmt` drift — only the regions touched here are rustfmt-clean.
