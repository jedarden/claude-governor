# Governor daemon on ex44: why it was off, and the observe-only posture it came back in

**Bead:** bf-hjage
**Date:** 2026-08-06
**Host:** `hetzner-ex44`
**Status:** Resolved — daemon re-enabled in observe-only (`--dry-run`) mode; full Act deliberately still gated (conditions below)

## What the bead asked

`claude-governor.service` had been `disabled`/`inactive` with `governor-state.json` frozen at
`2026-06-28T19:51:09Z` (39 days stale by the time this was worked). Investigate why, then either
restart the daemon or record the decision to keep it off.

## What the investigation actually found

The "why was it stopped" question turned out to be less important than a second problem that the
investigation surfaced: **the deployed binary could no longer read the usage API at all.**

### 1. The stop reason is not recoverable

- `journalctl --user -u claude-governor.service` had **zero** entries. `journalctl --user
  --list-boots` retains exactly one boot, first entry `2026-08-03 17:59` — the user journal does not
  reach back to June. Nothing about the stop survives.
- Timeline: state froze `2026-06-28 15:51 EDT`; the system booted `2026-06-29 09:55` and has been up
  since (`uptime`: 38 days). The unit was `disabled`, so a `WantedBy=default.target` unit does not
  come back on boot, and `Restart=always` only covers a process that dies while the unit is loaded —
  neither would have restarted it.
- `docs/notes/bf-48qtz.md` (2026-07-09) reports the daemon running with a PID after that reboot, and
  writing state to `~/.needle/state/governor-state.json` — a path that holds no such file today. So
  it was most likely started by hand at least once after the reboot without being `enable`d, then
  lost again. Either reading (disabled at the reboot, or started-by-hand-then-stopped) is consistent
  with the evidence; there is no record that distinguishes them.
- No correlation with the `alerts.auto_bead: false` decision (docs-878a) could be established, and
  none is needed to explain the outage: an un-`enable`d unit plus a reboot is sufficient.

### 2. The deployed binary was 3.5 months stale and could not poll

`~/.local/bin/cgov` was built **2026-04-23** (v0.1.0). Against the current API:

```
$ cgov poll
Error: Failed to parse response: invalid type: null, expected struct UsageWindow at line 1 column 365
```

The usage endpoint now returns `null` for a window; the April binary deserialises the windows as
required structs. Current `src/poller.rs` already handles this (`Option<UsageWindow>` +
`window_or_default`), so the source was fine — only the deployment was stale.

This makes restarting-as-was actively harmful, not merely useless. A trial run of the old binary
rewrote `governor-state.json` with a **fresh `updated_at` and June's usage numbers verbatim**
(`sonnet_pct: 42.0`, `five_hour_resets_at: 2026-06-28T23:20`, `stale: false`) — i.e. it would have
flipped `doctor`'s `state_freshness` check green while every forecast underneath it was 39 days old.
The pre-restart snapshot is preserved at
`~/.config/claude-governor/governor-state.stale-20260628.json.bak`.

The April binary also predates two fixes that matter here:

- `is_cutoff_alert_consistent()` (f5750d2, 2026-04-23) — the FP guard from `bf-h7toj`. Confirmed
  absent from the installed binary.
- `safe_worker_count_or_hold()` (99d2d54, 2026-07-22, ADR-002) — the installed binary still contains
  the string `"insufficient burn rate data, using max_workers as ceiling"`, i.e. the pre-ADR-002
  behaviour where a zero-sample restart targets `max_workers` immediately.

### 3. Full Act would have been unsafe for a config reason, independent of the binary

`agents.needle-sonnet` on this host is:

```yaml
launch_cmd: "needle run --agent claude-code-glm-5 --workspace {workspace} --session-prefix needle-cgov"
session_pattern: "needle-cgov-*"
min_workers: 0
max_workers: 8      # 8 + sprint.max_workers_boost (3) under an active sprint
```

- `{workspace}` is substituted with the daemon's **current working directory**
  (`src/worker.rs:200-206`, `std::env::current_dir()`). Under a systemd user unit with no
  `WorkingDirectory=`, that is `/home/coding` — not a bead workspace. Governor-launched workers would
  run `needle` against the wrong directory.
- `session_pattern: needle-cgov-*` matches **zero** live sessions. The 16 workers actually running on
  this host are `needle-claude-code-glm-4_7-*` in `/home/coding/aide-de-camp`, launched and managed
  outside cgov. cgov therefore sees `current = 0` and would scale up *on top of* that fleet rather
  than managing it.

So even with a correct binary, un-gated scaling would have launched up to 8 misconfigured workers
alongside an existing 16-worker fleet.

## What was done

1. **Redeployed the binary.** Built `--release` from the current tree (v0.1.1) and installed it over
   `~/.local/bin/cgov` via rename (safe while the collector holds the old inode). Old binary kept at
   `~/.local/bin/cgov.bak-20260423`. `cgov poll` now succeeds (5h 8%, 7d 47%, weekly_scoped 2%).
2. **Re-enabled the daemon observe-only.** `~/.config/systemd/user/claude-governor.service`
   `ExecStart` is now `cgov _daemon --dry-run`, with a comment pointing at this note, then
   `systemctl --user daemon-reload && enable --now`.

   `--dry-run` gates exactly four blocks in `run_governor_cycle` (`src/governor.rs:6359, 6431, 6487,
   6552`): reconcile-launch, scale up, graceful scale down, and the emergency-brake kill. Polling,
   the collector pass, burn-rate EMA, capacity forecast, the confidence cone, calibrator scoring,
   safe-mode evaluation and state persistence all still run. That is ADR-001's Observe loop,
   available today with no code change.
3. **Verified.** One live cycle logs `polled usage: Fable=2.0%, all_models=47.0%, 5h=8.0%` →
   `target workers: 0` → `no scaling action this cycle (dry-run)` → `cycle complete`. `tmux ls |
   grep needle-cgov` is still 0 sessions.

`cgov doctor` before → after:

| | before | after |
|---|---|---|
| passed | 11 | 18 |
| warning | 2 | 1 |
| failed | 4 | 2 |

`daemon_running`, `state_freshness`, `collector_running` and `pricing_coverage` now pass. The two
remaining failures are expected and self-clearing: `burn_rate_samples` needs ~30 min of EMA samples,
and `prediction_accuracy` (15% median error) is now scored over 342 real predictions instead of 10
stale ones — it needs fresh cycles to move, which is exactly what the blackout prevented.

## Why Act stays gated, and what unlatches it

Observe-only is a posture, not an oversight. Turning the Act half on requires an operator decision
that is out of scope for this bead, because it means cgov starts spending subscription quota on a
host whose fleet it does not currently manage. Remove `--dry-run` only once **all** of these hold:

1. `launch_cmd` no longer depends on the daemon's cwd — either an absolute `--workspace` path, or
   `WorkingDirectory=` set on the unit.
2. The `session_pattern` / live-fleet mismatch is resolved: either cgov is given its own pool to
   manage, or its pattern is pointed at the pool that actually runs, with agreement that cgov (not
   the current external launcher) owns scaling it. Two autoscalers on one pool is worse than none.
3. `burn_rate_samples` passes and `prediction_accuracy` is inside threshold on fresh data — i.e. the
   telemetry that was starved by the blackout has actually recovered.

`alerts.auto_bead` stays `false` and is *not* blocked on this: alerts are logged either way
(`src/alerts.rs:312-326`), and its own re-enable condition (FP rate < 5% over a 100-alert window) can
now finally accumulate, because the daemon that grows that counter is running again.

## Footguns left in place, deliberately

- `cgov.service` still exists as a stale duplicate unit (`ExecStart=cgov daemon`, no `--dry-run`,
  `Restart=on-failure`). It is `disabled`; enabling it would start a second, fully-Act daemon.
  Not removed here — deleting an operator's unit file is their call, not this bead's.
- `cgov enable` / `cgov init` install the unit from `config/claude-governor.service`, which has the
  plain `ExecStart`. They skip existing files unless `--force`, so the normal path is safe, but
  `cgov enable --force` **will** silently revert the observe-only ExecStart.

Both of these are arguments for ADR-001 (`docs/plan/plan.md:2078`): as long as Observe and Act share
one unit and one flag, the posture lives in a hand-edited `ExecStart` that any `--force` reinstall
can undo. The `--dry-run` unit edit is the bridge; `cgov _observe` / `cgov _act` as separately
supervised units is the fix.
