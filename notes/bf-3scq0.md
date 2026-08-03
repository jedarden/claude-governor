# Pluck Workspace Path Settings Documentation

## Overview

Pluck (br/bf/bead-forge) uses workspace paths to discover and manage beads across different directories. Each workspace represents a repository or project directory with a `.beads/` subdirectory containing bead tracking data.

## How Workspace Discovery Works

### Single Workspace Discovery (Default)

By default, Pluck discovers the workspace by searching upward from the current directory:

```bash
# Start at current directory and search up for .beads/
bf list
bf claim
```

**Algorithm (from `src/config.rs:find_beads_dir()`):**
1. Start at current directory
2. Check if `.beads/` exists as a subdirectory
3. If not found, move to parent directory
4. Repeat until root directory is reached

**Code implementation:**
```rust
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

### Explicit Workspace Specification

The `--workspace` flag allows explicit workspace specification:

```bash
# Use specific workspace directory
bf --workspace /home/coding/claude-governor list
bf -w /path/to/project claim
```

**CLI definition (from `src/cli/mod.rs`):**
```rust
#[arg(short, long, global = true)]
pub workspace: Option<PathBuf>,
```

### Multi-Workspace Discovery

For worker fleets that need to discover beads across multiple workspaces, Pluck provides:

**`find_workspaces()` function (from `src/claim.rs`):**
```rust
pub fn find_workspaces(start_path: &Path) -> Result<Vec<PathBuf>> {
    let mut workspaces = Vec::new();
    let mut current = start_path.to_path_buf();
    loop {
        let beads_dir = current.join(".beads");
        if beads_dir.is_dir() {
            workspaces.push(current.clone());
        }
        if !current.pop() {
            break;
        }
    }
    Ok(workspaces)
}
```

This returns ALL workspaces found when searching upward, not just the first one.

## Workspace Path Configuration

### ClaimConfig Workspace Settings

For worker fleets, workspace path configuration is managed through `ClaimConfig`:

**Structure (from `src/claim.rs`):**
```rust
pub struct ClaimConfig {
    /// Worker identifier
    pub worker_id: String,
    
    /// Search all workspaces instead of just current one
    pub any_workspace: bool,
    
    /// Additional workspace paths to search (only used with any_workspace)
    pub workspace_paths: Vec<PathBuf>,
    
    // ... other fields
}
```

**Usage patterns:**
```rust
// Single workspace (default)
let config = ClaimConfig::new("worker-1".to_string());

// Multi-workspace with explicit paths
let config = ClaimConfig::new("fleet-worker".to_string())
    .with_any_workspace(true)
    .with_workspace_paths(vec![
        PathBuf::from("/home/coding/project1"),
        PathBuf::from("/home/coding/project2"),
    ]);
```

## How Workspace Paths Affect Bead Discovery

### Bead Claiming

When a worker claims a bead, workspace paths control:

1. **Discovery scope:** Which `.beads/` directories are searched
2. **Priority:** Local workspace is checked first
3. **Fallback behavior:** Archives in other workspaces

**Claim process with workspace paths:**
```rust
// Single workspace: claims from current workspace only
claim_any(&config, &storage, Some(local_workspace))?

// Multi-workspace: searches all configured paths
let workspaces = find_workspaces(start_path)?;
claim_any(&config, &storage, workspaces.first())?
```

### Ready Candidates

Ready bead discovery respects workspace boundaries:

```bash
# Find ready beads in current workspace only
bf ready

# Find ready beads across multiple workspaces (via ClaimConfig)
bf claim --any-workspace --workspace-path /path1 --workspace-path /path2
```

## Storage Location

### Workspace Directory Structure

Each workspace has this structure:

```
/path/to/workspace/
├── .beads/
│   ├── beads.db           # SQLite database (query cache + bf-only tables)
│   ├── issues.jsonl       # Append-only audit log (source of truth)
│   ├── config.yaml        # Workspace configuration
│   ├── metadata.json     # Database metadata
│   ├── .bf_history/      # JSONL history backups
│   └── traces/           # Trace output for test beads
└── [project files]
```

### Current Configured Workspaces

**Available workspaces on this system:**
```bash
/home/coding/.beads/                 # Home directory workspace
/home/coding/SIGIL/.beads/          # SIGIL project
/home/coding/vibecodeleaderboard-backend/.beads/
/home/coding/pdftract/.beads/
/home/coding/gantry-rs/.beads/
/home/coding/pdftract-php/.beads/
/home/coding/pose-detection/.beads/
/home/coding/SEAM/.beads/
/home/coding/sun-sim/.beads/
/home/coding/aide-de-camp/.beads/
```

## Configuration Files

### Workspace Configuration (`~/.beads/config.yaml`)

**Example configuration:**
```yaml
# Bead ID prefixes
issue_prefixes:
  - "bf"

# Default priority for new beads
default_priority: 2

# Default type for new beads  
default_type: "task"

# Scoring weights
scoring:
  priority_weight: 0.4
  blockers_weight: 0.3
  age_weight: 0.2
  labels_weight: 0.1

# Claim TTL (minutes)
claim_ttl_minutes: 30

# Auto-flush configuration
sync:
  auto_flush: true

# Checkpoint configuration
checkpoint:
  enabled: false
  interval_minutes: 60
  push: false
```

## Integration with NEEDLE Workers

### NEEDLE Launch Command

In Claude Governor configuration, workspace paths are specified in the launch command:

**Example from `config/governor.yaml`:**
```yaml
agents:
  needle-sonnet:
    launch_cmd: "needle run --agent claude-code-glm-5 --workspace {workspace} --session-prefix needle-cgov"
    session_pattern: "needle-cgov-*"
    heartbeat_dir: "~/.needle/state/heartbeats"
```

The `{workspace}` placeholder gets replaced with the actual workspace path at runtime.

### Worker Workspace Resolution

**From `src/worker.rs`:**
```rust
let workspace = std::env::current_dir()
    .unwrap()
    .to_string_lossy()
    .to_string();
let launch_cmd = config.launch_cmd
    .replace("{workspace}", &workspace);
```

## Current Workspace Examples

### cgov-polish-queue Workspace

**Purpose:** Dedicated queue for polish bead generation
**Location:** `~/cgov-polish-queue/`
**Contains:** Only meta-beads for polish generation

### claude-governor Workspace  
**Purpose:** Main cgov project development
**Location:** `/home/coding/claude-governor/`
**Contains:** Development beads for cgov project

## Best Practices

1. **Workspace per project:** Each repository should have its own `.beads/` directory
2. **Explicit paths for fleets:** Worker fleets should use explicit workspace paths for predictable behavior
3. **Upward discovery for interactive use:** Single workspace commands rely on upward discovery from current directory
4. **Multi-workspace for coordination:** Use `any_workspace` + `workspace_paths` when workers need to find work across multiple projects

## Commands Reference

```bash
# List beads in current workspace
bf list

# List beads in specific workspace
bf --workspace /path/to/project list

# Claim from current workspace
bf claim --assignee worker-1

# Claim from multiple workspaces (via API)
bf claim --any-workspace --workspace-path /path1 --workspace-path /path2

# Find workspaces programmatically
# (uses find_workspaces() function)
```

## Related Files

**Source code:**
- `src/config.rs` - Workspace discovery and configuration loading
- `src/claim.rs` - Multi-workspace claiming and workspace finding
- `src/cli/mod.rs` - CLI argument parsing for workspace parameter
- `src/worker.rs` - NEEDLE worker launch command expansion

**Documentation:**
- `CLAUDE.md` - Project-specific instructions
- `README.md` - bead-forge overview and architecture
