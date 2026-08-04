# Filter Patterns Reference

**Last Updated:** 2026-08-03
**Purpose:** Complete reference for Pluck filter patterns and common query examples

---

## Core Filter Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `state` | string | `"open"` | Bead status filter (`open`, `closed`, `in_progress`) |
| `assignee` | string\|null | `null` | Worker assignment filter |
| `labels` | string[] | `[]` | Required labels (inclusive - bead must have ALL) |
| `exclude_labels` | string[] | `["deferred", "human", "blocked"]` | Labels to exclude |
| `ephemeral` | boolean | `false` | Include ephemeral beads |
| `pinned` | boolean | `false` | Include pinned beads |
| `is_template` | boolean | `false` | Include template beads |

## Filter Application Order

Filters are applied in this order (SQL WHERE clause construction):

1. **Status filter** - `WHERE status = 'open'`
2. **Assignee filter** - `AND assignee IS NULL` (or specific worker)
3. **Exclusion filters** - `AND ephemeral = 0 AND is_template = 0`
4. **Label exclusion** - `AND NOT EXISTS (SELECT 1 FROM labels WHERE label IN (...))`
5. **Label inclusion** - `AND EXISTS (SELECT 1 FROM labels WHERE label IN (...))`

**Important:** Label exclusion (#4) happens BEFORE label inclusion (#5). A bead with both included and excluded labels will be excluded.

---

## CLI Filter Examples

### Basic Status Filters

```bash
# All open beads (default)
bf list
bf list --state open

# Closed beads
bf list --state closed

# In-progress beads
bf list --state in_progress
```

### Assignee Filters

```bash
# Unassigned beads (ready to claim)
bf ready  # Shorthand for list --assignee ''

# Beads claimed by specific worker
bf list --assignee worker-1
```

### Label Filters

```bash
# Beads with specific label (inclusive)
bf list --labels polish
bf list --labels rust,documentation  # Must have BOTH labels

# Override default exclusions
bf list --exclude-labels ''  # Don't exclude any labels
```

### Combined Filters

```bash
# Open priority 3 beads
bf list --state open --priority 3

# High-priority polish beads
bf list --labels polish --priority 3,4

# Documentation beads ready to claim
bf ready --labels documentation
```

---

## SQL Filter Patterns

### The "Ready" Query (Core Claim Pattern)

```sql
-- This is what bf ready uses internally
SELECT DISTINCT i.id, i.title, i.priority, i.type
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
  )
ORDER BY i.priority DESC, i.created_at ASC
LIMIT 1;
```

### Label-Based Queries

```sql
-- Beads with specific label
SELECT DISTINCT i.id, i.title
FROM issues i
JOIN labels l ON i.id = l.issue_id
WHERE l.label = 'polish' AND i.status = 'open';

-- Beads WITHOUT excluded labels
SELECT i.id, i.title
FROM issues i
WHERE i.status = 'open'
AND NOT EXISTS (
    SELECT 1 FROM labels
    WHERE issue_id = i.id
    AND label IN ('deferred', 'human', 'blocked')
);
```

### Dependency Filtering

```sql
-- Beads blocked by unresolved dependencies
SELECT i.id, i.title
FROM issues i
INNER JOIN dependencies d ON i.id = d.issue_id
WHERE i.status = 'open'
AND d.depends_on_id IN (
    SELECT id FROM issues WHERE status NOT IN ('closed', 'done')
);
```

---

## Common Patterns by Use Case

### For NEEDLE Workers

```bash
# Standard claim
bf claim --assignee worker-1

# Claim from specific workspace
bf claim --assignee worker-1 --workspace /home/coding/vista
```

### For Monitoring

```bash
# Check if work is available
bf ready --limit 0 | wc -l

# Find stale assignments
sqlite3 .beads/beads.db "
SELECT id, assignee FROM issues
WHERE updated_at < (strftime('%s', 'now') - 3600)
AND assignee IS NOT NULL;
"
```

### For Debugging

```bash
# See what's actually in the database (no filters)
sqlite3 .beads/beads.db "SELECT id, title, status FROM issues WHERE status='open';"

# Check what's excluded
sqlite3 .beads/beads.db "
SELECT i.id, l.label
FROM issues i JOIN labels l ON i.id = l.issue_id
WHERE l.label IN ('deferred', 'human', 'blocked');
"
```

---

## Performance Considerations

### Good: Uses indexes

```sql
SELECT id FROM issues WHERE status = 'open';
```

### Bad: Full table scan

```sql
SELECT id FROM issues WHERE description LIKE '%urgent%';
```

### Optimization Tips

1. Always filter by `status` first
2. Use `EXISTS` for label checks
3. Avoid `SELECT *`
4. Add `LIMIT` for interactive queries

---

## Related Documentation

- **Troubleshooting:** `docs/bead-visibility-troubleshooting.md`
- **Quick Reference:** `docs/bead-visibility-quickref.md`
- **Complete Visibility Map:** `docs/research/bead-visibility-configuration.md`

---

**End of Filter Patterns Reference**
