# Pluck Search Process Analysis - Bead bf-2revpp

**Date:** 2026-08-03
**Workspace:** `/home/coding/claude-governor`
**Task:** Analyze Pluck search process from debug output

---

## Summary

Analyzed captured Pluck debug output from `bf-1jfy31` to understand the search/filter process and identify why Pluck may fail to find open beads.

---

## Files Analyzed

1. **`pluck-debug-output.txt`** - Primary Pluck execution output (JSONL format, 13KB, 10 lines)
2. **`notes/bf-56wnh-pluck-debug.log`** - NEEDLE worker boot and bead discovery logs (132 lines)
3. **`notes/bf-1jfy31.md`** - Documentation of debug capture methodology

---

## Key Finding: Command Output Format, Not Internal Search Process

The captured `pluck-debug-output.txt` contains **command results**, not the **internal search process**. This is a critical distinction:

### What Was Captured
```bash
RUST_LOG=debug bf ready --limit 0 --format json > pluck-debug-output.txt 2>&1
```

This produced:
- **10 lines** of JSONL output (one bead per line)
- Each line: Complete bead metadata (id, title, description, status, labels, priority, timestamps)
- All beads shown have `status: "open"` (ready/unblocked)

### What Was NOT Captured
- **No internal Pluck search logic visible**
- **No filter application process shown**
- **No candidate evaluation logs**
- **No query construction details**

---

## Search/Filter Process Identified

### 1. Command-Level Filter
The `bf ready` command applies these filters:
- **Status filter:** Only shows beads with `status: "open"` (unblocked)
- **Limit filter:** `--limit 0` = unlimited results (bypasses default limit of 10)
- **Format filter:** `--format json` = JSONL output structure

### 2. Internal Query Construction (Not Visible)
The debug output shows **results only**, not:
- How Pluck queries the SQLite bead store (`.beads/beads.db`)
- What SQL query is constructed
- How labels are applied/excluded
- How workspace path is used
- Filter parameter application order

### 3. Bead Discovery Process
From `bf-56wnh-pluck-debug.log`, we see:
```
2026-08-03T23:43:25.813029Z DEBUG needle::strand::explore: discovered workspace workspace=/home/coding/claude-governor
...
2026-08-03T23:43:25.813346Z DEBUG needle::strand::explore: workspace discovery complete root=/home/coding/ count=40
```

This shows NEEDLE's workspace discovery, not Pluck's bead filtering.

---

## Error Messages and Warnings

### In Pluck Output (pluck-debug-output.txt)
- **No errors or warnings present** - clean JSONL output only
- No indications of search failures or filter issues

### In NEEDLE Worker Log (bf-56wnh-pluck-debug.log)
Several warnings found, but **not related to Pluck search**:

1. **Line 68-74:** Splice strand warning (escalation bead creation disabled)
   ```
   WARN needle::strand: ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   Splice strand is ENABLED but strands.splice.report_workspace is NOT SET!
   Worker failure and live-loop detection will NOT create escalation beads.
   ```

2. **Lines 75-93:** Regex parse errors in secret sanitizer (gitleaks rules)
   - Multiple regex compilation errors for allowlist rules
   - gitleaks rule `generic-api-key` exceeds size limit
   - Not related to bead filtering

3. **Line 99:** Splice workspace not set warning
   ```
   WARN needle::config: strands.splice.enabled is true, but strands.splice.report_workspace is not set.
   ```

### Critical Observation
**No Pluck-specific errors or warnings** - The search itself appears to function correctly from a technical standpoint.

---

## Why Pluck May Fail to Find Open Beads

Based on the analysis, potential failure modes:

### 1. **Label-Based Exclusion (Most Likely)**
From bead **bf-2mwvej** in the output:
> "NEEDLE own hardcoded work-selection filter (a compiled-in constant excluding beads labeled deferred, human, or blocked)"

- **Hardcoded filter in NEEDLE** excludes beads with these labels: `deferred`, `human`, `blocked`
- This filter lives in `NEEDLE/src/strand/pluck.rs` - NOT in claude-governor
- Beads labeled `deferred` become invisible to the work-selector
- **4 investigation beads** carry the `deferred` label and are now permanently excluded

### 2. **Query Filter Mismatch**
The `bf ready` command shows:
- Only `status: "open"` beads
- If beads have `status: "blocked"` or other states, they won't appear

### 3. **Workspace Path Issues**
From NEEDLE log:
```
discovered workspace workspace=/home/coding/claude-governor
```
- Workspace must be in the discovered path list
- If workspace is misconfigured, beads won't be found

### 4. **Database State**
- `.beads/beads.db` is the live store
- If database is corrupted or out of sync, queries may fail
- JSONL checkpoint (`.beads/issues.jsonl`) may not reflect live state

---

## Debug Logging Limitations

### RUST_LOG=debug Produced No Internal Logs
From `bf-1jfy31.md`:
> "The `RUST_LOG=debug` environment variable was set, but no additional debug log lines appeared in the output."

**This means:**
- bead-forge's `ready` subcommand has minimal debug output
- Internal query construction is not logged at DEBUG level
- Search process is opaque from command-line debugging

### Recommended Debugging Approaches
For deeper debugging, consider:
1. **`RUST_LOG=trace`** - Maximum verbosity (may show internal operations)
2. **Module-specific logging:** `RUST_LOG=bead_forge=debug,sled=debug` (include database layer)
3. **Source-level debugging:** Add custom log statements to bead-forge Pluck implementation
4. **Direct SQLite query inspection:** Query `.beads/beads.db` directly to see what beads exist

---

## Findings Summary

### Search Process
- **Not visible in captured output** - only results shown
- Query construction happens internally in bead-forge/Pluck
- No debug logs expose the filter application logic

### Filter Process
- **Command-level:** `bf ready` filters by `status: "open"`
- **NEEDLE-level:** Hardcoded filter excludes `deferred`, `human`, `blocked` labels
- **Database-level:** SQLite query filters bead store

### Errors/Warnings
- **No Pluck-specific errors** - search functions correctly
- NeedLE warnings are unrelated (Splice configuration, secret sanitizer regex)

### Why Beads Are Invisible
- **Most likely cause:** NEEDLE's hardcoded label filter (`deferred`, `human`, `blocked`)
- This filter is **outside claude-governor's control** (lives in NEEDLE repo)
- Beads with these labels become permanently unworkable

---

## Recommendations

1. **For claude-governor workspace:**
   - Avoid using `deferred`, `human`, `blocked` labels on beads
   - Use alternative labels like `hold`, `manual-review`, `dependency-wait`

2. **For deeper debugging:**
   - Use `RUST_LOG=trace` to capture more internal logging
   - Query `.beads/beads.db` directly: `sqlite3 .beads/beads.db "SELECT id, title, status, labels FROM beads WHERE status='open';"`
   - Check NEEDLE's `src/strand/pluck.rs` for the hardcoded filter

3. **For structural resolution:**
   - File Pluck investigation beads against the NEEDLE repo instead
   - Only NEEDLE can modify the hardcoded label filter

---

## Acceptance Criteria

- ✅ **Debug output analyzed** - Reviewed all captured files
- ✅ **Search/filter process identified** - Documented command and NEEDLE-level filters
- ✅ **Errors/warnings noted** - Cataloged all warnings, confirmed none are Pluck-related
- ✅ **Findings documented** - Comprehensive analysis with root cause identification

---

## References

- **Bead bf-1jfy31:** "Capture and Save Pluck Debug Output"
- **Bead bf-5wmnh3:** "Document Pluck debug execution with RUST_LOG"
- **Bead bf-9gjr8i:** "Pluck debug flags investigation complete"
- **Bead bf-2mwvej:** "OPS-GATED: 4 Pluck-investigation beads are structurally unresolvable"
- **Pluck configuration:** `docs/plan/pluck-configuration.md`
