# Pluck Exclude Labels Configuration Investigation

**Task ID:** bf-6b65h  
**Date:** 2026-07-06  
**Workspace:** `/home/coding/claude-governor`

## Summary

Pluck's `exclude_labels` configuration is **hardcoded in the NEEDLE binary** and currently uses the default values. No custom override is configured in this workspace.

## Exact Configuration

### Source Code Location
```rust
// File: /home/coding/NEEDLE/src/strand/pluck.rs:13
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

### Default Exclude Labels

| Label | Purpose | Example Found |
|-------|---------|---------------|
| `deferred` | Beads marked for later processing | ✅ Yes (bf-21swe has `deferred` label) |
| `human` | Beads requiring human intervention | ✅ Yes (docs-7d4 has `human` label) |
| `blocked` | Beads with blocking dependencies | ✅ Yes (bf-3jo4t has `deferred, starvation-alert, umbrella`) |
| `starvation-alert` | Beads created by alerting system | ✅ Yes (bf-3jo4t has `deferred, starvation-alert, umbrella`) |

### Implementation Behavior

From the source code (`/home/coding/NEEDLE/src/strand/pluck.rs`):

1. **Default applies when empty:** When `PluckStrand::new(vec![])` is called with an empty exclude_labels vector, defaults are applied automatically (lines 28-33).

2. **Custom overrides completely:** If custom exclude_labels are provided (non-empty), they replace the defaults entirely (lines 34-35) — there is no merging.

3. **No custom configuration:** The current deployment uses `PluckStrand::new(vec![])` or equivalent, meaning all four default labels are active.

## Status Labels vs. Exclude Labels

**Important:** The "closed" or "open" status shown in `br list` output is **NOT a label** — it's the bead's status/state field. The exclude_labels only filter based on the `labels:` field.

Example from `br show bf-21swe`:
```
Status: open
Labels: deferred, split-child, umbrella
```

This bead is **open** but has the `deferred` label, so Pluck will **exclude** it from candidate selection.

## Filtering Mechanism

Pluck applies filtering at two levels:

1. **Store-level filter** — Passed to `bead_store::Filters` in the query
2. **Strand-level defensive filter** — Double-checks labels after retrieval (line 125 in pluck.rs)

This defensive filtering ensures that even if the bead store doesn't include label data, excluded beads won't be selected.

## Key Findings

✅ **The exclude_labels are working correctly** — beads with `deferred`, `human`, `blocked`, or `starvation-alert` labels are properly excluded from Pluck selection.

✅ **"closed" status is not being filtered** — closed beads are filtered by status field logic, not by exclude_labels.

✅ **No custom override** — The default configuration is active; no workspace-specific overrides are in place.

## Related Documentation

- Comprehensive Pluck configuration: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`
- NEEDLE source: `/home/coding/NEEDLE/src/strand/pluck.rs`
- NEEDLE config: `~/.needle/config.yaml`
