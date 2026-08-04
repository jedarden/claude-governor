# Bead Visibility Quick Reference

**Last Updated:** 2026-08-03

## Six-Layer Priority (Highest → Lowest)

1. **Database filters** (SQL WHERE) - Immutable at runtime
2. **Workspace .needle.yaml** - Per-workspace overrides
3. **Global config.yaml** - System-wide settings
4. **Hardcoded defaults** - Fallback: `["deferred", "human", "blocked"]`
5. **Workspace exclusions** - Affects discovery only
6. **.beads/config.yaml** - Does NOT affect visibility

## Default Exclude Labels

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Location:** `/home/coding/NEEDLE/src/strand/pluck.rs:21`

## Common Pitfalls

| Pitfall | Symptom | Fix |
|---------|---------|-----|
| `exclude_labels: []` | Excludes nothing (not defaults) | Omit key or include all three defaults |
| Custom labels without defaults | Loses default exclusions | Always include `deferred`, `human`, `blocked` |
| Wrong working directory | Queries wrong database | `cd` to workspace or use `--workspace` |
| Config not reloaded | Edits ignored | Restart cgov/NEEDLE |
| Case-sensitive labels | `Deferred` ≠ `deferred` | Use consistent lowercase |

## Filter Syntax

```bash
# CLI
bf ready --limit 0
bf list --state open --priority 3
bf list --labels polish,rust
bf claim --any-workspace

# SQL
SELECT id FROM issues WHERE status='open' AND assignee IS NULL;
```

## Health Check

```bash
# Quick diagnostic
sqlite3 .beads/beads.db "PRAGMA integrity_check;"        # Should return "ok"
bf ready --limit 0 | wc -l                                # Should be > 0 if open beads exist
```

## Key Files

| File | Purpose |
|------|---------|
| `~/.config/needle/config.yaml` | Global NEEDLE config |
| `<workspace>/.needle.yaml` | Workspace overrides (optional) |
| `<workspace>/.beads/config.yaml` | Lifecycle config (NOT visibility) |
| `~/.config/needle/explore-excluded` | Workspace discovery exclusions |

## Emergency Commands

```bash
# Find where a bead actually is
find ~/ -name "beads.db" -exec sqlite3 {} "SELECT 'found' FROM issues WHERE id='bf-xxx';" \; 2>/dev/null

# Database corruption
br doctor --repair

# Check all open beads
sqlite3 .beads/beads.db "SELECT id, title FROM issues WHERE status='open';"

# Check excluded labels
sqlite3 .beads/beads.db "SELECT DISTINCT label FROM labels WHERE label IN ('deferred', 'human', 'blocked');"
```

## Related Documentation

- **Full troubleshooting guide:** `docs/bead-visibility-troubleshooting.md`
- **Complete visibility map:** `docs/research/bead-visibility-configuration.md`
- **Query patterns:** `docs/pluck-query-results.md`
