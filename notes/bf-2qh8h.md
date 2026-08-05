# Pluck Debug Output Summary - bf-2qh8h

## Task Completed: Run Pluck with debug output to capture search process

**Date:** 2026-08-04
**Commands Used:**
```bash
# Main database connectivity test
cargo test test_pluck_database_connectivity -- --nocapture --exact

# Filter combinations test
cargo test --test pluck_filter_combinations_test -- --nocapture

# Workspace mismatch test
cargo test --test pluck_workspace_mismatch_test -- --nocapture

# Combined output with capture
cargo test test_pluck_database_connectivity pluck_filter_combinations pluck_workspace_mismatch -- --nocapture 2>&1 | tee pluck-debug-output-full.log
```

## Execution Summary

### Method Used
1. **Direct test execution** - Ran Pluck's built-in Rust tests with `--nocapture` flag
2. **Three test suites executed:**
   - `test_pluck_database_connectivity` - Database connection and query construction
   - `test_pluck_filter_combinations` - Filter impact analysis
   - `test_pluck_workspace_mismatch` - Workspace path resolution testing

### Commands Documented

**View live pluck debug logs:**
```bash
tail -f ~/.needle/logs/needle-*.stderr.log | grep "strand.pluck"
```

**Query pluck telemetry events:**
```bash
needle logs --filter 'event_type=strand.pluck.starvation_detected' --since 1h --format jsonl
```

**Enable real-time debug output:**
```bash
# Edit ~/.config/needle/config.yaml
telemetry:
  stdout_sink:
    enabled: true
    format: normal
    color: auto
```

## Key Findings from Debug Output

### 1. Pluck Filter Parameters
```
Workspace path: /home/coding/claude-governor/.beads/beads.db
State filter: 'open'
Labels (include filter): [] (0 labels)
Exclude labels (exclude filter): ["deferred", "human", "blocked"] (3 labels)
Assignee filter: 'IS NULL' (always filters unassigned issues)
```

### 2. Pluck Query Construction Process

Pluck builds SQL queries step-by-step with detailed logging:

```
=== QUERY CONSTRUCTION START ===
Workspace: /home/coding/claude-governor/.beads/beads.db
Initial parameters provided:
  - state_filter: 'open'
  - labels_filter: 0 labels
  - exclude_labels_filter: 3 labels
✓ Added SELECT clause for counting distinct issue IDs
✓ Added FROM clause (issues table aliased as 'i')
✓ Added LEFT JOIN for labels table
✓ Added WHERE clause with state filter: 'open'
✓ Added assignee filter: IS NULL (Pluck always filters unassigned issues)
✓ Added exclude_labels filter (NOT EXISTS):
    Excluded labels: ["deferred", "human", "blocked"]
○ No labels filter (empty - no label inclusion requirement)
=== QUERY CONSTRUCTION COMPLETE ===
Total query components: 6
Total filter parameters tracked: 5
Final query parameters: ["state:open", "assignee:NULL", "exclude:deferred", "exclude:human", "exclude:blocked"]
```

### 3. Final SQL Query Executed

```sql
SELECT COUNT(DISTINCT i.id)
FROM issues i
LEFT JOIN labels l ON l.issue_id = i.id
WHERE i.status = 'open'
AND i.assignee IS NULL
AND NOT EXISTS (
  SELECT 1 FROM labels
  WHERE issue_id = i.id
  AND label IN ('deferred', 'human', 'blocked')
)
```

### 4. Query Execution Results

- **Claimable issues (Pluck query result): 36**
- **Issues excluded by Pluck filters: 83**
- **Total issues in database: 1,262**
- **Open issues: 41**
- **Issues with labels: 768**

### 5. Filter Impact Analysis

| Filter Combination | Result Count | % Reduction |
|---|---|---|
| BASE - No filters | 1,262 | 0% |
| Only state = 'open' | 41 | 96.75% |
| Only assignee IS NULL | 373 | 70.44% |
| Only exclude_labels (deferred, human, blocked) | 1,179 | 6.58% |
| state + assignee | 40 | 96.83% |
| state + exclude_labels | 36 | 97.15% |
| assignee + exclude_labels | 369 | 70.76% |
| **FULL QUERY (all filters)** | **36** | **97.15%** |

## Root Cause Analysis - Workspace Path Issue

**Problem Identified:**
Pluck uses `workspace='.'` which resolves to the **parent directory** instead of the current working directory when executed by NEEDLE.

**Evidence from Tests:**
```
=== ROOT CAUSE ANALYSIS ===
Problem: Pluck uses workspace='.' which resolves differently than expected

Current directory resolution:
  - In shell: cwd = /home/coding/claude-governor
  - In NEEDLE: workspace='.' → /home/coding (parent!)

Result:
  - Correct database: /home/coding/claude-governor/.beads/beads.db → 36 ready beads
  - Wrong database:   /home/coding/.beads/beads.db → 0 ready beads
```

**Impact:**
When Pluck queries the wrong database, it finds 0 candidates, causing the starvation issue.

**Solution:**
Pluck needs absolute workspace path, not relative '.'

### Database Comparison

**Correct Database (/home/coding/claude-governor):**
```
✅ Connection successful
   Total issues: 1262
   Open issues: 41
   Ready beads (Pluck query): 36
```

**Wrong Database (/home/coding - parent):**
```
✅ Connection successful
   Total issues: 5
   Open issues: 0
   Ready beads (Pluck query): 0
   ⚠️  BLOCKING: Pluck would return 0 candidates here!
```

## Conclusion

Pluck successfully finds **36 claimable beads** when using the correct database path (`/home/coding/claude-governor/.beads/beads.db`), but returns **0 beads** when using relative path `.` due to workspace resolution issues in the NEEDLE context.

The debug output shows:
1. **All filter parameters are properly logged before execution**
2. **Query construction is step-by-step with detailed logging**
3. **The SQL query is valid and executes successfully**
4. **The root cause is workspace path resolution, not query construction**

### Filter Performance Analysis

The filters work correctly:
- **State filter** (`status = 'open'`) reduces 1,262 → 41 issues (96.75% reduction)
- **Assignee filter** (`IS NULL`) reduces 41 → 40 issues
- **Exclude labels filter** (deferred, human, blocked) reduces 40 → 36 issues
- **Final result**: 36 claimable beads

The problem is that when Pluck uses `workspace='.'`, it resolves to `/home/coding` instead of `/home/coding/claude-governor`, causing it to query the wrong database.

## Acceptance Criteria Met

- ✅ Pluck executed with debug/verbose flags (`cargo test -- --nocapture`)
- ✅ Complete output captured and saved (`pluck-debug-output-full.log`)
- ✅ Debug output shows search/filter process (query construction, verification, execution)
- ✅ Command and flags documented

## Files Generated

- `notes/bf-2qh8h.md` - This comprehensive analysis
- `pluck-debug-output-full.log` - Complete test output with all debug information (8.0 KB, 265 lines)

## Commands Documented

```bash
# Run all Pluck tests with debug output
cargo test test_pluck_database_connectivity pluck_filter_combinations pluck_workspace_mismatch -- --nocapture 2>&1 | tee pluck-debug-output-full.log

# Individual tests
cargo test test_pluck_database_connectivity -- --nocapture --exact
cargo test --test pluck_filter_combinations_test -- --nocapture
cargo test --test pluck_workspace_mismatch_test -- --nocapture

# Check database directly
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status = 'open' AND assignee IS NULL;"

# Run bead-forge ready command
bf ready --limit 0
```
