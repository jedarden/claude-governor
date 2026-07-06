# Root Cause Analysis: Pluck Bead Invisibility

**Bead ID:** bf-4xsc6  
**Date:** 2026-07-06  
**Workspace:** `/home/coding/claude-governor`

---

## Executive Summary

**Root Cause:** Setting `exclude_labels: []` in the global configuration **activates default label filtering** rather than disabling it. The code interprets an empty array as "use defaults" not "exclude nothing".

---

## Configuration Evidence

### Global Configuration (Active)
**File:** `~/.config/needle/config.yaml:24`

```yaml
strands:
  pluck:
    exclude_labels: []        # Empty array
    split_after_failures: 3
```

### Code Behavior (Source of Issue)
**File:** `/home/coding/NEEDLE/src/strand/pluck.rs:28-36`

```rust
pub fn new(exclude_labels: Vec<String>) -> Self {
    let labels = if exclude_labels.is_empty() {
        DEFAULT_EXCLUDE_LABELS            // ← EMPTY ARRAY TRIGGERS DEFAULTS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        exclude_labels
    };
    // ...
}
```

### Default Exclude Labels (Actually Active)
**File:** `/home/coding/NEEDLE/src/strand/pluck.rs:13`

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

---

## The Contradiction

### Reproduction Evidence (from bf-3js6h)
**Log file:** `/home/coding/.needle/logs/needle-relaunch-claude-governor-cgov-1.stderr.log`

```
2026-07-06T12:43:05.404136Z  INFO ... strand found candidates strand=pluck candidates=6 excluded=17
2026-07-06T12:43:06.874733Z  INFO ... strand found candidates strand=pluck candidates=0 excluded=23
```

**Pattern:** Progressive filtering from 6→0 candidates, excluded count 17→23

### Manual Check Evidence (from bf-3js6h)
```bash
$ br list --status open | wc -l
49
```

**Claim:** "All 49 open beads are eligible for Pluck (none have excluded labels)"

---

## Root Cause Identification

### The Counterintuitive Behavior

**User Intent:** Setting `exclude_labels: []` was likely intended to **disable** label filtering

**Actual Behavior:** Empty array **activates** the default filter excluding:
- `deferred` - Beads marked for later processing
- `human` - Beads requiring human intervention  
- `blocked` - Beads with blocking dependencies
- `starvation-alert` - Beads flagged for starvation risk

### Why This Causes Invisibility

1. **Configuration applies defaults:** When NEEDLE starts, it reads `exclude_labels: []` from config
2. **Code interprets empty as default:** The `PluckStrand::new()` function checks `if exclude_labels.is_empty()`
3. **Defaults are activated:** The 4 default labels become active filter criteria
4. **Beads get filtered:** Any bead with these labels is excluded from candidate selection
5. **Starvation occurs:** If enough beads acquire these labels, Pluck finds 0 candidates

### Evidence Labels Were Present

From child bead bf-6b65h investigation:

| Label | Example Bead | Status |
|-------|-------------|--------|
| `deferred` | bf-21swe | ✅ Has label |
| `human` | docs-7d4 | ✅ Has label |
| `starvation-alert` | bf-3jo4t | ✅ Has label |
| `blocked` | bf-3jo4t | ✅ Has label (with other labels) |

---

## Verification of Root Cause

### Test the Configuration
```bash
# Current config activates defaults
cat ~/.config/needle/config.yaml | grep -A 2 "pluck:"
# Output:
#   pluck:
#     exclude_labels: []        ← This activates DEFAULT_EXCLUDE_LABELS

# Verify code behavior
grep -A 10 "fn new" /home/coding/NEEDLE/src/strand/pluck.rs
# Shows: if exclude_labels.is_empty() → use DEFAULT_EXCLUDE_LABELS
```

### Check if Beads Have Excluded Labels
```bash
# Find beads with deferred label
sqlite3 .beads/beads.db "SELECT id FROM issues WHERE status='open' AND id IN (SELECT issue_id FROM labels WHERE label='deferred');"

# Find beads with any excluded label
sqlite3 .beads/beads.db "SELECT id, title FROM issues WHERE status='open' AND id IN (SELECT issue_id FROM labels WHERE label IN ('deferred','human','blocked','starvation-alert'));"
```

---

## The Fix Path

### Option 1: Explicitly Set Empty Behavior (Code Change)
**File:** `/home/coding/NEEDLE/src/strand/pluck.rs`

Change the logic to treat empty array as "no filtering":

```rust
pub fn new(exclude_labels: Vec<String>) -> Self {
    let labels = if exclude_labels.is_empty() {
        vec![]  // Empty means no filtering, not defaults
    } else {
        exclude_labels
    };
    // ...
}
```

### Option 2: Use Sentinel Value (Config Change)
**File:** `~/.config/needle/config.yaml`

```yaml
strands:
  pluck:
    exclude_labels: ["none"]  # Special value to disable defaults
```

Update code to handle `"none"` as disable signal.

### Option 3: Explicit Empty List (Config Change)
**File:** `~/.config/needle/config.yaml`

```yaml
strands:
  pluck:
    exclude_labels: null  # Use null to mean "no filtering"
```

Update code to handle `null` vs `[]` differently.

### Option 4: Explicitly List All Labels (Current Workaround)
**File:** `~/.config/needle/config.yaml`

```yaml
strands:
  pluck:
    exclude_labels: []  # Current (activates defaults)
```

To disable: explicitly set to `exclude_labels: [""]` or similar sentinel.

---

## Recommended Fix

**Short-term (config):** Document that `exclude_labels: []` activates defaults

**Long-term (code):** Change behavior to:
- `exclude_labels: null` → Activate defaults  
- `exclude_labels: []` → No filtering (empty array)
- `exclude_labels: ["deferred", ...]` → Custom filtering

This makes the configuration intuitive: empty = empty.

---

## Impact Assessment

### Affected Workspaces
All workspaces using the global configuration with `exclude_labels: []`

### Symptom
Beads with `deferred`, `human`, `blocked`, or `starvation-alert` labels are invisible to Pluck, causing starvation even when open beads exist.

### Severity
**High** - This is the primary bead selection strand. When it starves, all bead processing stops.

---

## Related Documentation

- **Pluck configuration:** `notes/bf-3y6nm.md`
- **Workspace path verification:** `notes/bf-2bxsv.md`
- **Exclude labels investigation:** `notes/bf-6b65h.md`
- **Starvation reproduction:** `notes/bf-3js6h.md`
- **NEEDLE source:** `/home/coding/NEEDLE/src/strand/pluck.rs`

---

## Conclusion

The root cause is a **design flaw in the configuration logic**: empty array activates defaults instead of disabling filtering. This is counterintuitive and causes unexpected bead invisibility when users set `exclude_labels: []` intending to disable label-based filtering.

The evidence clearly links the configuration setting to the symptom:
1. Config has `exclude_labels: []`
2. Code interprets empty as "use defaults" 
3. Defaults exclude 4 specific labels
4. Beads with these labels are filtered out
5. Pluck starves when too many beads have these labels

**Fix path identified:** Change code behavior to treat `null` as "use defaults" and `[]` as "no filtering".
