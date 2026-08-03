# Pluck Configuration Investigation - bf-54ppq

**Date:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`  
**Purpose:** Document Pluck configuration settings affecting bead visibility

## Executive Summary

Pluck's **exclude_labels configuration is filtering out open beads** that have the `deferred` label. This is the primary configuration setting causing bead invisibility in the workspace.

---

## Current Configuration Values

### 1. Exclude Labels (PRIMARY CAUSE)

**Location:** `~/.config/needle/config.yaml` → `strands.pluck.exclude_labels`

**Current Value:**
```yaml
strands:
  pluck:
    exclude_labels:
    - deferred    # ← Filters out beads marked for later processing
    - human       # ← Filters out beads requiring human intervention  
    - blocked     # ← Filters out beads with blocking dependencies
    split_after_failures: 3
```

**Impact on Bead Visibility:**
- Any bead with the `deferred` label is **completely invisible** to Pluck
- This applies at both the store-level query and strand-level defensive filter
- Beads remain in the database but are excluded from work selection

**Affected Open Beads (with `deferred` label):**
| Bead ID | Title | Additional Labels |
|---------|-------|-------------------|
| bf-156nn7 | config/claude-governor.service still ships MemoryMax=512M | failure-count:1, plan-gap |
| bf-1y51s | Diagnose configuration filter and exclude_labels issues | failure-count:2, split-child, umbrella |
| bf-3js6h | Reproduce Pluck starvation issue | split-child, umbrella |
| bf-4k2j5 | Investigate Pluck configuration and workspace setup | umbrella (has assignee) |

---

### 2. Workspace Path Configuration

**Location:** `~/.config/needle/config.yaml` → `workspace.default`

**Current Value:**
```yaml
workspace:
  default: /home/coding
  home: /home/coding/.needle
  labels: []
```

**Claude Governor Workspace:**
- **Path:** `/home/coding/claude-governor`
- **Bead store:** `/home/coding/claude-governor/.beads/`
- **Database:** `/home/coding/claude-governor/.beads/beads.db`
- **JSONL checkpoint:** `/home/coding/claude-governor/.beads/issues.jsonl`

**Verification:** ✅ Workspace path is correct and accessible

---

### 3. Filter Configuration

**Three-tier filtering implementation:**

1. **Store-level filter** (via bead_store query):
   - Filters by assignee (if specified)
   - Filters by `exclude_labels` (deferred, human, blocked)

2. **Strand-level defensive filter** (pluck.rs:125):
   - Removes beads with excluded labels
   - Defensive guard against store inconsistencies

3. **Claimability filter** (pluck.rs:130-133):
   - Removes beads in `InProgress` status
   - Removes `Open` beads with stale assignee
   - Prevents SELECTING→CLAIMING→RETRYING spin loop

**No issues found** - filter logic is working as designed.

---

### 4. Bead Store Configuration

**Location:** `/home/coding/claude-governor/.beads/config.yaml` (does not exist - using defaults)

**Active values** (from `br config list`):
```yaml
issue_prefixes: ["bf"]
default_priority: 2
default_type: task
claim_ttl_minutes: 30
```

---

## Database State

**Current bead counts:**
```
Total issues:    1208
By status:
  blocked:       53
  closed:        1127
  done:          2
  in_progress:   8
  open:          18
```

**Open beads visible to `bf ready`:** 10 beads (after `deferred` filter applied)

**Open beads with `deferred` label:** 4 beads (invisible to Pluck)
- bf-156nn7, bf-1y51s, bf-3js6h, bf-4k2j5

---

## Configuration Sourcing Summary

| Setting | Source | Location | Type | Current Value |
|---------|--------|----------|------|---------------|
| Default exclude_labels | NEEDLE config | `~/.config/needle/config.yaml:18-21` | YAML list | `["deferred", "human", "blocked"]` |
| Workspace default | NEEDLE config | `~/.config/needle/config.yaml:28` | YAML path | `/home/coding` |
| Current workspace | CLI/environment | NEEDLE assignment | Runtime | `/home/coding/claude-governor` |
| Bead store path | Derived from workspace | `{workspace}/.beads/` | Directory | `/home/coding/claude-governor/.beads/` |
| split_after_failures | NEEDLE config | `~/.config/needle/config.yaml:22` | Integer | `3` |
| Strand enablement | NEEDLE config | `~/.config/needle/config.yaml:15-16` | YAML map | `pluck: auto` |

---

## Conclusions

### Primary Cause Identified

**The `deferred` exclude_label is working as designed** - it is filtering out beads marked for later processing. This is not a configuration bug but rather the expected behavior of the exclude_labels feature.

### Why Beads Are Invisible

1. **Beads with `deferred` label are intentionally hidden** from Pluck's work selection
2. This applies to **4 open beads** that would otherwise be candidates for processing
3. The filtering occurs at both the store query and strand defensive levels

### Configuration Is Correct

- Workspace path is correct and accessible
- Filter configuration is working as designed
- No overly restrictive patterns found
- The exclude_labels feature is functioning exactly as intended

---

## Recommendations

1. **If deferred beads should be processed:** Remove the `deferred` label from the exclude_labels list in `~/.config/needle/config.yaml`

2. **If deferred beads should remain hidden:** Current configuration is correct - no changes needed

3. **For visibility without processing:** Consider using a separate label (e.g., `visible-deferred`) that is not in the exclude_labels list, while keeping `deferred` for actual deferral

---

## Related Documentation

- NEEDLE config: `~/.config/needle/config.yaml`
- Pluck configuration documentation: `docs/plan/pluck-configuration.md`
- Workspace path documentation: `docs/pluck-workspace-paths.md`
- Previous debug session: `notes/bf-56wnh-pluck-debug.log`
