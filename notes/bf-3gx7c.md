# Bead Visibility Configuration Files in Pluck/NEEDLE

## Configuration File Locations and Scope

### 1. Global Configuration
**Location:** `~/.config/needle/config.yaml`  
**Scope:** System-wide, affects all workspaces and NEEDLE operations  
**Precedence:** Highest-level defaults, overridden by workspace-specific configs

#### Key visibility settings in global config:
```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
    split_after_failures: 3
```

**Impact:** The `exclude_labels` array controls which labels hide beads from Pluck queries. Beads with any of these labels are filtered out from standard `bf list` and `bf ready` output.

---

### 2. Workspace-Level Configuration
**Location:** `.beads/config.yaml` (within each workspace)  
**Scope:** Workspace-specific, overrides global settings  
**Precedence:** Overrides global config for this workspace only  
**Purpose:** Currently used for secret protection allowlists, not bead visibility

#### Example workspace config:
```yaml
secret_protection:
  allowlist:
    - "tests/fixtures/encoding/generate_unmapped_gl"
    - "crates/pdftract-core/tests/test_truncated_fl"
```

**Note:** As of bead-forge 0.4.0, workspace-level config does **NOT** yet support overriding `exclude_labels`. This is a potential configuration gap.

---

### 3. Database-Level Visibility Controls
**Location:** `.beads/beads.db` (SQLite database)  
**Scope:** Workspace-specific, live bead data  
**Precedence:** Final authority on bead visibility

#### Visibility columns in the `issues` table:

| Column | Type | Visibility Impact |
|--------|------|-------------------|
| `status` | TEXT | 'open', 'blocked', 'in_progress', 'closed', 'done', 'tombstone' |
| `deleted_at` | DATETIME | Soft delete: NULL = visible, any value = hidden |
| `compaction_level` | INTEGER | NULL = archived/compacted, 0 = active |
| `ephemeral` | INTEGER | 1 = ephemeral (excluded from ready queries), 0 = normal |
| `pinned` | INTEGER | Pinned beads shown differently, not hidden |
| `is_template` | INTEGER | 1 = template (excluded from ready queries), 0 = normal |

#### Indexes that control visibility:
- **`idx_issues_ready`**: Filters for `status='open' AND ephemeral=0 AND pinned=0 AND is_template=0`
- **`idx_issues_list_active_order`**: Filters out `status IN ('closed', 'tombstone')` and templates
- **`idx_issues_tombstone`**: Index on `status='tombstone'` for efficient filtering

---

### 4. JSONL Checkpoint Files
**Locations:** 
- `.beads/issues.jsonl` - Primary bead checkpoint
- `.beads/beads.base.jsonl` - Base bead data
- `.beads/events.jsonl` - Event history

**Scope:** Workspace-specific, git-tracked backups  
**Precedence:** Read-only reference; database is live authority  
**Purpose:** Durability and git sync, not runtime visibility control

---

## Configuration Precedence Order

1. **Database** (`.beads/beads.db`) - Live data, final authority
2. **Global config** (`~/.config/needle/config.yaml`) - Default `exclude_labels`
3. **Workspace config** (`.beads/config.yaml`) - Workspace overrides (currently limited to secret protection)
4. **JSONL files** - Checkpoint reference only

---

## Why Beads Might Be Hidden

Based on the configuration above, beads are hidden from standard `bf list`/`bf ready` output if they have:

1. **Any excluded label** (from global `strands.pluck.exclude_labels`):
   - `deferred`
   - `human`
   - `blocked`

2. **Soft delete flag set**: `deleted_at IS NOT NULL`

3. **Archived/compacted**: `compaction_level IS NULL`

4. **Status filtering**:
   - `status = 'closed'` (excluded from active views)
   - `status = 'tombstone'` (fully excluded)

5. **Special bead types**:
   - `ephemeral = 1` (excluded from `bf ready`)
   - `is_template = 1` (excluded from `bf ready`)

---

## Potential Configuration Gaps

### Issue 1: No workspace-level `exclude_labels` override
**Problem:** Some workspaces may want different visibility rules (e.g., show `blocked` beads in some contexts).  
**Current state:** Only global `exclude_labels` exists in `~/.config/needle/config.yaml`.  
**Impact:** All workspaces share the same label-based visibility rules.

### Issue 2: `bf ready` filter not exposed in config
**Problem:** The `bf ready` filter (`status='open' AND ephemeral=0 AND pinned=0 AND is_template=0`) is hardcoded.  
**Current state:** No way to configure which statuses or flags are considered "ready".  
**Impact:** Cannot customize "ready bead" definition per workspace.

### Issue 3: Multiple visibility layers
**Problem:** Labels, status, compaction, and soft delete all interact to hide beads.  
**Current state:** No unified "visibility report" command to show why specific beads are hidden.  
**Impact:** Difficult to debug "missing bead" issues.

---

## Verification Commands

### Check global config:
```bash
grep -A 3 "exclude_labels:" ~/.config/needle/config.yaml
```

### Check workspace config:
```bash
cat .beads/config.yaml 2>/dev/null || echo "No workspace config"
```

### Check for soft-deleted beads:
```bash
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE deleted_at IS NOT NULL;"
```

### Check for compacted/archived beads:
```bash
sqlite3 .beads/beads.db "SELECT compaction_level, COUNT(*) FROM issues GROUP BY compaction_level;"
```

### List beads with excluded labels:
```bash
# For each excluded label
sqlite3 .beads/beads.db "SELECT id, title FROM issues WHERE id IN (SELECT issue_id FROM bead_labels WHERE label = 'deferred');"
```

### Show all beads regardless of visibility:
```bash
bf list --all --limit 0
```

---

## Recommendations

1. **Document workspace-specific needs:** Each workspace should document its visibility requirements in `.beads/config.yaml` comments, even if not yet supported.

2. **Add workspace override support:** Extend `.beads/config.yaml` schema to support:
   ```yaml
   visibility:
     exclude_labels:
       - human
       - deferred
     show_blocked: false
   ```

3. **Create visibility debugging command:** Add `bf visibility --check <bead-id>` to report all reasons a bead is hidden.

4. **Centralize configuration:** Merge NEEDLE's `~/.config/needle/config.yaml` (strands.pluck.exclude_labels) with bead-forge's workspace config to avoid confusion.
