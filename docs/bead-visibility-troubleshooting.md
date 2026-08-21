# Bead Visibility Troubleshooting Guide — Historical

> For the current NEEDLE/`bead-rs` implementation and active four-label list,
> use [`docs/plan/pluck-configuration.md`](plan/pluck-configuration.md).
> Commands and SQL on this page are retained for older `bf`/`br` investigations.

**Last Updated:** 2026-08-20
**Purpose:** Quick reference for diagnosing and fixing bead visibility issues

---

## Quick Diagnosis Flow

When beads aren't being discovered or processed, follow this flow:

```
Step 1: Verify beads exist in database
├─ sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status='open';"
├─ If 0: No open beads (not a visibility issue)
└─ If >0: Continue to Step 2

Step 2: Check ready candidates
├─ bf ready --limit 0 | wc -l
├─ If >0: Visibility is working (beads are being found)
└─ If 0: Continue to Step 3

Step 3: Check excluded labels
├─ sqlite3 .beads/beads.db "SELECT COUNT(DISTINCT issue_id) FROM labels WHERE label IN ('deferred', 'human', 'blocked');"
├─ If count matches open count: All beads are excluded (add/remove labels)
└─ If < open count: Continue to Step 4

Step 4: Check workspace path
├─ pwd  # Verify you're in the correct workspace
├─ ls -la .beads/beads.db  # Verify database exists
└─ If database missing: Run bf init

Step 5: Check database integrity
├─ sqlite3 .beads/beads.db "PRAGMA integrity_check;"
├─ If not "ok": Database corruption → run br doctor --repair
└─ If "ok": Configuration issue → check config files
```

---

## Common Pitfalls (by Category)

### 1. Exclude Labels Pitfalls

#### Pitfall 1.1: Empty exclude_labels Expects Defaults
**Problem:** You set `strands.pluck.exclude_labels: []` expecting to disable exclusions.

**Why:** The current Pluck implementation replaces an empty list with its compiled defaults.

**Fix:**
```yaml
# Uses compiled defaults: deferred, human, blocked
strands:
  pluck:
    exclude_labels: []

# Explicit equivalent of the compiled defaults
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked

# Omitting the key also uses the compiled defaults
strands:
  pluck:
    split_after_failures: 3
```

#### Pitfall 1.2: Custom Labels Override Defaults Completely
**Problem:** You add a custom label expecting it to be merged with defaults, but defaults are lost.

**Why:** Custom exclude_labels REPLACE defaults, not merge

**Fix:** Always include all three defaults when customizing:
```yaml
strands:
  pluck:
    exclude_labels:
      - deferred    # Required
      - human       # Required
      - blocked     # Required
      - my-custom-label  # Your addition
```

#### Pitfall 1.3: Case Sensitivity in Labels
**Problem:** Beads with label `Deferred` (capital D) are not excluded by `deferred` filter.

**Why:** Label matching is case-sensitive

**Fix:** Use consistent lowercase labeling:
```bash
# Check what labels actually exist
sqlite3 .beads/beads.db "SELECT DISTINCT label FROM labels;"

# Fix inconsistent labels
bf update bf-123abc --remove-label Deferred --add-label deferred
```

### 2. Workspace Path Pitfalls

#### Pitfall 2.1: Wrong Working Directory
**Problem:** Running commands from parent directory instead of workspace.

**Symptom:** `bf: No .beads directory found` or queries wrong database

**Fix:**
```bash
# WRONG - from parent directory
cd /home/coding
bf ready  # Looks in /home/coding/.beads/ (may not exist)

# CORRECT - from workspace
cd /home/coding/claude-governor
bf ready  # Looks in /home/coding/claude-governor/.beads/
```

#### Pitfall 2.2: Workspace in explore-excluded but Specified Directly
**Problem:** Workspace is in `~/.config/needle/explore-excluded` but you're using `--workspace` flag, so it should work...but it doesn't.

**Why:** explore-excluded only affects auto-discovery, NOT direct `--workspace` specification

**Fix:** Either remove from excluded or use direct specification:
```bash
# Option 1: Remove from excluded
sed -i '/\/home\/coding\/myproject/d' ~/.config/needle/explore-excluded

# Option 2: Use direct workspace specification (always works)
needle run --agent claude-print-opus --workspace /home/coding/myproject
```

#### Pitfall 2.3: Multiple .beads/ Directories in Path
**Problem:** Workspace discovery finds wrong `.beads/` directory when multiple exist in parent path.

**Example:** Both `/home/coding/.beads/` and `/home/coding/claude-governor/.beads/` exist

**Fix:** Always specify workspace explicitly:
```bash
# Explicit workspace (always correct)
bf --workspace /home/coding/claude-governor ready

# Or cd into workspace first
cd /home/coding/claude-governor
bf ready  # Uses nearest .beads/ directory
```

### 3. Database Filter Pitfalls

#### Pitfall 3.1: Beads Blocked by Dependencies
**Problem:** Open beads with no excluded labels still don't appear in `bf ready`.

**Why:** Database-level filter excludes beads with unresolved blocking dependencies

**Diagnosis:**
```bash
# Find blocked beads
sqlite3 .beads/beads.db <<'EOF'
SELECT i.id, i.title 
FROM issues i
INNER JOIN dependencies d ON i.id = d.issue_id
WHERE i.status='open' 
AND d.type IN ('blocks', 'parent-child')
AND d.depends_on_id IN (SELECT id FROM issues WHERE status NOT IN ('closed', 'done', 'completed'));
EOF
```

**Fix:** Either:
1. Complete the blocking bead first
2. Remove invalid blocking dependencies: `bf dep remove bf-child bf-blocker`
3. Add the `blocked` label: `bf update bf-child --add-label blocked`

#### Pitfall 3.2: Ephemeral or Template Beads
**Problem:** Beads you just created don't appear in `bf ready`.

**Why:** Beads may be marked `ephemeral=1` or `is_template=1`

**Diagnosis:**
```bash
# Check ephemeral/template status
sqlite3 .beads/beads.db "SELECT id, title, ephemeral, is_template FROM issues WHERE status='open';"
```

**Fix:** If creating meta-beads or tracking beads that should persist:
```bash
# Convert ephemeral to regular bead
bf update bf-xxx --ephemeral false

# Templates should stay is_template=1 (they're not meant to be claimed)
```

### 4. Configuration Pitfalls

#### Pitfall 4.1: Config Not Reloaded After Changes
**Problem:** You edited `~/.config/needle/config.yaml` but changes don't take effect.

**Why:** NEEDLE/cgov only reads config on startup

**Fix:** Restart the daemon/service:
```bash
# For cgov
cgov restart

# For NEEDLE fleet
pkill needle && needle run --agent ...
```

#### Pitfall 4.2: Workspace .needle.yaml Conflicts with Global
**Problem:** Workspace has `.needle.yaml` that overrides global config unexpectedly.

**Why:** Workspace-level config takes precedence over global config

**Diagnosis:**
```bash
# Check for workspace override
cat .needle.yaml

# Or check what's actually in use
needle config show
```

**Fix:** Either:
1. Remove `.needle.yaml` to use global config
2. Edit `.needle.yaml` to match desired behavior

### 5. Multi-Workspace Pitfalls

#### Pitfall 5.1: Bead in Wrong Workspace
**Problem:** Bead exists but isn't found by multi-workspace query.

**Why:** Worker is pointing to different workspace than where bead was created

**Fix:** Check bead location and worker workspace:
```bash
# Find where the bead actually is
find ~/ -name "beads.db" -exec sqlite3 {} "SELECT 'found' FROM issues WHERE id='bf-xxx';" \; 2>/dev/null

# Check where worker is pointing
ps aux | grep needle | grep -o -- '--workspace [^ ]*'

# Point worker to correct workspace
needle run --agent claude-print-opus --workspace /path/to/correct/workspace
```

#### Pitfall 5.2: Cross-Workspace Dependencies
**Problem:** Bead A in workspace 1 blocks bead B in workspace 2, but the blocking isn't detected.

**Why:** Dependencies only work within the same workspace database

**Fix:** Keep related beads in same workspace, or use meta-beads to coordinate cross-workspace work

---

## Filter Syntax Reference

### CLI Filter Examples

```bash
# Basic filters
bf list --state open                    # Only open beads
bf list --assignee worker-1            # Beads claimed by worker-1
bf list --labels polish,rust           # Beads with BOTH labels
bf list --exclude-labels ''            # Don't exclude any labels

# Combined filters
bf ready --limit 10                     # First 10 ready beads
bf list --state open --priority 3      # Open priority 3 beads
bf list --labels polish --exclude-labels ''  # Polish beads (no default exclusions)

# Multi-workspace
bf claim --any-workspace               # Claim from any workspace
bf list --workspace /path/to/ws        # List from specific workspace
```

### SQL Filter Patterns

```sql
-- The exact query bf ready uses
SELECT DISTINCT i.id
FROM issues i
WHERE i.status = 'open'
  AND i.assignee IS NULL
  AND i.ephemeral = 0
  AND i.pinned = 0
  AND i.is_template = 0
  AND NOT EXISTS (
    SELECT 1 FROM labels 
    WHERE issue_id = i.id 
    AND label IN ('deferred', 'human', 'blocked')
  );

-- Beads with specific labels
SELECT i.id, i.title
FROM issues i
INNER JOIN labels l ON i.id = l.issue_id
WHERE l.label = 'polish' AND i.status = 'open';

-- Beads WITHOUT specific labels
SELECT i.id, i.title
FROM issues i
WHERE i.status = 'open'
  AND NOT EXISTS (
    SELECT 1 FROM labels 
    WHERE issue_id = i.id 
    AND label = 'deferred'
  );
```

---

## Configuration File Best Practices

### ~/.config/needle/config.yaml

```yaml
# GOOD: Minimal, rely on compiled defaults
workspace:
  default: /home/coding
  home: /home/coding/.needle

strands:
  explore:
    enabled: true
    workspaces: []          # Empty = auto-discover
    workspace_root: /home/coding/

# Pluck uses compiled defaults: ["deferred", "human", "blocked"]

# GOOD: Explicit custom labels (includes defaults)
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
      - experimental        # Your custom label

# BAD: Empty array (excludes nothing)
strands:
  pluck:
    exclude_labels: []     # WRONG!
```

### Workspace .needle.yaml (Optional)

```yaml
# Only use if you need workspace-specific overrides
strands:
  pluck:
    enabled: true
    exclude_labels:
      - deferred
      - human
      - blocked
      - workspace-specific-label
```

### .beads/config.yaml (Optional)

```yaml
# This does NOT affect visibility (only lifecycle/scoring)
issue_prefixes:
  - bf
default_priority: 2
default_type: task
claim_ttl_minutes: 30
scoring:
  priority_weight: 0.4
  blockers_weight: 0.3
  age_weight: 0.2
  labels_weight: 0.1
```

---

## Health Check Commands

Run these to verify system health:

```bash
# 1. Database integrity
sqlite3 .beads/beads.db "PRAGMA integrity_check;"
# Expected: "ok"

# 2. Schema validity
sqlite3 .beads/beads.db ".schema issues" | grep -q "CREATE TABLE issues"
# Expected: (no error)

# 3. Label table exists
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM labels;"
# Expected: (number, not error)

# 4. Open beads count
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status='open';"
# Expected: (number >= 0)

# 5. Ready beads (after all filters)
bf ready --limit 0 | wc -l
# Expected: (number, should be <= open count)

# 6. Excluded by labels
sqlite3 .beads/beads.db "SELECT COUNT(DISTINCT issue_id) FROM labels WHERE label IN ('deferred', 'human', 'blocked');"
# Expected: (number of beads with excluded labels)

# 7. Blocked by dependencies
sqlite3 .beads/beads.db "SELECT COUNT(DISTINCT i.id) FROM issues i INNER JOIN dependencies d ON i.id = d.issue_id WHERE i.status='open' AND d.depends_on_id IN (SELECT id FROM issues WHERE status NOT IN ('closed', 'done'));"
# Expected: (number of beads with unresolved blockers)
```

---

## Starvation Prevention

### Monitoring Script

```bash
#!/bin/bash
# pluck-health-check.sh - Run periodically to detect starvation

WORKSPACE="/home/coding/claude-governor"
cd "$WORKSPACE" || exit 1

OPEN_COUNT=$(sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status='open';")
READY_COUNT=$(bf ready --limit 0 | grep -c "^\[bf-" || echo 0)

if [ "$OPEN_COUNT" -gt 0 ] && [ "$READY_COUNT" -eq 0 ]; then
    echo "WARNING: Pluck starvation detected in $WORKSPACE"
    echo "  Open beads: $OPEN_COUNT"
    echo "  Ready beads: $READY_COUNT"
    echo "  All open beads are excluded or blocked"
    
    # Create alert bead
    bf create --type human \
        --title "Pluck starvation in $WORKSPACE" \
        --description "$OPEN_COUNT open beads, $READY_COUNT ready. Investigate filter configuration." \
        --labels "alert,pluck-starvation" || true
fi
```

### Regular Maintenance

```bash
#!/bin/bash
# Weekly maintenance

# 1. Optimize database
sqlite3 .beads/beads.db "PRAGMA optimize;"

# 2. Check integrity
sqlite3 .beads/beads.db "PRAGMA integrity_check;" | grep -v "^ok"

# 3. Vacuum if needed
DB_SIZE=$(du -m .beads/beads.db | cut -f1)
if [ "$DB_SIZE" -gt 100 ]; then
    sqlite3 .beads/beads.db "VACUUM;"
fi
```

---

## Related Documentation

- **Complete Visibility Map:** `docs/research/bead-visibility-configuration.md` - Six-layer configuration system
- **Workspace Paths:** `docs/pluck-workspace-paths.md` - Workspace discovery and configuration
- **Query Results:** `docs/pluck-query-results.md` - Query patterns and SQL examples
- **Starvation Reproduction:** `docs/research/pluck-starvation-reproduction.md` - Historical bug analysis

---

## Quick Reference Summary

| Issue | Symptom | Check | Fix |
|-------|---------|-------|-----|
| No open beads | `bf ready` returns 0, DB shows 0 open | `sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status='open';"` | Create beads or check status |
| All excluded | `bf ready` returns 0, DB shows N open | Check excluded labels count | Remove/adjust labels or fix filter config |
| Wrong workspace | Beads exist but not found | `pwd` and `ls .beads/beads.db` | `cd` to correct workspace or use `--workspace` |
| Database corruption | Integrity check fails | `PRAGMA integrity_check;` | `br doctor --repair` |
| Config not applied | Edits don't take effect | Check if NEEDLE restarted | `cgov restart` or restart NEEDLE |
| Blocked beads | Open beads don't appear | Check dependencies table | Complete blockers or remove deps |

---

**End of Troubleshooting Guide**
