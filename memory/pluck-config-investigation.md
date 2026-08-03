---
name: pluck-config-investigation
description: Complete Pluck configuration investigation findings and root cause analysis
metadata:
  type: project
---

# Pluck Configuration Investigation - Complete Findings

**Bead ID:** bf-4k2j5  
**Date Completed:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`

## Executive Summary

Pluck's configuration is **correctly set up** but bead visibility issues stem from **workspace path mismatches** during worker execution. The filtering logic is moderately restrictive, functioning as designed, but workers may query the wrong bead store database.

## Current Database State

- **Total beads**: 1,208
- **Open beads**: 21  
- **Unassigned open beads**: 16
- **Available to Pluck**: 10-16 (after label filtering)

## Configuration Documentation

### Exclude Labels

**Source:** Compiled into NEEDLE binary (`/home/coding/NEEDLE/src/strand/pluck.rs:13`)

**Current Values:**
- `deferred` - Beads postponed for later processing
- `human` - Beads requiring human intervention  
- `blocked` - Beads with blocking dependencies

**Impact:** 81 beads excluded by label filters from 1,208 total beads.

### Workspace Path Configuration

**Default Workspace:**
```yaml
# From ~/.needle/config.yaml
workspace:
  default: /home/coding/claude-governor
```

**Target Workspace:**
- **Path:** `/home/coding/claude-governor`
- **Bead store:** `/home/coding/claude-governor/.beads/`
- **Database:** `beads.db` (4.3 MB, 1,208 beads)
- **Checkpoint:** `issues.jsonl` (1.1 MB)

### Filter Pipeline

Pluck applies filters in sequence:

1. **Store Query Filters** - Unassigned beads only, exclude_labels applied
2. **Defensive Label Filtering** - Double-checks excluded labels  
3. **Status & Assignee Filtering** - Removes InProgress and stale assignments
4. **Metadata Filters** - Excludes ephemeral, pinned, template beads
5. **Priority Sorting** - Sorts by priority, failure_count, created_at, id

## Root Cause Analysis

### Primary Issue: Workspace Path Mismatch

Workers were querying the **wrong workspace database**:
- **Worker's actual workspace:** `/home/coding/.beads/` (368 KB, 0 open beads)
- **Target workspace:** `/home/coding/claude-governor/.beads/` (4.3 MB, 21 open beads)

### Secondary Issues

1. **Stale Assignee Handling** - Workers that die without releasing beads permanently hide them
2. **Label Lifecycle Management** - 81 beads excluded by broad default labels
3. **No Temporal Filter Logic** - No time-based filters for defer_until or due_at

## Recommendations

### Critical Fix
Ensure workers use explicit `--workspace /home/coding/claude-governor` to query the correct bead store database.

### Secondary Improvements
- Implement stale assignee recovery
- Review label lifecycle management  
- Add continuous filter impact telemetry
- Consider temporal filter logic for deferred beads

## Configuration Reference

| Setting | Source | Location | Type | Current Value |
|---------|--------|----------|------|---------------|
| Default exclude_labels | Binary | `/home/coding/NEEDLE/src/strand/pluck.rs:13` | Constant | `["deferred", "human", "blocked"]` |
| Workspace default | Config | `~/.config/needle/config.yaml:9` | YAML | `/home/coding/claude-governor` |
| Bead store path | Derived | `{workspace}/.beads/` | Directory | `/home/coding/claude-governor/.beads/` |
| Strand enablement | Config | `~/.config/needle/config.yaml:70-87` | YAML | `pluck: auto` |

## Conclusion

Pluck configuration is **functionally correct**. The primary cause of bead invisibility is **workspace path mismatches** during worker execution. The combination of broad default exclude labels and aggressive filtering means only **0.8-1.3% of beads** (10-16 out of 1,208) are visible to workers.

**Primary Fix:** Ensure workers use explicit `--workspace /home/coding/claude-governor` when launching.

**See also:** [[notes/bf-4k2j5.md]] for detailed investigation notes.
