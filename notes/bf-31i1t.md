# bf-31i1t — Locate the `alerts:` block in live `~/.config/claude-governor/governor.yaml`

Read-only inspection. No config was modified.

## File

`/home/coding/.config/claude-governor/governor.yaml` — 143 lines total, mtime 2026-05-03 23:56.

## Grep

```
$ grep -n '^alerts:' ~/.config/claude-governor/governor.yaml
135:alerts:
```

Exactly one match; the mapping is top-level (column 0).

## Line range

The `alerts:` block spans **lines 135–143**, and it is the last block in the file.

- 135 — `alerts:` (mapping key)
- 136–141 — child keys
- 142–143 — continuation comment lines belonging to `auto_bead` (no keys)

Lines 133–134 are the leading comment header (`# Alert configuration` / `# Controls alert firing and cooldown behavior`), immediately above the block but not part of the mapping.

## Immediate child keys

All children are at indent level 2 and are scalars — the block has no nested sub-mappings.

| Line | Key | Value |
| --- | --- | --- |
| 136 | `enabled` | `true` |
| 137 | `cooldown_minutes` | `60` |
| 138 | `min_severity` | `warning` |
| 139 | `low_cache_eff_threshold` | `0.30` |
| 140 | `low_cache_eff_intervals` | `5` |
| 141 | `auto_bead` | `false` |

`auto_bead` is disabled with an inline rationale (alert predicates at 100% FP rate, ref `docs-878a`); the trailing comments on 142–143 note that alerts are logged to `governor.log` without spawning beads, and that re-enabling should wait until the FP rate is under 5% over a 100-alert window.
