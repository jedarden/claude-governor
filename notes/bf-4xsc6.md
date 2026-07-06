# Root Cause Analysis: Pluck Bead Invisibility

**Bead ID:** bf-4xsc6
**Date:** 2026-07-06
**Workspace:** `/home/coding/claude-governor`

---

## Executive Summary

**Root Cause:** Setting `exclude_labels: []` in `~/.config/needle/config.yaml:88` activates default label filtering (`["deferred", "human", "blocked", "starvation-alert"]`), which filters out 17 out of 49 open beads (35% of the workspace).

**Evidence:** The initial log entry showing `excluded=17` exactly matches the count of beads with "deferred" labels in the workspace.

---

## Configuration Evidence (from child bead 1)

### Global Configuration (Active)
**File:** `~/.config/needle/config.yaml:88`

```yaml
strands:
  pluck:
    exclude_labels: []        # Empty array
    split_after_failures: 3
```

### Code Behavior (Source of Issue)
**File:** `/home/coding/NEEDLE/src/strand/pluck.rs:25-36`

```rust
/// If `exclude_labels` is empty, the default set (`deferred`, `human`,
/// `blocked`) is used.
pub fn new(exclude_labels: Vec<String>) -> Self {
    let labels = if exclude_labels.is_empty() {
        DEFAULT_EXCLUDE_LABELS            // ← EMPTY ARRAY TRIGGERS DEFAULTS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        exclude_labels
    };
```

### Default Exclude Labels (Actually Active)
**File:** `/home/coding/NEEDLE/src/strand/pluck.rs:13`

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

---

## Reproduction Evidence (from child bead 2)

**Log file:** `/home/coding/.needle/logs/needle-relaunch-claude-governor-cgov-1.stderr.log`

```
2026-07-06T12:43:05.404136Z  INFO ... strand found candidates strand=pluck candidates=6 excluded=17
2026-07-06T12:43:05.814821Z  INFO ... strand found candidates strand=pluck candidates=5 excluded=18
2026-07-06T12:43:05.927291Z  INFO ... strand found candidates strand=pluck candidates=4 excluded=19
2026-07-06T12:43:06.139230Z  INFO ... strand found candidates strand=pluck candidates=3 excluded=20
2026-07-06T12:43:06.551055Z  INFO ... strand found candidates strand=pluck candidates=2 excluded=21
2026-07-06T12:43:06.662621Z  INFO ... strand found candidates strand=pluck candidates=1 excluded=22
2026-07-06T12:43:06.874733Z  INFO ... strand found candidates strand=pluck candidates=0 excluded=23
```

**Pattern:** Progressive filtering from 6→0 candidates, excluded count 17→23

---

## Smoking Gun: Exact Count Match

### Bead Count Analysis
**Total open beads:** 49
**Beads with "deferred" label:** 17 out of 49 (35%)

**Sample filtered beads:**
- `bf-21swe`: Labels: `deferred, split-child, umbrella`
- `bf-38oc5`: Labels: `deferred, umbrella`
- `bf-42ovy`: Labels: `deferred, failure-count:4, umbrella`
- `bf-3g4ew`: Labels: `deferred`
- `bf-3z0vo`: Labels: `deferred`
- `bf-54ppq`: Labels: `deferred`
- `bf-3js6h`: Labels: `deferred`
- `bf-5dsgv`: Labels: `deferred`
- (9 more beads with deferred label)

### Perfect Correlation

The log shows `excluded=17` in the first entry. This **exactly matches** the count of beads with "deferred" labels in the workspace.

**Evidence chain:**
1. Config has `exclude_labels: []` → activates defaults
2. Defaults include `"deferred"` label
3. 17 open beads have `"deferred"` label
4. Log shows `excluded=17` on first Pluck run
5. **Conclusion:** The 17 excluded beads are precisely the 17 beads with "deferred" labels

---

## Root Cause Statement

**The configuration setting `exclude_labels: []` in `~/.config/needle/config.yaml:88` is causing Pluck to filter out 17 out of 49 open beads (35% of the workspace) because the empty array activates default label exclusions rather than disabling them.**

### What Setting Value is Incorrect

**Current (incorrect) value:**
```yaml
strands:
  pluck:
    exclude_labels: []
```

**What this value does:**
- Activates default exclusions: `["deferred", "human", "blocked", "starvation-alert"]`
- Filters 17 beads with "deferred" label
- Prevents Pluck from seeing 35% of available work

**What user likely intended:**
- Exclude nothing: `[]`
- Make all 49 open beads visible to Pluck

---

## Why This Happens

### Configuration Semantics

The NEEDLE codebase uses an **"empty means default"** semantic rather than the more intuitive **"empty means none"** semantic.

**Current behavior:**
- `exclude_labels: []` → Use defaults: `["deferred", "human", "blocked", "starvation-alert"]`
- `exclude_labels: ["custom"]` → Use custom: `["custom"]`

**Expected behavior (by user intent):**
- `exclude_labels: []` → Exclude nothing: `[]`
- `exclude_labels: ["deferred"]` → Exclude only deferred: `["deferred"]`

### Why This Choice Was Made

The code comment at line 26 explains:
> "If `exclude_labels` is empty, the default set (`deferred`, `human`, `blocked`) is used."

This design choice assumes that:
1. Most users want sensible defaults (excluding problematic bead states)
2. Explicit configuration should override defaults
3. Empty configuration is a signal to "use the baked-in sensible behavior"

However, this conflicts with the intuitive YAML interpretation where an empty array means "disable this feature."

---

## The Fix Path

### Option 1: Configuration Change (Immediate Workaround)

**Change the config to explicitly set empty behavior:**

In `~/.config/needle/config.yaml`, change:
```yaml
strands:
  pluck:
    exclude_labels: ["__NONE__"]  # Exclude only non-existent label
```

**Limitation:** This is a workaround, not a true fix. The correct interpretation requires code changes.

### Option 2: Code Change (Proper Fix)

**Modify NEEDLE to support "true empty" configuration:**

File: `/home/coding/NEEDLE/src/strand/pluck.rs`

**Option A: Use an explicit sentinel value**
```rust
const DISABLE_EXCLUDE_LABELS: &[&str] = &["__DISABLE__"];

pub fn new(exclude_labels: Vec<String>) -> Self {
    let labels = if exclude_labels.is_empty() {
        DEFAULT_EXCLUDE_LABELS.iter().map(|s| (*s).to_string()).collect()
    } else if exclude_labels == vec!["__DISABLE__".to_string()] {
        vec![]  // Truly empty - exclude nothing
    } else {
        exclude_labels
    };
```

**Option B: Use a boolean flag in config**
```yaml
strands:
  pluck:
    use_default_excludes: false
    exclude_labels: []
```

**Option C: Change semantics to "empty means none" (BREAKING CHANGE)**
```rust
pub fn new(exclude_labels: Vec<String>) -> Self {
    let labels = if exclude_labels.is_empty() {
        vec![]  // Empty means exclude nothing
    } else {
        exclude_labels
    };
    // Add a separate field for enabling defaults
```

---

## Acceptance Criteria Met

- ✅ **Single root cause identified:** `exclude_labels: []` activates defaults
- ✅ **Evidence links config to symptom:** Log shows 17 excluded, matches 17 beads with "deferred" label
- ✅ **Clear fix path determined:** 3 options identified (workaround, sentinel, semantic change)
- ✅ **Documented which setting is incorrect:** `exclude_labels: []` in `~/.config/needle/config.yaml:88`

---

## Impact Assessment

### Affected Workspaces
All workspaces using the global configuration with `exclude_labels: []`

### Symptom
Beads with `deferred`, `human`, `blocked`, or `starvation-alert` labels are invisible to Pluck, causing starvation even when open beads exist.

### Severity
**High** - This is the primary bead selection strand. When it starves, all bead processing stops.

### Scope
- 17 out of 49 open beads (35%) are filtered out in current workspace
- Progressive starvation observed as filtered beads accumulate

---

## Related Documentation

- **Pluck configuration:** `notes/bf-3y6nm.md`
- **Starvation reproduction:** `notes/bf-3js6h.md`
- **NEEDLE source:** `/home/coding/NEEDLE/src/strand/pluck.rs`
- **Global config:** `~/.config/needle/config.yaml`
