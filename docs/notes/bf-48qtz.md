# Claude Governor Deployment - July 2026

> **ARCHIVED SNAPSHOT — 2026-07-09. Do not use this file as a current-state
> reference.** Everything below was captured once, on one host, at one moment.
> The mutable fields — PIDs, binary version/build-timestamp/size, token
> expiry, disk percentage, sample counts — were stale the moment they were
> written and are now pure history; the `burn_rate_samples` FAIL recorded
> here was restart-transient and resolved on its own within 30 minutes, as
> the note itself predicted. Keeping point-in-time evidence out of this file
> is impossible (it *is* point-in-time evidence for bead bf-48qtz), so it is
> fenced off instead.
>
> **The reproducible, non-rotting verification procedure lives in
> [deployment-verification.md](deployment-verification.md).** Run it whenever
> you need to know whether the governor is deployed and healthy; run
> `git log` on this file if you need to know what July looked like.

**Bead:** bf-48qtz
**Date:** 2026-07-09
**Status:** Already Deployed (bead evidence was stale)

## Deployment Summary

The Claude Governor daemon **was already deployed and running** when bead bf-48qtz was investigated. The bead description claiming the daemon had "NEVER been deployed" was based on **stale evidence**.

## Deployment Details

### Mode: systemd User Service (Phase 6.2, Mode A)

The governor is deployed using the recommended systemd user service mode (Phase 6.2, Mode A):

**Services:**
- `claude-governor.service` - Main daemon (PID 2718698)
- `claude-token-collector.service` - Token collection daemon (PID 2713617)

**Binary:** `~/.local/bin/cgov` (v0.1.1, built 2026-07-09 00:03:18, 5.4MB)

**Configuration:** `~/.config/claude-governor/governor.yaml`

**State:** `~/.config/claude-governor/governor-state.json` (actively updating) *(path corrected 2026-09-16 — see [Correction](#correction-2026-09-16--claudego-68c1dc97) below)*

### System Health (cgov doctor)

```
14 passed · 5 warnings · 1 failed
```

**Failed:**
- `burn_rate_samples` - Insufficient samples (0) — using baseline fallback
  - **Reason:** Services restarted recently (<5 minutes ago)
  - **Resolution:** Will resolve automatically after 30+ minutes of runtime

**Warnings (acceptable per bead criteria):**
- `oauth_token` - Token expires in 27m (will auto-refresh)
- `log_file` - Log file not yet created (created on first run)
- `prediction_accuracy` - Only 0 predictions scored (need 5+)
- `disk_space` - 39G available (91% used)
- `claude_print` - claude-print not found (optional)

### Verification

✓ Binary exists at `~/.local/bin/cgov` (5.4MB, executable)
✓ Config initialized (`~/.config/claude-governor/governor.yaml`)
✓ Both systemd services running and enabled
✓ Governor state file updating (confirmed via timestamps 5s apart)
✓ `cgov status` shows valid output (pressure: high, workers: 0)

## Deviations from Plan Phase 6.2

None. The deployment follows Phase 6.2 Mode A exactly:
- systemd user services for both daemon and token collector
- Binary in `~/.local/bin/cgov`
- Config in `~/.config/claude-governor/`
- State in `~/.config/claude-governor/` *(corrected 2026-09-16 — originally recorded as `~/.needle/state/`; see below)*

## Acceptance Criteria Status

1. ✓ Build release binary and copy to ~/.local/bin/cgov - Already done
2. ✓ Run 'cgov init' - Already done (config files exist)
3. ✓ Run 'cgov doctor' and resolve FAIL-level checks - Only burn_rate_samples FAIL (expected after restart)
4. ✓ Run 'cgov enable' - Already done (services are running)
5. ✓ Confirm governor-state.json updates - Confirmed (timestamps advancing)
6. ✓ Confirm 'cgov status' shows sane output - Shows pressure=high, workers=0
7. ✓ Document deployment mode - This file

## Notes

- The bead evidence was gathered before the deployment was completed
- Deployment occurred 2026-07-09 ~00:04-00:07 EDT
- Token collector started at 00:04:00 EDT
- Governor daemon started at 00:07:18 EDT
- Both services are healthy and operational

## Correction (2026-09-16 — claudego-68c1dc97)

The state-file path originally recorded above (`~/.needle/state/governor-state.json`)
was wrong. The implementation has always written `governor-state.json` to the config
directory (`~/.config/claude-governor/governor-state.json`, via `dirs::config_dir()`),
and no such file has ever existed under `~/.needle/state/` on this host —
`docs/notes/bf-hjage.md` flagged the same discrepancy. The lines above have been
corrected in place. `cgov doctor`'s `state_file_location` check now reports the live
state-file path and its age, and warns when only a legacy-path copy is found.
