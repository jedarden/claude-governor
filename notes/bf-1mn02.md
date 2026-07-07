# Pluck Starvation Root Cause Analysis

**Bead:** bf-1mn02
**Date:** 2026-07-06
**Workspace:** /home/coding/claude-governor

## Summary

Open beads exist but Pluck found none because **ALL open beads are unassigned**, and Pluck queries for beads assigned to a specific agent.

## Data

Total analysis of open beads in `/home/coding/claude-governor/.beads/beads.db`:

| Metric | Count |
|--------|-------|
| Total open beads | 41 |
| Unassigned (assignee IS NULL/empty) | 41 (100%) |
| Assigned to an agent | 0 (0%) |
| With `deferred` label (excluded) | 18 |
| Without excluded labels | 23 |

## Root Cause

**Pluck queries for beads WHERE assignee = <agent_id>**, but all 41 open beads have `assignee = NULL`.

### The Problem Flow

1. NEEDLE worker (e.g., `claude-code-glm47-test-pluck-debug`) queries Pluck for work
2. Pluck executes: `SELECT * FROM issues WHERE status = 'open' AND assignee = 'claude-code-glm47-test-pluck-debug'`
3. Query returns 0 rows because no beads are assigned to that agent
4. Worker sees 0 claimable beads → starvation alert

### Secondary Filter Impact

Even if the assignee issue were fixed, 18 of 41 open beads have the `deferred` label and would be excluded by DEFAULT_EXCLUDE_LABELS:

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

This leaves 23 potentially claimable beads.

## Solutions

### Option 1: Query for Unassigned Beads (Recommended)

Pluck should query for unassigned beads when a worker has no assigned work:

```sql
-- Current query (returns 0)
SELECT * FROM issues WHERE status = 'open' AND assignee = '<agent_id>';

-- Proposed query (would return 23)
SELECT * FROM issues 
WHERE status = 'open' 
AND (assignee IS NULL OR assignee = '')
AND assignee != '<other_agent_id>';  -- Skip beads claimed by others
```

### Option 2: Pre-assign Beads to Workers

Have NEEDLE automatically assign unassigned beads to available workers before they query Pluck.

### Option 3: Hybrid Assignment Strategy

1. First, query for beads assigned to this worker
2. If none, query for unassigned beads
3. Assign an unassigned bead to this worker atomically

## Related Files

- Pluck source: `/home/coding/NEEDLE/src/strand/pluck.rs`
- Investigation scripts: `/home/coding/claude-governor/scratch/test_pluck_*.py`
- Configuration doc: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`

## Verification

```bash
# Verify all open beads are unassigned
sqlite3 /home/coding/claude-governor/.beads/beads.db "
SELECT COUNT(*) FROM issues 
WHERE status = 'open' 
AND (assignee IS NULL OR assignee = '');
"
# Returns: 41

# Verify no open beads are assigned
sqlite3 /home/coding/claude-governor/.beads/beads.db "
SELECT COUNT(*) FROM issues 
WHERE status = 'open' 
AND assignee IS NOT NULL AND assignee != '';
"
# Returns: 0
```
