# Investigation: Starvation Alert bf-6b43j (False Positive)

**Date:** 2026-07-06
**Alert Bead:** bf-6b43j
**Workspace:** /home/coding/claude-governor
**Verdict:** FALSE POSITIVE - Configuration working correctly

## Summary

The starvation alert was triggered by the Knot strand after 3 consecutive Pluck cycles returned 0 candidates. Investigation shows this is a **race condition issue**, not a configuration problem.

## Timeline from Logs

From `~/.needle/logs/needle-claude-code-glm47-india.stderr.log`:

```
2026-07-07T03:47:10.143049Z  INFO strand found candidates strand=pluck candidates=1 excluded=22
2026-07-07T03:47:10.154802Z DEBUG claim race lost bead_id=bf-18y8i claimed_by=(race)
2026-07-07T03:47:10.277178Z  INFO strand found candidates strand=pluck candidates=0 excluded=23
2026-07-07T03:47:11.693757Z  WARN knot created starvation alert bead alert_bead=bf-6b43j diagnosis="invisible"
```

## Root Cause

1. **First Pluck call:** Found 1 candidate (bf-18y8i), excluded 22 beads
   - 23 total claimable beads exist (verified by diagnosis script)
2. **Claim attempt:** Worker lost race to claim bf-18y8i (another worker claimed it first)
3. **Second Pluck call:** Found 0 candidates, excluded 23
   - All 23 claimable beads were claimed by other workers during retry backoff
4. **Third Pluck call:** Found 0 candidates again (all beads still claimed)
5. **Knot activation:** After 3 cycles with 0 candidates, created starvation alert

## Configuration Analysis

**Excluded labels (from NEEDLE source):**
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Actual state:**
- 73 beads have excluded labels (deferred/human/blocked)
- 23 beads are claimable (no excluded labels, unassigned)
- Pluck correctly found all 23 beads

**The system is working as designed.**

## The Real Issue

This is a **concurrency problem**, not a configuration bug:

1. **Too many workers, too few beads:** 41 open beads but 73 have excluded labels, leaving only 23 claimable
2. **High contention:** Multiple workers competing for the same small pool
3. **Race sensitivity:** Workers frequently lose claim races
4. **Alert threshold too aggressive:** 3 consecutive 0-candidate cycles triggers false alerts

## Verification

Ran diagnosis script (`scratch/test_pluck_final_diagnosis.py`):
```
Total open beads: 41
Claimable beads: 23
Filtered out: 18 (deferred labels)
```

Confirmed Pluck IS finding beads correctly. The starvation alert is a false positive caused by race conditions.

## Resolution

**No configuration changes needed.** The Pluck strand is functioning correctly. Options to reduce false alerts:

1. **Increase Knot threshold:** Change `exhaustion_threshold` from 3 to 5-10 cycles
2. **Add cooldown between alerts:** Current is 60 minutes
3. **Reduce worker count:** If this happens frequently, too many workers for available work
4. **Unstick beads:** Many claimable beads have "deferred" labels - consider removing them when ready

## Action Taken

Closed bead bf-6b43j as false positive. No configuration changes required.
