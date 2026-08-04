# Bead bf-3jo4t: Starvation alert - beads invisible to worker

## Root Cause Identified

The starvation alert was caused by a **misconfiguration in the explore strand**, not actual bead starvation.

### Configuration Issue

The explore strand in `~/.config/needle/config.yaml` had:
```yaml
explore:
  enabled: true
  workspaces: []        # ← EMPTY LIST - root cause!
  workspace_root: /home/coding/
```

An empty `workspaces` list means the explore strand doesn't monitor ANY workspaces, even though `workspace_root` is set. This causes the explore strand to report "Pluck found none" because it's not configured to look anywhere.

### Evidence

1. **Actual bead availability:** 37 unclaimed beads exist in `/home/coding/claude-governor`
2. **Workers are processing beads:** All 7 workers are actively executing beads (per `needle doctor`)
3. **Explore strand misconfigured:** Has `workspaces: []` despite being enabled
4. **Other workers unaffected:** Main workers use default workspace config and work fine

### Resolution

Add `/home/coding/claude-governor` to the explore strand's `workspaces` list:

```yaml
explore:
  enabled: true
  workspaces:
    - /home/coding/claude-governor
  workspace_root: /home/coding/
```

This allows the explore strand to monitor the correct workspace where beads actually exist.

## Investigation Steps Taken

1. Ran `bf ready` and `bf list --status open` to verify bead availability
2. Checked NEEDLE configuration and explore strand settings  
3. Analyzed worker status and confirmed workers are processing beads
4. Identified the empty `workspaces: []` configuration as root cause
5. Verified workers are unaffected by using default workspace path

## Additional Findings

- The workers use the default workspace setting (`/home/coding/claude-governor`)
- The explore strand needs explicit workspace configuration
- Starvation alerts from explore don't indicate actual worker starvation
- All 7 workers are healthy and actively processing beads
