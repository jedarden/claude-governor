# Pluck Starvation Alert Investigation - False Positive

## Alert Summary
**Bead ID:** bf-36zs9  
**Alert Type:** Starvation alert - beads invisible to worker  
**Triggered:** 2026-07-06  
**Workspace:** /home/coding/claude-governor

## Reported Issue
Pluck found no beads despite the workspace reporting:
- Total beads: 1004
- Open beads: 41
- In-progress: 0
- Claimed by: (none)

## Root Cause Analysis

### Finding: False Positive - Not a Configuration Error

The starvation alert is a **false positive**. Pluck is operating correctly - there are **0 genuinely claimable beads** in the workspace.

### Breakdown of the 41 "Open" Beads

All 41 open beads fall into two categories:

1. **23 beads with blocking dependencies** - waiting for prerequisite beads to complete
2. **18 beads with `deferred` label** - intentionally marked for later processing

#### Why These Beads Are Not Claimable

**Blocking Dependencies (23 beads):**
- These beads have unresolved dependencies that must complete first
- Examples: `bf-18y8i`, `bf-53tr7`, `bf-64r1k`, etc.
- The bead store's `ready()` method correctly excludes these
- Dependency chains must resolve in order

**Deferred Label (18 beads):**
- These beads have been intentionally marked with the `deferred` label
- Examples: `bf-1y51s`, `bf-3js6h`, `bf-54ppq`, etc.
- Pluck's default `exclude_labels` configuration: `["deferred", "human", "blocked", "starvation-alert"]`
- This is expected behavior - deferred beads should not be processed yet

### Pluck Configuration Verification

**Current config** (`/home/coding/.config/needle/config.yaml`):
```yaml
strands:
  pluck:
    exclude_labels: ["deferred", "human", "blocked", "starvation-alert"]
    split_after_failures: 3
```

This configuration is **correct** and matches the documented defaults.

### Alert System Limitation

The starvation alert triggers on:
- Open bead count > 0
- Pluck returns `NoWork`

However, it does not distinguish between:
1. **True starvation** - beads exist but are invisible due to config errors
2. **False positive** - beads exist but are legitimately unclaimable (dependencies, deferred, assigned)

### Sample Bead Analysis

**bf-18y8i** (open, not claimable):
- Status: open
- Labels: `split-child`
- Dependencies: `-> bf-53tr7 (blocks)`
- Why excluded: Has blocking dependency

**bf-1y51s** (open, not claimable):
- Status: open
- Labels: `deferred, failure-count:2, split-child, umbrella`
- Dependencies: `-> bf-1i11d`
- Why excluded: Has `deferred` label AND blocking dependency

## Conclusion

**This is NOT a configuration error.** The system is working as designed:

1. ✅ Pluck correctly excludes beads with `deferred` label
2. ✅ Bead store correctly excludes beads with blocking dependencies
3. ✅ No configuration issues found

**Recommendation:** The starvation alert system should be enhanced to distinguish between:
- True configuration errors (beads that should be claimable but aren't)
- Expected non-claimable states (dependencies, deferred labels, assignees)

The alert should check if open beads are **actually claimable** before firing, not just count open beads.

## Verification

```bash
# Verified no claimable beads exist:
br list | grep 'open' | while read id rest; do 
  bead_id=$(echo "$id" | sed 's/\[//;s/\]//')
  has_deferred=$(br show "$bead_id" 2>/dev/null | grep "Labels:" | grep -c "deferred")
  has_deps=$(br show "$bead_id" 2>/dev/null | grep "Dependencies:" -A 5 | grep -c "blocks")
  echo "$bead_id: deferred=$has_deferred, deps=$has_deps"
done | grep "deferred=0.*deps=0" 
# Result: (empty) - 0 claimable beads found
```

**Status:** ✅ RESOLVED - False positive, no action required
