# Bead Visibility Configuration - Complete Mapping

This document maps all configuration files that control whether Pluck (and other NEEDLE strands) can see and work with beads.

## Configuration Layers (Priority Order: Highest → Lowest)

### 1. Workspace-Specific `.needle.yaml` (Highest Priority)
**Location:** `/path/to/workspace/.needle.yaml`  
**Format:** YAML  
**Scope:** Single workspace  
**Precedence:** Overrides global settings

**Key visibility controls:**
```yaml
strands:
  pluck:
    enabled: true              # Master switch for pluck strand
    exclude_labels:            # Labels that hide beads from pluck
      - deferred
      - human
      - blocked
    split_after_failures: 3    # Stop plucking after N failures
  
  explore:
    enabled: true              # Master switch for explore strand
    workspaces:                # Which workspaces to explore
      - /path/to/workspace1
      - /path/to/workspace2
```

**Examples found in codebase:**
- `/home/coding/domain-check/.needle.yaml` - excludes `deferred`, `human`, `blocked`
- `/home/coding/AgentScribe/.needle.yaml` - excludes `deferred`, `human`, `blocked`
- `/home/coding/spaxel/.needle.yaml` - excludes `deferred`, `human`, `blocked`
- `/home/coding/NEEDLE/.needle.yaml` - empty exclude_labels (no filtering)

### 2. Global NEEDLE Configuration
**Location:** `~/.config/needle/config.yaml`  
**Format:** YAML  
**Scope:** All workspaces (defaults)  
**Precedence:** Applied when workspace `.needle.yaml` doesn't specify

**Key visibility controls:**
```yaml
strands:
  pluck:
    exclude_labels:
      - deferred              # Default excluded labels
      - human
      - blocked
    split_after_failures: 3
  
  explore:
    enabled: true
    workspaces: []            # Empty = discover all
    workspace_root: /home/coding/
  
  mend:
    stuck_threshold_secs: 300
    idle_timeout: 120
  
  weave:
    enabled: true
    max_beads_per_run: 5
  
  pulse:
    enabled: true
    severity_threshold: 3     # Only high-severity issues
```

**Current global settings (from `~/.config/needle/config.yaml`):**
- `pluck.exclude_labels`: `[deferred, human, blocked]`
- `explore.enabled`: `true`
- `explore.workspace_root`: `/home/coding/`
- `weave.max_beads_per_run`: `5`

### 3. Workspace Explore Exclusion List
**Location:** `~/.config/needle/explore-excluded`  
**Format:** Plain text (one workspace path per line)  
**Scope:** Explore strand only  
**Purpose:** Explicitly hide workspaces from explore discovery

**Current content:**
```
/home/coding/SEAM
```

This file is checked by the explore strand to skip workspaces even if they're discovered.

### 4. Bead Store Configuration (`.beads/config.yaml`)
**Location:** `/path/to/workspace/.beads/config.yaml`  
**Format:** YAML  
**Scope:** Bead creation/display settings (NOT visibility filtering)

**Settings (indirect visibility impact):**
```yaml
issue_prefixes: [bf]           # Bead ID prefix
default_priority: 2            # Priority for new beads
default_type: task             # Type for new beads
claim_ttl_minutes: 30          # How long beads stay claimed

secret_protection:
  allowlist:                   # False positive allowances
    - "onepassword"
```

**Note:** These settings don't filter beads from pluck/explore, but control bead creation and display behavior.

### 5. NEEDLE Adapter Configuration (Indirect)
**Location:** `~/.config/needle/adapters/*.yaml`  
**Format:** YAML  
**Scope:** Agent invocation behavior  
**Visibility impact:** NONE (controls how agents run, not what beads they see)

**Example (`claude-print-opus.yaml`):**
```yaml
name: claude-print-opus
agent_cli: claude-print
timeout_secs: 1200
provider: anthropic
model: opus
```

## Bead Visibility Filters (From `br list` help)

Beads can be filtered by these attributes (via CLI, not config files):

- `--status <STATUS>` - Filter by status (open, in_progress, closed)
- `--type <TYPE>` - Filter by type (task, bug, feature, etc.)
- `--assignee <ASSIGNEE>` - Filter by who's assigned
- `--priority <PRIORITY>` - Filter by priority level
- `--annotation <ANNOTATION>` - Filter by key=value metadata

**Note:** These are CLI filters, not persistent configuration.

## Bead Selection Criteria (From `br ready` help)

The `br ready` command (used by pluck strand to find beads) ranks beads by:

1. **Downstream impact** - Beads blocking the most other beads
2. **Priority** - Higher priority first
3. **Age** - Older beads first (within same priority)

Ready beads are: **open AND unblocked** (no `blocked_by` dependencies).

## Configuration Precedence Summary

```
1. Workspace .needle.yaml (highest)
   └─ Override: pluck.exclude_labels, explore.workspaces, etc.

2. Global ~/.config/needle/config.yaml (default)
   └─ Applied when workspace config missing/empty

3. Explore-excluded file (hard exclusion)
   └─ Skips workspaces regardless of other settings

4. .beads/config.yaml (display only)
   └─ Controls bead creation, not filtering
```

## Key Discovery: Pluck Visibility Formula

A bead is **visible to pluck** when ALL of these are true:

```yaml
# Global or workspace config allows it:
strands.pluck.enabled: true
bead.labels ∉ strands.pluck.exclude_labels

# Bead state allows it:
bead.status: open
bead.blocked_by: ∅                    # No blocking dependencies

# Explore discovery allows it (if using explore):
workspace ∉ ~/.config/needle/explore-excluded
workspace ∈ strands.explore.workspaces  # OR discovered automatically
```

## Testing Visibility

To check if a bead is visible to pluck:

```bash
# List all ready beads (what pluck sees)
bf ready

# List with filters (test visibility)
bf list --status open --type task

# Check if workspace is explore-excluded
cat ~/.config/needle/explore-excluded

# Test workspace config
cat /path/to/workspace/.needle.yaml | grep -A 5 "strands.pluck"
```

## Related Beads

- **bf-14eo0**: ArgoCD cluster secret verification
- **bf-dge6t**: Bead visibility analysis (completed)
- **bf-1876c**: Pluck filter parameter logging

## Files Referenced

- `~/.config/needle/config.yaml` - Global NEEDLE configuration
- `~/.config/needle/explore-excluded` - Explore exclusion list
- `~/.config/needle/adapters/*.yaml` - Agent adapters (no visibility impact)
- `/home/coding/*/.needle.yaml` - Workspace-specific configs
- `/home/coding/*/.beads/config.yaml` - Bead store configs (display only)

---
**Generated:** 2026-08-03  
**Workspace:** /home/coding/claude-governor  
**Bead ID:** bf-15prd
