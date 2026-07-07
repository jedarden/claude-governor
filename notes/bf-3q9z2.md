# Pluck Starvation Alert Investigation - bf-3q9z2

## Issue Summary

**Alert:** Pluck reported finding 0 candidates despite 41 open beads existing in the workspace.

**Root Cause:** Workspace configuration mismatch - NEEDLE configured with wrong default workspace.

## Investigation Update (2026-07-07)

### Workspace Configuration Issue

The NEEDLE configuration has the **wrong default workspace**:

```yaml
# ~/.needle/config.yaml
workspace:
  default: /home/coding/telegram-claude-bridge  # ❌ WRONG
```

But the current workspace is `/home/coding/claude-governor` with:
- **Total beads:** 1007
- **Open beads:** 41
- **Claimable beads:** 23 (after filtering deferred labels)

### Evidence

1. **telegram-claude-bridge** (current default): 148 total, 11 open
2. **claude-governor** (actual workspace): 1007 total, 41 open
3. Starvation alert statistics match claude-governor, not the configured default

### Solution

Update NEEDLE configuration to set claude-governor as the default workspace:

```bash
# Edit ~/.needle/config.yaml
workspace:
  default: /home/coding/claude-governor  # ✓ CORRECT
```

## Previous Investigation (Dependency Blocking Analysis)

**Previous Root Cause:** The 23 non-deferred open beads all have blocking dependencies that are still open, making them unavailable for processing.

## Investigation Findings

### 1. Documentation Error

**File:** `/home/coding/claude-governor/docs/plan/pluck-configuration.md`

The documentation incorrectly states:
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

**Actual code** (`/home/coding/NEEDLE/src/strand/pluck.rs:13`):
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Action Required:** Update documentation to remove `starvation-alert` from the documented exclude labels.

### 2. Bead State Analysis

**Total open beads:** 41
- **Deferred beads:** 18 (excluded by Pluck's default filter)
- **Non-deferred beads:** 23 (these should be candidates)

**Label distribution:**
- `split-child`: 37 beads
- `deferred`: 18 beads
- `umbrella`: 17 beads
- `failure-count:*`: Various counts

### 3. Dependency Blocking

All 23 non-deferred beads have blocking dependencies:

```sql
SELECT COUNT(*) FROM dependencies d
JOIN issues i ON d.issue_id = i.id
WHERE i.status = 'open' 
  AND i.id NOT IN (SELECT issue_id FROM labels WHERE label = 'deferred')
  AND d.type = 'blocks';
-- Result: 23 blocking dependencies
```

**Example dependency chain:**
- `bf-18y8i` (open) blocked by `bf-53tr7` (open)
- `bf-53tr7` (open) blocked by `bf-45tkc` (completed)
- `bf-45tkc` (completed) blocked by `bf-4e424` (?)

### 4. Pluck Behavior

From logs (`~/.needle/logs/needle-claude-code-glm47-india.stderr.log`):

```
INFO needle::strand: strand found candidates strand=pluck candidates=4 excluded=19
INFO needle::strand: strand found candidates strand=pluck candidates=2 excluded=21
INFO needle::strand: strand found candidates strand=pluck candidates=1 excluded=22
INFO needle::strand: strand found candidates strand=pluck candidates=0 excluded=23
```

The decreasing candidate count shows beads were being processed and claimed, eventually reaching 0 when all remaining beads were blocked by open dependencies.

### 5. Bead Store Query Behavior

The `get_ready_candidates` function in bead-forge filters out beads with open blocking dependencies:

```rust
// From ~/bead-forge/src/claim.rs:412+
LEFT JOIN dependencies d ON d.depends_on_id = i.id 
  AND d.type IN ('blocks', 'parent-child', 'conditional-blocks', 'waits-for')
```

The query includes anti-join logic that excludes beads where blocking dependencies are still open.

## Conclusion

**Pluck is working correctly.** The starvation alert was a false alarm caused by:

1. All 23 non-deferred beads having blocking dependencies
2. Those blocking dependencies still being open
3. Bead store correctly filtering out blocked beads via `get_ready_candidates()`

## Recommendations

1. **Fix documentation:** Update `pluck-configuration.md` to match actual code
2. **Review dependency chains:** Determine if these dependency chains are intentional or stale
3. **Consider dependency cleanup:** If dependencies are stale, they should be removed to unblock work

## Files Updated

- `notes/bf-3q9z2.md` - This investigation report

## Next Steps

1. Update `/home/coding/claude-governor/docs/plan/pluck-configuration.md` to fix documentation
2. Commit changes and close bead bf-3q9z2
