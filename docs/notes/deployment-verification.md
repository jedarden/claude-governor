# Deployment verification — reproducible current-state runbook

This is the procedure for answering "is the governor deployed and healthy on
this host right now?" It contains **no recorded evidence**: every claim about
current state comes from running the commands below at the moment you need the
answer. Point-in-time evidence belongs in dated bead notes (e.g.
[bf-48qtz.md](bf-48qtz.md), archived) — never here, and never in a fresh note
that a future reader might mistake for current state.

Every command is safe to run against a live deployment: doctor and status are
read-only diagnostics; `systemctl --user status`/`is-active`/`is-enabled` and
`journalctl` are read-only.

## Topology you are verifying

The daemon is a systemd **user** service. Depending on how recently the host
was migrated to the observe/act split, one of two topologies is live:

| Topology | Units | Notes |
|---|---|---|
| Split (current) | `claude-governor-observe.service` + `claude-governor-act.service` | Installed by `cgov enable`; observe and act are independently startable/stoppable (`cgov start observe`, `cgov start act`) |
| Monolith (legacy) | `claude-governor.service` | Pre-ADR combined unit; new installs remove it. A monolith unit running the enforcing daemon satisfies both the observe and act checks |
| Collector | `claude-token-collector.service` | Independent; feeds token usage data. Expected `enabled` and `active` |

Do not assume which topology a host is on — check (step 1), and treat a live
legacy monolith as a migration leftover to flag, not as a failure by itself.

## Step 0 — the warm-up rule

**After any (re)start, doctor output is not interpretable for ~30 minutes.**
Three checks are cold-start-degraded by design:

- `burn_rate_samples` — FAIL below 3 samples, WARN at 3–4, PASS at 5+. The
  check's own remediation is "run governor for at least 30 minutes".
- `prediction_accuracy` — WARN until 5+ predictions have been scored.
- `log_file` — WARN until the first daemon run creates the log.

So: start (or observe a restart of) the services → wait ≥30 minutes → *then*
run step 2 and apply the criteria below. A `burn_rate_samples` FAIL five
minutes after a restart is expected behaviour, not a deployment defect. This
is exactly the trap the July snapshot recorded as an "unresolved doctor
failure" — it was never unresolved; it needed warm-up.

## Step 1 — service checks

```bash
systemctl --user is-active  claude-governor-observe.service claude-governor-act.service \
                            claude-governor.service claude-token-collector.service
systemctl --user is-enabled claude-governor-observe.service claude-governor-act.service \
                            claude-token-collector.service
systemctl --user status claude-governor-observe.service claude-governor-act.service \
                        claude-token-collector.service --no-pager
```

Pass criteria:

- The units of the live topology are `active`; the collector is `active` and
  `enabled`.
- `Main PID` is recent relative to `Active:` (a unit that has been
  crash-looping shows many short `Duration:` entries — check
  `journalctl --user -u <unit> -n 50` for restart storms).
- If the legacy `claude-governor.service` is present it should be `inactive`;
  if it is `active` while the split units are also installed, flag the host as
  mid-migration.

## Step 2 — `cgov doctor`

```bash
cgov doctor                    # human-readable
cgov doctor --json             # machine-readable
echo $?                        # 0 = no FAIL-level checks; 1 = at least one FAIL
```

Exit-code semantics: **warnings never move the exit code; only FAIL-level
checks do.** A doctor run that prints `21 passed · 6 warning · 0 failed` exits
0 and counts as passing verification; a run with any `✗` exits 1.

`--skip-live` omits the live claude-print adapter probe (one trivial
subscription dispatch per adapter). Use it when you are verifying governor
deployment and do not want to spend subscription calls; the static
env-scrub check still runs. For a full deployment verification, run without
it at least once — the live probe is the only check that catches the
exit-127 dispatch failure mode.

Apply the criteria in the table below to each non-PASS line.

## Step 3 — `cgov status`

```bash
cgov status            # rich dashboard on a TTY, raw state JSON otherwise
cgov status --summary  # NEEDLE prompt-injection format
echo $?                # 0 = safe, 2 = cutoff risk in some window, 3 = emergency brake
```

Two caveats when interpreting status:

1. **Status reads the frozen state file, not the live daemon.** Check the
   `updated_at` field (JSON) against the clock. With the daemon stopped,
   status happily replays the last deciding cycle — `updated_at` age is the
   tell. Doctor's `state_freshness`/`observe_running` are the authority on
   liveness; status is the authority on *what the governor last decided*.
2. **`workers.current` can mislead when the daemon is down** — it reflects
   the frozen state, not a live count. Cross-check live workers with
   `cgov workers` (heartbeat-based) and `tmux ls`.

Exit codes 2/3 are meaningful and scriptable: 2 = one or more windows at
cutoff risk, 3 = emergency brake engaged. Neither is a deployment-verification
failure (the deployment is working well enough to be worried); both mean
"look at the forecast now".

## Pass/fail criteria

### Hard failures — must resolve before declaring the deployment healthy

Any of these in `cgov doctor` (exit 1) means not healthy, with one
scope-limited exception noted:

| Check | Fails when | First response |
|---|---|---|
| `config_parseable` | governor.yaml unreadable/invalid | `cgov config` |
| `sqlite_integrity` / `jsonl_db_sync` | DB corrupt or checkpoint drifting | stop, do not improvise; investigate before restarting anything |
| `state_freshness` | state file ≥600s old (WARN 120–600s) | if the daemon should be up: `cgov start observe` |
| `observe_running` | observe loop stopped while a state file exists | `cgov start observe` |
| `collector_running` | collector stopped and fleet data ≥900s old | `cgov start collector` |
| `api_reachability` | usage API unreachable | network/proxy first, daemon second |
| `pricing_coverage` | recent usage on unpriced models | add the named model(s) to `pricing.models` (figures are then auto-resolved, rounded up until exact entries exist) |
| `disk_space` | ≥95% used on the state dir (WARN 80–95%) | free space before the DB is at risk |
| `claude_print_adapters` | env-scrub static check or live dispatch fails | re-run `deploy/install-claude-print-adapters.sh` |
| `prediction_accuracy` | median error ≥10% with 5+ scored | check for unusual usage patterns; safe mode may activate |
| `burn_rate_samples` | <3 samples **and** uptime ≥30 min | only a failure outside the warm-up window — see step 0 |
| `oauth_token` | credential file unreadable/unparseable | re-auth; collector/daemon will recover. Expiring-soon is only a WARN (auto-refresh) |

The `observe_running` exception: a FAIL is *correct and expected* when the
operator has deliberately stopped the governor (maintenance, freeze,
decommission). It still exits 1 — a deliberate stop is a real state, not a
healthy one — but the remediation is "confirm the stop was intended and
record it", not "restart". Never restart the daemon to make a check green
without knowing why it was stopped.

### Expected warnings — documented postures, not defects

These WARNs are the system working as designed. Record them, don't chase them:

| Check | Warns when | Why acceptable |
|---|---|---|
| `act_running` | act paused (`cgov start act` not run) | Explicit safety posture: scaling/alerting stay gated until trusted. Observe keeps forecasting while paused |
| `alert_fp_telemetry` | <6 tracked alert types at 100 samples with <5% FP | Re-enablement bar for `alerts.auto_bead`; fills only while observe runs. 0% FP on everything is the *good* direction |
| `log_file` | log not yet created | Created on first daemon run; harmless pre-first-run |
| `disk_space` | 80–95% used | Advisory; act at ≥95% instead |
| `prediction_accuracy` | 5–10% median error | Calibrating; only ≥10% is a failure |
| `burn_rate_samples` | 3–4 samples | Cold start; passes at 5+ |
| `oauth_token` | expiring soon | Auto-refreshes; only a *failing* refresh is a problem |
| `collector_running` | systemd active but fleet data ≥300s old, or stopped with fleet data <900s old | Lag, not death. If it persists across two doctor runs, treat as hard failure |
| `state_freshness` | 120–600s old | Slow cycle; a second consecutive stale reading escalates it |

A verification result of "0 failed, only these warnings" is a **pass**.

### Cascade signature — one root cause, many failures

When the daemon is down, doctor fails in a characteristic cluster because
downstream checks consume the state the observe loop writes:
`observe_running` FAIL + `state_freshness` FAIL + `collector_running` WARN
("fleet Ns old" at the same age as the state file) + `prediction_accuracy`/
`burn_rate_samples` degrading as data ages. All of it resolves from one
remediation — start the loop and wait out the warm-up — so fix the root, then
re-run doctor once, rather than chasing each line.

## One-shot scripted verification

```bash
cgov doctor --json >/dev/null; d=$?
cgov status --summary >/dev/null; s=$?
echo "doctor=$d status=$s"   # healthy deployment: doctor=0; status 0/2/3 per forecast
```

`doctor=0` is the deployment-verification pass condition. A non-zero `status`
with `doctor=0` means the deployment is healthy but the *subscription* is
under pressure — different problem, different runbook.

## Worked interpretation example (non-authoritative)

Recorded 2026-09-25 on this host to show the criteria applied — by the time
you read this it describes nothing. Daemon had been deliberately stopped the
previous morning; doctor printed `18 passed · 5 warning · 4 failed`, exit 1:

- `observe_running` FAIL ("stopped, state 93342s old") and `state_freshness`
  FAIL (same age) — the cascade signature of the deliberate stop. Correct
  response: confirm the stop was intended, then either record the paused
  posture or start observe and warm up.
- `pricing_coverage` FAIL naming an unpriced model — a real config gap,
  independent of the stop. Fix in `pricing.models` regardless of daemon state.
- `prediction_accuracy` FAIL (median error ≥10%) on aged data — re-read after
  warm-up before believing it; accuracy on a stale state file says nothing
  about the current loop.

No PID, binary size, build timestamp, or disk figure was recorded — those are
the fields that made the July snapshot rot.
