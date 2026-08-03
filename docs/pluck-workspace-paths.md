# Pluck Workspace Path Configuration

## Overview

Pluck (also known as `br` or `bead-forge`) uses workspace paths to discover and manage beads across multiple projects and repositories. The workspace path configuration controls where Pluck looks for bead databases and how the NEEDLE system (which manages Pluck workers) discovers workspaces for automated tasks.

## Configuration Location

The primary workspace path configuration is stored in the NEEDLE configuration file:

```
~/.config/needle/config.yaml
```

This configuration controls both NEEDLE's worker management and how Pluck discovers beads across workspaces.

## Current Workspace Path Configuration

As of 2026-08-03, the workspace path settings in `~/.config/needle/config.yaml` are:

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

### Configuration Fields

- **`workspace.default`** (`/home/coding`): The default base directory where workspace projects are located. This is the starting point for workspace discovery.

- **`workspace.home`** (`/home/coding/.needle`): NEEDLE's internal workspace directory for managing its own state and temporary files.

- **`strands.explore.workspace_root`** (`/home/coding/`): The root directory that the explore strand uses when searching for workspaces. The explore strand is responsible for discovering and analyzing code across workspaces.

- **`strands.explore.workspaces`** (`[]`): An explicit list of workspace paths. When empty, the explore strand auto-discovers workspaces from `workspace_root`. When populated, only these specific workspaces are explored.

- **`strands.explore.enabled`** (`true`): Controls whether the explore strand is active for workspace discovery.

## Workspace Exclusions

Certain workspaces can be excluded from discovery. The current exclusion is stored in:

```
~/.config/needle/explore-excluded
```

Current content:
```
/home/coding/SEAM
```

This file contains workspace paths (one per line) that should be excluded from automated discovery and exploration.

## How Workspace Paths Affect Bead Discovery

### 1. Per-Workspace Bead Storage

Each workspace directory contains its own bead database in the `.beads/` subdirectory:

```
/home/coding/claude-governor/.beads/
/home/coding/vista/.beads/
/home/coding/spaxel/.beads/
```

Each `.beads/` directory contains:
- `beads.db` - SQLite database (live bead store, query cache + bf-only tables)
- `issues.jsonl` - Git-tracked checkpoint file (append-only audit log, source of truth)
- `config.yaml` - Workspace-specific configuration (optional)
- `metadata.json` - Database metadata
- `.bf_history/` - JSONL history backups
- `traces/` - Trace output for test beads

### 2. Multi-Workspace Discovery

When NEEDLE workers need to find beads (e.g., the polish queue generator), they:

1. **Start from workspace.default** (`/home/coding`)
2. **Scan for .beads/ directories** - Each directory containing a `.beads/` subdirectory is recognized as a workspace with beads
3. **Read workspace-specific beads** - Using `bf ready` or `bf list` commands within each workspace
4. **Skip excluded workspaces** - Workspaces listed in `explore-excluded` are ignored

### 3. Agent Launch Commands

Individual NEEDLE agents are launched with specific workspace paths:

```bash
needle run --agent claude-print-opus --workspace /home/coding/cgov-polish-queue
```

The `--workspace` parameter tells the agent which workspace's beads to work with. This allows:
- Multiple agents to work in different workspaces simultaneously
- Isolation of beads between projects
- The polish queue to be a dedicated workspace for generation meta-beads

### 4. Bead Visibility

Beads are **not globally visible** across workspaces. Each workspace has its own bead database:

- A bead created in `/home/coding/vista/.beads/` is only visible to agents working in that workspace
- The polish queue (`/home/coding/cgov-polish-queue/.beads/`) contains only meta-beads for generation tasks
- Cross-workspace dependencies use bead IDs but still require agents to be aware of which workspace contains which beads

**Visibility Controls:**
- **Global exclude_labels** (in `~/.config/needle/config.yaml`): Beads with `deferred`, `human`, or `blocked` labels are hidden from discovery
- **Database filters**: Status, soft delete, compaction level, ephemeral flags
- **Workspace boundaries**: Each `.beads/` directory maintains separate bead database

**Ready Candidates Discovery:**
- **Command**: `bf ready` finds ready beads in current workspace
- **Multi-workspace**: Via `ClaimConfig` with `any_workspace=true` and explicit workspace paths
- **Filter criteria**: `status='open' AND ephemeral=0 AND pinned=0 AND is_template=0`

## Workspace Discovery Implementation

### Single Workspace Discovery (Default)

The default workspace discovery algorithm searches upward from the current directory for a `.beads/` subdirectory:

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

This means that when you run `bf list` from within `/home/coding/claude-governor/`, Pluck automatically finds `/home/coding/claude-governor/.beads/`.

### Multi-Workspace Discovery

The multi-workspace discovery is implemented in `/home/coding/bead-forge/src/claim.rs`:

```rust
// From: /home/coding/bead-forge/src/claim.rs
// Returns: ALL workspaces found when searching upward, not just the first one
// Used by: Worker fleets that need to discover beads across multiple projects
fn find_workspaces() -> Vec<PathBuf> {
    // Implementation searches for all .beads/ directories
    // Returns comprehensive list of discovered workspaces
}
```

This is used by worker fleets that need to claim beads across multiple projects.

### Explicit Workspace Specification

You can explicitly specify workspaces via CLI flags:

```bash
# Work in a specific workspace
bf --workspace /home/coding/claude-governor list

# Claim from a specific workspace
bf --workspace /home/coding/claude-governor claim --assignee worker-1

# Multi-workspace claiming (via API)
bf claim --any-workspace --workspace-path /path1 --workspace-path /path2
```

## Currently Active Workspaces

This system currently has 18+ active workspaces under `/home/coding/`:

**Key Workspaces:**
- `/home/coding/.beads/` - Home directory workspace
- `/home/coding/claude-governor/.beads/` - Main cgov project workspace
- `/home/coding/cgov-polish-queue/.beads/` - Polish queue workspace (special purpose)
- `/home/coding/SIGIL/.beads/` - SIGIL project workspace
- `/home/coding/bead-forge/.beads/` - Bead-forge development workspace
- `/home/coding/NEEDLE/.beads/` - NEEDLE project workspace
- And 12+ other project workspaces (vista, spaxel, miroir, etc.)

**Polish Queue Workspace (Special Purpose):**
- **Location:** `/home/coding/cgov-polish-queue/`
- **Purpose:** Dedicated queue for polish bead generation
- **Contains:** Only meta-beads for polish generation
- **Safety:** Workers pointed here can only find meta-beads, preventing accidental work on real repos

## Pluck (br) Workspace Configuration

Pluck itself can have workspace-specific configuration in `.beads/config.yaml`, but this is optional:

```bash
br config path              # Shows: ./.beads/config.yaml (relative to current workspace)
br config list             # Lists all config values for current workspace
```

### Default Global Pluck Settings

When no workspace-specific config exists, Pluck uses these defaults:

```yaml
issue_prefixes: ["bf"]
default_priority: 2
default_type: task
claim_ttl_minutes: 30
```

These settings apply to all workspaces unless overridden in a workspace's `.beads/config.yaml`.

## Related Configuration Files

### cgov (claude-governor) Configuration

The cgov governor uses workspace paths to manage NEEDLE workers:

```
~/.config/claude-governor/governor.yaml
```

This configuration defines:
- Which agents to launch (e.g., `polish-opus`)
- The launch commands (which include `--workspace` parameters)
- Worker limits (min/max) per agent type

Example from governor.yaml:
```yaml
polish-opus:
  launch_cmd: "needle run --agent claude-print-opus --workspace /home/coding/cgov-polish-queue"
  session_pattern: "needle-claude-print-opus-*"
  heartbeat_dir: "~/.needle/state/heartbeats"
  min_workers: 0
  max_workers: 4
  subscription: true
```

### NEEDLE Adapters

The NEEDLE adapters directory contains agent-specific configurations:

```
~/.config/needle/adapters/
├── claude-print-opus.yaml
├── claude-print-fable.yaml
└── ...
```

These adapters define how agents interact with Pluck within their assigned workspaces.

## Source Code References

The workspace path configuration system is implemented in these key files:

**Core Workspace Discovery:**
- `/home/coding/bead-forge/src/config.rs` - Workspace discovery (`find_beads_dir()`) and configuration loading
- `/home/coding/bead-forge/src/claim.rs` - Multi-workspace claiming (`find_workspaces()`, `ClaimConfig`)

**Governor Integration:**
- `/home/coding/claude-governor/src/config.rs` - Governor configuration with workspace placeholders
- `/home/coding/claude-governor/src/worker.rs` - NEEDLE worker launch command expansion
- `/home/coding/claude-governor/src/governor.rs` - Worker management and workspace extraction from launch commands

## Summary

**Workspace path configuration is centralized in NEEDLE's `config.yaml`** (`~/.config/needle/config.yaml`), with these key settings:

1. **`workspace.default: /home/coding`** - Base directory for workspace discovery
2. **`workspace.home: /home/coding/.needle`** - NEEDLE's internal workspace
3. **`strands.explore.workspace_root: /home/coding/`** - Root for explore strand discovery
4. **`strands.explore.workspaces: []`** - Explicit workspace list (empty = auto-discover)
5. **Exclusions in `~/.config/needle/explore-excluded`** - Workspaces to skip

Bead discovery works by scanning for `.beads/` directories under the workspace root, with each workspace maintaining its own isolated bead database. Agents are assigned to specific workspaces via the `--workspace` flag in their launch commands, enabling parallel processing across multiple projects while keeping beads properly isolated.

The implementation is split between Pluck (workspace discovery algorithms in `bead-forge/src/config.rs` and `claim.rs`) and NEEDLE (worker management and agent launch commands). This architecture supports both interactive single-project work and automated fleet operations across multiple repositories.
