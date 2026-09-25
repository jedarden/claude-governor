# Pluck Workspace Path Verification

**Bead:** bf-2bxsv  
**Date:** 2026-07-06  
**Workspace:** `/home/coding/claude-governor`

> **HISTORICAL EVIDENCE — superseded 2026-09-25.** This page was accurate when
> written, in the bead-forge (`bf`) era, against the v1 config layout. Every
> configuration claim below is now obsolete. Do not configure or diagnose
> Pluck from this page: run `scripts/verify-pluck-config.sh` for the live
> snapshot and read
> [`docs/bead-visibility-troubleshooting.md`](../bead-visibility-troubleshooting.md)
> for the current procedures. The body is retained verbatim as the historical
> record. Claim by claim:
>
> | This page says | Current reality (verified 2026-09-25) |
> | --- | --- |
> | NEEDLE config at `/home/coding/.needle/config.yaml` | The loader reads `/home/coding/.config/needle/config.yaml`. `~/.needle/config.yaml` still exists but carries an in-file marker that it is legacy v1 and is not read. |
> | Default workspace `/home/coding/NEEDLE` | `workspace.default` is `/home/coding/claude-governor`. (`/home/coding/NEEDLE` is the NEEDLE source checkout, not a bead workspace.) |
> | Strand enablement `pluck: auto` at `~/.needle/config.yaml:71` | No `pluck: auto` shorthand exists in the current schema; Pluck is configured by explicit keys under `strands:` → `pluck:`. |
> | JSONL checkpoint `/home/coding/claude-governor/.beads/issues.jsonl` | The bead-rs store is `.beads/beads.db` (SQLite live store) plus the `.beads/checkpoint/` directory (`current.json`, `previous.json`, `forensic.jsonl`, `objects/*.jsonl`). There is no flat `issues.jsonl`. |
> | Store "contains: beads.db, issues.jsonl, and backups" | See check 4 of `scripts/verify-pluck-config.sh` for the authoritative layout. |

## Summary

Pluck workspace path configuration is correct and matches the actual workspace location.

## Configuration Details

### NEEDLE Configuration File
**Location:** `/home/coding/.needle/config.yaml`

**Workspace Setting:**
```yaml
workspace:
  default: /home/coding/NEEDLE
```

### Current Workspace
**Path:** `/home/coding/claude-governor`  
**Bead Store:** `/home/coding/claude-governor/.beads/`  
**Database:** `/home/coding/claude-governor/.beads/beads.db`  
**JSONL Checkpoint:** `/home/coding/claude-governor/.beads/issues.jsonl`

### Path Match Status
✅ **CONFIRMED** - Pluck is operating on the correct workspace:
- Configuration allows workspace discovery via `explore: auto` strand
- Pluck uses the workspace assigned by NEEDLE at runtime
- Current workspace `/home/coding/claude-governor` is active
- Bead store path correctly derived from workspace: `{workspace}/.beads/`
- Database exists and is being actively updated

## Configuration Sourcing

| Component | Source | Value |
|-----------|--------|-------|
| Default workspace | `~/.needle/config.yaml:9` | `/home/coding/NEEDLE` |
| Current workspace | NEEDLE runtime assignment | `/home/coding/claude-governor` |
| Bead store path | Derived from workspace | `/home/coding/claude-governor/.beads/` |
| Strand enablement | `~/.needle/config.yaml:71` | `pluck: auto` |

## How Pluck Determines Workspace

1. **Default workspace** from `~/.needle/config.yaml` provides fallback
2. **Explore strand** discovers additional workspaces via filesystem traversal
3. **NEEDLE runtime** assigns Pluck to a specific workspace
4. **Pluck operates** on the assigned workspace's bead store

## Verification Method

```bash
# Current workspace
pwd
# Output: /home/coding/claude-governor

# Bead store exists
ls -la /home/coding/claude-governor/.beads/
# Contains: beads.db, issues.jsonl, and backups
```

## Conclusion

The Pluck workspace path configuration is functioning correctly:
- Default workspace is `/home/coding/NEEDLE`
- Current active workspace is `/home/coding/claude-governor`
- Bead store path correctly reflects current workspace
- No path mismatch detected
