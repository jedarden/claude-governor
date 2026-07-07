# Pluck Configuration Documentation

**Documented:** 2026-07-06  
**Workspace:** `/home/coding/claude-governor`  
**Purpose:** Baseline documentation of Pluck configuration settings affecting bead visibility

## Overview

Pluck is the primary work-selection strand in NEEDLE. It handles >90% of all bead processing by querying the bead store for unassigned, ready beads, filtering by excluded labels, and sorting them in deterministic priority order.

## Current Configuration

### 1. Exclude Labels Configuration

**Source:** Compiled into NEEDLE binary (`/home/coding/NEEDLE/src/strand/pluck.rs`)

**Default Exclude Labels:**
- `deferred` - Beads marked for later processing
- `human` - Beads requiring human intervention
- `blocked` - Beads with blocking dependencies

**Implementation:**
```rust
// From: /home/coding/NEEDLE/src/strand/pluck.rs:13
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Behavior:**
- When `PluckStrand::new(vec![])` is called with empty exclude_labels, defaults are applied
- Custom exclude_labels override defaults completely (not merged)
- Filtering is applied twice: once via bead store query, once defensively in the strand

**No custom override configured** - Pluck uses the default exclude_labels in current deployment.

### 2. Workspace Path Settings

**Source:** `~/.needle/config.yaml`

**Default Workspace:**
```yaml
workspace:
  default: /home/coding/NEEDLE
```

**Claude Governor Workspace:**
- Path: `/home/coding/claude-governor`
- Bead store: `/home/coding/claude-governor/.beads/`
- Database: `/home/coding/claude-governor/.beads/beads.db`
- JSONL checkpoint: `/home/coding/claude-governor/.beads/issues.jsonl`

**Workspace Discovery:**
- Pluck operates on the workspace it's assigned by NEEDLE
- Explore strand discovers additional workspaces via filesystem traversal
- Both `default` workspace and discovered workspaces are valid targets

### 3. Filter Configuration

**Source:** `~/.needle/config.yaml` (strand configuration)

**Current Strand Configuration:**
```yaml
strands:
  pluck: auto    # Primary work from the auto-discovered workspace
  explore: auto  # Look for work in other workspaces
  mend: true     # Maintenance and cleanup (always on - reap stale beads)
  knot: true     # Alert human when stuck (always on)
```

**Filter Implementation:**
Pluck applies three levels of filtering when selecting beads:

1. **Store-level filter** (via `bead_store::Filters`):
   - Filters by assignee (if specified)
   - Filters by exclude_labels (passed to store query)

2. **Strand-level defensive filter** (line 125 in pluck.rs):
   - Removes beads with excluded labels
   - Defensive guard against stores that don't include label data

3. **Claimability filter** (line 130-133 in pluck.rs):
   - Removes beads in `InProgress` status
   - Removes `Open` beads with stale assignee
   - Prevents SELECTING→CLAIMING→RETRYING spin loop

**Priority Sorting:**
Candidates are sorted in deterministic order: `(priority ASC, created_at ASC, id ASC)`

## Configuration Sourcing Summary

| Setting | Source | Location | Type | Current Value |
|---------|--------|----------|------|---------------|
| Default exclude_labels | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:13` | Constant | `["deferred", "human", "blocked"]` |
| Custom exclude_labels | Not configured | N/A | Runtime override | None (uses defaults) |
| Workspace default | NEEDLE config | `~/.needle/config.yaml:9` | YAML path | `/home/coding/claude-governor` |
| Current workspace | CLI/environment | NEEDLE assignment | Runtime | `/home/coding/claude-governor` |
| Bead store path | Derived from workspace | `{workspace}/.beads/` | Directory | `/home/coding/claude-governor/.beads/` |
| Strand enablement | NEEDLE config | `~/.needle/config.yaml:70-87` | YAML map | `pluck: auto` |
| Filter logic | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:105-133` | Rust code | Three-tier filtering |

## Additional Settings

### Split Configuration
- **split_after_failures**: `3` (default threshold, line 39 in pluck.rs)
- **Trigger**: When first candidate has `failure-count:N` label where N >= threshold
- **Result**: Returns `StrandResult::Split` instead of `BeadFound`

### Bead Store Configuration (br/bead-forge)
**File:** `/home/coding/claude-governor/.beads/config.yaml`
```yaml
# All values are commented - using br/bead-forge defaults:
# issue_prefix: claude-governor
# default_priority: 2
# default_type: task
```

**Active values** (from `br config list`):
- `issue_prefixes: ["bf"]`
- `default_priority: 2`
- `default_type: task`
- `claim_ttl_minutes: 0`

## Environment Variables

No Pluck-specific environment variables are currently set. The following environment variables may affect NEEDLE behavior:

- `NEEDLE_WORKSPACE` - Override default workspace path
- `FABRIC_AUTH_TOKEN` - Fabric event endpoint authentication

## Known Issues and Considerations

1. **No custom exclude_labels configured** - All deployments use the same default set
2. **Exclude labels are hardcoded** - Changing defaults requires recompiling NEEDLE
3. **Filtering is defensive** - Double-filtering prevents store inconsistencies but adds overhead
4. **Workspace switching** - Workers can switch between workspaces via explore strand, which can cause complexity

## Related Documentation

- NEEDLE source: `/home/coding/NEEDLE/src/strand/pluck.rs`
- NEEDLE config: `~/.needle/config.yaml`
- Bead store config: `/home/coding/claude-governor/.beads/config.yaml`
- Bead store source: `~/bead-forge/`
- Explore strand bugs: `/home/coding/NEEDLE/docs/notes/explore-strand-bugs.md`
