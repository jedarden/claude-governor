# Restart Governor Daemon and Collector

**Date:** 2026-05-03
**Bead:** bf-40duv

## Problem

The governor daemon was stopped (state 742594s stale, ~7 days). The token collector was also stuck (742593s old).

## Actions Taken

1. Verified log directory exists at `~/.local/share/claude-governor/`
2. Ran `cgov init` to ensure all directories and service files were correct
3. Started both services with `cgov start`
4. Verified health with `cgov doctor`

## Result

Both services are now running and healthy:
- `claude-governor.service`: active (running), state fresh (22s old)
- `claude-token-collector.service`: active (running), fleet 20s old
- Governor is targeting 8 workers (90% ceiling)

## Notes

The log directory already existed but was empty. After starting the daemon, `cgov doctor` shows all critical checks passing:
- daemon_running: ✓
- collector_running: ✓
- state_freshness: ✓
