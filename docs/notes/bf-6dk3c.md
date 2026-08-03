# Pluck Open Beads Query Test Results

**Date:** 2026-08-03  
**Task:** Test Pluck query for open beads  
**Bead ID:** bf-6dk3c

## Test Summary

Comprehensive testing of Pluck's ability to discover and filter open beads using the new test script `scripts/test-pluck-open-beads.sh`.

## Key Findings

### 1. Database State (2026-08-03)

```
Total issues: 1208
├── closed: 1132 (93.7%)
├── blocked: 50 (4.1%)
├── open: 17 (1.4%)
├── in_progress: 7 (0.6%)
└── done: 2 (0.2%)
```

### 2. Pluck Filter Configuration

**Excluded labels in `~/.config/needle/config.yaml`:**
```yaml
strands:
  pluck:
    exclude_labels:
    - deferred
    - human
    - blocked
```

### 3. Ready Bead Discrepancy Found

**Expected Behavior:**
- SQL query with all filters applied: **8 ready beads**
- Filters: `status='open' AND ephemeral=0 AND pinned=0 AND is_template=0 AND NOT IN excluded_labels AND NOT IN blocked_cache`

**Actual Behavior:**
- `bf ready --limit 0` returns: **9 beads**
- Extra bead: `bf-156nn7` (has 'deferred' label but appears in output)

### 4. Bug: bf-156nn7 Not Being Filtered

**Bead details:**
- **ID:** `bf-156nn7`
- **Title:** "config/claude-governor.service still ships MemoryMax=512M — the exact value that caused the July 2026 OOM crash-loop"
- **Labels:** `deferred`, `failure-count:1`, `plan-gap`
- **Priority:** 1
- **Status:** open
- **Blocked cache:** Not present
- **Ephemeral/pinned/is_template:** All 0

**Expected:** Should be EXCLUDED from `bf ready` due to 'deferred' label  
**Actual:** INCLUDED in `bf ready` output

### 5. Other Deferred Beads Correctly Filtered

Three other beads with 'deferred' labels are correctly excluded:
- `bf-1y51s` - "Diagnose configuration filter and exclude_labels issues"
- `bf-3js6h` - "Reproduce Pluck starvation issue"  
- `bf-5dsgv` - "Investigate Pluck configuration and bead visibility settings"

All three are also in the blocked cache, which may be a factor.

## Test Script Created

**File:** `scripts/test-pluck-open-beads.sh`

**Features:**
1. Database overview (total issues, status breakdown)
2. Excluded labels analysis (which beads have excluded labels)
3. Blocked issues cache inspection
4. Expected vs. actual ready beads comparison
5. Filter breakdown (shows filtering cascade step-by-step)
6. Specific `bf-156nn7` issue investigation
7. Comprehensive summary reporting

**Usage:**
```bash
bash scripts/test-pluck-open-beads.sh
```

## Root Cause Analysis

The discrepancy appears to be in the Pluck strand's label filtering logic. Possible causes:

1. **Incomplete label filtering** - Pluck may not be checking all excluded labels
2. **Caching issue** - The `bf-156nn7` bead may have been assigned the 'deferred' label after being cached as ready
3. **Filtering order** - The label filter may not be applied consistently across all query paths
4. **Multiple label handling** - Beads with multiple labels (deferred + others) may have filtering issues

## Related Documentation

- **Pluck Workspace Paths:** `docs/pluck-workspace-paths.md`
- **Pluck Starvation Investigation:** `docs/research/pluck-starvation-reproduction.md`
- **Basic Query Script:** `scripts/basic-pluck-query.sh`

## Recommendations

1. **Fix Pluck label filtering** - Investigate why `bf-156nn7` is not being excluded despite having the 'deferred' label
2. **Add test to CI** - Include `scripts/test-pluck-open-beads.sh` in any test suite to catch future filtering bugs
3. **Review blocked cache** - The relationship between blocked cache and label filtering needs clarification
4. **Update documentation** - Once fixed, update the Pluck starvation documentation to reflect the correct behavior

## Impact Assessment

**Low to Medium impact:**
- Only affects 1 bead currently (`bf-156nn7`)
- The bead is properly deferred and should not be worked
- Workers may accidentally claim a bead that should be excluded
- Could cause confusion if deferred work appears ready

**No immediate action required** but should be fixed to maintain proper bead filtering semantics.

## Test Deliverables

- ✅ Comprehensive test script created
- ✅ Discovered and documented filtering bug
- ✅ All tests committed to repository
- ✅ Documentation updated with findings

---

**Test Complete:** Pluck query for open beads successfully tested and bug documented.