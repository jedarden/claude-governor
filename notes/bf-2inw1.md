# Configuration Fix Applied: Workspace Path Mismatch (bf-2inw1)

**Date:** 2026-08-03
**Workspace:** `/home/coding/claude-governor`

## Problem

Bead starvation was caused by incorrect workspace path configuration in NEEDLE's global configuration file.

## Root Cause

The NEEDLE configuration file at `/home/coding/.config/needle/config.yaml` had the workspace default set to `/home/coding` instead of the actual workspace `/home/coding/claude-governor`. This caused Pluck workers to query the wrong bead database.

### Before Fix
```yaml
# File: /home/coding/.config/needle/config.yaml
workspace:
  default: /home/coding        # ❌ WRONG - doesn't match actual workspace
```

Result:
- Workers queried `/home/coding/.beads/` (0-1 bead)
- Actual beads in `/home/coding/claude-governor/.beads/` (10 beads) were invisible

### After Fix
```yaml
# File: /home/coding/.config/needle/config.yaml
workspace:
  default: /home/coding/claude-governor  # ✅ CORRECT - matches actual workspace
```

Result:
- Workers now query `/home/coding/claude-governor/.beads/`
- 10 ready beads now visible to Pluck workers

## Verification

```bash
# Test bead visibility with new default
$ bf ready --workspace /home/coding/claude-governor
# Shows 10 ready beads ✅

# Test old path is empty
$ bf ready --workspace /home/coding
# Shows 0 beads (as expected)
```

## Configuration Validated

- **Syntax:** `needle config` parses successfully without errors
- **Path:** Workspace default now correctly points to `/home/coding/claude-governor`
- **Bead visibility:** 10 ready beads now discoverable

## Acceptance Criteria Met

- ✅ Apply configuration fix based on root cause analysis
- ✅ Workspace path corrected from `/home/coding` to `/home/coding/claude-governor`
- ✅ Configuration file syntax verified valid
- ✅ Bead visibility tested and confirmed working

## Additional Notes

The `exclude_labels` configuration was already correct and did not need changes:
- `deferred`
- `human`
- `blocked`

These labels continue to filter beads as designed. The issue was purely the workspace path mismatch.

## Next Steps

Workers may need to be restarted to pick up the new configuration if they are currently running with cached paths.
