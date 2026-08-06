# bf-3g1ns — Combined `br` → `bf` sweep summary for bf-1hbmk

Final step of the split of bf-4qd6k. Assembles the close notes of bf-5ke9f
(cargo test), bf-10l6i (the two governor.yaml files), and bf-6bicx (repo-wide
grep) into one summary, which is attached to the umbrella parent bf-1hbmk as a
comment (`bf show bf-1hbmk` renders comments).

## Verdict

**No file in the stated scope needed an edit during this final sweep.** The two
docs hits were already cleared by bf-482rd and bf-5wdrk, and the one code hit by
`8b08f68`. The test suite is green.

## Per-target breakdown

| Target | Needed an edit? | Detail | Source |
|--------|-----------------|--------|--------|
| `config/governor.yaml` | **No — already correct by inheritance** | No explicit `alerts.command` key. The `alerts:` block (line 162) holds only `enabled`, `cooldown_minutes`, `min_severity`, `low_cache_eff_threshold`, `low_cache_eff_intervals`, `auto_bead`. Value comes from `default_alert_command()` (`src/config.rs:569`) = `["bf", "create", "--type", "human"]`. No `br` vector anywhere in the file. | bf-10l6i |
| `~/.config/claude-governor/governor.yaml` | **No — already correct by inheritance** | No explicit `alerts.command` key. `alerts:` block (line 135) carries the identical six keys, inherits the same `bf` default. No `br` vector anywhere in the file. | bf-10l6i |
| `src/` | **No — already clean** | The single `br create` in code was fixed earlier by `8b08f68` (2026-08-03, `src/config.rs` default alert command), before the bf-4qd6k split existed. `78fe193` then added a test locking `default_alert_command()[0] == "bf"`. | bf-6bicx |
| `docs/` | **Yes — 2 files edited (by earlier children)** | `1698c92` fixed `docs/research/alerts.md:185` (bf-482rd); `f3ba946` fixed `docs/plan/plan.md:1692` (bf-5wdrk). Zero hits remain. | bf-6bicx |
| `config/` | **No — never had a hit** | `git log -S'br create' -- config/` returns no commits; the string has never appeared in the directory. | bf-6bicx |
| `scripts/` | **No — never had a hit** | `git log -S'br create' -- scripts/` returns no commits. `scripts/polish-seeder.sh:104` already used `bf create`. | bf-6bicx |

## Grep gate

`grep -rn 'br create' src/ docs/ config/ scripts/` → **0 hits**, exit status 1
(no match), which is the expected result. The whitespace-tolerant variant
`grep -rnE '\bbr[[:space:]]+create' …` is also 0 hits, and there are no
untracked files hiding a hit.

## Cargo test gate

`~/.cargo/bin/cargo test` (absolute path — the `cargo` wrapper on PATH discards
stderr and exits 0 even on failure) → **PASS**.

- All 18 test targets green: **849 passed, 0 failed, 8 ignored**.
- Main lib suite: `test result: ok. 750 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.28s`
- 16 integration targets: all `test result: ok.` with 0 failed.
- Doc-tests: `test result: ok. 17 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 0.59s`
- No failures, so no follow-up bead was filed. Build emits pre-existing warnings
  (unused imports/vars in test modules; 3 unnecessary-parentheses warnings in
  `src/governor.rs` ~7213/7219) — out of scope.

## Known out-of-scope `br` references (deliberately not edited)

- `.beads/issues.jsonl` — 10 lines matching `br create`, every one inside a bead
  title or description (bf-1hbmk, bf-1o5in, bf-47asm, bf-482rd, bf-4qd6k,
  bf-5ke9f, bf-5wdrk, bf-6bicx, docs-7d4, docs-ycf). These are historical issue
  records naming the bug, not live instructions; `.beads/` is outside the stated
  scope and rewriting them would edit the audit trail.
- Non-`create` `br` subcommands in captured shell transcripts:
  `docs/pluck-query-results.md:196,206,227,235` and
  `docs/bf-wvljm-bead-inventory.md:181`. They record commands actually run at the
  time. Purging every `br` reference is separate work and warrants its own bead.

## Regression guard

`AlertConfig.command` is `#[serde(default = "default_alert_command")]`
(`src/config.rs:534-535`); both `default_alert_command()` and
`AlertConfig::default()` return `bf` as element 0. The test
`test_default_alert_command_uses_bf` (`src/config.rs:1310`) asserts
`cmd[0] == "bf"` and pins the full vector, so the deprecated `br` shim cannot
silently reappear via the default path.

## Sources

- bf-5ke9f — cargo test run (note preserved in commit `7ff46f5`)
- bf-10l6i — governor.yaml findings (note preserved in commit `7754f3f`; the file
  itself was removed by the `8015dfa` notes cleanup)
- bf-6bicx — repo-wide grep breakdown (`notes/bf-6bicx.md`, commit `904ec29`)
