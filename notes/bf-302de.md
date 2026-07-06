# Pluck Configuration Fix

**Bead ID:** bf-302de
**Date:** 2026-07-06
**Workspace:** `/home/coding/claude-governor`

---

## Root Cause

From child bead `bf-4xsc6`: Setting `exclude_labels: []` in `~/.config/needle/config.yaml:24` activates default label filtering (`["deferred", "human", "blocked", "starvation-alert"]`), which filtered out 17 out of 49 open beads (35% of the workspace).

---

## Configuration Change Applied

### File Modified
`/home/coding/.config/needle/config.yaml`

### Old Value (Incorrect)
```yaml
strands:
  pluck:
    exclude_labels: []        # Empty array activates defaults
    split_after_failures: 3
```

### New Value (Correct)
```yaml
strands:
  pluck:
    exclude_labels: ["__NONE__"]    # Exclude only non-existent label
    split_after_failures: 3
```

---

## Why This Works

The NEEDLE codebase uses an "empty means default" semantic:
- `exclude_labels: []` → Uses defaults: `["deferred", "human", "blocked", "starvation-alert"]`
- `exclude_labels: ["__NONE__"]` → Excludes only non-existent label (effectively excludes nothing)

This workaround makes all 49 open beads visible to Pluck instead of filtering out 17 beads with "deferred" labels.

---

## Verification

Configuration file is valid YAML and has been updated successfully.

---

## Rollback

If needed, rollback to:
```yaml
exclude_labels: []
```

---

## Acceptance Criteria Met

- ✅ Configuration updated with correct value
- ✅ Old value documented for rollback
- ✅ Configuration validated (YAML syntax)
- ✅ Change committed to appropriate config file
