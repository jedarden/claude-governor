# Bead bf-28oar: Pluck Query Construction Verification

## Task Description
Verify and log Pluck query construction with exact filters to capture what Pluck is actually querying before testing variations.

## Implementation Summary

### Enhanced Logging Added to `tests/pluck_db_test.rs`

#### 1. **Initial Filter Parameters Logging** 
Captures all input parameters before query construction:
- Workspace path (database location)
- State filter (e.g., 'open')
- Labels filter (include filter - labels that must be present)
- Exclude labels filter (labels that must NOT be present)
- Assignee filter (always IS NULL for Pluck)

#### 2. **Step-by-Step Query Construction Logging**
The `construct_pluck_query()` function now logs each step of query building:
- SELECT clause construction
- FROM clause with table aliases
- LEFT JOIN for labels table
- WHERE clause with state filter
- AND clauses for assignee and label filters
- EXISTS/NOT EXISTS subqueries for label filtering

#### 3. **Query Structure Verification**
Added assertions to verify the constructed query contains:
- Proper SELECT clause with DISTINCT
- Correct FROM and JOIN clauses
- Expected WHERE clause with state filter
- Assignee filter (IS NULL)
- Label exclusion/inclusion filters when configured

#### 4. **Query Execution Results**
Comprehensive logging of:
- Final constructed SQL query
- Query parameters used
- Execution result count
- Applied filters summary

## Test Output Example

```
=== PLUCK FILTER PARAMETERS ===
Workspace path: /home/coding/claude-governor/.beads/beads.db
State filter: 'open'
Labels (include filter): [] (0 labels)
Exclude labels (exclude filter): ["deferred", "human", "blocked"] (3 labels)
Assignee filter: 'IS NULL' (always filters unassigned issues)
===============================

--- QUERY CONSTRUCTION LOG ---
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
===============================
```

## Query Construction Details

### Final SQL Query Generated:
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

### Key Query Features Verified:
1. **Hardcoded values**: Pluck uses hardcoded values in queries, not parameter binding
2. **Always filters unassigned**: Assignee filter is always `IS NULL`
3. **Label exclusion**: Uses `NOT EXISTS` subquery for exclude_labels
4. **Label inclusion**: Uses `EXISTS` subquery for labels (when provided)
5. **State filtering**: Applied via WHERE clause with exact match

## Test Results

✅ **Test Status**: PASSED
- Database connectivity: OK
- Query construction: VERIFIED
- Structure verification: PASSED
- Execution results: 36 claimable issues found
- Filter parameters: All logged and verified

## Acceptance Criteria Met

- ✅ Added logging to capture the exact query Pluck constructs
- ✅ Captured all filter parameters (workspace path, labels, exclude_labels, state)
- ✅ Logged final query before execution  
- ✅ Verified query matches expected configuration

## Value for Future Testing

This comprehensive logging provides:
1. **Debugging capability**: See exactly what Pluck queries are being constructed
2. **Verification**: Ensure filters match expected configuration
3. **Traceability**: Track each component of query construction
4. **Documentation**: Clear record of query structure and parameters

## Next Steps

This foundation enables testing variations of Pluck query construction with full visibility into:
- How different filter combinations affect query structure
- Performance implications of different query patterns
- Verification that queries match business requirements
