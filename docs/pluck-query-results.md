# Pluck Query Results — Historical Query Reference

> For the current NEEDLE/`bead-rs` implementation, use
> [`docs/plan/pluck-configuration.md`](plan/pluck-configuration.md). This page
> preserves older `bf`/`br`-era SQL and query examples for investigation only;
> its generic `issues` fields are not the authoritative Pluck filter contract.

## Overview

Pluck (the `br` CLI / bead-forge) queries beads from SQLite databases located in `.beads/beads.db` within each workspace. Query results are the foundation of NEEDLE's bead claiming system, cgov's capacity calculations, and the polish loop's generation pipeline.

**All Pluck queries share the same core pattern**: filter the `issues` table (beads are stored as "issues" for br compatibility) by status, assignee, and label constraints, then return matching bead IDs and their metadata for claiming, listing, or analysis.

---

## Database Schema

### The `issues` Table (Beads)

Every workspace's `.beads/beads.db` contains an `issues` table with this schema:

```sql
CREATE TABLE issues (
    id TEXT PRIMARY KEY,              -- Bead ID (e.g., "bf-11rt2")
    title TEXT NOT NULL,              -- Bead title
    description TEXT,                  -- Bead body (markdown)
    status TEXT NOT NULL,              -- "open", "closed", "in_progress"
    assignee TEXT,                    -- Currently assigned worker (NULL = unassigned)
    priority INTEGER DEFAULT 2,        -- 1 (low), 2 (default), 3 (high), 4 (critical)
    type TEXT DEFAULT 'task',          -- "task", "bug", "feature", "genesis"
    ephemeral INTEGER DEFAULT 0,       -- 1 = temporary bead, excluded from discovery
    pinned INTEGER DEFAULT 0,          -- 1 = pinned for priority, always included
    is_template INTEGER DEFAULT 0,     -- 1 = template bead, excluded from discovery
    created_at INTEGER,                -- Unix timestamp (seconds)
    updated_at INTEGER,                -- Unix timestamp (seconds)
    closed_at INTEGER                  -- Unix timestamp (seconds), NULL if open
);
```

### The `labels` Table

Many-to-many relationship between beads and labels:

```sql
CREATE TABLE labels (
    issue_id TEXT NOT NULL,           -- Foreign key to issues.id
    label TEXT NOT NULL,               -- Label name (e.g., "deferred", "human", "blocked")
    PRIMARY KEY (issue_id, label)
);
```

### Supporting Tables

- **`events`** — Audit log of bead state changes (created, claimed, closed, commented)
- **`metadata`** — Database-level metadata (format version, last sync, compaction state)

---

## Core Query Patterns

### 1. The "Ready" Query (Claimable Beads)

This is the most common Pluck query — used by NEEDLE workers to find beads they can claim:

```sql
-- Finds beads ready for claiming
SELECT DISTINCT i.id
FROM issues i
LEFT JOIN labels l ON l.issue_id = i.id
WHERE i.status = 'open'                    -- Must be open
  AND i.assignee IS NULL                    -- Must be unassigned
  AND i.ephemeral = 0                       -- Not ephemeral
  AND i.pinned = 0                          -- Not pinned (unless your config includes pinned)
  AND i.is_template = 0                     -- Not a template
  AND NOT EXISTS (                          -- Exclude excluded labels
      SELECT 1 FROM labels
      WHERE issue_id = i.id
      AND label IN ('deferred', 'human', 'blocked')
  );
```

**What this returns:** All bead IDs that are ready to be claimed by a worker.

**Used by:**
- `bf ready` — CLI command to list claimable beads
- NEEDLE workers claiming work
- The polish loop checking queue depth

### 2. The "Open Beads" Query

Counts or lists all open beads regardless of assignee:

```sql
-- Count all open beads
SELECT COUNT(*) FROM issues WHERE status = 'open';

-- List open beads with metadata
SELECT id, title, assignee, priority, type, created_at, updated_at
FROM issues
WHERE status = 'open'
ORDER BY priority DESC, updated_at DESC;
```

**Used by:**
- `bf list --state open` — Interactive listing
- Capacity calculations (e.g., cgov's backlog depth check)
- Workspace health monitoring

### 3. The "By Worker" Query (Assigned to a Specific Agent)

```sql
-- Find all beads claimed by a specific worker
SELECT id, title, status, updated_at
FROM issues
WHERE assignee = 'worker-name'
  AND status IN ('open', 'in_progress')
ORDER BY updated_at DESC;
```

**Used by:**
- `bf list --assignee worker-name` — See what a worker is working on
- Stale worker detection (unchanged updated_at)
- Worker capacity balancing

### 4. The "Exclude Labels" Query

Finds beads that do NOT have certain labels:

```sql
-- Beads without excluded labels
SELECT COUNT(DISTINCT issue_id)
FROM labels
WHERE label NOT IN ('deferred', 'human', 'blocked');

-- Alternative: find beads WITH excluded labels (for reporting)
SELECT COUNT(DISTINCT issue_id)
FROM labels
WHERE label IN ('deferred', 'human', 'blocked');
```

**Used by:**
- Filtering out deferred/blocked/human-only beads
- Reporting how many beads are excluded
- Label analytics

### 5. The "By Label" Query

Find beads with specific labels (inclusive filtering):

```sql
-- Beads with a specific label
SELECT DISTINCT i.id, i.title, i.status
FROM issues i
JOIN labels l ON l.issue_id = i.id
WHERE l.label = 'polish'
  AND i.status = 'open';
```

**Used by:**
- `bf list --labels polish,rust` — Targeted filtering
- Cohort analysis (all polish beads)
- Label-based workload segmentation

---

## Filter Parameters

All Pluck queries support these filter parameters, applied in combination:

| Parameter | Type | Description | Default |
|-----------|------|-------------|---------|
| `state` | `string` | Bead status filter | `"open"` |
| `assignee` | `string \| null` | Worker assignment | `null` (unassigned only) |
| `labels` | `string[]` | Required labels (inclusive) | `[]` (no label requirement) |
| `exclude_labels` | `string[]` | Labels to exclude | `["deferred", "human", "blocked"]` |
| `ephemeral` | `boolean` | Include ephemeral beads | `false` |
| `pinned` | `boolean` | Include pinned beads | `false` |
| `is_template` | `boolean` | Include template beads | `false` |

### Filter Application Order

Filters are applied in this logical order (SQL WHERE clause construction):

1. **Status filter** (`state`) — `WHERE status = 'open'`
2. **Assignee filter** (`assignee`) — `AND assignee IS NULL` (or specific worker)
3. **Exclusion filters** (`ephemeral`, `is_template`) — `AND ephemeral = 0 AND is_template = 0`
4. **Label exclusion** (`exclude_labels`) — `AND NOT EXISTS (SELECT 1 FROM labels WHERE label IN (...))`
5. **Label inclusion** (`labels`) — `AND EXISTS (SELECT 1 FROM labels WHERE label IN (...))` (if specified)

---

## Query Result Formats

### CLI Output Formats

Pluck (`br` CLI) supports multiple output formats:

#### 1. Human-Readable (default)

```bash
$ br ready
# Open beads in workspace: claude-governor
bf-11rt2  Document Pluck query results                task  high 2026-08-03
bf-10abc  Fix cgov null handling in poller            bug    med 2026-08-02
bf-9xyz  Implement polish queue seeder              feature  low 2026-08-01
```

#### 2. JSON

```bash
$ br ready --format json
{
  "beads": [
    {
      "id": "bf-11rt2",
      "title": "Document Pluck query results",
      "status": "open",
      "priority": 3,
      "type": "task",
      "labels": ["documentation"],
      "created_at": 1722728400,
      "updated_at": 1722728400
    }
  ],
  "total": 1
}
```

#### 3. JSONL (for streaming/parsing)

```bash
$ br ready --format jsonl
{"id":"bf-11rt2","title":"Document Pluck query results","status":"open","priority":3,"type":"task"}
{"id":"bf-10abc","title":"Fix cgov null handling","status":"open","priority":2,"type":"bug"}
```

#### 4. Compact (for scripting)

```bash
$ br ready --format compact
bf-11rt2 bf-10abc bf-9xyz
```

### SQLite Result Sets

When querying directly via SQLite (for custom scripts or integrations):

```sql
-- Return as structured result set
SELECT 
    i.id,
    i.title,
    i.status,
    i.assignee,
    i.priority,
    i.type,
    GROUP_CONCAT(l.label, ',') AS labels,
    i.created_at,
    i.updated_at
FROM issues i
LEFT JOIN labels l ON l.issue_id = i.id
WHERE i.status = 'open'
GROUP BY i.id
ORDER BY i.priority DESC, i.updated_at DESC;
```

**Result:**
| id | title | status | assignee | priority | type | labels | created_at | updated_at |
|----|-------|--------|----------|----------|------|--------|------------|------------|
| bf-11rt2 | Document Pluck query results | open | NULL | 3 | task | documentation | 1722728400 | 1722728400 |

---

## Performance Characteristics

### Indexed Columns

The `issues` table is indexed on:
- `id` (PRIMARY KEY)
- `status` (for status filtering)
- `assignee` (for worker assignment queries)
- `updated_at` (for sorting by recency)

The `labels` table is indexed on:
- `(issue_id, label)` (composite PRIMARY KEY)
- `label` (for label filtering queries)

### Query Optimization Tips

1. **Always filter by `status` first** — This is the most selective filter and uses the index
2. **Use `EXISTS` for label exclusion** — More efficient than `LEFT JOIN + WHERE IS NULL`
3. **Avoid `SELECT *`** — Specify only the columns you need
4. **Limit result sets** — Add `LIMIT 100` for interactive queries

### Common Performance Patterns

```sql
-- Good: Uses status index
SELECT id FROM issues WHERE status = 'open' AND assignee IS NULL LIMIT 50;

-- Bad: Full table scan on description
SELECT id FROM issues WHERE description LIKE '%urgent%';

-- Good: Uses labels label index with EXISTS
SELECT id FROM issues i WHERE EXISTS (
    SELECT 1 FROM labels l WHERE l.issue_id = i.id AND l.label = 'urgent'
);

-- Bad: Inefficient label join
SELECT DISTINCT i.id FROM issues i 
JOIN labels l ON l.issue_id = i.id 
WHERE l.label = 'urgent';
```

---

## Usage in the NEEDLE/Cgov Ecosystem

### 1. NEEDLE Worker Claiming

Every NEEDLE worker runs this query (via `bf claim`) to find work:

```bash
# Equivalent to:
bf claim --assignee worker-name --any-workspace
```

Which executes:
```sql
-- Find claimable bead across all workspaces
SELECT id FROM issues i
WHERE status = 'open' 
  AND assignee IS NULL
  AND ephemeral = 0
  AND NOT EXISTS (
    SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred','human','blocked')
  )
LIMIT 1;
```

### 2. Cgov Backlog Depth Calculation

The cgov daemon uses query results to check if a pool has real work before scaling up:

```bash
# In governor.rs (simplified)
ready_count = bf ready --workspace /home/coding/cgov-polish-queue | wc -l
if ready_count > running_workers {
    boost_subscription_workers()  # Only boost if real backlog exists
}
```

### 3. Polish Queue Seeder

The seeder script checks query results before creating new meta-beads:

```bash
# Only seed if backlog is low
ready_count=$(bf ready --workspace /home/coding/claude-governor | wc -l)
if [ $ready_count -lt $LOW_WATER ]; then
    bf create "Polish-gen: claude-governor" ...
fi
```

### 4. Interactive Development

Developers use query results for planning and debugging:

```bash
# What's blocking this project?
bf list --labels blocked --state open

# What am I working on?
bf list --assignee jedarden

# What needs documentation?
bf list --labels documentation --state open
```

---

## Testing and Validation

### Test Coverage

The test suite (`tests/pluck_db_test.rs`) validates:

1. **Database connectivity** — Can open and query the database
2. **Integrity checks** — `PRAGMA integrity_check` passes
3. **Schema validity** — Expected tables exist (`issues`, `labels`, `events`)
4. **Query correctness** — Counts match expectations for open/assigned/labeled beads
5. **Pluck query simulation** — The main claimable query returns results

### Running Tests

```bash
# Run Pluck database tests
cargo test pluck_db_test

# Run with query output visible
cargo test pluck_db_test -- --nocapture

# Expected output:
# === PLUCK FILTER PARAMETERS ===
# workspace_path: /home/coding/claude-governor/.beads/beads.db
# Pluck filter parameter - state: open
# Pluck filter parameter - labels: []
# Pluck filter parameter - exclude_labels: ["deferred", "human", "blocked"]
# 
# === PLUCK DATABASE CONNECTIVITY TEST RESULTS ===
# Database path: /home/coding/claude-governor/.beads/beads.db
# File exists: true
# Connection successful: true
# Database integrity check: true
# Database schema valid: true
# Total issues in database: 42
# Open issues: 8
# Issues with labels: 15
# Claimable issues (Pluck query result): 3
# Issues excluded by Pluck filters: 2
```

---

## Error Handling and Edge Cases

### Common Query Failures

1. **Database file doesn't exist**
   - Symptom: `Database file does not exist: /path/to/.beads/beads.db`
   - Cause: No `.beads/` directory in workspace
   - Fix: Run `bf init` to create bead database

2. **Database corruption**
   - Symptom: `database disk image is malformed`
   - Cause: Failed write, disk full, or crash
   - Fix: `br doctor --repair` (auto-flush must be on, or run `--flush-first` first)

3. **No results when beads exist**
   - Symptom: Query returns 0 beads but `bf list` shows some
   - Cause: All beads are filtered out by exclude_labels or assignee
   - Debug: Remove filters: `bf list --exclude-labels '' --assignee ''`

### Edge Cases in Query Logic

1. **Beads with both included and excluded labels**
   - Example: A bead labeled `polish` AND `deferred`
   - Result: Excluded (exclusion takes precedence)
   - SQL: `NOT EXISTS` clause filters it out before label inclusion

2. **Pinned ephemeral beads**
   - Example: `ephemeral=1` but `pinned=1`
   - Result: Included (pinned overrides ephemeral)
   - SQL: Query checks `pinned = 0` only when not checking pinned status

3. **Closed beads with recent updates**
   - Example: `status='closed'` but `updated_at` is recent
   - Result: Excluded (status filter applied first)
   - SQL: `WHERE status = 'open'` filters these out

---

## Advanced Query Patterns

### Multi-Workspace Aggregation

Query across multiple workspaces and aggregate results:

```sql
-- Union of ready beads across all workspaces
SELECT 'claude-governor' AS workspace, id 
FROM '/home/coding/claude-governor/.beads/beads.db'.issues
WHERE status = 'open' AND assignee IS NULL

UNION ALL

SELECT 'vista' AS workspace, id 
FROM '/home/coding/vista/.beads/beads.db'.issues
WHERE status = 'open' AND assignee IS NULL;
```

### Temporal Queries (Time-Based Filtering)

```sql
-- Beads updated in the last 24 hours
SELECT id, title, updated_at
FROM issues
WHERE updated_at > (strftime('%s', 'now') - 86400)
  AND status = 'open';

-- Beads that have been assigned too long (stale worker detection)
SELECT id, assignee, updated_at
FROM issues
WHERE assignee IS NOT NULL
  AND updated_at < (strftime('%s', 'now') - 3600)  -- 1 hour ago
  AND status IN ('open', 'in_progress');
```

### Analytics Queries

```sql
-- Bead distribution by priority
SELECT 
    CASE 
        WHEN priority = 1 THEN 'low'
        WHEN priority = 2 THEN 'medium'
        WHEN priority = 3 THEN 'high'
        WHEN priority = 4 THEN 'critical'
    END AS priority_level,
    COUNT(*) AS count
FROM issues
WHERE status = 'open'
GROUP BY priority
ORDER BY priority DESC;

-- Label co-occurrence (which labels appear together)
SELECT 
    l1.label AS label1,
    l2.label AS label2,
    COUNT(*) AS co_occurrence
FROM labels l1
JOIN labels l2 ON l1.issue_id = l2.issue_id AND l1.label < l2.label
GROUP BY l1.label, l2.label
ORDER BY co_occurrence DESC;
```

---

## Integration with Other Tools

### Direct SQLite Access

For custom scripts that don't need the full `br` CLI:

```bash
# Count open beads directly
sqlite3 .beads/beads.db "SELECT COUNT(*) FROM issues WHERE status = 'open';"

# Find claimable beads
sqlite3 .beads/beads.db "
SELECT id FROM issues i
WHERE status = 'open' AND assignee IS NULL
AND NOT EXISTS (
    SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred','human','blocked')
);"
```

### Python Integration

```python
import sqlite3

def get_claimable_beads(workspace_path):
    db_path = f"{workspace_path}/.beads/beads.db"
    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()
    
    query = """
    SELECT id, title, priority 
    FROM issues i
    WHERE status = 'open' AND assignee IS NULL
    AND NOT EXISTS (
        SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred','human','blocked')
    )
    ORDER BY priority DESC, updated_at DESC
    """
    
    cursor.execute(query)
    return [{"id": row[0], "title": row[1], "priority": row[2]} for row in cursor.fetchall()]
```

### Shell Script Integration

```bash
#!/bin/bash
# Check if workspace has ready beads
workspace="/home/coding/claude-governor"
ready_count=$(sqlite3 "${workspace}/.beads/beads.db" \
  "SELECT COUNT(*) FROM issues WHERE status='open' AND assignee IS NULL;")

if [ "$ready_count" -gt 0 ]; then
    echo "Workspace has $ready_count ready beads"
    # Launch worker or take action
else
    echo "No ready beads in workspace"
fi
```

---

## Summary

Pluck query results are the foundation of the entire NEEDLE/cgov workflow:

1. **Core query pattern** — Filter `issues` table by status, assignee, and labels
2. **The "Ready" query** — Finds claimable beads for workers to work on
3. **Filter parameters** — `state`, `assignee`, `labels`, `exclude_labels`, `ephemeral`, `pinned`, `is_template`
4. **Result formats** — Human-readable, JSON, JSONL, compact
5. **Performance** — Indexed on `status`, `assignee`, `updated_at`, and label fields
6. **Testing** — Comprehensive test coverage validates database integrity and query correctness
7. **Integration** — Used by NEEDLE workers, cgov daemon, polish seeder, and interactive development

The test suite in `tests/pluck_db_test.rs` validates that queries work correctly and return expected results, ensuring that the bead discovery system remains reliable as the codebase evolves.

**Related Documentation:**
- `docs/pluck-workspace-paths.md` — Workspace discovery and configuration
- `CLAUDE.md` — Beads (br CLI) usage and auto-flush behavior
- `tests/pluck_db_test.rs` — Test implementation and query examples
