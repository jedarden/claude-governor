# Pluck exclude_labels Filter Test Results

**Task:** Test Pluck exclude_labels filter in isolation  
**Date:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`

## Executive Summary

The `exclude_labels` filter is **NOT too broad**. The default configuration `['deferred', 'human', 'blocked']` filters only **1 bead (7.1%)** from the pool of unassigned open beads. The filter works correctly with exact label matching and supports multiple labels as expected.

## Test Environment

- **Total open beads:** 16
- **Unassigned open beads:** 14 (2 have empty-string assignees instead of NULL)
- **Database:** `.beads/beads.db` (SQLite, 4.3 MB)
- **Test method:** Direct SQL queries simulating Pluck's filter logic

## Test Results

### Baseline (No Label Filters)

**Unassigned open beads: 14**

```
bf-156nn7, bf-4c4ip, bf-5be7lz, bf-2h8n23, bf-3ww0k4, bf-3zdrza,
bf-5pupcb, bf-1zrdbo, bf-56ywhe, bf-2mwvej, bf-9gjr8i, bf-uc3582,
bf-pdjq78, bf-3uj0g1
```

### Test 1: Empty exclude_labels (no filtering)

**Result:** 14 beads  
**Filtered:** 0 beads (0%)

### Test 2: exclude_labels = ['deferred']

**Result:** 13 beads  
**Filtered:** 1 bead (7.1%)  
**Excluded bead:** `bf-156nn7` (has `deferred` label)

### Test 3: exclude_labels = ['human']

**Result:** 14 beads  
**Filtered:** 0 beads (0%)  
**Note:** No beads in workspace have the `human` label

### Test 4: exclude_labels = ['blocked']

**Result:** 14 beads  
**Filtered:** 0 beads (0%)  
**Note:** No beads in workspace have the `blocked` label

### Test 5: exclude_labels = ['deferred', 'human', 'blocked'] (DEFAULT)

**Result:** 13 beads  
**Filtered:** 1 bead (7.1%)  
**Excluded bead:** `bf-156nn7` (has `deferred` label)

### Test 6: exclude_labels = ['deferred', 'split-child']

**Result:** 6 beads  
**Filtered:** 8 beads (57.1%)  
**Excluded beads:** `bf-156nn7, bf-4c4ip, bf-5be7lz, bf-2h8n23, bf-3ww0k4, bf-3zdrza, bf-9gjr8i, bf-pdjq78`

### Test 7: exclude_labels = ['umbrella']

**Result:** 13 beads  
**Filtered:** 1 bead (7.1%)  
**Excluded bead:** `bf-5be7lz` (has `umbrella` label)

### Test 8: Wildcard Pattern ('deferred%')

**Result:** 13 beads  
**Note:** Uses SQL `LIKE` pattern matching, NOT standard Pluck behavior

## Key Findings

### 1. Default Filter is Minimal (NOT Too Broad)

The default `exclude_labels = ['deferred', 'human', 'blocked']` configuration:
- Filters **1 bead (7.1%)** from 14 unassigned open beads
- Only the `deferred` label is actively used in this workspace
- The `human` and `blocked` labels have zero impact currently

**Conclusion:** The default filter is conservative and appropriate.

### 2. Assignee Field Issue Detected

Two open beads have **empty-string assignees** instead of `NULL`:
- `bf-1y51s` - Diagnose configuration filter and exclude_labels issues
- `bf-3js6h` - Reproduce Pluck starvation issue

These beads are **incorrectly excluded** from the unassigned count because Pluck queries use `assignee IS NULL` instead of `assignee IS NULL OR assignee = ''`.

**Impact:** Undercounts available beads by 2 (14.3% of open beads).

### 3. Label Distribution in Workspace

Current label usage on open beads:
- `deferred`: 3 beads (bf-156nn7, bf-1y51s, bf-3js6h)
- `split-child`: 10 beads (majority of workspace)
- `umbrella`: 3 beads (bf-5be7lz, plus 2 with deferred)
- `plan-gap`: 6 beads
- `failure-count:X`: 6 beads
- `human`: 0 beads
- `blocked`: 0 beads

### 4. Filter Behavior Verification

The `exclude_labels` filter:
- Uses exact label matching (SQL `NOT EXISTS` with `IN` clause)
- Supports multiple labels correctly (AND logic: excludes if ANY label matches)
- Does NOT support wildcard patterns (unless using SQL `LIKE` directly)
- Is applied AFTER status/assignee filtering in the query pipeline

## Filter Impact Analysis

### Default Configuration Impact

```
exclude_labels = ['deferred', 'human', 'blocked']
├── Active filters: deferred (1 bead excluded)
├── Inactive filters: human (0 beads), blocked (0 beads)
└── Total filtered: 1 bead (7.1%)
```

### Aggressive Configuration Example

```
exclude_labels = ['deferred', 'split-child']
├── Active filters: deferred (1 bead), split-child (7 beads)
└── Total filtered: 8 beads (57.1%)
```

**Recommendation:** Do NOT add `split-child` to default exclude_labels unless intentionally filtering out half the workspace.

## Recommendations

1. **Keep default exclude_labels as-is:** `['deferred', 'human', 'blocked']` is appropriately conservative.

2. **Fix assignee NULL check:** Update Pluck queries to use:
   ```sql
   assignee IS NULL OR assignee = ''
   ```
   This will make 2 more beads visible to workers.

3. **Monitor label usage:** The `deferred` label is the only active exclusion label. Consider whether `human` and `blocked` should remain in the default if they're never used.

4. **Document label semantics:** If `split-child` or `umbrella` labels have specific meanings, document whether they should be excluded by default.

## Test Script

The complete test script is available at:
```
tests/test_exclude_labels.sh
```

Run it to reproduce these results:
```bash
bash tests/test_exclude_labels.sh
```

## Conclusion

The `exclude_labels` filter is **working correctly and is NOT too broad**. The default configuration filters only 7.1% of beads, which is appropriate for excluding beads marked as deferred, human-only, or blocked. The only issue discovered is the empty-string assignee handling, which is unrelated to `exclude_labels` itself.
