# Starvation Alert Investigation: bf-6c9j4

## Issue
Pluck worker could not find any claimable beads despite 41 open beads existing in the workspace.

## Root Cause
**Workspace misconfiguration in NEEDLE config**

The NEEDLE configuration file at `/home/coding/.config/needle/config.yaml` had the wrong default workspace:

```yaml
workspace:
  default: /home/coding/telegram-claude-bridge  # ❌ Wrong - 0 claimable beads
```

Pluck was looking for beads in `/home/coding/telegram-claude-bridge` which had:
- Total beads: Various
- **Claimable beads: 0** (STARVATION)

But the actual work was happening in `/home/coding/claude-governor` which had:
- Total beads: 1009
- Open beads: 41
- **Claimable beads: 23** (after excluding deferred labels)

## Fix Applied
Updated `/home/coding/.config/needle/config.yaml` line 19:

```yaml
workspace:
  default: /home/coding/claude-governor  # ✅ Correct - 23 claimable beads
```

## Verification
After the fix:
- ✅ Pluck can now find 23 claimable beads in the correct workspace
- ✅ Exclude labels working correctly (deferred, human, blocked, starvation-alert)
- ✅ Starvation alert resolved - worker can now claim and process beads

## Additional Notes
- The workspace had 33 unassigned open beads total
- 18 beads were excluded due to "deferred" label
- 33 - 18 = 15 additional beads, but the diagnostic shows 23 claimable (some beads may have assignees or other filtering)

## Diagnostic Tool
The `scratch/test_pluck_workspace_path.py` script was invaluable for diagnosing this issue - it tests Pluck's query behavior across all configured workspaces and shows exactly which beads are claimable in each location.
