# All Configuration Files Controlling Bead Visibility

**Documented:** 2026-08-03  
**Bead ID:** bf-15prd  
**Workspace:** `/home/coding/claude-governor`

## Summary

Bead visibility in the Pluck/NEEDLE/bead-forge ecosystem is controlled by **five primary configuration sources**, with priority from highest to lowest:

1. **Compiled constants** (DEFAULT_EXCLUDE_LABELS in NEEDLE source)
2. **NEEDLE global config** (`~/.config/needle/config.yaml`)
3. **Workspace-specific config** (`.beads/config.yaml` per workspace)
4. **NEEDLE adapter configs** (`~/.config/needle/adapters/*.yaml`)
5. **Runtime CLI flags** (`--workspace`, environment variables)

---

## 1. NEEDLE Global Configuration

**File:** `~/.config/needle/config.yaml`

**Purpose:** Primary configuration for NEEDLE fleet behavior, including Pluck strand settings.

### Key Settings for Bead Visibility

#### Workspace Paths
```yaml
workspace:
  default: /home/coding              # Base directory for workspace discovery
  home: /home/coding/.needle         # NEEDLE's internal workspace
  labels: []                         # Additional workspace labels (currently empty)
```

**Impact:** Determines which `.beads/` directories Pluck can discover. Each workspace has isolated bead databases.

#### Pluck Strand Configuration
```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
    split_after_failures: 3
```

**Impact:** Controls which beads are hidden from Pluck discovery:
- `deferred` - Beads marked for later processing
- `human` - Beads requiring human intervention  
- `blocked` - Beads with blocking dependencies

#### Explore Strand Configuration
```yaml
strands:
  explore:
    enabled: true
    workspaces: []
    workspace_root: /home/coding/
```

**Impact:** Controls multi-workspace discovery. When `workspaces: []`, NEEDLE auto-discovers all `.beads/` directories under `workspace_root`.

### File Location and Format
- **Location:** `~/.config/needle/config.yaml`
- **Format:** YAML
- **Scope:** Global (affects all NEEDLE operations)
- **Reload:** Requires NEEDLE daemon restart

---

## 2. Workspace-Specific Configuration (Per-Bead-Store)

**File:** `./.beads/config.yaml` (in each workspace directory)

**Purpose:** Workspace-level bead settings (default values, prefixes, etc.)

### Current Settings (from `br config list`)
```yaml
Config:
  issue_prefixes: ["bf"]
  default_priority: 2
  default_type: task
  claim_ttl_minutes: 30
```

**Impact:** These settings affect bead creation and claiming behavior but **do not control visibility** during Pluck queries.

### File Location and Format
- **Location:** `{workspace}/.beads/config.yaml` (e.g., `/home/coding/claude-governor/.beads/config.yaml`)
- **Format:** YAML
- **Scope:** Per-workspace (isolated per project)
- **Reload:** Read on each `bf` command invocation

**Note:** This file is **optional**. If missing, Pluck uses hardcoded defaults.

---

## 3. NEEDLE Adapter Configurations

**Directory:** `~/.config/needle/adapters/`

**Purpose:** Define how agent adapters interact with workspaces and bead stores.

### Key Adapters

#### claude-print-opus.yaml
```yaml
name: claude-print-opus
description: Claude Code (opus) via claude-print — subscription billing
invoke_template: "cd {workspace} && /home/coding/.local/bin/claude-print --model {model} --max-turns 100 --output-format stream-json --dangerously-skip-permissions --no-inherit-hooks < {prompt_file}"
timeout_secs: 1200
```

**Impact:** The `invoke_template` includes `cd {workspace}`, which determines which bead database the agent operates on.

#### claude-code-glm-4.7.yaml
```yaml
name: claude-code-glm-4.7
description: Claude Code via ZAI proxy (GLM-4.7 free tier, cost-effective)
invoke_template: systemd-run --user --scope -p MemoryMax=12G bash -c 'cd {workspace} && ...'
timeout_secs: 600
```

**Impact:** Agents launched with these adapters only see beads in their assigned workspace.

### File Location and Format
- **Location:** `~/.config/needle/adapters/*.yaml`
- **Format:** YAML
- **Scope:** Per-agent-type
- **Reload:** Read on agent launch

---

## 4. Compiled Constants (Source Code)

**File:** `/home/coding/NEEDLE/src/strand/pluck.rs`

**Purpose:** Hardcoded defaults when no runtime configuration is provided.

### DEFAULT_EXCLUDE_LABELS
```rust
// From: /home/coding/NEEDLE/src/strand/pluck.rs:13
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Impact:** When Pluck strand is initialized with empty `exclude_labels`, these defaults are applied.

### Precedence
1. If NEEDLE config has custom `strands.pluck.exclude_labels`, those override
2. If NEEDLE config has empty exclude_labels, defaults are used
3. Cannot be changed without recompiling NEEDLE

---

## 5. Runtime CLI Flags and Environment

**Purpose:** Override configured behavior at runtime.

### Key Flags

#### `--workspace <path>`
```bash
bf --workspace /home/coding/claude-governor list
needle run --agent claude-print-opus --workspace /home/coding/cgov-polish-queue
```

**Impact:** Explicitly specifies which workspace's bead database to operate on.

#### `bf ready --limit 0`
**Impact:** Queries only the current workspace's bead database for ready candidates.

### Environment Variables
- `NEEDLE_WORKSPACE` - Override default workspace path
- No Pluck-specific environment variables currently set

---

## Configuration Precedence (Priority Order)

From **highest priority** (overrides everything) to **lowest** (defaults):

1. **Runtime CLI flags** (`--workspace`, `--assignee`)
   - Immediate effect, single invocation
   - Example: `bf --workspace /path/to/workspace list`

2. **NEEDLE global config** (`~/.config/needle/config.yaml`)
   - Overrides compiled defaults
   - Example: `strands.pluck.exclude_labels: ["deferred", "human"]`

3. **Compiled constants** (`DEFAULT_EXCLUDE_LABELS` in pluck.rs)
   - Used when no config override exists
   - Example: `["deferred", "human", "blocked"]`

4. **Workspace config** (`.beads/config.yaml`)
   - Per-workspace defaults for creation/claiming
   - Does NOT affect visibility filters

5. **Adapter configs** (`~/.config/needle/adapters/*.yaml`)
   - Determine workspace assignment per agent
   - Example: `invoke_template: "cd {workspace} && ..."`

---

## Relationship Between Config Files

```
Runtime CLI Flags
       │
       ├─► NEEDLE Global Config (~/.config/needle/config.yaml)
       │        │
       │        ├─► Overrides compiled constants
       │        │
       │        └─► Sets workspace paths
       │                 │
       │                 └─► Workspace Config (.beads/config.yaml)
       │                          │
       │                          └─► Per-workship defaults (creation, claiming)
       │
       └─► Adapter Configs (~/.config/needle/adapters/*.yaml)
                │
                └─► Agent invocation (includes {workspace} placeholder)
                         │
                         └─► Agent operates in specific workspace
                                  │
                                  └─► Bead database (.beads/beads.db)
```

---

## Configuration Files Summary Table

| Config File | Location | Format | Scope | Affects Visibility? | Reload |
|-------------|----------|--------|-------|---------------------|--------|
| NEEDLE global | `~/.config/needle/config.yaml` | YAML | Global | ✅ Yes (exclude_labels, workspace paths) | Daemon restart |
| Workspace config | `{workspace}/.beads/config.yaml` | YAML | Per-workspace | ❌ No (creation/claiming only) | Per command |
| Adapter configs | `~/.config/needle/adapters/*.yaml` | YAML | Per-agent-type | ✅ Yes (workspace assignment) | Agent launch |
| Compiled constants | `/home/coding/NEEDLE/src/strand/pluck.rs` | Rust code | Binary | ✅ Yes (default exclude_labels) | Recompile |
| CLI flags | Command line arguments | CLI args | Per-invocation | ✅ Yes (workspace override) | N/A |

---

## Verification Commands

### Check Current Configuration
```bash
# 1. View NEEDLE global config
cat ~/.config/needle/config.yaml

# 2. View workspace config
br config list
br config path

# 3. Test bead visibility
bf ready --limit 0

# 4. Verify database state
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status='open';"

# 5. Check which workspaces exist
find /home/coding -name ".beads" -type d | head -20
```

### Test Filter Impact
```bash
# Beads with excluded labels
sqlite3 .beads/beads.db <<'EOF'
SELECT id, title, status 
FROM issues 
WHERE status='open' 
AND id IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked'));
EOF

# Ready candidates (excludes labels)
bf ready --limit 0
```

---

## Related Documentation

- **Pluck Configuration:** `/home/coding/claude-governor/docs/plan/pluck-configuration.md`
- **Workspace Paths:** `/home/coding/claude-governor/docs/pluck-workspace-paths.md`
- **Starvation Reproduction:** `/home/coding/claude-governor/docs/research/pluck-starvation-reproduction.md`
- **NEEDLE Source:** `/home/coding/NEEDLE/src/strand/pluck.rs`
- **bead-forge Source:** `/home/coding/bead-forge/`

---

## Conclusion

Bead visibility is controlled by **five configuration sources** with clear precedence:

1. **CLI flags** (highest) - Override everything for single invocation
2. **NEEDLE global config** - Primary runtime configuration
3. **Compiled constants** - Default fallback
4. **Workspace config** - Per-workspace defaults (not visibility)
5. **Adapter configs** - Agent workspace assignment

The **most important file for visibility** is `~/.config/needle/config.yaml`, specifically:
- `strands.pluck.exclude_labels` - Which labels hide beads
- `workspace.default` and `strands.explore.workspace_root` - Which workspaces are discovered

**Documentation Complete** - All configuration files controlling bead visibility have been identified, located, and documented with their relationships and precedence.
