# bf-1o5in — Whole-repo grep for `br create`

**Result: grep was already clean. No files needed editing.**

## Acceptance criteria

Required command, run from the repo root:

```
$ grep -rn 'br create' src/ docs/ config/ scripts/
$ echo $?
1
```

No hits. Exit status 1 (no match), as required.

## Scope covered

| Directory | Files scanned |
|-----------|---------------|
| `src/`    | 20 |
| `docs/`   | 18 |
| `config/` | 4  |
| `scripts/`| 6  |

Additional checks beyond the literal grep, all clean:

- Whitespace-tolerant variant `grep -rnE '\bbr\s+create'` over the same four
  directories — no hits.
- Untracked files (`git ls-files --others --exclude-standard`) — no hits.

## Why it was already clean

The two sibling beads landed the only in-scope occurrences before this sweep
ran, and the code default was fixed alongside them:

| Commit | File | Change |
|--------|------|--------|
| `1698c92` | `docs/research/alerts.md:185` | `br create --type human` → `bf create --type human` (bf-482rd) |
| `f3ba946` | `docs/plan/plan.md:1692` | default documented as `` `bf create --type human "..."` `` (bf-5wdrk) |
| `78fe193` | `src/config.rs` | test locking `default_alert_command()[0] == "bf"` |

The corrected `bf create` form is now present in 10 places across
`src/config.rs`, `src/alerts.rs`, `src/governor.rs`, `scripts/polish-seeder.sh`,
and four docs files.

## Out-of-scope observations (not edited)

A repo-wide `git grep 'br create'` does still return hits, all of them outside
this bead's scope. Recording them here so the next sweep does not re-flag them:

- **`.beads/issues.jsonl`** — 8 hits, every one inside a bead *description*
  (including this bead's own description and its siblings bf-47asm, bf-1hbmk,
  bf-4qd6k, bf-482rd, bf-5wdrk, plus closed beads docs-7d4 and docs-ycf). These
  are historical issue records, not live instructions. Rewriting them would
  edit the audit trail rather than fix anything, and `.beads/` is not in the
  stated scope of `src/`, `docs/`, `config/`, `scripts/`.

Separately, a few non-`create` `br` subcommands survive in docs as captured
shell transcripts. Left alone deliberately — they record commands that were
actually run at the time, so changing them would falsify the record rather than
prevent a future `br` invocation:

- `docs/pluck-query-results.md:196,206,227,235` — `$ br ready ...` transcript output
- `docs/bf-wvljm-bead-inventory.md:181` — describes sampling method as `br list --json`

If the intent is to purge every `br` reference and not just `br create`, that is
a distinct piece of work and warrants its own bead.

## Not run

`cargo test` was not run — no code changed, and the test gate for this sweep is
tracked separately by bf-4qd6k.
