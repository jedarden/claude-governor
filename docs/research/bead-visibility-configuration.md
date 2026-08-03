# Bead Visibility Configuration — Complete Map

**Documented:** 2026-08-03  
**Bead ID:** bf-15prd  
**Workspace:** `/home/coding/claude-governor`

## Executive Summary

Bead visibility in Pluck is controlled by a **four-layer configuration system**:

1. **Hardcoded defaults** (compile-time constants in NEEDLE binary)
2. **Global NEEDLE configuration** (`~/.config/needle/config.yaml`)
3. **Workspace exclusions** (`~/.config/needle/explore-excluded`)
4. **Database-level filters** (SQLite query constraints in `get_ready_candidates`)

This document maps every configuration source that affects whether Pluck can see and process beads.

---

## Layer 1: Hardcoded Defaults (Compile-Time)

### Location
- **File:** `/home/coding/NEEDLE/src/strand/pluck.rs:21`
- **Type:** Rust constant (compiled into NEEDLE binary)

### Configuration
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

### Behavior
- **When used:** Only when `strands.pluck.exclude_labels` in `config.yaml` is **empty or unset**
- **Fallback mechanism:** If no configuration is provided, these labels are always excluded
- **Modification:** Requires recompiling NEEDLE (`cargo build --release`)

### Excluded Labels Explained
| Label | Purpose | Typical Use Case |
|-------|---------|------------------|
| `deferred` | Beads postponed to later | Low-priority backlog items |
| `human` | Requires human intervention | Manual review needed, not for automation |
| `blocked` | Blocked by dependencies | Waiting on other beads to complete |

### Impact
These three labels are the **first line of defense** against Pluck processing inappropriate beads. Any bead with these labels is invisible to the Pluck strand, regardless of other settings.

---

## Layer 2: Global NEEDLE Configuration

### Location
- **File:** `~/.config/needle/config.yaml`
- **Format:** YAML
- **Scope:** System-wide (affects all NEEDLE operations)

### Relevant Section

```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
    split_after_failures: 3
```

### Field Specifications

#### `strands.pluck.exclude_labels`
- **Type:** List of strings
- **Default (when empty):** `["deferred", "human", "blocked"]` (from Layer 1)
- **Behavior:** Beads with any of these labels are excluded from Pluck selection
- **Precedence:** Overrides hardcoded defaults when non-empty
- **Example customization:**
  ```yaml
  strands:
    pluck:
      exclude_labels:
        - deferred
        - human
        - blocked
        - custom-label    # Additional exclusion
  ```

#### `strands.pluck.split_after_failures`
- **Type:** Integer
- **Default:** `3`
- **Behavior:** Auto-split beads after N consecutive failures (0 = disabled)
- **Impact:** Does not affect visibility, but affects bead lifecycle

### Other Relevant Sections

#### Workspace Discovery Configuration
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

These settings control **where** Pluck looks for bead databases, not **what** beads it filters. See [Workspace Discovery](#workspace-discovery) below.

---

## Layer 3: Workspace Exclusions

### Location
- **File:** `~/.config/needle/explore-excluded`
- **Format:** Plain text (one workspace path per line)
- **Scope:** NEEDLE's explore strand discovery

### Content Example
```
/home/coding/SEAM
```

### Behavior
- **What it does:** Prevents the explore strand from discovering and processing specific workspaces
- **Impact scope:** Only affects automated workspace discovery, NOT direct `--workspace` specification
- **Use case:** Exclude workspaces that should not be touched by automation (e.g., personal experiments, archived projects)

### Relationship to Pluck
This is **indirectly related** to bead visibility:
- Pluck itself does NOT read this file
- The explore strand uses it to skip workspaces during discovery
- If a workspace is excluded, its `.beads/` directory is never scanned
- Therefore, all beads in excluded workspaces are invisible to automated Pluck operations

### Interaction with Direct Workspace Specification
```bash
# This STILL works even if workspace is excluded:
needle run --agent claude-print-opus --workspace /home/coding/SEAM

# The exclusion only affects:
# - Auto-discovery from workspace_root
# - Multi-workspace "claim any" operations
```

---

## Layer 4: Database-Level Filters

### Location
- **File:** `<workspace>/.beads/beads.db` (SQLite database)
- **Table:** `issues` (primary bead table)
- **Implementation:** SQL WHERE clauses in `get_ready_candidates()`

### Source Code Location
- **File:** `/home/coding/bead-forge/src/claim.rs:414-600`
- **Function:** `get_ready_candidates()`

### Filter Conditions (SQL WHERE Clause)

```sql
WHERE i.status = 'open'                    -- 1. Status filter
  AND i.ephemeral = 0                      -- 2. Ephemeral filter
  AND i.pinned = 0                         -- 3. Pinned filter
  AND i.is_template = 0                    -- 4. Template filter
  AND i.deleted_at IS NULL                 -- 5. Soft delete filter
  AND NOT EXISTS (                         -- 6. Dependency blocker filter
      SELECT 1 FROM dependencies blocker_dep
      INNER JOIN issues blocker ON blocker.id = blocker_dep.depends_on_id
      WHERE blocker_dep.issue_id = i.id
      AND blocker_dep.type IN ('blocks', 'parent-child', 'conditional-blocks', 'waits-for')
      AND blocker.status NOT IN ('closed', 'tombstone', 'done', 'completed')
  )
```

### Filter Breakdown

| # | Filter | Column | Condition | Purpose |
|---|--------|--------|-----------|---------|
| 1 | Status | `status` | `= 'open'` | Only open beads are candidates |
| 2 | Ephemeral | `ephemeral` | `= 0` | Exclude temporary/meta beads |
| 3 | Pinned | `pinned` | `= 0` | Exclude manually pinned beads |
| 4 | Template | `is_template` | `= 0` | Exclude template beads |
| 5 | Soft delete | `deleted_at` | `IS NULL` | Exclude soft-deleted beads |
| 6 | Blockers | `dependencies` join | No active blockers | Exclude beads with unresolved dependencies |

### Column Schema References

```sql
-- From issues table schema:
status TEXT NOT NULL DEFAULT 'open',
ephemeral INTEGER NOT NULL DEFAULT 0,
pinned INTEGER NOT NULL DEFAULT 0,
is_template INTEGER NOT NULL DEFAULT 0,
deleted_at DATETIME,
```

### Label Filtering (Separate Query)

Label-based filtering (`deferred`, `human`, `blocked`) is **NOT** in the main `get_ready_candidates` SQL query. Instead:

1. **Pluck strand** applies label filtering after fetching candidates
2. **Implementation:** `NOT EXISTS (SELECT 1 FROM labels WHERE label IN (...))`
3. **Layer 1 + Layer 2** control which labels are excluded

### Database Indexes (Performance)

```sql
CREATE INDEX idx_issues_status ON issues(status);
CREATE INDEX idx_issues_priority ON issues(priority);
CREATE INDEX idx_labels_label ON labels(label);
CREATE INDEX idx_labels_issue ON labels(issue_id);
```

These indexes ensure the visibility queries execute efficiently even with thousands of beads.

---

## Layer 5: Per-Workspace Configuration (Optional)

### Location
- **File:** `<workspace>/.beads/config.yaml`
- **Format:** YAML
- **Scope:** Single workspace only
- **Current state:** **NOT present** in `/home/coding/claude-governor/.beads/`

### Purpose (When Present)
Workspace-specific Pluck configuration can override global defaults:
```yaml
# Example workspace-specific config
exclude_labels:
  - deferred
  - custom-workspace-label
claim_ttl_minutes: 60
```

### Priority
**Per-workspace config > Global config > Hardcoded defaults**

However, this workspace does **not** currently use per-workspace overrides, so global defaults apply.

---

## Configuration Priority & Precedence

### Decision Tree for Bead Visibility

```
Is bead in excluded workspace? (explore-excluded file)
├─ Yes → INVISIBLE to auto-discovery (but visible if --workspace specified directly)
└─ No → Continue

Does bead pass database filters? (Layer 4 SQL WHERE)
├─ No → INVISIBLE (status, ephemeral, pinned, is_template, deleted_at, blockers)
└─ Yes → Continue

Does bead have excluded labels? (Layer 2 config → Layer 1 defaults)
├─ Yes → INVISIBLE to Pluck strand
└─ No → VISIBLE to Pluck strand
```

### Precedence Order (Highest to Lowest)

1. **Database-level filters** (Layer 4) — Hard SQL constraints, cannot be overridden
2. **Label exclusions** (Layer 2 config → Layer 1 defaults) — Can be customized in config.yaml
3. **Workspace exclusions** (Layer 3) — Affects discovery, not direct access
4. **Hardcoded defaults** (Layer 1) — Only when config.yaml has empty exclude_labels

### Modification Difficulty

| Layer | File | Modification Difficulty | Requires Restart? |
|-------|------|-------------------------|-------------------|
| 1 | `/home/coding/NEEDLE/src/strand/pluck.rs` | Hard (recompile NEEDLE) | Yes |
| 2 | `~/.config/needle/config.yaml` | Easy (text edit) | Yes (cgov/NEEDLE) |
| 3 | `~/.config/needle/explore-excluded` | Easy (text edit) | No (read on each discovery) |
| 4 | Database schema | Very Hard (migration) | N/A |

---

## Complete Configuration File Inventory

### Primary Visibility Control Files

| File | Location | Format | Controls | Priority |
|------|----------|--------|----------|----------|
| `config.yaml` | `~/.config/needle/config.yaml` | YAML | Global label exclusions, workspace discovery | High |
| `explore-excluded` | `~/.config/needle/explore-excluded` | Plain text | Workspace discovery exclusions | Medium |
| `beads.db` | `<workspace>/.beads/beads.db` | SQLite | Database-level filters (status, ephemeral, etc.) | Highest |
| Source code | `/home/coding/NEEDLE/src/strand/pluck.rs:21` | Rust | Default excluded labels (fallback) | Low |

### Secondary/Related Files

| File | Location | Purpose |
|------|----------|---------|
| `config.yaml` (workspace) | `<workspace>/.beads/config.yaml` | Workspace-specific overrides (optional) |
| `issues.jsonl` | `<workspace>/.beads/issues.jsonl` | Git-tracked checkpoint, not visibility control |
| `adapters/*.yaml` | `~/.config/needle/adapters/` | Agent launch configs, not visibility |

---

## Current System State (2026-08-03)

### Active Configuration

**Layer 1 (Hardcoded):**
```
DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked"]
```

**Layer 2 (Global Config):**
```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
```

**Layer 3 (Workspace Exclusions):**
```
/home/coding/SEAM
```

**Layer 4 (Database Filters):**
- Enforced by SQL query in `get_ready_candidates()`
- 20 open beads in database
- 5 beads with excluded labels (deferred)
- 15 ready beads visible to Pluck

### Verification Commands

```bash
# Check current config
cat ~/.config/needle/config.yaml | grep -A 5 "strands.pluck"

# Check excluded workspaces
cat ~/.config/needle/explore-excluded

# Verify database-level visibility
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status='open';"
# Output: 20

# Verify label exclusions working
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status='open' AND id IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked'));"
# Output: 5

# Verify ready candidates (after all filters)
bf ready --limit 0 | grep -c "^\[bf-"
# Output: 15
```

---

## Workspace Discovery Mechanics

### How Pluck Finds Workspaces

```
1. Start from workspace.default (/home/coding)
2. Scan directories for .beads/ subdirectories
3. Exclude paths in explore-excluded file
4. For each discovered workspace:
   a. Open <workspace>/.beads/beads.db
   b. Run get_ready_candidates() SQL query
   c. Apply label exclusions from config
   d. Return filtered candidate list
```

### Multi-Workspace Operations

```bash
# Claim from any workspace (respects explore-excluded)
bf claim --any-workspace

# Direct workspace specification (bypasses explore-excluded)
bf claim --workspace /home/coding/SEAM
```

---

## Common Visibility Issues

### Issue 1: "N open beads, 0 found"

**Symptom:** Database shows open beads, but `bf ready` returns nothing

**Causes:**
1. All open beads have excluded labels (deferred/human/blocked)
2. Database corruption or missing indexes
3. Wrong workspace path
4. All beads are blocked by dependencies

**Diagnosis:**
```bash
# Check label exclusions
sqlite3 .beads/beads.db "SELECT id, title FROM issues WHERE status='open' AND id IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked'));"

# Check blockers
sqlite3 .beads/beads.db "SELECT i.id FROM issues i INNER JOIN dependencies d ON i.id = d.issue_id WHERE i.status='open' AND d.type IN ('blocks', 'parent-child');"
```

### Issue 2: Workspace Not Discovered

**Symptom:** Workspace beads not found during auto-discovery

**Causes:**
1. Workspace in `explore-excluded` file
2. No `.beads/` directory in workspace
3. `workspace_root` does not include workspace parent directory

**Diagnosis:**
```bash
# Check if excluded
grep /home/coding/myproject ~/.config/needle/explore-excluded

# Check workspace_root
grep workspace_root ~/.config/needle/config.yaml

# Verify .beads exists
ls -la /home/coding/myproject/.beads/
```

### Issue 3: Label Exclusions Not Working

**Symptom:** Beads with excluded labels still appear

**Causes:**
1. Config.yaml has empty exclude_labels (expecting defaults, but bug exists)
2. Labels table missing or corrupted
3. Case sensitivity in label names

**Diagnosis:**
```bash
# Verify config is loaded
cat ~/.config/needle/config.yaml | grep -A 3 "exclude_labels"

# Check labels in database
sqlite3 .beads/beads.db "SELECT DISTINCT label FROM labels;"
```

---

## Configuration Change Workflow

### Changing Label Exclusions

1. **Edit global config:**
   ```bash
   nano ~/.config/needle/config.yaml
   # Modify strands.pluck.exclude_labels
   ```

2. **Restart affected services:**
   ```bash
   cgov restart              # If cgov is running NEEDLE workers
   # Or restart NEEDLE fleet manually
   ```

3. **Verify changes:**
   ```bash
   bf ready --limit 0        # Check if new labels are excluded
   ```

### Adding Workspace Exclusions

1. **Edit exclusions file:**
   ```bash
   echo "/home/coding/new-project" >> ~/.config/needle/explore-excluded
   ```

2. **No restart required** (read on each discovery operation)

3. **Verify:**
   ```bash
   cat ~/.config/needle/explore-excluded
   ```

---

## Related Documentation

- **Pluck Workspace Paths:** `/home/coding/claude-governor/docs/pluck-workspace-paths.md`
- **Starvation Reproduction:** `/home/coding/claude-governor/docs/research/pluck-starvation-reproduction.md`
- **NEEDLE Architecture:** `/home/coding/claude-governor/docs/research/needle-architecture.md`

---

## Summary

**Bead visibility is controlled by four distinct layers:**

1. **Hardcoded defaults** (`["deferred", "human", "blocked"]` in `pluck.rs`)
2. **Global config** (`strands.pluck.exclude_labels` in `config.yaml`)
3. **Workspace exclusions** (`explore-excluded` file)
4. **Database filters** (SQL WHERE clauses in `get_ready_candidates`)

**Precedence:** Database filters > Label exclusions (config > defaults) > Workspace exclusions > Hardcoded defaults

**Key files:**
- `~/.config/needle/config.yaml` — Primary visibility configuration
- `~/.config/needle/explore-excluded` — Workspace discovery exclusions
- `<workspace>/.beads/beads.db` — Database-level filters (immutable at runtime)
- `/home/coding/NEEDLE/src/strand/pluck.rs` — Default label constants (compile-time)

---

**Documentation Complete** — This map provides a comprehensive inventory of all configuration sources affecting bead visibility in the Pluck system.
