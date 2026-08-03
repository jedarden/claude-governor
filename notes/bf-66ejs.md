# Pluck Filter Configuration Documentation

**Bead ID:** bf-66ejs  
**Completed:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`

## Summary

This document answers the core questions about Pluck's filter configuration: what filters exist, where they're stored, and how they affect bead visibility.

---

## 1. Filter Configuration Location

**Primary Location:** Compiled into NEEDLE binary  
**Source File:** `/home/coding/NEEDLE/src/strand/pluck.rs`  
**Line Numbers:** Lines 21, 39, 309-534

The filter configuration is **hardcoded in the NEEDLE binary** and not stored in external configuration files. To change filter settings, you must modify the source code and recompile NEEDLE.

### No Workspace-Specific Configuration

- **Workspace config:** `~/.needle/config.yaml` - Does NOT contain Pluck filter settings
- **Bead store config:** `.beads/config.yaml` - Does NOT exist in this workspace (using bead-forge defaults)
- **Environment variables:** No env var overrides for filter settings

---

## 2. Currently Applied Filters

Pluck applies filters in **three sequential stages**:

### Stage 1: Store-Level Filter (Line 309-320)
**Applied via:** `store.ready(&filters).await`

```rust
let filters = Filters {
    assignee: None,  // No assignee filtering at store level
    exclude_labels: self.exclude_labels.clone(),
    ..Default::default()
};
```

**Filters applied:**
- Beads with labels matching `exclude_labels` list

### Stage 2: Label-Based Defensive Filter (Line 353-411)
**Purpose:** Defensive guard against bead stores that don't include label data in every query

```rust
candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
```

**Filters applied:**
- Beads with any label in `exclude_labels` list

### Stage 3: Status/Assignee Filter (Line 413-469)
**Purpose:** Remove beads that are never claimable (would cause retry loops)

```rust
candidates.retain(|b| {
    !(matches!(b.status, crate::types::BeadStatus::InProgress)
        || (b.status == crate::types::BeadStatus::Open && b.assignee.is_some()))
});
```

**Filters applied:**
- Beads with `InProgress` status (claimed by another worker)
- Beads with `Open` status that have an assignee (stale assignment)

---

## 3. Current Filter Values

### Exclude Labels (Default)

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:21`

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Labels excluded from bead visibility:**

| Label | Purpose | Behavior |
|-------|---------|----------|
| `deferred` | Beads marked for later processing | Never presented as candidates |
| `human` | Beads requiring human intervention | Never presented as candidates |
| `blocked` | Beads with active blocking dependencies | Never presented as candidates |

**How to change:** Modify `DEFAULT_EXCLUDE_LABELS` constant and recompile NEEDLE

### Split Trigger Threshold

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:39` (default), Line 87 (instance)

```rust
split_after_failures: 3,  // default threshold
```

**Behavior:** When first candidate has `failure-count:N` label where N ≥ 3, returns `StrandResult::Split` instead of `BeadFound`

**How to change:** Pass custom threshold to `PluckStrand::with_split_threshold()` or modify default value

---

## 4. How Filters Affect Bead Visibility

### Filter Flow Diagram

```
All Beads in Store
    ↓
[Stage 1: Store Query with exclude_labels]
    ↓
Beads returned by store.ready()
    ↓
[Stage 2: Defensive Label Filter]
    ↓ Removes: beads with deferred/human/blocked labels
    ↓
[Stage 3: Status/Assignee Filter]
    ↓ Removes: InProgress beads + Open beads with assignee
    ↓
[Sort: (priority ASC, created_at ASC, id ASC)]
    ↓
[Split Trigger Check]
    ↓ Triggers Split if: failure-count ≥ 3
    ↓
Final Candidates → Claim Processing
```

### Visibility Impact

**Before filtering:** All beads in store (including deferred, human, blocked, InProgress, assigned)

**After filtering:** Only beads that are:
- Open status
- No assignee
- NOT labeled `deferred`, `human`, or `blocked`
- Failure count < 3 (otherwise triggers Split instead)

### Telemetry and Visibility

**Tracked metrics** (accessible via `PluckStrand::last_filtering_stats()`):
- `open_count` - beads before filtering
- `excluded_count` - beads removed during filtering  
- `exclusion_reasons` - specific reasons for each exclusion (label:*, status:*, assignee:*)

**Log output:** DEBUG level logs for each excluded bead with specific reason

---

## 5. Special Filter Cases

### NEEDLE Internal Config Filter

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:500-521`

When a split would be triggered, Pluck checks if the bead references NEEDLE-internal configuration. If so:
- Split is **skipped**
- Candidate is **filtered out** from remaining list
- Rationale: Such beads have no legitimate resolution path from inside a target repo

### Priority Sorting (Not a filter, but affects visibility order)

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:471-483`

```rust
// Sort order: (priority ASC, created_at ASC, id ASC)
candidates.sort_by(|a, b| {
    a.priority
        .cmp(&b.priority)
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.id.cmp(&b.id))
});
```

**Effect:** Lower priority numbers, earlier creation dates, and lower IDs are presented first

---

## 6. Configuration Summary Table

| Setting | Value | Source | Mutable | Location |
|---------|-------|--------|----------|----------|
| `exclude_labels` | `["deferred", "human", "blocked"]` | Compiled constant | No (requires recompile) | `/home/coding/NEEDLE/src/strand/pluck.rs:21` |
| `split_after_failures` | `3` | Compiled default | No (requires recompile) | `/home/coding/NEEDLE/src/strand/pluck.rs:39` |
| Store-level filter | Applied via `Filters` struct | Runtime | Yes (via code change) | `/home/coding/NEEDLE/src/strand/pluck.rs:309-320` |
| Defensive label filter | Applied in strand | Runtime | No (always active) | `/home/coding/NEEDLE/src/strand/pluck.rs:353-411` |
| Status/assignee filter | Applied in strand | Runtime | No (always active) | `/home/coding/NEEDLE/src/strand/pluck.rs:413-469` |

---

## 7. Key Insights

1. **Hardcoded configuration:** All filter settings are compiled into the NEEDLE binary - no runtime configuration files
2. **Three-tier defense:** Filters are applied at store level, strand level (defensive), and status level for robust filtering
3. **Defensive design:** Double-filtering (store + strand) prevents inconsistencies from different bead store backends
4. **No customization:** Changing defaults requires recompiling NEEDLE - no per-workspace filter configuration
5. **Visibility is strict:** Only open, unassigned beads without excluded labels and with failure count < 3 are visible

---

## Related Documentation

- Comprehensive Pluck documentation: `/home/coding/claude-governor/notes/bf-44thq.md`
- NEEDLE source code: `/home/coding/NEEDLE/src/strand/pluck.rs`
- Bead store defaults: bead-forge configuration in `~/bead-forge/`

---

## Acceptance Criteria Status

✅ **Find the filter configuration in Pluck** - Located in `/home/coding/NEEDLE/src/strand/pluck.rs`  
✅ **Document what filters are currently applied** - Three stages: store-level, label-based, status/assignee  
✅ **Document where this configuration is stored** - Hardcoded in NEEDLE binary, no external config files  
✅ **Document how filters affect bead visibility** - Sequential filtering reduces all beads to only open, unassigned, unlabeled candidates