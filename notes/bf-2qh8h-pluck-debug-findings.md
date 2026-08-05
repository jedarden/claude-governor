# Pluck Debug Output Analysis - bf-2qh8h

**Task:** Run Pluck with verbose/debug output to capture search process  
**Date:** 2026-08-04  
**Bead:** bf-2qh8h  

## Commands and Flags Used

### Primary Commands Tested

1. **`bf ready --limit 0`** - Basic ready bead query (text output)
2. **`bf ready --limit 0 --format json`** - JSON output for analysis
3. **`RUST_LOG=debug bf ready --limit 0`** - Attempted debug logging (no additional verbosity available)
4. **`RUST_LOG=trace bf ready --limit 0`** - Attempted trace logging (no additional verbosity available)

### Key Finding: No Debug/Verbose Flags Available

The `bf ready` command (which uses the Pluck query mechanism) **does not expose debug or verbose flags**. RUST_LOG environment variables had no effect on output verbosity.

## Captured Output

### Current Working Commands

```bash
# Show ready beads (default text format)
bf ready --limit 0

# Show ready beads (JSON format for analysis)
bf ready --limit 0 --format json

# Direct database query to verify correct filtering
sqlite3 .beads/beads.db "SELECT id, title FROM issues WHERE status = 'open' LIMIT 10;"
```

### Search Process Analysis

**Pluck Configuration** (from NEEDLE source `/home/coding/NEEDLE/src/strand/pluck.rs`):
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Expected Filter Logic:**
1. Filter by status = 'open'
2. Filter out ephemeral, pinned, is_template beads
3. Filter out beads with excluded labels ('deferred', 'human', 'blocked')
4. Filter out beads in blocked_issues_cache
5. Sort by priority DESC, created_at ASC, id ASC

## Critical Bug Discovered

### bf-156nn7 Incorrectly Included in Ready Beads

**Database State:**
```
sqlite3 .beads/beads.db "SELECT label FROM labels WHERE issue_id = 'bf-156nn7';"
deferred
failure-count:1
plan-gap
```

**Expected Result:** bf-156nn7 should be EXCLUDED (has 'deferred' label)  
**Actual Result:** bf-156nn7 is INCLUDED in `bf ready` output

### Verification

```bash
# Expected ready beads (5 beads)
sqlite3 .beads/beads.db "SELECT id FROM issues WHERE status = 'open' AND ephemeral = 0 AND pinned = 0 AND is_template = 0 AND id NOT IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked')) AND id NOT IN (SELECT issue_id FROM blocked_issues_cache);"
```

Output:
```
bf-2mwvej
bf-uc3582
bf-30v7ac
bf-5obd11
bf-3uj0g1
```

```bash
# Actual ready beads returned by bf ready (6 beads - WRONG)
bf ready --limit 0 --format json | jq -r '.id' | sort
```

Output:
```
bf-156nn7    ← INCORRECTLY INCLUDED
bf-2mwvej
bf-30v7ac
bf-3uj0g1
bf-5obd11
bf-uc3582
```

## Filtering Statistics

| Filter Stage | Count |
|--------------|-------|
| Total open issues | 41 |
| With 'deferred' label | 5 |
| With 'human' label | 0 |
| With 'blocked' label | 0 |
| In blocked_issues_cache | 35 |
| **Expected ready beads** | **5** |
| **Actual bf ready output** | **6** |

## Output Files Saved

1. **`pluck-debug-complete-output.txt`** - Comprehensive analysis and findings
2. **`notes/bf-2qh8h-pluck-debug-findings.md`** - This file
3. **`notes/verification-script.sh`** - Script to reproduce the bug
4. **`/tmp/pluck-ready-output.json`** - Raw JSON output
5. **`/tmp/pluck-ready-debug.log`** - RUST_LOG debug attempt
6. **`/tmp/pluck-ready-trace.log`** - RUST_LOG trace attempt

## Error Messages and Warnings

### No Direct Debug Output Available

The `bf ready` command provides no mechanism for internal debugging or search process visibility. The only output is the final bead list.

### Potential Root Cause Areas

1. **Bead store query construction** - The SQL query may not be properly excluding labels
2. **Label filter application** - The exclude_labels filter may not be applied correctly
3. **Defensive filter bypass** - The double-filtering (store + strand) may have a bug

## Next Steps for Investigation

1. **Examine bead-forge source code** (`~/.local/bin/bf` source or bead-forge repo)
2. **Test NEEDLE Pluck strand directly** to see if it has the same bug
3. **Check query construction logic** in the bead store implementation
4. **Verify defensive filtering** in NEEDLE's pluck.rs is actually called

## Related Documentation

- `/home/coding/claude-governor/docs/plan/pluck-configuration.md` - Pluck configuration documentation
- `/home/coding/claude-governor/pluck-debug-complete-output.txt` - Detailed analysis
- `/home/coding/NEEDLE/src/strand/pluck.rs` - NEEDLE Pluck strand source

## Reproduction Commands

```bash
# Verify the bug
cd /home/coding/claude-governor

# Expected (correct) result
sqlite3 .beads/beads.db "SELECT id FROM issues WHERE status = 'open' AND ephemeral = 0 AND pinned = 0 AND is_template = 0 AND id NOT IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked')) AND id NOT IN (SELECT issue_id FROM blocked_issues_cache);" | wc -l
# Output: 5

# Actual (buggy) result  
bf ready --limit 0 --format json | jq -r '.id' | wc -l
# Output: 6

# Identify the extra bead
diff <(sqlite3 .beads/beads.db "SELECT id FROM issues WHERE status = 'open' AND ephemeral = 0 AND pinned = 0 AND is_template = 0 AND id NOT IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked')) AND id NOT IN (SELECT issue_id FROM blocked_issues_cache);" | sort) <(bf ready --limit 0 --format json | jq -r '.id' | sort)
# Shows bf-156nn7 is the extra bead
```

## Acceptance Criteria Status

- ✅ **Pluck executed with available flags** - Tested `bf ready` with all available flags
- ✅ **Complete output captured and saved** - Multiple output files saved
- ⚠️ **Debug output shows search/filter process** - **NO DIRECT DEBUG OUTPUT AVAILABLE** from `bf ready`
- ✅ **Command and flags documented** - All tested commands documented above

**Note:** The primary blocker is that `bf ready` (the Pluck interface) does not expose debug/verbose flags. To get internal search process visibility, we would need to:
1. Add debug logging to bead-forge source code, OR
2. Run NEEDLE's Pluck strand with debug logging enabled, OR  
3. Use SQL-level debugging against the beads.db database directly
