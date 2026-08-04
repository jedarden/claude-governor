# Pluck Configuration Investigation (bf-5dsgv)

## Summary

Investigated Pluck configuration and bead visibility settings in the claude-governor workspace. Found critical configuration and potential issues.

## Configuration Files Identified

### 1. Global NEEDLE Configuration
**Location:** `/home/coding/.config/needle/config.yaml`

This is the primary configuration file controlling Pluck behavior across all workspaces.

### 2. NEEDLE Adapters Directory
**Location:** `/home/coding/.config/needle/adapters/`

Contains agent-specific adapter configurations:
- `claude-code-glm-4.7.yaml` - Current lab adapter
- `claude-print-opus.yaml` - Subscription Opus adapter
- `claude-print-fable.yaml` - Subscription Fable adapter

### 3. Workspace-Specific Configuration
**Location:** `/home/coding/claude-governor/.needle-predispatch-sha`

Contains only the predispatch SHA, no Pluck-specific overrides.

## Current Pluck Configuration

```yaml
strands:
  pluck:
    exclude_labels:
    - deferred
    - human
    - blocked
    split_after_failures: 3
    persistent_starvation_records: false
```

**Location:** `/home/coding/.config/needle/config.yaml` lines 23-28

## Workspace Path Settings

```yaml
workspace:
  default: /home/coding
  home: /home/coding/.needle
  labels: []
```

**Location:** `/home/coding/.config/needle/config.yaml` lines 18-21

## Current Filter Configuration

### exclude_labels
- `deferred` - Beads marked for later consideration
- `human` - Beads requiring human intervention
- `blocked` - Beads with blocking dependencies

### split_after_failures
- Value: `3`
- Purpose: Split beads after multiple consecutive failures

## Critical Finding: Configuration Not Applied

**ISSUE:** Bead `bf-156nn7` has label `deferred` but appears in the ready list despite being excluded by configuration.

**Evidence:**
```bash
$ bf show bf-156nn7
Labels: deferred, failure-count:1

$ bf ready --limit 20
[bf-156nn7] config/claude-governor.service still ships MemoryMax=512M...
```

**Expected behavior:** Bead `bf-156nn7` should NOT appear in ready list due to `deferred` label.

## All Open Beads Analysis

**Total open beads:** 40

**Beads with excluded labels:** 1
- `bf-156nn7` - status: open, labels: [deferred, failure-count:1]

**Status blocked beads:** 18 (correctly excluded from ready)
- These have `status: blocked` (not label) and are correctly filtered

## Beads That Should Be Visible But Aren't

Based on current analysis, all open beads without excluded labels are appearing in the ready list correctly. The issue is that one bead WITH an excluded label (`deferred`) IS appearing when it shouldn't.

## Other Configuration Files

### explore-excluded
**Location:** `/home/coding/.config/needle/explore-excluded`

Contains workspace path exclusions:
```
/home/coding/SEAM
```

This prevents the explore strand from discovering beads in the SEAM workspace.

## Recommendations

1. **URGENT:** Investigate why `exclude_labels` configuration is not being respected
2. Verify Pluck strand is actually reading the configuration file
3. Check if there's a cached or stale configuration being used
4. Consider restarting the NEEDLE daemon or workers to reload configuration

## Commands Used

```bash
# List configuration files
find /home/coding/claude-governor -type f \( -name "*.yaml" -o -name "*.json" \)

# Check Pluck configuration
needle config | grep -A 10 "pluck:"

# List ready beads
bf ready --limit 20

# Show bead details
bf show bf-156nn7

# Check all beads with excluded labels
bf list --json --limit 200 | python3 -c "..."
```

## Next Steps

1. Verify Pluck is using the correct configuration file
2. Test if restarting workers applies the exclude_labels filter
3. Check for any workspace-level configuration overrides
4. Investigate if this is a NEEDLE version-specific bug
