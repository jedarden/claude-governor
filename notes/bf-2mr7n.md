# Bead bf-2mr7n: Pluck Query Logging Verification

**Date:** 2026-08-03
**Task:** Verify Pluck query logging output matches configuration

## Summary

Ran the Pluck database test with comprehensive logging enabled to verify that query construction matches the expected configuration. All filter parameters and query structure match the documented configuration.

## Test Execution

### Command Run
```bash
cargo test test_pluck_database_connectivity -- --nocapture
```

### Expected Configuration

Based on `/home/coding/.config/needle/config.yaml`:

```yaml
strands:
  pluck:
    exclude_labels:
    - deferred
    - human
    - blocked
    split_after_failures: 3
    persistent_starvation_records: false

workspace:
  default: /home/coding
  home: /home/coding/.needle
  labels: []
```

## Actual Logging Output Analysis

### 1. Filter Parameters Logged

```
=== PLUCK FILTER PARAMETERS ===
workspace_path: /home/coding/claude-governor/.beads/beads.db
labels (include filter): []
exclude_labels (exclude filter): ["deferred", "human", "blocked"]
state (status filter): open
===============================
```

**✅ VERIFIED:** All filter parameters match expected configuration
- `workspace_path`: Points to the correct database location
- `labels` (include filter): Empty array `[]` matches configuration
- `exclude_labels`: Exact match with `["deferred", "human", "blocked"]`
- `state`: Correctly set to `"open"` for ready bead queries

### 2. Query Construction Parameters

```
=== PLUCK QUERY CONSTRUCTION ===
Using state filter: 'open'
Using exclude_labels filter: ["deferred", "human", "blocked"]
Using labels filter: []
===============================
```

**✅ VERIFIED:** Query construction uses the exact filter values from configuration

### 3. Final Query Structure

The actual SQL query constructed:

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

**✅ VERIFIED:** Query structure implements the configured filters correctly:
- `i.status = 'open'` - Filters for open beads
- `i.assignee IS NULL` - Filters for unassigned beads
- `NOT EXISTS (...)` - Excludes beads with the configured exclude_labels

### 4. Query Results

```
Claimable issues (Pluck query result): 14
Issues excluded by Pluck filters: 81
```

**Database Statistics:**
- Total issues in database: 1212
- Open issues: 17
- Issues with labels: 723
- Claimable (ready): 14
- Excluded by filters: 81

## Comparison with Actual Pluck Output

### Current `bf ready` Output (10 beads)
```
[bf-5mxydp] Create safety branches and backup current state
[bf-9gjr8i] Investigate Pluck's help to identify debug flags
[bf-4c4ip] Run Pluck with verbose debug output
[bf-156nn7] config/claude-governor.service still ships MemoryMax=512M
[bf-1rac5m] bf-4fnc20 stuck in status=blocked with zero actual blocking dependencies
[bf-5pupcb] Default alert-bead command hardcodes deprecated br instead of canonical bf
[bf-1zrdbo] Implement ADR-001: split cgov daemon into _observe/_act processes
[bf-56ywhe] Recurring OAuth token-refresh failures never root-caused
[bf-2mwvej] OPS-GATED: 4 Pluck-investigation beads are structurally unresolvable
[bf-3uj0g1] Repo hygiene: tracked backup and debug-output artifacts
```

### Discrepancy Analysis

**⚠️ DISCREPANCY FOUND:** The test query predicts 14 claimable beads, but actual `bf ready` returns only 10 beads.

**Known Issue (from bf-4c4ip analysis):**
- Bead `bf-156nn7` has label `deferred` but appears in ready list
- This suggests the exclude_labels filter may not be working correctly in production Pluck
- The test query correctly excludes deferred labels (14 vs larger number)

### Potential Causes for 14 vs 10 Discrepancy

1. **Dependency filtering:** The test query doesn't check for blocking dependencies, which production Pluck may filter
2. **Additional filters:** Production Pluck may apply additional filters not captured in the test
3. **Stale data:** The database may have changed between test run and `bf ready` execution
4. **Assignee changes:** Some beads may have been assigned since the database snapshot

## Discrepancies Found

### 1. **Count Mismatch** (14 test vs 10 production)
- **Test result:** 14 claimable beads
- **Production result:** 10 ready beads
- **Impact:** Test query is more permissive than production Pluck
- **Likely cause:** Missing dependency blocking logic in test query

### 2. **Filter Implementation Verification**
- **Test:** Correctly implements exclude_labels in SQL
- **Production:** May have filter application issue (known issue from bf-4c4ip)

## Conclusions

### ✅ What Matches
1. **Filter parameters:** All logged filter parameters exactly match configuration
2. **Exclude labels:** `["deferred", "human", "blocked"]` correctly implemented
3. **State filter:** `"open"` correctly applied
4. **Labels filter:** Empty array correctly results in no label inclusion filtering
5. **Query structure:** SQL query correctly implements the filter logic

### ⚠️ What Doesn't Match
1. **Bead count:** Test predicts 14, production returns 10
2. **Filter effectiveness:** Known issue where deferred beads appear in production

## Recommendations

1. **Add dependency filtering to test:** The test query should also filter out beads with blocking dependencies to match production behavior
2. **Investigate production filter bug:** Why does `bf-156nn7` (deferred) appear in `bf ready` output?
3. **Run test with fresh data:** Re-run test after updating database to ensure current comparison
4. **Add logging to production Pluck:** Enable similar logging in actual Pluck to compare real-time

## Test Environment

- **Test file:** `tests/pluck_db_test.rs`
- **Database:** `/home/coding/claude-governor/.beads/beads.db`
- **Configuration source:** `/home/coding/.config/needle/config.yaml`
- **Test execution:** Successful with all assertions passing

## Verification Status

**Acceptance Criteria Status:**
- ✅ Run the code with logging enabled
- ✅ Capture the log output
- ✅ Verify filter parameters match expected configuration
- ✅ Verify final query matches expected query structure
- ✅ Document any discrepancies found

**All acceptance criteria have been met.**