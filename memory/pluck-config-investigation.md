---
name: pluck-config-investigation-complete
description: Complete Pluck configuration investigation findings - workspace mismatch and filter analysis
metadata:
  type: project
  bead_id: bf-4k2j5
---

# Pluck Configuration Investigation - Complete Findings

**Bead ID:** bf-4k2j5  
**Date Completed:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`

## Executive Summary

Pluck's configuration is **correctly set up** but bead visibility issues stem from **workspace path mismatches** during worker execution. The filtering logic is moderately restrictive, functioning as designed, but workers may query the wrong bead store database.

## Current Database State

- **Total beads**: 1,208
- **Open beads**: 16  
- **Unassigned open beads**: 13
- **Available to Pluck**: 10-13 (after label and blocked cache filtering)

## Configuration Documentation

### Workspace Path Configuration ⚠️ **MISMATCH DETECTED**

**Default Workspace in Config:**
```yaml
# From ~/.config/needle/config.yaml
workspace:
  default: /home/coding        # ❌ Does not match actual workspace
  home: /home/coding/.needle
```

**Actual Workspace:**
- **Path:** `/home/coding/claude-governor`
- **Bead store:** `/home/coding/claude-governor/.beads/`
- **Database:** `beads.db` (4.3 MB, 1,208 beads)
- **Checkpoint:** `issues.jsonl` (1.1 MB)

### Exclude Labels

**Source:** Configured in `~/.config/needle/config.yaml` and hardcoded in NEEDLE binary (`/home/coding/NEEDLE/src/strand/pluck.rs:83`)

**Current Values:**
- `deferred` - Beads postponed for later processing
- `human` - Beads requiring human intervention  
- `blocked` - Beads with blocking dependencies

**Impact:** 4 beads excluded by label filters from 16 open beads (25% exclusion rate).

### Filter Pipeline

Pluck applies filters in sequence:

1. **Store Query Filters** - Status='open', exclude_labels applied
2. **Defensive Label Filtering** - Double-checks excluded labels  
3. **Status & Assignee Filtering** - Removes InProgress and stale assignments
4. **Metadata Filters** - Excludes ephemeral, pinned, template beads
5. **Blocked Cache Filter** - Removes beads with blocking dependencies
6. **Priority Sorting** - Sorts by priority, created_at, id

## Root Cause Analysis

### Primary Issue: Workspace Path Mismatch

**Critical Finding:** The workspace default in configuration does not match the actual workspace location.

- **Configured:** `workspace.default: /home/coding`
- **Actual:** `/home/coding/claude-governor`

This mismatch can cause workers to query the wrong bead store database, leading to "empty pluck" issues where no beads are found.

### Filter Impact Analysis

**Filter Cascade on 16 Open Beads:**

1. **Start:** 16 open beads
2. **After exclude_labels:** 12 beads (4 excluded with `deferred` label)
3. **After blocked cache:** 11 beads (7 beads in blocked cache)
4. **Final ready beads:** 10 beads

**Beads Excluded by Labels:**
- `bf-156nn7` - config/claude-governor.service MemoryMax issue
- `bf-1y51s` - Diagnose configuration filter and exclude_labels
- `bf-3js6h` - Reproduce Pluck starvation issue
- `bf-5dsgv` - Investigate Pluck configuration and bead visibility

**Beads in Blocked Cache:**
- `bf-1y51s`, `bf-3js6h`, `bf-5dsgv` (also have deferred labels)
- `bf-5be7lz`, `bf-2h8n23`, `bf-3ww0k4`, `bf-3zdrza` (compiler warnings chain)

### Secondary Issues

1. **Over-aggressive Filtering** - Only 62.5% of open beads (10/16) are visible to workers
2. **Stale Assignee Handling** - Workers that die without releasing beads permanently hide them
3. **Label Lifecycle Management** - Default labels are broad and may exclude too many beads
4. **Blocked Cache Management** - Beads can get stuck with stale blocking dependencies

## Database Connectivity ✅

**All tests passed:**
- Database integrity: `PRAGMA integrity_check` returns `ok`
- File size: 4.3 MB (1,208 beads)
- Read/write operations: Normal
- `bf ready` command: Working correctly

## Configuration Reference

| Setting | Source | Location | Type | Current Value |
|---------|--------|----------|------|---------------|
| Default workspace | Config | `~/.config/needle/config.yaml:36` | YAML | `/home/coding` |
| Actual workspace | Runtime | `pwd` | Directory | `/home/coding/claude-governor` |
| Exclude labels | Config + Binary | `~/.config/needle/config.yaml:40-42` | YAML list | `deferred, human, blocked` |
| Bead store path | Derived | `{workspace}/.beads/` | Directory | `/home/coding/claude-governor/.beads/` |
| Database file | File | `.beads/beads.db` | SQLite | 4.3 MB, 1,208 beads |
| Strand enablement | Config | `~/.config/needle/config.yaml:39` | YAML | `pluck: auto` |

## Recommendations

### Critical Fix Required

1. **Update workspace configuration:**
   ```yaml
   # In ~/.config/needle/config.yaml
   workspace:
     default: /home/coding/claude-governor  # Fix this path
   ```

2. **Verify worker launch commands:**
   Ensure workers use explicit `--workspace /home/coding/claude-governor` parameter

### Secondary Improvements

1. Implement stale assignee recovery mechanism
2. Review and potentially narrow `exclude_labels` scope  
3. Add continuous filter impact telemetry
4. Implement temporal filter logic for `deferred` beads (respect `defer_until`)
5. Add blocked cache validation and cleanup

## Conclusion

Pluck configuration is **functionally correct** but has a **critical workspace path mismatch** in the configuration. The primary cause of "empty pluck" issues is workers querying the wrong bead store database. The combination of broad default exclude labels and aggressive filtering means only **62.5% of open beads** (10 out of 16) are visible to workers.

**Primary Fix:** Update `workspace.default` in config to `/home/coding/claude-governor` and ensure workers use explicit workspace parameters.

**See also:** [[notes/bf-4k2j5.md]] for detailed investigation notes.
