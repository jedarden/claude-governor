# bf-21swe — Verify safe-mode warning message fix works correctly

Final verification of the `cgov scale` safe-mode messaging chain (bf-3xthw, bf-5enwf).

## What was verified

Ran the **real `cgov` binary** end-to-end against a fully isolated home
(`HOME` + all `XDG_*` vars pointed at a temp dir), so nothing in the developer's
`~/.config/claude-governor` or `~/.local/share/claude-governor` was read or written.

### 1. Safe mode active — `cgov scale 4`

State seeded with `safe_mode.active = true`, `trigger = "median_error"`, worker
`test-agent` at `current: 2, target: 2, min: 1, max: 10`.

```
$ cgov scale 4
Target worker count set to 4 for all agents
NOTE: Safe mode remains active and will reassert its target on the next cycle
```

stderr (the `log::warn!` sink):

```
[2026-08-05T13:38:46Z WARN  cgov] [governor] WARN: manual scale override during safe mode
```

`governor.log` (the persisted audit record, written via `append_to_governor_log`):

```
2026-08-05T13:38:46.170178895+00:00 [governor] WARN: manual scale override during safe mode
```

Exit code `0`.

### 2. Message content, order, and routing

| Check | Result |
| --- | --- |
| Log warning text is exactly `[governor] WARN: manual scale override during safe mode` | ✅ |
| Log line carries an RFC3339 timestamp prefix | ✅ |
| Stdout notification about reasserting appears | ✅ |
| Confirmation precedes the `NOTE:` line (what happened, then what happens next) | ✅ |
| The `WARN` line stays out of stdout; the `NOTE` line stays out of the log | ✅ |

The two messages are addressed to different audiences and are correctly separated:
the `WARN` is the operator's audit trail, the `NOTE` is the terminal message.

### 3. State after the scale

```
safe_mode.active  = True
safe_mode.trigger = median_error
worker target     = 4
```

The scale is genuinely applied *and* safe mode genuinely stays active — so the
notification is not a lie.

### 4. Negative control — safe mode inactive

```
$ cgov scale 3
Target worker count set to 3 for all agents
```

No `NOTE:` line, no `WARN` on stderr, and `governor.log` was never created. This
is what makes the positive result meaningful: the messaging is conditional on
safe mode, not printed unconditionally by every `scale`.

### 5. No regression — safe mode still reasserts on the next cycle

`compute_target_workers` derives the next target from each worker's
`min`/`max`/`current` and the capacity forecast; it never reads `worker.target`.
Confirmed empirically with a populated binding-window forecast
(`safe_worker_count_p75 = 3`, wide cone): manual targets of `2`, `4`, `9`, and
`10` all recompute to `3`. The manual override survives exactly one cycle, as the
notification promises.

### 6. Test suite

`cargo test` — **all suites pass, 0 failures** (723 unit tests + 15 integration
suites + 13 doctests).

## Gap found and closed

The pre-existing reassert test (`manual_scale_target_does_not_influence_next_cycle_target`)
builds its state from `make_state`, whose `capacity_forecast` is **empty**. With no
forecast, `compute_target_workers` short-circuits to a "hold at current" path — so
that test never exercised the forecast-driven branch that does the actual
reasserting, and it only ever compared two runs against each other rather than
against a known value.

Added `safe_mode_reasserts_forecast_derived_target_over_manual_scale` to
`tests/scale_safe_mode_stdout_test.rs`, which populates the binding window and
pins the recomputed target to a concrete third value (`3`) that neither a leaked
manual target (`4`/`9`/`10`) nor the hold-at-current fallback (`2`) could produce.

## Conclusion

All acceptance criteria met. Both messages work correctly, in the right order and
format, on the right streams; safe mode reasserts on the next cycle; no
regressions. The parent bead can be closed.
