# bf-4bzt9 — Re-dispatch verification

**Date:** 2026-08-05
**Bead:** bf-4bzt9 — "Add governor cycle behavior verification tests"
**Status on re-dispatch:** `in_progress`, labels `deferred`, `failure-count:1`
**Original resolution:** commit `bc8da4c` (`test(bf-4bzt9): verify governor cycle behavior against MockPoller`), closed by `48ce7ce`

## Why this re-dispatch occurred

The bead had already been completed, committed, and pushed (`origin/main` and `HEAD` are
identical — 0 ahead, 0 behind). It was subsequently reopened with `failure-count:1` and the
`deferred` label. Same pattern as the bf-27ulv re-dispatch: work landed, then the bead was
handed out again. No code defect was found on re-inspection.

## Verification performed

All work was re-verified rather than taken on trust.

### Acceptance criteria → tests

Every criterion is covered by a test in `src/governor.rs` (`mod mock_poller_tests`) that drives
the real `run_governor_cycle` against `MockPoller` and asserts on the state the cycle persisted.

| Criterion | Test |
| --- | --- |
| `poller.poll()` is called | `test_cycle_polls_once_and_persists_polled_usage` (`poll_count == 1`), `test_second_cycle_repolls_and_computes_window_deltas` (`poll_count == 2`) |
| State updates are applied | `test_cycle_polls_once_and_persists_polled_usage`, `test_second_cycle_repolls_and_computes_window_deltas` (snapshot shift + window deltas) |
| Emergency brake triggers at 98% | `test_cycle_holds_emergency_brake_safe_mode_at_98_percent`, `test_cycle_clears_emergency_brake_safe_mode_below_98_percent`, `test_cycle_forecast_at_98_percent_forces_zero_target` |
| State is written to disk | `test_cycle_writes_state_to_disk_each_run` (state file, `.prev.json` rollover, `updated_at` advances) |
| Error handling when poll fails | `test_cycle_survives_poll_failure_and_keeps_previous_usage`; also `test_cycle_flags_stale_poll_data` |
| Uses mock poller + fixtures | All of the above use `MockPoller` plus `smoke_alert_config()` / `smoke_governor_config()` |

### Test runs

- `cargo test --lib mock_poller_tests` — 33 passed, 0 failed.
- `cargo test --test governor_cycle_behavior_test --test governor_cycle_snapshot_test` — 5 passed
  and 9 passed, 0 failed.

### Mutation check on the 98% threshold

The original commit claimed the brake tests *pin* `EMERGENCY_BRAKE_THRESHOLD` rather than merely
restate it. Verified directly: flipping `src/governor.rs:40` from `98.0` to `99.0` fails exactly
two tests —

```
test_cycle_forecast_at_98_percent_forces_zero_target
test_cycle_holds_emergency_brake_safe_mode_at_98_percent
```

— and nothing else. The constant was restored to `98.0` afterwards; the working tree carries no
source change.

## Known, deliberate coverage gaps

Unchanged from the original commit, and documented in a comment above the tests rather than
faked:

- The scaling *decision* reads live tmux worker counts (`worker::count_workers`), which is always
  0 in a test process.
- The `EmergencyBrake` decision arm requires `current > 0`, so it is unreachable in-process. The
  brake is pinned instead at the two points the cycle does reach: the forecast it persists, and
  the safe_mode hold/clear decision against the threshold.

## Outcome

No further code changes were required. This note is the commit for the re-dispatch.
