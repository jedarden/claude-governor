# Pluck Filter and Label Settings Documentation

**Bead ID:** bf-44thq  
**Documented:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`

## Summary

This document provides a complete structured summary of all Pluck filter and label settings as configured in the NEEDLE deployment used by the claude-governor workspace.

---

## 1. Exclude Labels Configuration

### Default Exclude Labels (Compiled into NEEDLE binary)

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:21`

| Label | Purpose | Behavior |
|-------|---------|----------|
| `deferred` | Beads marked for later processing | Excluded from candidate selection |
| `human` | Beads requiring human intervention | Excluded from candidate selection |
| `blocked` | Beads with active blocking dependencies | Excluded from candidate selection |

**Implementation:**
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

### Custom Override

**Current Status:** Not configured  
**Behavior:** When `PluckStrand::new(vec![])` is called with empty `exclude_labels`, defaults are applied  
**Override Behavior:** Custom exclude_labels completely replace defaults (not merged)

---

## 2. Filter Levels (Three-Tier Architecture)

Pluck applies filtering in three sequential stages:

### Level 1: Store-Level Filter

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:309-320`

```rust
let filters = Filters {
    assignee: None,  // No assignee filtering
    exclude_labels: self.exclude_labels.clone(),
    ..Default::default()
};
```

**Applied via:** `store.ready(&filters).await`  
**Filters by:**
- `exclude_labels` - beads with any matching label are excluded

### Level 2: Label-Based Defensive Filter

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:353-411`

**Purpose:** Defensive guard against stores that don't include label data in every query type  
**Behavior:** Filters candidates that have any label in the `exclude_labels` list

```rust
candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
```

**Telemetry:** Tracks excluded beads and their exclusion reasons

### Level 3: Status/Assignee Filter

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:413-469`

**Filters out:**
1. **InProgress status beads** - beads currently claimed by another worker
2. **Open beads with assignee** - Open beads that have a stale assignee

```rust
candidates.retain(|b| {
    !(matches!(b.status, crate::types::BeadStatus::InProgress)
        || (b.status == crate::types::BeadStatus::Open && b.assignee.is_some()))
});
```

**Rationale:** These beads are never claimable — the claimer will reject them every time, causing a hot loop

---

## 3. Split Trigger Configuration

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:485-534`

### split_after_failures

**Value:** `3` (default threshold, line 39)  
**Trigger Condition:** When first candidate has `failure-count:N` label where N >= threshold  
**Result:** Returns `StrandResult::Split` instead of `BeadFound`

**Implementation:**
```rust
if failure_count >= self.split_after_failures {
    return StrandResult::Split(Box::new(first_candidate.clone()), failure_count);
}
```

### NEEDLE Internal Config Filter

**Additional check during split trigger:**  
If bead references NEEDLE-internal configuration, split is skipped and candidate is filtered out.

**Rationale:** Such beads have no legitimate resolution path from inside a target repo and should not be split into child beads there.

---

## 4. Priority Sorting

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:471-483`

**Sort Order (Deterministic):**
```rust
(priority ASC, created_at ASC, id ASC)
```

**Applied after:** All filters have been applied

---

## 5. Bead Store Configuration

**File:** `/home/coding/claude-governor/.beads/config.yaml`  
**Status:** Not present (using br/bead-forge defaults)

**Default values (from bead-forge):**
| Setting | Default Value |
|---------|----------------|
| `issue_prefixes` | `["bf"]` |
| `default_priority` | `2` |
| `default_type` | `task` |
| `claim_ttl_minutes` | `0` |

---

## 6. Workspace Configuration

**Source:** `~/.needle/config.yaml`

**Workspace Path:** `/home/coding/claude-governor`  
**Bead Store Location:** `/home/coding/claude-governor/.beads/`  
**Database:** `/home/coding/claude-governor/.beads/beads.db`  
**JSONL Checkpoint:** `/home/coding/claude-governor/.beads/issues.jsonl`

**Strand Configuration:**
```yaml
strands:
  pluck: auto    # Primary work from the auto-discovered workspace
  explore: auto  # Look for work in other workspaces
  mend: true     # Maintenance and cleanup (always on)
  knot: true     # Alert human when stuck (always on)
```

---

## 7. Filtering Statistics and Telemetry

**Tracked Metrics:**
- `open_count` - Count of beads returned by store before filtering
- `excluded_count` - Count of beads excluded during filtering
- `exclusion_reasons` - List of reasons for each exclusion (label:*, status:*, assignee:*)

**Access via:** `PluckStrand::last_filtering_stats()`

**Emitted at:** DEBUG level for individual excluded beads

---

## 8. Complete Filter Settings Summary Table

| Filter Type | Level | Setting | Value | Source |
|-------------|-------|---------|-------|--------|
| Exclude Labels | 1 & 2 | `deferred` | ✅ Excluded | Compiled constant |
| Exclude Labels | 1 & 2 | `human` | ✅ Excluded | Compiled constant |
| Exclude Labels | 1 & 2 | `blocked` | ✅ Excluded | Compiled constant |
| Status Filter | 3 | `InProgress` status | ✅ Excluded | Runtime filter |
| Status Filter | 3 | `Open` with assignee | ✅ Excluded | Runtime filter |
| Split Trigger | Post-filter | `failure-count:N >= 3` | ✅ Triggers Split | Runtime check |
| Split Trigger | Post-filter | NEEDLE internal config | ✅ Skips split | Runtime check |
| Priority Sort | Post-filter | Sort order | `(priority ASC, created_at ASC, id ASC)` | Runtime sort |

---

## 9. Configuration Sourcing Summary

| Setting | Source | Location | Type | Mutable |
|---------|--------|----------|------|---------|
| Default exclude_labels | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:21` | Constant | No (requires recompile) |
| Custom exclude_labels | Not configured | N/A | Runtime override | Yes |
| split_after_failures | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:39` | Constant | No (requires recompile) |
| Workspace default | NEEDLE config | `~/.needle/config.yaml` | YAML | Yes |
| Strand enablement | NEEDLE config | `~/.needle/config.yaml` | YAML | Yes |

---

## 10. Known Limitations

1. **No custom exclude_labels configured** - All deployments use the same default set
2. **Exclude labels are hardcoded** - Changing defaults requires recompiling NEEDLE
3. **Filtering is defensive** - Double-filtering (store + strand) prevents store inconsistencies but adds overhead
4. **No environment variable overrides** for filter settings

---

## Related Documentation

- NEEDLE source: `/home/coding/NEEDLE/src/strand/pluck.rs`
- NEEDLE config: `~/.needle/config.yaml`
- Existing docs: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`
- Bead store config: `~/bead-forge/`
