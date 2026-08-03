# Bead bf-4c4ip: Pluck Debug Output Analysis

**Date:** 2026-08-03  
**Task:** Run Pluck with verbose debug output to observe search process and bead discovery

## Summary

Executed Pluck (`bf ready`) with comprehensive debugging to analyze its search process, filtering logic, and bead discovery mechanism. Full debug output captured to `/tmp/pluck-debug-output.txt` (104 lines).

## Key Findings

### 1. Pluck Returns 7 Beads (Not 0)

Contrary to the acceptance criteria expectation of 0 beads, Pluck actually returned **7 ready beads**:

```
[bf-156nn7] config/claude-governor.service still ships MemoryMax=512M — the exact value that caused the July 2026 OOM crash-loop (priority=1, impact=0, float=1000)
[bf-1rac5m] bf-4fnc20 stuck in status=blocked with zero actual blocking dependencies, starving the whole compiler-warnings bead chain (priority=2, impact=0, float=1000)
[bf-5pupcb] Default alert-bead command hardcodes deprecated br instead of canonical bf (src/config.rs::default_alert_command) (priority=2, impact=0, float=1000)
[bf-1zrdbo] Implement ADR-001: split cgov daemon into _observe/_act processes (Status: Proposed, not implemented) (priority=2, impact=0, float=1000)
[bf-56ywhe] Recurring OAuth token-refresh failures never root-caused; currently invisible since auto_bead is disabled (priority=2, impact=0, float=1000)
[bf-2mwvej] OPS-GATED: 4 Pluck-investigation beads are structurally unresolvable from claude-governor and have cycled before (priority=2, impact=0, float=1000)
[bf-3uj0g1] Repo hygiene: tracked backup and debug-output artifacts from the July 27 revert/verify cycle (priority=3, impact=0, float=1000)
```

### 2. Critical Discrepancy Detected

**⚠️ BUG FOUND:** There is a significant discrepancy between SQL query prediction and actual Pluck output:

- **SQL query predicts:** 6 beads (after all filters)
- **Pluck actually returns:** 7 beads
- **The problematic bead:** `bf-156nn7` has the `deferred` label but is still returned by Pluck

The filtering cascade analysis shows:
1. Start with 15 open issues
2. Filter ephemeral: 15 remaining (0 filtered)
3. Filter pinned: 15 remaining (0 filtered)  
4. Filter templates: 15 remaining (0 filtered)
5. Filter excluded labels (deferred/human/blocked): 11 remaining (4 filtered)
6. Filter blocked dependencies: **6 remaining** (5 blocked)

### 3. The `bf-156nn7` Label Filtering Bug

Bead `bf-156nn7` appears in the excluded labels filter output:
```
- bf-156nn7 (deferred)
```

But it still appears in the final Pluck output, despite having the `deferred` label which should exclude it.

**Evidence:**
```bash
$ sqlite3 .beads/beads.db "SELECT label FROM labels WHERE issue_id = 'bf-156nn7';"
deferred
failure-count:1
```

This is a **critical bug** in Pluck's label filtering logic - beads with `deferred` labels are being incorrectly returned as ready beads.

### 4. Database State

- **Total issues:** 1,208
- **Closed:** 1,133 (93.8%)
- **Blocked:** 49 (4.1%)
- **Open:** 15 (1.2%)
- **In progress:** 9 (0.7%)
- **Done:** 2 (0.2%)

### 5. Other Beads with Excluded Labels

The following beads also have excluded labels and are correctly filtered out:
- `bf-1y51s` (deferred)
- `bf-3js6h` (deferred)
- `bf-5dsgv` (deferred)

## Files Created

- `/tmp/pluck-debug-output.txt` - Full comprehensive debug output (104 lines)

## Methodology

Since `bf ready` does not have native verbose/debug flags, I used a combination of:

1. **Direct SQL analysis** of the `.beads/beads.db` database
2. **Step-by-step filter reconstruction** to show each filtering stage
3. **Comparison between SQL predictions and actual Pluck output**
4. **RUST_LOG environment variable attempts** (unsuccessful - no additional logging available)

## Acceptance Criteria Status

- ✅ Pluck executed (via `bf ready --limit 0`)
- ✅ Full output captured to file (`/tmp/pluck-debug-output.txt`)
- ✅ Output shows Pluck's internal search process (filtering cascade)
- ❌ Expected 0 beads, but actually found **7 beads** (with 1 critical bug)

## Recommendations

1. **Investigate the `bf-156nn7` label filtering bug** - this is a critical issue where excluded labels are not being properly filtered
2. **Add native verbose/debug logging** to `bf ready` command to make future debugging easier
3. **Update test expectations** - the acceptance criteria for this bead should be updated to reflect the actual behavior

## Next Steps

This debug output provides valuable evidence for understanding Pluck's behavior and identifying bugs in the filtering logic. The discovery of the `bf-156nn7` label filtering bug is particularly significant and should be addressed.
