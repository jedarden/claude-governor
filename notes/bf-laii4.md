# bf-laii4 — Delta test suite: warnings, runtime, redundancy

Closes the non-functional acceptance criteria on the delta-computation test
work (parent `bf-g9mg9`, after children `bf-14u9s`, `bf-1o5pq`, `bf-410er`).

All measurements taken 2026-08-06 with `~/.cargo/bin/cargo` directly, because
this repo's `cargo` wrapper can swallow stderr and report a false success.

## 1. Warnings — zero attributable to the delta tests

`cargo test --no-run --all-targets` emits **38 warnings** across the workspace,
extracted by primary span from `--message-format=json` so each is pinned to a
file and line rather than counted from the summary lines.

Not one of them lands in delta-test code. The delta test surface is:

| Location | Warnings |
| --- | --- |
| `src/governor.rs` `window_delta_tests` (lines 1569–3929) | 0 |
| `src/governor.rs` `mock_poller_tests` (line 10022+) | 0 |
| `tests/delta_logging_runtime_test.rs` | 0 |
| `tests/governor_cycle_snapshot_test.rs` | 0 |
| `src/snapshot_fixtures.rs` | 0 |

Where the 38 actually live — all pre-existing and unrelated to this work:

- `src/governor.rs` ×17 — production code and the cold-start / seeding tests
  (`is_structurally_inactive` dead, `total_tmux_count` write-only,
  six `unnecessary parentheses`, unused `composite_risk_config`,
  `cone_scaling_config`, `target_ceiling`, `std_pct_hr_seeded`, `baseline`,
  `weekly_scoped_model_at_startup`, `first_poll_model`)
- `src/burn_rate.rs` ×4, `src/alerts.rs`, `src/capacity_summary.rs`,
  `src/narrator.rs`, `src/poller.rs` ×1 each
- `tests/first_startup_cold_start_test.rs` ×6,
  `tests/weekly_scoped_model_rotation_test.rs` ×5,
  `tests/safe_mode_stdout_notification_test.rs` ×2

The count was 38 before the removals below and 38 after, so the dedup introduced
no new warning and silenced none — the delta tests were already clean, and the
unused fixtures from gap G3 do not warn because `snapshot_fixtures` is a `pub`
module.

## 2. Runtime — 2.96 s for the delta subset, under the 5 s bar

Measured against the compiled test binaries directly (three runs each, so
cargo's own freshness check is not counted as test time).

| Subset | Tests | Wall clock |
| --- | --- | --- |
| `window_delta_tests` (delta arithmetic + rendering) | 45 | **5 ms** |
| All lib tests matching `delta`, excluding `mock_poller_tests` | 58 | **84–98 ms** |
| ... plus `delta_logging_runtime_test` + `governor_cycle_snapshot_test` | 68 | **2.92–2.96 s** ✅ |
| ... plus the 5 end-to-end `mock_poller_tests` delta tests | 73 | 9.08–9.23 s ❌ |

**The delta test subset runs in 2.96 s.** Nearly all of it is one test:
`tests/delta_logging_runtime_test.rs` at 2.87 s; the other 67 tests together
account for under 90 ms.

### The end-to-end cycle tests are the exception, and not on delta arithmetic

Five tests in `mock_poller_tests` match the name `delta` and push a
name-matched sweep to 6.2 s (lib only). Per-test cost:

| Test | Cycles | Time |
| --- | --- | --- |
| `test_delta_fields_across_governor_cycles` | 3 | 5725 ms |
| `test_cycle_computes_negative_deltas_when_windows_reset` | 2 | 2912 ms |
| `test_second_cycle_repolls_and_computes_window_deltas` | 2 | 2891 ms |
| `test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing` | 1 | 1480 ms |
| `test_first_cycle_does_not_fabricate_deltas_without_a_previous_snapshot` | 1 | 1449 ms |

The cost is flat at ~1.4 s per `run_governor_cycle` call and is not delta
arithmetic: `run_governor_cycle` calls `collector::run_collection_pass()`
unconditionally (`src/governor.rs:4755`), which reads the real `~/.claude`
JSONL history and the real SQLite DB on every cycle. `dry_run` does not gate it.
There are no sleeps in these tests.

So these are end-to-end governor-cycle tests that happen to assert on delta
fields, not delta-computation tests. Making them fast means injecting the
collector into `run_governor_cycle` — a production-code change, out of scope
here. Flagging rather than fixing; worth its own bead if the cycle tests become
a bottleneck.

## 3. Redundancy — five tests removed

The `bf-14u9s` audit surfaced gaps G1–G4 but no redundancy, so this pass
re-read the 50 `window_delta_tests` bodies directly. Five were strictly
subsumed by another test in the same module and were removed (−278 lines):

| Removed | Already covered by |
| --- | --- |
| `test_first_poll_no_previous_snapshot` | `test_first_poll_reports_no_deltas` — same `(None, Some)` call on the same 10/20/15 reading; the extra assertion was `previous.is_none()` on a literal `None` |
| `test_delta_computation_skipped_on_first_poll` | `test_missing_baseline_first_poll_yields_none_in_every_field` — same 25/45/35 reading, plus per-window assertions and the fabricated-baseline contrast |
| `test_no_snapshots_available_no_panic` | `test_missing_both_snapshots_yield_none_in_every_field` — the `(None, None)` pairing |
| `test_previous_snapshot_without_current_no_panic` | `test_missing_current_snapshot_yields_none_in_every_field` — the `(Some, None)` pairing |
| `test_second_poll_with_both_snapshots` | `test_calculate_window_pct_delta_basic` + `test_both_snapshots_present_match_calculate_window_pct_delta` |

The last one is the notable removal: 120 lines that re-implemented the match
arms of `window_deltas_from_snapshots` in the test body and asserted against
that copy, so it could not have failed if the real function changed. The only
arithmetic it exercised was the 10/20/15 → 12.5/22/18 case already covered by
`test_calculate_window_pct_delta_basic`.

All four `Option` pairings of `window_deltas_from_snapshots` remain covered.
The coverage map doc comment above `window_delta_tests` was updated: the removed
names are struck from case 2 and a "Deduplication" section records each removal
against its surviving cover, so a later reader can tell a deliberate deletion
apart from silently dropped coverage.

Deliberately kept despite surface similarity:

- `test_first_poll_reports_no_deltas_regardless_of_current_values` — table-driven
  over four value sets including all-zero, not a single case.
- `test_identical_snapshots_zero_deltas` vs
  `test_identical_fixture_snapshots_produce_zero_deltas` — same shape, but one
  uses hand-written literals and the other `baseline_snapshot()`; keeping both
  preserves the fixture-drift guard alongside a fixture-independent check.

## 4. No regressions

```
cargo test --all-targets
  lib                                    746 passed;  0 failed  (9.32s)
  delta_logging_runtime_test               1 passed;  0 failed  (2.87s)
  governor_cycle_snapshot_test             9 passed;  0 failed  (0.00s)
  fixtures                                12 passed;  0 failed
  weekly_scoped_model_rotation_test       11 passed;  0 failed
  governor_cycle_behavior_test              5 passed;  0 failed
  scale_safe_mode_stdout_test               7 passed;  0 failed
  safe_mode_stdout_notification_test        5 passed;  0 failed
  continuously_calibrated_regression_test   3 passed;  0 failed
  heartbeat_orphan_cleanup_test             3 passed;  0 failed
  pluck_workspace_mismatch_test             2 passed;  0 failed
  pluck_db_test / pluck_filter_combinations / test_workspace_path_formats /
  first_startup_cold_start_test             1 passed each; 0 failed
                                                       (3 ignored)

cargo test --doc                          16 passed;  0 failed  (5 ignored)
```

Lib count moved 751 → 746, exactly the five removals. Zero failures anywhere;
full suite wall clock 12.4 s.

## Verdict against the acceptance criteria

- ✅ Zero warnings attributable to the delta tests (38 workspace-wide, all
  located elsewhere, count unchanged by this work).
- ✅ Delta test subset runs in **2.96 s**, under 5 s. The 6.2 s figure for a
  naive `cargo test delta` name match comes from five end-to-end governor-cycle
  tests whose time is `collector::run_collection_pass()`, recorded above.
- ✅ No test regressions: 746 lib + 62 integration + 16 doctests, 0 failures.
