# Pluck Configuration Investigation - Complete Findings

**Bead ID:** bf-3fi8d  
**Date:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`  
**Purpose:** Comprehensive documentation of Pluck configuration settings and root cause analysis of bead visibility issues

## Executive Summary

Pluck's configuration is **correctly set up** but bead invisibility issues stem from **workspace path mismatches** during worker execution. The filtering logic is moderately restrictive, functioning as designed, but workers may query the wrong bead store database.

## Current Database State

**As of 2026-08-03:**
- **Total beads**: 1,208
- **Open beads**: 21  
- **Unassigned open beads**: 16
- **Available to Pluck**: 10-16 (after label filtering)

## Complete Configuration Documentation

### 1. Exclude Labels Configuration

**Source:** Compiled into NEEDLE binary (`/home/coding/NEEDLE/src/strand/pluck.rs:13`)

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Current Values:**
- `deferred` - Beads postponed for later processing
- `human` - Beads requiring human intervention  
- `blocked` - Beads with blocking dependencies

**Behavior:**
- Default labels apply when no custom override is configured
- Filtering is applied twice: once via bead store query, once defensively in the strand
- Custom exclude_labels override defaults completely (not merged)

**Impact:** From test data, **81 beads were excluded** by label filters from a pool of 1,208 total beads.

### 2. Workspace Path Configuration

**Default Workspace:**
```yaml
# From ~/.needle/config.yaml
workspace:
  default: /home/coding/NEEDLE
```

**Target Workspace:**
- **Path:** `/home/coding/claude-governor`
- **Bead store:** `/home/coding/claude-governor/.beads/`
- **Database:** `beads.db` (4.3 MB, 1,208 beads)
- **Checkpoint:** `issues.jsonl` (1.1 MB)

**Workspace Resolution:**
1. Pluck uses `std::env::current_dir()` when no explicit `--workspace` argument provided
2. Can be overridden with CLI flags: `--workspace` or `-w`
3. Explore strand discovers additional workspaces via filesystem traversal

### 3. Filter Pipeline Architecture

Pluck applies filters in a specific sequence during `evaluate()`:

#### Stage 1: Store Query Filters
```rust
Filters {
    assignee: None,                    // Unassigned beads only
    exclude_labels: ["deferred", "human", "blocked"],
    exclude_ids: HashSet::new(),
}
```

#### Stage 2: Defensive Label Filtering
- **Double-check:** Verifies store didn't miss excluded labels
- **Reason:** Some `bf ready --json` outputs omit label data
- **Prevents:** SELECTING→CLAIMING→RETRYING hot loop
- **Filters:** Any bead with labels matching exclude_list

#### Stage 3: Status & Assignee Filtering
Removes beads that are:
- `InProgress` status (already claimed by another worker)
- `Open` status with non-NULL assignee (stale assignments)

#### Stage 4: Metadata Filters
- `ephemeral = 0` - Excludes temporary/transient beads
- `pinned = 0` - Excludes administratively pinned beads
- `is_template = 0` - Excludes template beads

#### Stage 5: Priority Sorting
```rust
Sort key: (priority ASC, failure_count ASC, created_at ASC, id ASC)
```

### 4. Complete Filter Chain

```
Store Query → Defensive Label Check → Status/Assignee Check → Metadata Filters → Sort → Return
```

**SQL Equivalent:**
```sql
SELECT DISTINCT i.id
FROM issues i
LEFT JOIN labels l ON l.issue_id = i.id
WHERE i.status = 'open'
  AND i.assignee IS NULL
  AND i.ephemeral = 0
  AND i.pinned = 0
  AND i.is_template = 0
  AND NOT EXISTS (
    SELECT 1 FROM labels
    WHERE issue_id = i.id
    AND label IN ('deferred', 'human', 'blocked')
  )
ORDER BY i.priority ASC, i.created_at ASC, i.id ASC
```

## Root Cause Analysis: Bead Invisibility

### Primary Issue: Workspace Path Mismatch

**The Problem:**
From debug log analysis (bf-4f5fw), workers were querying the **wrong workspace database**:

- **Worker's actual workspace:** `/home/coding/.beads/` (368 KB, 5 total beads, 0 open)
- **Target workspace:** `/home/coding/claude-governor/.beads/` (4.3 MB, 1,208 total beads, 21 open)

**Evidence from logs:**
```
2026-08-03T23:04:53.873069Z DEBUG ...strand.pluck: Bead store returned 0 candidates count=0
```

The bead store query returned **exactly 0 candidates** because it was querying `/home/coding/.beads/` (which has 0 open beads) instead of `/home/coding/claude-governor/.beads/` (which has multiple ready beads).

### Secondary Issues

#### 1. Stale Assignee Handling
**Issue:** `Open` beads with an assignee are excluded even if the assignee is stale/dead.

**Current behavior:**
```rust
// Lines 422-424 in pluck.rs
matches!(b.status, crate::types::BeadStatus::InProgress) ||
(b.status == crate::types::BeadStatus::Open && b.assignee.is_some())
```

**Impact:** If a worker dies or crashes without releasing beads, those beads become permanently invisible to Pluck.

**Recommendation:** Implement stale-assignee detection with automatic assignment cleanup.

#### 2. Label Lifecycle Management
**Issue:** Default `exclude_labels` are quite broad and may hide legitimate work:
- `deferred` - Could include legitimate postponed work that should be reconsidered
- `human` - May flag complex beads as "human-only" when they could be partially automated
- `blocked` - Dependencies might be resolved but beads remain labeled

**Impact:** 81 beads excluded by label filters from 1,208 total beads.

**Recommendation:** Review if these labels are over-applied or if beads should be automatically re-labeled when conditions change.

#### 3. No Temporal Filter Logic
**Issue:** No time-based filters for:
- `defer_until` (beads with temporary postponement)
- `due_at` (urgency-based prioritization)

**Impact:** Beads postponed for a specific time may be invisible even after the defer period expires.

## Filter Impact Statistics

**Current Filtering Effectiveness:**
- **Total beads in database:** 1,208
- **Open beads:** 21 (before filtering)
- **Claimable issues (after filtering):** 10-16
- **Excluded by filters:** 81+ beads

### High-Impact Filters
1. **`status != 'open'`** - Excludes 1,187 beads (closed, blocked, done, in_progress)
2. **`assignee IS NOT NULL`** - Excludes beads with stale assignments (~5 beads)
3. **`label IN ('deferred', 'human', 'blocked')`** - Excludes 81 beads

### Medium-Impact Filters
4. **`ephemeral = 1`** - Excludes temporary beads
5. **`pinned = 1`** - Excludes administratively held beads
6. **`is_template = 1`** - Excludes template beads

## Configuration Sources Reference

| Setting | Source | Location | Type | Current Value |
|---------|--------|----------|------|---------------|
| Default exclude_labels | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:13` | Constant | `["deferred", "human", "blocked"]` |
| Custom exclude_labels | Not configured | N/A | Runtime override | None (uses defaults) |
| Workspace default | NEEDLE config | `~/.config/needle/config.yaml:9` | YAML path | `/home/coding/claude-governor` |
| Current workspace | CLI/environment | NEEDLE assignment | Runtime | `/home/coding/claude-governor` |
| Bead store path | Derived from workspace | `{workspace}/.beads/` | Directory | `/home/coding/claude-governor/.beads/` |
| Strand enablement | NEEDLE config | `~/.config/needle/config.yaml:70-87` | YAML map | `pluck: auto` |
| Filter logic | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:105-133` | Rust code | Three-tier filtering |

## Additional Configuration Settings

### Split Configuration
- **split_after_failures:** `3` (default threshold)
- **Trigger:** When first candidate has `failure-count:N` label where N >= threshold
- **Result:** Returns `StrandResult::Split` instead of `BeadFound`

### Bead Store Configuration (br/bead-forge)
**File:** `/home/coding/claude-governor/.beads/config.yaml`

**Active values:**
- `issue_prefixes: ["bf"]`
- `default_priority: 2`
- `default_type: task`
- `claim_ttl_minutes: 0`

## Recommendations

### 1. Fix Workspace Path Assignment (Critical)
**Action:** Ensure workers are explicitly assigned to the correct workspace:
```bash
needle run --workspace /home/coding/claude-governor --agent <agent> ...
```

**Priority:** HIGH - This is the primary cause of bead invisibility.

### 2. Implement Stale Assignee Recovery
**Action:** Add logic to detect and clear stale assignments:
```bash
# Identify potentially stale assignments:
bf list --format json | jq '.[] | select(.status == "open" and .assignee != null)'
```

**Priority:** MEDIUM - Prevents bead starvation from worker crashes.

### 3. Review Default Exclude Labels
**Action:** Audit beads with `deferred`, `human`, `blocked` labels to determine if they're:
- Correctly categorized (legitimate exclusions)
- Over-applied (should be re-labeled automatically)
- Stale (conditions changed but label wasn't updated)

**Priority:** MEDIUM - May reveal hidden work capacity.

### 4. Add Telemetry for Filter Impact
**Action:** Log filtering stats even when NOT starving to track filter impact:
- PluckStrand already has `last_filtering_stats()` method (lines 195-206)
- Currently only emits telemetry when starving
- **Recommendation:** Always log filtering stats for visibility

**Priority:** LOW - Improves observability.

### 5. Consider Temporal Filter Logic
**Action:** If `defer_until` field exists in schema, add logic to:
- Skip beads where `defer_until > NOW()`
- Include beads where `defer_until <= NOW()` and remove `deferred` label

**Priority:** LOW - Feature enhancement for better bead lifecycle management.

## Validation Steps

### Verify Workspace Assignment
```bash
# 1. Check current workspace
pwd

# 2. Verify ready beads in target workspace
bf ready

# 3. Direct database query
sqlite3 /home/coding/claude-governor/.beads/beads.db \
  "SELECT COUNT(*) FROM issues WHERE status='open' AND assignee IS NULL;"
```

### Monitor Filter Effectiveness
```bash
# Enable debug logging
RUST_LOG=needle::strand::pluck=debug needle run --workspace /home/coding/claude-governor ...

# Look for these log lines:
# - "Bead store returned X candidates"
# - "No beads excluded by label filter"
# - "No beads excluded by status/assignee filter"
```

## Conclusion

**Root Cause:** Pluck configuration is **functionally correct** but bead invisibility is caused by **workspace path mismatches** during worker execution. Workers were querying `/home/coding/.beads/` (empty) instead of `/home/coding/claude-governor/.beads/` (containing work).

**Configuration Quality:** The filter configuration is **moderately restrictive** but working as designed. The combination of broad default exclude labels, aggressive stale-assignee filtering, and multiple metadata filters creates a system where **10-16 out of 1,208 beads** (0.8-1.3%) are visible to workers.

**Primary Fix:** Ensure workers use explicit `--workspace /home/coding/claude-governor` to query the correct bead store database.

**Secondary Improvements:** Implement stale assignee recovery, review label lifecycle management, and add continuous filter impact telemetry.

## Child Beads Referenced

- **bf-22ks5:** Workspace path verification
- **bf-2ur41:** Filter configuration review  
- **bf-4f5fw:** Search output analysis (0 beads found)
- **bf-56wnh:** Debug output analysis

## Files Referenced

- `/home/coding/.config/needle/config.yaml` - Main NEEDLE config
- `/home/coding/NEEDLE/src/strand/pluck.rs` - Pluck strand implementation
- `/home/coding/claude-governor/.beads/beads.db` - Target bead store database
- `/home/coding/claude-governor/.beads/config.yaml` - Bead store configuration

## Date Completed

2026-08-03
