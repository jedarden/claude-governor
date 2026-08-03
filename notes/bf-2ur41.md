# Pluck Filter Configuration Review (bf-2ur41)

## Task Completed

Successfully reviewed Pluck's filter configuration and documented all active filters, their logic, and potential restrictiveness issues.

## Filter Configuration Documentation

### 1. Configuration Location
- **Config file**: `/home/coding/.config/needle/config.yaml`
- **Source code**: `/home/coding/NEEDLE/src/strand/pluck.rs`
- **Config section**: `strands.pluck`

### 2. Active Filter Values

```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
    split_after_failures: 3
```

**Default exclude labels** (hardcoded in source, line 21):
- `deferred` - Beads postponed for later
- `human` - Beads requiring human intervention
- `blocked` - Beads blocked by dependencies

### 3. Complete Filter Pipeline

Pluck applies filters in a specific sequence during the `evaluate()` function:

#### Stage 1: Store Query Filters (lines 309-313)
```rust
Filters {
    assignee: None,                    // Unassigned beads only
    exclude_labels: ["deferred", "human", "blocked"],
    exclude_ids: HashSet::new(),
}
```

#### Stage 2: Defensive Label Filtering (lines 360-411)
- **Double-check**: Verifies store didn't miss excluded labels
- **Reason**: Some `bf ready --json` outputs omit label data
- **Prevents**: SELECTING→CLAIMING→RETRYING hot loop
- **Filters**: Any bead with labels matching exclude_list

#### Stage 3: Status & Assignee Filtering (lines 416-469)
Removes beads that are:
- `InProgress` status (already claimed by another worker)
- `Open` status with non-NULL assignee (stale assignments)

#### Stage 4: Metadata Filters (inferred from test notes)
- `ephemeral = 0` - Excludes temporary/transient beads
- `pinned = 0` - Excludes administratively pinned beads
- `is_template = 0` - Excludes template beads

#### Stage 5: Priority Sorting (lines 472-483)
```rust
Sort key: (priority ASC, failure_count ASC, created_at ASC, id ASC)
```

## Identified Restrictive Patterns

### 1. **Excluded Labels Hide Work**
**Issue**: Default `exclude_labels` are quite broad:
- `deferred` - Could include legitimate postponed work that should be reconsidered
- `human` - May flag complex beads as "human-only" when they could be partially automated
- `blocked` - Dependencies might be resolved but beads remain labeled

**Impact**: From bf-5xlaw.md, **81 beads were excluded by label filters** from a pool of 1,208 total beads.

**Recommendation**: Review if these labels are over-applied or if beads should be automatically re-labeled when conditions change.

### 2. **Stale Assignee Filter May Cause Starvation**
**Issue**: `Open` beads with an assignee are excluded even if the assignee is stale/dead.

**Current behavior**:
```rust
// Lines 422-424
matches!(b.status, crate::types::BeadStatus::InProgress) ||
(b.status == crate::types::BeadStatus::Open && b.assignee.is_some())
```

**Impact**: If a worker dies or crashes without releasing beads, those beads become permanently invisible to Pluck.

**Recommendation**: Implement stale-assignee detection:
- Check if assignee worker is still alive
- Auto-clear stale assignments after timeout
- Or add a "stale_assignee" exclusion reason to telemetry

### 3. **No Temporal Filters**
**Issue**: No time-based filters for:
- `defer_until` (beads with temporary postponement)
- `due_at` (urgency-based prioritization)

**Impact**: Beads postponed for a specific time may be invisible even after the defer period expires.

**Current status**: These fields exist in schema but aren't actively filtered.

### 4. **Ephemeral/Pinned/Template Filters Not Visible**
**Issue**: The metadata filters (`ephemeral`, `pinned`, `is_template`) are mentioned in test notes but not clearly visible in the main Pluck filtering logic.

**Impact**: Hard to verify if these filters are working as intended.

## Filter Logic Analysis

### Current Filter Chain
```
Store Query → Defensive Label Check → Status/Assignee Check → Sort → Return
```

### Query Pattern (from test notes)
```sql
-- Approximate SQL equivalent of Pluck filtering:
SELECT DISTINCT i.id
FROM issues i
LEFT JOIN labels l ON l.issue_id = i.id
WHERE i.status = 'open'
  AND i.assignee IS NULL
  AND i.ephemeral = 0
  AND i.pinned = 0
  AND i.is_template = 0
  AND NOT EXISTS (
    SELECT 1 FROM labels
    WHERE issue_id = i.id
    AND label IN ('deferred', 'human', 'blocked')
  )
ORDER BY i.priority ASC, i.created_at ASC, i.id ASC
```

### Current Statistics (from bf-5xlaw.md)
- **Total beads in database**: 1,208
- **Open beads**: 21 (before filtering)
- **Claimable issues (after filtering)**: 20
- **Excluded by filters**: 81

## Filters That Prevent Beads from Appearing

### High-Impact Filters
1. **`status != 'open'`** - Excludes 1,187 beads (closed, blocked, done, in_progress)
2. **`assignee IS NOT NULL`** - Excludes beads with stale assignments
3. **`label IN ('deferred', 'human', 'blocked')`** - Excludes 81 beads

### Medium-Impact Filters
4. **`ephemeral = 1`** - Excludes temporary beads
5. **`pinned = 1`** - Excludes administratively held beads
6. **`is_template = 1`** - Excludes template beads

### Verification
**Filter verification test** (from bf-4026a.md):
- Standard Pluck query returns 20 beads
- Query without label filters would return 21+ beads
- Difference = filter effectiveness

## Recommendations

### 1. Review Default Exclude Labels
**Action**: Audit beads with `deferred`, `human`, `blocked` labels to determine if they're:
- Correctly categorized (legitimate exclusions)
- Over-applied (should be re-labeled automatically)
- Stale (conditions changed but label wasn't updated)

### 2. Implement Stale Assignee Recovery
**Action**: Add logic to detect and clear stale assignments:
```bash
# Identify potentially stale assignments:
bf list --format json | jq '.[] | select(.status == "open" and .assignee != null)'
```

### 3. Add Telemetry for Filter Impact
**Action**: Already partially implemented in PluckStrand:
- `last_filtering_stats()` method exists (lines 195-206)
- Emits `PluckStarvationDetected` telemetry when all beads filtered
- **Recommendation**: Log filtering stats even when NOT starving to track filter impact

### 4. Consider Temporal Filter Logic
**Action**: If `defer_until` field exists in schema, add logic to:
- Skip beads where `defer_until > NOW()`
- Include beads where `defer_until <= NOW()` and remove `deferred` label

## Conclusion

Pluck's filter configuration is **moderately restrictive**. The combination of:
- Broad default exclude labels (deferred/human/blocked)
- Aggressive stale-assignee filtering
- Multiple metadata filters

creates a system where **20 out of 1,208 beads** (1.7%) are visible to workers.

The filters are **not too restrictive** for normal operations, but the **stale assignee handling** and **label lifecycle management** could be improved to prevent bead starvation.

## Files Referenced

- `/home/coding/.config/needle/config.yaml` - Main NEEDLE config
- `/home/coding/NEEDLE/src/strand/pluck.rs` - Pluck strand implementation
- `/home/coding/claude-governor/notes/bf-4026a.md` - Basic Pluck query test results
- `/home/coding/claude-governor/notes/bf-5xlaw.md` - Database connectivity test
- `/home/coding/claude-governor/notes/bf-22ks5.md` - Workspace path verification

## Date Completed

2026-08-03
