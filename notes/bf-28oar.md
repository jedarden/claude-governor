# Pluck Query Construction and Filtering - Complete Analysis

## Task: bf-28oar - Verify and log Pluck query construction with exact filters

## Summary

This document captures the complete analysis of how Pluck constructs and executes queries, including all filter parameters and defensive filtering steps.

## Pluck Query Construction Process

### 1. Workspace Configuration
- **Workspace path**: `/home/coding/claude-governor`
- **Database path**: `.beads/beads.db` (SQLite)
- **Database exists**: True

### 2. Filter Parameters
Pluck uses the following filter parameters:

| Parameter | Default Value | Description |
|-----------|---------------|-------------|
| `assignee` | `None` | Only return beads assigned to this actor (None = unassigned only) |
| `exclude_labels` | `["deferred", "human", "blocked", "starvation-alert"]` | Labels that exclude beads from selection |
| `status` | `"open"` | Only return open beads |

### 3. SQL Query Construction

The exact SQL query Pluck constructs:

```sql
-- Without assignee filter
SELECT id, title, status, assignee, priority, created_at
FROM issues
WHERE status = 'open'
ORDER BY priority ASC, created_at ASC, id ASC

-- With assignee filter
SELECT id, title, status, assignee, priority, created_at
FROM issues
WHERE status = 'open' AND assignee = ?
ORDER BY priority ASC, created_at ASC, id ASC
```

**Key Points:**
- Always filters by `status = 'open'`
- Optionally filters by assignee if specified
- Sorts by deterministic order: `priority ASC, created_at ASC, id ASC`
- The `id` tie-breaker ensures identical ordering across platforms

### 4. Defensive Filtering (PluckStrand)

After receiving results from the store, Pluck applies additional defensive filtering:

1. **Exclude labels filter** (defensive guard):
   ```rust
   candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
   ```

2. **In-progress status filter**:
   ```rust
   candidates.retain(|b| {
       !(matches!(b.status, crate::types::BeadStatus::InProgress)
           || (b.status == crate::types::BeadStatus::Open && b.assignee.is_some()))
   });
   ```

3. **Stale assignee filter**:
   - Removes open beads with a non-None assignee
   - These are not claimable because another worker has them

## Test Results

### Test 1: Default Pluck Query
- **Configuration**: No assignee, default exclude labels
- **Store results**: 46 beads
- **After defensive filtering**: 28 claimable, 18 filtered
- **Filtering reasons**:
  - 18 beads filtered due to `deferred` label

### Test 2: Pluck Query with Agent
- **Configuration**: Agent `claude-code-glm47-test-pluck-debug`, default exclude labels
- **Store results**: 0 beads
- **After defensive filtering**: 0 claimable, 0 filtered
- **Result**: STARVATION - No beads assigned to this agent

### Test 3: Custom Exclude Labels
- **Configuration**: No assignee, exclude labels `["deferred", "human"]`
- **Store results**: 46 beads
- **After defensive filtering**: 28 claimable, 18 filtered
- **Same results as Test 1**: `blocked` and `starvation-alert` labels not present in workspace

### Test 4: No Exclude Labels
- **Configuration**: No assignee, no exclude labels
- **Store results**: 46 beads
- **After defensive filtering**: 46 claimable, 0 filtered
- **Result**: All open beads are claimable (including deferred ones)

## Key Findings

### 1. Query Construction is Correct
The SQL query Pluck constructs is exactly as specified in the configuration:
- Correctly filters by `status = 'open'`
- Correctly applies optional assignee filter
- Correctly sorts by deterministic priority order

### 2. Defensive Filtering Works as Intended
The defensive filtering in PluckStrand successfully removes:
- Beads with excluded labels (18 beads with `deferred` label)
- In-progress beads
- Open beads with stale assignees

### 3. Label Filtering is Effective
- Default exclude labels: `["deferred", "human", "blocked", "starvation-alert"]`
- 18 of 46 beads (39%) were filtered out due to `deferred` label
- No beads had `human`, `blocked`, or `starvation-alert` labels in this workspace

### 4. Agent Assignment Query Returns Empty Results
When querying for beads assigned to a specific agent (`claude-code-glm47-test-pluck-debug`):
- 0 store results
- This indicates no beads are currently assigned to this agent
- The agent should query without assignee filter to find unassigned work

## Verification

The query construction matches the expected configuration from:
- Pluck source: `/home/coding/NEEDLE/src/strand/pluck.rs:13`
- Default exclude labels: `["deferred", "human", "blocked", "starvation-alert"]`
- Documentation: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`

## Next Steps

This foundational logging captures exactly what Pluck queries. Future beads can:
- Test variations of filter parameters
- Verify different workspace configurations
- Test with different label combinations
- Verify sorting order determinism

## Test Script

The complete test script is available at:
`/home/coding/claude-governor/scratch/test_pluck_exact_query_with_logging.py`

This script can be run to verify Pluck query construction in any workspace.
