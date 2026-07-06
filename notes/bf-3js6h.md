# Pluck Starvation Issue Reproduction

**Date:** 2026-07-06
**Bead:** bf-3js6h
**Workspace:** /home/coding/claude-governor

## Issue Summary

Pluck strand returns 0 candidates despite having 49 open beads available in the workspace.

## Evidence

### 1. Bead Count Verification

```bash
$ br list --status open | wc -l
49
```

All 49 open beads are eligible for Pluck (none have excluded labels like "deferred", "human", "blocked", "starvation-alert").

### 2. Log Evidence of Starvation

From `/home/coding/.needle/logs/needle-relaunch-claude-governor-cgov-1.stderr.log`:

```
2026-07-06T12:43:05.404136Z  INFO ... strand found candidates strand=pluck candidates=6 excluded=17 elapsed_ms=2
2026-07-06T12:43:05.814821Z  INFO ... strand found candidates strand=pluck candidates=5 excluded=18 elapsed_ms=2
2026-07-06T12:43:05.927291Z  INFO ... strand found candidates strand=pluck candidates=4 excluded=19 elapsed_ms=2
2026-07-06T12:43:06.139230Z  INFO ... strand found candidates strand=pluck candidates=3 excluded=20 elapsed_ms=2
2026-07-06T12:43:06.551055Z  INFO ... strand found candidates strand=pluck candidates=2 excluded=21 elapsed_ms=2
2026-07-06T12:43:06.662621Z  INFO ... strand found candidates strand=pluck candidates=1 excluded=22 elapsed_ms=2
2026-07-06T12:43:06.874733Z  INFO ... strand found candidates strand=pluck candidates=0 excluded=23 elapsed_ms=2
2026-07-06T12:43:07.095944Z  INFO ... strand found candidates strand=explore candidates=1 excluded=0 elapsed_ms=62
```

### 3. Starvation Event

**Time:** 2026-07-06T12:43:06.874733Z
**Event:** Pluck strand returned `candidates=0 excluded=23`
**Context:** 49 open beads available in workspace

This shows Pluck progressively filtering out beads until it found 0 candidates, despite having nearly 4 dozen open beads available.

## Analysis

The pattern shows:
1. **Progressive filtering:** Pluck went from 6 candidates → 0 candidates across multiple attempts
2. **Excluded count increased:** From 17 excluded → 23 excluded beads
3. **Explore strand worked:** Immediately after Pluck found 0 candidates, Explore strand found 1 candidate
4. **All beads eligible:** Current state shows all 49 open beads lack excluded labels

## Potential Root Causes

1. **Label-based filtering:** All beads may have acquired excluded labels during the timeframe
2. **Claimability filter:** Beads may be filtered by InProgress status or stale assignee
3. **Database state:** Bead store may have inconsistent state vs. JSONL checkpoint
4. **Workspace path issues:** Pluck may be looking at wrong workspace

## Next Steps for Investigation

1. Check bead labels during the starvation timeframe
2. Verify workspace path configuration in NEEDLE
3. Review Pluck's filtering logic in `/home/coding/NEEDLE/src/strand/pluck.rs`
4. Check for race conditions in bead claim/release cycles
5. Verify bead store database integrity

## Related Documentation

- Pluck configuration: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`
- NEEDLE source: `/home/coding/NEEDLE/src/strand/pluck.rs`
- NEEDLE config: `~/.needle/config.yaml`
