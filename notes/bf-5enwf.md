# bf-5enwf — Full verification and regression check: safe-mode warning

Verification of the safe-mode manual-scale messaging (log warning + stdout notification)
introduced across the preceding beads in this chain.

## Summary

All acceptance criteria met. The full suite is green with no regressions, both messages were
manually confirmed against the real `cgov` binary, and the reassert claim was traced to the
code path that makes it true.

One real gap was found and closed: the pre-existing **log message** test did not verify
anything. See "Gap found" below.

## 1. Regression check — full suite

`cargo test --all` (run locally via `~/.cargo/bin/cargo`; see "Note on the toolchain"):

| Target | Result |
| --- | --- |
| lib + bin unit tests | 723 passed, 0 failed |
| 14 integration test binaries | 100 passed, 0 failed, 3 ignored |
| doc tests | 13 passed, 0 failed, 5 ignored |
| **Total** | **823 passed, 0 failed, 8 ignored** |

No regressions. The suite was run both before and after the test additions below; both green.

Pre-existing (unrelated) noise, left untouched: ~30 `unused import` / `unused variable`
warnings, and `cargo fmt --check` drift in `src/governor.rs`, `src/poller.rs`, `src/main.rs`,
and four `tests/pluck_*` files. None of it is from this chain of work. The one file this bead
touched is `rustfmt`-clean.

## 2. Targeted tests

Log message (`src/main.rs` unit tests) — 3 passed:

- `tests::test_scale_safe_mode_warning_log_message`
- `tests::test_scale_without_safe_mode_no_warning`
- `tests::test_scale_safe_mode_stdout_notification`

Stdout notification — 11 passed across two binaries:

- `tests/safe_mode_stdout_notification_test.rs` — 5 passed
- `tests/scale_safe_mode_stdout_test.rs` — 6 passed (3 pre-existing + 3 added here)

## 3. Manual verification

Ran the real `cgov` binary against an isolated temp `HOME`/`XDG_*` tree seeded with safe mode
active and one agent at `target: 2`, `min: 1`, `max: 10`:

```
$ cgov scale 4
Target worker count set to 4 for all agents
NOTE: Safe mode remains active and will reassert its target on the next cycle
```

```
$ cat $XDG_DATA_HOME/claude-governor/governor.log
2026-08-05T13:32:59.381797138+00:00 [governor] WARN: manual scale override during safe mode
```

Confirmed:

- **Order** — the WARN is emitted first (`run_scale_command`, `src/main.rs:679`, before
  validation), the stdout NOTE last (`src/main.rs:722`, after the scale is persisted). The
  operator reads "what happened" before "what happens next".
- **Routing** — the WARN goes to the log only and never appears on stdout; the NOTE goes to
  stdout. They target different audiences.
- **Format** — both strings match the specification byte for byte; the log line carries an
  RFC3339 timestamp prefix.
- **Truthfulness** — after the scale, persisted state still has `safe_mode.active == true` and
  `workers["test-agent"].target == 4`. The scale really applied and safe mode really persisted.
- **Negative control** — with safe mode inactive, neither message is emitted.

### Safe mode reasserts on the next cycle

`compute_target_workers` (`src/governor.rs:3889`) derives the next target from each worker's
`min`/`max`/`current` plus the capacity forecast — it never reads `worker.target`. The manually
scaled value is therefore not an input to the next cycle and is recomputed away, under
safe-mode-tightened hysteresis and ceilings (`src/governor.rs:5169-5230`). This is the
mechanism that makes the notification's promise true, and it is now pinned by a test.

## 4. Gap found and closed

`test_scale_safe_mode_warning_log_message` in `src/main.rs` **re-implements the production
logic inside its own test body** — it appends the warning line to a temp log itself, then
asserts the line is present. It cannot fail on a regression.

Demonstrated by mutation: deleting the `append_to_governor_log(...)` call from
`run_scale_command` leaves that test **passing**. The same tautology affects
`tests/safe_mode_stdout_notification_test.rs`, which asserts against an in-test `Cursor` buffer
it wrote itself.

Three tests were added to `tests/scale_safe_mode_stdout_test.rs`, which drives the real binary:

- `scale_during_safe_mode_writes_warning_to_log_file` — asserts the WARN lands in the real
  `governor.log` with a parseable RFC3339 timestamp, and does not leak to stdout.
- `scale_without_safe_mode_writes_no_warning_to_log_file` — negative control.
- `manual_scale_target_does_not_influence_next_cycle_target` — pins the reassert invariant by
  asserting the manual target has *no* influence on `compute_target_workers`, rather than
  pinning a specific number that forecast heuristics would churn.

Each was mutation-verified to fail on a real regression:

| Mutation | New test | Old test |
| --- | --- | --- |
| Remove `append_to_governor_log` from `run_scale_command` | **FAILED** (caught) | passed (missed) |
| Make `compute_target_workers` honour `ws.target` | **FAILED** (caught) | n/a |

Both mutations were reverted; `git diff` on `src/` is empty.

The tautological tests in `src/main.rs` and `tests/safe_mode_stdout_notification_test.rs` were
left in place — they are harmless, and rewriting them is out of scope for a verification bead.
Worth a follow-up.

## Note on the toolchain

`cargo` on `PATH` is `/home/coding/.local/bin/cargo`, a wrapper that offloads `cargo test` to
remote CI (`cargo-remote` → Argo). The first full-suite run went remote and passed; a
subsequent targeted run failed to schedule ("pod never started"), an infrastructure flake
unrelated to the code. All results above were therefore reproduced locally with
`~/.cargo/bin/cargo`, which bypasses the wrapper.
