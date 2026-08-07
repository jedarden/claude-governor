# bf-38oc5 — stale-heartbeat handling: verified already implemented

**Outcome: no code change required.** The bead's static review was written against an
older tree; the behavior it asks for landed in earlier beads and is covered by tests.
This note records the verification so the next reader does not re-open the question.

## Where each requirement lives (`src/worker.rs`)

| Requirement | Implementation |
| --- | --- |
| 60s named threshold | `STALE_HEARTBEAT_THRESHOLD: i64 = 60` (line 20), compared against `Heartbeat::timestamp` in `read_heartbeats_with_sessions` |
| Stale + dead tmux session → remove file, log at info | `read_heartbeats_with_sessions` (~line 584): `fs::remove_file` with `log::info!` on success, `log::warn!` on failure |
| Stale + live session → treat as executing | same block sets `hb.is_idle = false`, so an outdated idle status can never drive a shutdown |
| Excluded from `heartbeat_count` | `count_heartbeat_files` → `read_heartbeats`; swept entries `continue` before insertion |
| Excluded from `find_workers_to_stop` candidates | `select_workers_to_stop` filters candidates against the live tmux session set before sorting |

`find_workers_to_stop` takes a single `tmux list-sessions` snapshot and threads it into
`read_heartbeats_with_sessions`, so the orphan sweep and the liveness filter agree rather
than racing two separate tmux queries.

## Test coverage

All acceptance criteria have dedicated tests, all passing:

- (a) stale + dead session → `test_stale_heartbeat_dead_session_removed`
- (b) stale + live session → `test_stale_heartbeat_live_session_retained_as_executing`,
  `test_find_workers_to_stop_excludes_stale`, `select_workers_to_stop_excludes_dead_sessions`
- (c) fresh unchanged → `test_fresh_heartbeat_unchanged_behavior`,
  `test_one_second_below_threshold_not_stale`, `test_stale_threshold_boundary`
- consistency recovery → `count_workers_recovers_consistency_after_orphan_cleanup`
  (asserts `consistent` flips false → true and the orphan file is gone), plus
  `test_count_workers_consistent_after_cleanup`
- mixed fleets → `test_mixed_stale_and_fresh_heartbeats`

The tmux-dependent tests create real sessions via the `TmuxSession` guard and skip
cleanly when tmux is unavailable, so they are not vacuous on a normal dev box.

`cargo test`: 760 lib tests + all integration suites pass, 0 failures.

## Out of scope, deliberately

`src/doctor.rs::check_heartbeat_consistency` also classifies heartbeats as fresh/stale,
but it is a read-only diagnostic — it reports counts and never removes files or feeds
scaling decisions, so the plan's sweep requirement does not apply to it.

## Prior beads that did the work

`c58175d` (initial implementation), `9fdbdcf` (bf-en75g: log removal with path, handle
failures), `81a50ea`, `ab9cc76`, `30fc13c` (verification notes).
