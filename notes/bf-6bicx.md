# bf-6bicx — Repo-wide `br create` grep sweep result

Third step of the split of bf-4qd6k. Bookkeeping only; no source or docs
changes were required.

## Required command

Run from `/home/coding/claude-governor`:

```
$ grep -rn 'br create' src/ docs/ config/ scripts/
$ echo $?
1
```

**Hit count: 0.** Exit status 1 (no match), which is the expected result —
children bf-482rd and bf-5wdrk cleared the docs before this sweep ran.

Supporting checks, both also clean:

- Whitespace-tolerant variant `grep -rnE '\bbr[[:space:]]+create' src/ docs/ config/ scripts/` — 0 hits.
- Untracked files (`git ls-files --others --exclude-standard`) — none present.

## Per-directory breakdown (for pasting into the bf-1hbmk summary)

| Directory | Hits now | Needed edits during the sweep? | Detail |
|-----------|----------|--------------------------------|--------|
| `src/`     | 0 | **No — already clean** | The one `br create` in code was fixed earlier by `8b08f68` (2026-08-03, `src/config.rs` default alert command), before the bf-4qd6k split was created. `78fe193` later added a test locking `default_alert_command()[0] == "bf"`. |
| `docs/`    | 0 | **Yes — 2 files edited** | `1698c92` fixed `docs/research/alerts.md:185` (bf-482rd); `f3ba946` fixed `docs/plan/plan.md:1692` (bf-5wdrk). |
| `config/`  | 0 | **No — never had a hit** | `git log -S'br create' -- config/` returns no commits; the directory has never contained the string. |
| `scripts/` | 0 | **No — never had a hit** | `git log -S'br create' -- scripts/` returns no commits. `scripts/polish-seeder.sh:104` already used `bf create`. |

The corrected `bf create` form now appears in 10 places across `src/config.rs`,
`src/alerts.rs`, `src/governor.rs`, `scripts/polish-seeder.sh`, and four docs
files.

## Out-of-scope hits (not edited)

A repo-wide `git grep 'br create'` still matches **10 lines in
`.beads/issues.jsonl`** — every one inside a bead *title* or *description*:
bf-1hbmk, bf-1o5in, bf-47asm, bf-482rd, bf-4qd6k, bf-5ke9f, bf-5wdrk, bf-6bicx,
docs-7d4, docs-ycf. These are historical issue records naming the bug being
fixed, not live instructions. `.beads/` is outside the stated scope of `src/`,
`docs/`, `config/`, `scripts/`, and rewriting them would edit the audit trail
rather than fix anything.

(bf-1o5in recorded 8 such hits; the count rose to 10 because the bf-4qd6k split
added more beads whose descriptions quote the string.)

Non-`create` `br` subcommands also survive in docs as captured shell
transcripts — `docs/pluck-query-results.md:196,206,227,235` and
`docs/bf-wvljm-bead-inventory.md:181`. Left alone deliberately: they record
commands actually run at the time. Purging every `br` reference is distinct work
and warrants its own bead.

## Relationship to bf-1o5in

bf-1o5in ran the same sweep and also found zero hits. This bead adds the
per-directory edited/already-clean attribution that bf-1o5in did not break out.
No new discrepancy between the two runs.

## Not run

`cargo test` — no code changed here. The test gate for this sweep is recorded
separately by bf-5ke9f.
