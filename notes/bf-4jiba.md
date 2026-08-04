# Pluck Filter Test Results

## Test Objective
Identify the exact condition that causes Pluck to return 0 beads when combining filters.

## Test Environment
- **Main workspace:** /home/coding/claude-governor
- **Test workspace:** /home/coding/cgov-polish-queue
- **Default exclude_labels:** [deferred, human, blocked] (from ~/.config/needle/config.yaml)

## Systematic Test Results

### Test 1: Baseline - No Filters
```bash
bf ready  # Uses default filters
```
**Result:** 10 ready beads in main workspace, 0 in polish-queue

### Test 2: Direct SQLite Queries (Ground Truth)

**Main workspace (.beads/beads.db):**
- Total open beads: 47
- Unassigned open beads: 37
- Ready beads (excluding deferred/human/blocked): 35

**Polish-queue workspace:**
- Total beads: 16
- Open beads: 0
- Ready beads: 0

### Test 3: Filter Combination Analysis

**Scenario A: workspace_path + exclude_labels (Main workspace)**
- workspace_path: /home/coding/claude-governor
- exclude_labels: [deferred, human, blocked]
- **Result:** 10 beads (SUCCESS - not blocking)

**Scenario B: workspace_path + exclude_labels (Polish-queue)**
- workspace_path: /home/coding/cgov-polish-queue
- exclude_labels: [deferred, human, blocked]
- **Result:** 0 beads (BLOCKING CONDITION IDENTIFIED)

**Scenario C: exclude_labels only (no workspace_path filter)**
- exclude_labels: [deferred, human, blocked]
- Uses current workspace
- **Result:** 10 beads (SUCCESS - not blocking)

**Scenario D: workspace_path only (no label filtering)**
- workspace_path: /home/coding/cgov-polish-queue
- No label exclusion
- **Result:** 0 beads (BLOCKING - but due to no open beads)

## Root Cause Analysis

### Primary Blocking Condition
**Pluck returns 0 beads when the target workspace has NO open beads**, regardless of exclude_labels settings.

**Evidence:**
- Polish-queue has 16 total beads but 0 open beads
- All queries against polish-queue return 0 results
- The issue is NOT the exclude_labels filter

### Secondary Condition
**All open beads are excluded by exclude_labels filter**

**Example scenario:**
If a workspace has 5 open beads but all 5 are labeled with "deferred", "human", or "blocked", then even with open beads present, Pluck returns 0.

**Current main workspace status:**
- 47 open beads
- 35 ready beads (after excluding 2 with deferred labels)
- NOT currently blocked by exclude_labels

## Exact Blocking Conditions

Pluck returns 0 results in these cases:

1. **Workspace has no open beads** (polish-queue case)
   - SQL: `SELECT COUNT(*) FROM issues WHERE status='open'` returns 0
   - Solution: Add open beads to the workspace

2. **All open beads are excluded by labels**
   - SQL: All open beads have labels in ['deferred', 'human', 'blocked']
   - Solution: Remove excluded labels from beads or adjust exclude_labels config

3. **Combined: workspace_path points to empty workspace + exclude_labels active**
   - Even with exclude_labels disabled, 0 open beads = 0 results
   - Solution: Ensure target workspace has open, non-excluded beads

## Test Verification

**To verify which condition is causing 0 results:**

```bash
# Step 1: Check if workspace has open beads
bf list --status open --workspace /path/to/workspace

# Step 2: Check unassigned count
sqlite3 /path/to/workspace/.beads/beads.db \
  "SELECT COUNT(*) FROM issues WHERE status='open' AND assignee IS NULL;"

# Step 3: Check excluded labels
sqlite3 /path/to/workspace/.beads/beads.db \
  "SELECT COUNT(DISTINCT issue_id) FROM labels WHERE label IN ('deferred','human','blocked');"

# Step 4: Check ready count (after exclusion)
bf ready --workspace /path/to/workspace
```

## Configuration Files

**NEEDLE config (~/.config/needle/config.yaml):**
```yaml
pluck:
  exclude_labels:
    - deferred
    - human
    - blocked
  split_after_failures: 3
```

**Default exclude_labels are applied automatically** - no need to specify them in bf commands.

## CRITICAL UPDATE: Workspace Path Resolution Bug

### New Root Cause Discovered (2026-08-03)

**The blocking condition is NOT the workspace state - it's a workspace path resolution bug!**

Pluck uses `workspace="."` which resolves to `/home/coding` (parent directory) instead of `/home/coding/claude-governor` (actual workspace).

### Evidence from Systematic Testing

**Correct database (`/home/coding/claude-governor/.beads/beads.db`):**
- Total issues: 1,226
- Open issues: 46
- Ready beads (full Pluck query): **35**

**Wrong database (`/home/coding/.beads/beads.db`) where `.` resolves:**
- Total issues: 5
- Open issues: **0**
- Ready beads (full Pluck query): **0** ← **BLOCKING**

### Filter Impact Analysis (on correct database)

| Filter Combination | Result | Δ from Base | % Reduction |
|-------------------|--------|-------------|-------------|
| BASE (no filters) | 1,221 | 0 | 0% |
| Only state='open' | 47 | 1,174 | 96.15% |
| Only assignee IS NULL | 359 | 862 | 70.6% |
| Only exclude_labels | 1,140 | 81 | 6.63% |
| state + assignee | 37 | 1,184 | 96.97% |
| state + exclude_labels | 43 | 1,178 | 96.48% |
| assignee + exclude_labels | 358 | 863 | 70.68% |
| **FULL QUERY (all filters)** | **36** | **1,185** | **97.05%** |

**Key Finding:** No filter combination returns 0 results when querying the correct database.

### Actual Root Cause

When Pluck runs with `workspace="."`:
1. The path resolves to `/home/coding` (not `/home/coding/claude-governor`)
2. It queries `/home/coding/.beads/beads.db` which has 0 open issues
3. All filters work correctly, but return 0 candidates because the database is empty

This explains the debug logs from `bf-2qh8h`:
```
DEBUG needle::strand::pluck: Bead store returned 0 candidates count=0
DEBUG needle::strand::pluck: No beads excluded by label filter count=0
DEBUG needle::strand::pluck: No beads excluded by status/assignee filter count=0
```

The filters weren't excluding anything - Pluck was querying the wrong database.

### Test Files Created

1. **tests/pluck_filter_combinations_test.rs**
   - Systematic testing of each filter individually and in combination
   - Impact analysis showing each filter's contribution
   - Demonstrates no blocking conditions in correct database

2. **tests/pluck_workspace_mismatch_test.rs**
   - Compares results between parent and current workspace databases
   - Shows parent database has 0 open issues
   - Demonstrates the workspace resolution bug

### Solution

Pluck needs to use absolute workspace paths instead of relative `"."` to ensure it queries the correct beads database.

## Conclusion (Updated)

The blocking condition is **NOT a filter combination issue or workspace state**, but rather:

**ROOT CAUSE:** Workspace path resolution bug where `workspace="."` resolves to parent directory instead of the actual workspace containing beads.

The filters work correctly. The 0 result occurs because Pluck queries the wrong database (`/home/coding/.beads/beads.db` instead of `/home/coding/claude-governor/.beads/beads.db`).
