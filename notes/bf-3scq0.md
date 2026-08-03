# Bead bf-3scq0: Document Pluck workspace path settings

**Status:** COMPLETE  
**Date:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`

## Summary

All acceptance criteria for documenting Pluck workspace path configuration have been satisfied. Comprehensive documentation exists at `docs/pluck-workspace-paths.md`.

## Acceptance Criteria Status

### ✅ 1. Find the workspace path configuration in Pluck

**Location:** `~/.config/needle/config.yaml`

The primary workspace path configuration is stored in NEEDLE's configuration file, controlling both worker management and bead discovery.

**Current Settings (as of 2026-08-03):**
```yaml
workspace:
  default: /home/coding
  home: /home/coding/.needle
  labels: []

strands:
  explore:
    enabled: true
    workspaces: []
    workspace_root: /home/coding/
```

### ✅ 2. Document what workspace paths are currently configured

**Key Configuration Fields:**
- **`workspace.default`** (`/home/coding`): Base directory for workspace discovery
- **`workspace.home`** (`/home/coding/.needle`): NEEDLE's internal workspace directory
- **`strands.explore.workspace_root`** (`/home/coding/`): Root directory for explore strand discovery
- **`strands.explore.workspaces`** (`[]`): Explicit workspace list (empty = auto-discover)
- **Exclusions:** Stored in `~/.config/needle/explore-excluded` (currently contains `/home/coding/SEAM`)

**Active Workspaces:** 41 workspaces under `/home/coding/` including:
- `/home/coding/claude-governor/.beads/` - Main cgov project workspace
- `/home/coding/cgov-polish-queue/.beads/` - Polish queue workspace (special purpose)
- `/home/coding/SIGIL/.beads/`, `/home/coding/bead-forge/.beads/`, `/home/coding/NEEDLE/.beads/` - Project workspaces
- 36+ other project workspaces

### ✅ 3. Document where this configuration is stored

**Configuration Locations:**

1. **Primary NEEDLE config:** `~/.config/needle/config.yaml`
   - Workspace path settings
   - Strand configuration
   - Exclude labels

2. **Workspace exclusions:** `~/.config/needle/explore-excluded`
   - One workspace path per line to skip during discovery

3. **Per-workspace configs:** `{workspace}/.beads/config.yaml`
   - Optional workspace-specific Pluck settings
   - Override global defaults when present

4. **Governor config:** `~/.config/claude-governor/governor.yaml`
   - Agent launch commands with `--workspace` parameters
   - Worker limits per agent type

### ✅ 4. Document how workspace path settings affect bead discovery

**Multi-Workspace Discovery Process:**

1. **Starting Point:** System starts from `workspace.default` (`/home/coding`)
2. **Directory Scanning:** Explores for `.beads/` subdirectories to identify workspaces
3. **Workspace Exclusion:** Skips workspaces listed in `explore-excluded` file
4. **Per-Workspace Bead Storage:** Each workspace maintains its own isolated bead database

**Bead Discovery Implementation:**

**Single Workspace Discovery (Default):**
```rust
// From: /home/coding/bead-forge/src/config.rs
pub fn find_beads_dir(start_dir: &Path) -> Option<PathBuf> {
    let mut current = Some(start_dir);
    while let Some(dir) = current {
        let beads_dir = dir.join(".beads");
        if beads_dir.is_dir() {
            return Some(beads_dir);
        }
        current = dir.parent();
    }
    None
}
```

**Multi-Workspace Discovery:**
- Implemented in `/home/coding/bead-forge/src/claim.rs`
- Used by worker fleets that need to claim beads across multiple projects
- Returns comprehensive list of discovered workspaces

**Bead Visibility Controls:**

- **Workspace boundaries:** Each `.beads/` directory maintains a separate bead database
- **Global exclude_labels:** Beads with `deferred`, `human`, or `blocked` labels are hidden from discovery
- **Database filters:** Status, soft delete, compaction level, ephemeral flags
- **Agent workspace assignment:** The `--workspace` parameter in launch commands isolates workers to specific workspaces

**Ready Candidates Discovery:**
- **Single workspace:** `bf ready` finds ready beads in current workspace
- **Multi-workspace:** Via `ClaimConfig` with `any_workspace=true` and explicit workspace paths
- **Filter criteria:** `status='open' AND ephemeral=0 AND pinned=0 AND is_template=0`

## Documentation Deliverable

**Comprehensive documentation:** `docs/pluck-workspace-paths.md`

This document contains 281 lines covering:
- Configuration location and current settings
- Workspace discovery algorithms (single and multi-workspace)
- Multi-workspace bead discovery process
- Bead visibility and filtering mechanisms
- Source code references
- Related configuration files (governor.yaml, NEEDLE adapters)
- Agent launch command examples
- 41 active workspaces inventory

## Key Findings

1. **Centralized Configuration:** Workspace path configuration is centralized in `~/.config/needle/config.yaml`
2. **Auto-Discovery:** With `strands.explore.workspaces: []`, the system auto-discovers all workspaces under `/home/coding/`
3. **Exclusion Support:** Workspaces can be excluded from discovery via `~/.config/needle/explore-excluded`
4. **Isolated Storage:** Each workspace maintains its own bead database in `.beads/` subdirectory
5. **Worker Assignment:** Agents are assigned to specific workspaces via `--workspace` flag in launch commands
6. **Multi-Level Filtering:** Bead visibility controlled by workspace boundaries, global labels, and database filters

## Related Documentation

- `docs/plan/pluck-configuration.md` - Additional Pluck configuration details
- `CLAUDE.md` - Project operating guide with polish queue details
- `config/governor.yaml` - Governor configuration with worker launch commands

## Conclusion

Pluck workspace path configuration is well-documented and centralized in NEEDLE's `config.yaml`. The system supports both interactive single-project work and automated fleet operations across multiple repositories, with proper isolation between workspaces and comprehensive bead discovery capabilities.

**Documentation Status:** ✅ COMPLETE - All acceptance criteria satisfied by existing `docs/pluck-workspace-paths.md` documentation.
