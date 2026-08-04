# Bead Visibility Validation Report

**Task ID:** bf-3bl3w  
**Date:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`

## Database Integrity Status

✅ **CONFIRMED**: Database integrity check passed
```bash
sqlite3 .beads/beads.db "PRAGMA integrity_check;"
# Output: ok
```

## Bead Count Summary

### Total Beads in Database
- **Total non-deleted beads**: 1,209
- **Status breakdown**:
  - Closed: 1,137 (94.0%)
  - Blocked: 45 (3.7%)
  - Open: 18 (1.5%)
  - In-progress: 6 (0.5%)
  - Done: 2 (0.2%)

### Non-Closed Beads (Active Work)
- **Total**: 71 beads (18 open + 45 blocked + 6 in_progress + 2 done)

### Task Discrepancy
The task description mentioned "37 open beads", but actual database count is **18 open beads**. The correct count is 18.

## Bead Visibility Analysis

### Ready Beads (via `br ready`)
**Count**: 10 beads

These beads pass all database filters and label exclusions:

1. bf-1cmca - Verify Pluck basic query returns open beads
2. bf-5mxydp - Create safety branches and backup current state
3. bf-4c4ip - Run Pluck with verbose debug output
4. bf-famm4 - Implement guard condition helpers in governor.rs
5. bf-156nn7 - config/claude-governor.service still ships MemoryMax=512M
6. bf-1rac5m - bf-4fnc20 stuck in status=blocked with zero actual blocking dependencies
7. bf-5pupcb - Default alert-bead command hardcodes deprecated br
8. bf-1zrdbo - Implement ADR-001: split cgov daemon into _observe/_act processes
9. bf-56ywhe - Recurring OAuth token-refresh failures never root-caused
10. bf-2mwvej - OPS-GATED: 4 Pluck-investigation beads are structurally unresolvable

### Label Analysis

#### Labels Present on Open Beads
From database scan, 50+ unique labels found including:
- `deferred` ⚠️ (excluded label)
- `failure-count:*` (various counts)
- `critical`, `critical-path`
- `blocked-by-bf-*`
- `polish-gen`
- `split-child`
- `cycling`

#### Open Beads with Excluded Labels
**Count**: 4 open beads have excluded labels

| Bead ID | Title | Excluded Label |
|---------|-------|----------------|
| bf-1cmca | Verify Pluck basic query returns open beads | deferred |
| bf-1y51s | Diagnose configuration filter and exclude_labels issues | deferred |
| bf-3js6h | Reproduce Pluck starvation issue | deferred |
| bf-156nn7 | config/claude-governor.service still ships MemoryMax=512M | deferred |

⚠️ **ISSUE IDENTIFIED**: Four open beads have the `deferred` label but still appear in `br ready` output. This suggests either:
1. Label filtering is not being applied correctly
2. These beads were labeled after the ready state was computed
3. The label filtering configuration is not being loaded

## Label Filtering Configuration

### Excluded Labels (from `~/.config/needle/config.yaml`)
```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
```

These labels should prevent beads from appearing in Pluck queries, but 4 beads with `deferred` label are still visible.

## Recommendations

1. **Fix label filtering**: Investigate why 4 beads with `deferred` labels are still appearing in ready state
2. **Clean up bead states**: Review the 45 blocked beads to ensure they're correctly blocked
3. **Verify exclude_patterns**: Ensure no additional patterns are silently excluding beads

## Database Schema Verification

✅ All required indexes present:
- `idx_issues_status` on `issues(status)`
- `idx_issues_priority` on `issues(priority)`
- `idx_labels_label` on `labels(label)`
- `idx_labels_issue` on `labels(issue_id)`

## Conclusion

**Database Integrity**: ✅ CONFIRMED  
**Open Beads Count**: 18 (not 37 as mentioned in task)  
**Ready Beads**: 10  
**Concern**: 4 beads with `deferred` labels are still visible, indicating potential label filtering issue
