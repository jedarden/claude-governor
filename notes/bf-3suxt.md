# Pluck Configuration Fix - bf-3suxt

**Completed:** 2026-07-06
**Workspace:** `/home/coding/claude-governor`
**Task:** Fix Pluck configuration to make beads visible

## Fix Applied

Updated NEEDLE workspace configuration from `/home/coding/telegram-claude-bridge` to `/home/coding/claude-governor` in `~/.needle/config.yaml`.

### Change Made

```diff
 workspace:
-  default: /home/coding/telegram-claude-bridge
+  default: /home/coding/claude-governor
```

## Root Cause

The diagnosis in bf-1y51s identified that NEEDLE's default workspace was configured to the wrong directory. Pluck was searching for beads in `telegram-claude-bridge` (11 open beads) instead of `claude-governor` (45 open beads, 28 claimable).

## Verification Results

### Pre-Fix (Diagnosis)
- **Workspace:** `/home/coding/telegram-claude-bridge`
- **Open beads:** 11
- **Pluck results:** Incorrect workspace

### Post-Fix (Verification)
- **Workspace:** `/home/coding/claude-governor`
- **Total open beads:** 45
- **Claimable beads:** 28 (excluding deferred)
- **Pluck results:** ✓ Working correctly

### Diagnostic Confirmation

```bash
$ python3 scratch/pluck_filter_diagnosis.py
================================================================================
PLUCK FILTER DIAGNOSIS
================================================================================
Workspace: /home/coding/claude-governor

TEST 1: DEFAULT PLUCK CONFIGURATION
--------------------------------------------------------------------------------
Result: 28 claimable beads
✓ Pluck would find 28 beads - NO starvation detected
```

## Impact

- **Pluck visibility:** ✓ Restored - now finds all 28 claimable beads
- **Workspace path:** ✓ Corrected to `/home/coding/claude-governor`
- **Exclude labels:** ✓ No changes needed - not too broad (only `deferred` affects results)

## Acceptance Criteria

- ✓ Updated workspace path to correct directory
- ✓ Verified Pluck returns 28 claimable beads
- ✓ Confirmed exclude_labels configuration is appropriate

## Files Changed

- `~/.needle/config.yaml` - Updated `workspace.default` path

## Next Steps

This fix resolves the immediate Pluck visibility issue. Future considerations:
- Add workspace validation to NEEDLE startup
- Improve workspace discovery logging
- Add workspace awareness to diagnostic tools

## Related Beads

- **bf-1y51s:** Diagnosed configuration filter and exclude_labels issues
- **bf-3suxt:** Applied the configuration fix (this bead)
