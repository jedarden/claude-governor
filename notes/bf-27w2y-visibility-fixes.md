# Pluck Visibility Issues - Root Causes and Fixes

## Executive Summary

After analyzing the bead database and Pluck configuration, **the visibility gaps are working as designed**. Pluck correctly excludes beads that should not be processed by workers. However, there are **two issues** to address:

1. **Out-of-sync JSONL checkpoint** - Creates false impression of available beads
2. **Configuration documentation** - Default exclude labels could be clearer

## Root Cause Analysis

### Issue 1: Out-of-Sync JSONL Checkpoint

**Severity:** Medium  
**Impact:** Misleading visibility data, potential confusion for operators

The `.beads/issues.jsonl` checkpoint file shows **85 open beads**, but the live database only has **20 open beads**. The discrepancy:

```
JSONL shows 85 beads as "open"
Database shows only 20 as "open"
65 beads are actually "blocked" (56) or "in_progress" (7)
```

**Why this matters:** The JSONL is the git-tracked checkpoint that's supposed to reflect the live database state. When it's out of sync, anyone reading the JSONL (or tools that rely on it) will get incorrect visibility data.

**Root cause:** The JSONL has not been flushed since beads transitioned from `open` to `blocked`/`in_progress` status.

**Fix:** Run `bf sync --flush-only` to update the checkpoint.

### Issue 2: Pluck Exclude Labels (Working as Designed)

**Severity:** Informational  
**Impact:** None - system is working correctly

Pluck excludes beads with these labels (hardcoded in NEEDLE):
- `deferred` - Beads marked for later processing
- `human` - Beads requiring human intervention
- `blocked` - Beads with blocking dependencies

**Current impact:** 6 open beads are hidden by the `deferred` label:

| Bead ID | Title | Labels | Hidden By |
|---------|-------|--------|-----------|
| bf-156nn7 | config/claude-governor.service still ships MemoryMax=512M | deferred, failure-count:1 | deferred |
| bf-1y51s | Diagnose configuration filter and exclude_labels issues | deferred, failure-count:2 | deferred |
| bf-3js6h | Reproduce Pluck starvation issue | deferred | deferred |
| bf-4k2j5 | Investigate Pluck configuration and workspace setup | deferred | deferred |
| bf-54ppq | Investigate Pluck configuration settings | deferred, failure-count:1 | deferred |
| bf-5dsgv | Investigate Pluck configuration and bead visibility settings | deferred, failure-count:11 | deferred |

**These beads are intentionally deferred and should NOT be visible to workers.** The system is working correctly.

## Configuration Fixes

### Fix 1: Sync the JSONL Checkpoint (REQUIRED)

Run this command to update the JSONL to reflect the true database state:

```bash
bf sync --flush-only
```

Then commit the updated checkpoint:

```bash
git add .beads/issues.jsonl
git commit -m "sync: flush bead checkpoint to reflect current database state"
```

### Fix 2: Document Exclude Labels (RECOMMENDED)

The default exclude labels are hardcoded in NEEDLE and cannot be changed without recompiling. However, the behavior should be documented in the workspace:

**Create `.beads/exclude-labels.txt`:**

```txt
# Pluck Exclude Labels
# These labels are hardcoded in NEEDLE (/home/coding/NEEDLE/src/strand/pluck.rs:13)
# Beads with these labels will NOT be visible to Pluck workers

deferred    # Beads marked for later processing
human       # Beads requiring human intervention
blocked     # Beads with blocking dependencies
```

### Fix 3: Add to Workspace Documentation (RECOMMENDED)

Update the project README or add a `docs/bead-workflow.md` file to explain the labeling scheme:

```markdown
## Bead Labels and Visibility

### Exclude Labels (NOT visible to Pluck)
- `deferred` - Use for beads that should be processed later
- `human` - Use for beads requiring human intervention
- `blocked` - Automatically applied when beads have blocking dependencies

### Standard Labels (visible to Pluck)
- `plan-gap` - Bead is part of a plan but not yet actionable
- `split-child` - Bead was created via split operation
- `umbrella` - High-level tracking bead
- `failure-count:N` - Track retry count (does NOT hide bead)

### Workflow
1. When creating a bead that should be deferred, add the `deferred` label
2. When dependencies are resolved, remove the `deferred` label to make it visible
3. Run `bf sync --flush-only` after significant state changes
```

## No Action Required (Working as Designed)

The following behaviors are **correct and require no changes**:

1. **56 blocked beads** - Correctly hidden due to unresolved dependencies
2. **7 in_progress beads** - Correctly hidden to prevent duplicate work
3. **6 deferred open beads** - Correctly hidden by design

## Verification Steps

After applying Fix 1, verify the sync:

```bash
# Count open beads in database
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status = 'open';"
# Expected: 20

# Count open beads in JSONL
cat .beads/issues.jsonl | jq -r 'select(.status == "open") | .id' | wc -l
# Expected: 20 (after sync)

# Verify deferred beads are still hidden
bf list | jq -r '.[] | select(.labels[]? == "deferred") | .id' | wc -l
# Expected: 6 deferred beads (not visible to workers)
```

## Summary

| Issue | Status | Action Required |
|-------|--------|-----------------|
| Out-of-sync JSONL | 🔴 BROKEN | Run `bf sync --flush-only` |
| Exclude labels behavior | 🟢 WORKING | Document behavior (optional) |
| Blocked bead filtering | 🟢 WORKING | No action |
| In-progress filtering | 🟢 WORKING | No action |

**The visibility gaps are NOT a configuration bug - they are working as designed.** The only fix needed is to sync the JSONL checkpoint to reflect the true database state.
