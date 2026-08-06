# bf-4qd6k — Final gate: cargo test + br-to-bf sweep results

Final gate and bookkeeping for the `br create` default-alert-command fix
(umbrella parent: bf-1hbmk). Date: 2026-08-06.

This bead is the *gate*, not the investigation. Sibling bf-3g1ns assembled the
combined narrative onto bf-1hbmk from bf-5ke9f / bf-10l6i / bf-6bicx. Everything
below was re-verified first-hand here rather than inherited, because the whole
point of a final gate is that it does not take the earlier children's word for it.

## VERDICT

**Test suite passes. Zero files needed an edit during this final sweep.** Every
`br create` occurrence in the stated scope had already been cleared by an earlier
commit. The sweep confirmed the fix rather than performing it.

## CARGO TEST GATE — PASS

Run as `~/.cargo/bin/cargo test` with the **absolute path**, deliberately not bare
`cargo`: the wrapper on PATH discards stderr and exits 0 even on failure, so a bare
invocation is not a gate at all. Verified twice — once piped for the log, once with
output discarded to read the real exit status (piping loses it to the last stage).

```
~/.cargo/bin/cargo test  ->  exit code 0
```

| Metric | Result |
|---|---|
| Test targets | 18, all `test result: ok.` |
| Passed | 849 |
| Failed | **0** |
| Ignored | 8 (5 doc-tests, 3 integration) |

- Main lib suite: `750 passed; 0 failed; 0 ignored` (finished in 9.91s)
- 16 integration targets: all `test result: ok.`, 0 failed
- Doc-tests: `17 passed; 0 failed; 5 ignored`
- No `FAILED`, no `panicked`, no `failures:`, no `error[` anywhere in the log.

No failures, so no follow-up bead filed. Pre-existing build warnings are **out of
scope and untouched**: ~20 unused-import/unused-variable warnings in test modules,
1 unused doc comment, and 3 unnecessary-parentheses warnings in
`src/governor.rs:7207/7213/7219`. None are regressions from this fix.

## GREP GATE — 0 HITS

```
grep -rn 'br create' src/ docs/ config/ scripts/        -> 0 hits, exit 1
grep -rnE '\bbr[[:space:]]+create' src/ docs/ config/ scripts/ -> 0 hits, exit 1
```

The whitespace-tolerant variant is run alongside the literal so that `br  create`
or a tab-separated form cannot slip through the literal-string check.

## PER-TARGET: NEEDED AN EDIT vs. ALREADY CORRECT

| # | Target | Needed edit? | Evidence |
|---|---|---|---|
| 1 | `config/governor.yaml` | **No** — correct by inheritance | verified here |
| 2 | `~/.config/claude-governor/governor.yaml` | **No** — correct by inheritance | verified here |
| 3 | `src/` | **No** — already clean | fixed earlier by 8b08f68 |
| 4 | `docs/` | **Yes — 2 files**, by earlier children | 1698c92, f3ba946 |
| 5 | `config/` (dir-wide) | **No** — never had a hit | `git log -S` empty |
| 6 | `scripts/` | **No** — never had a hit | `git log -S` empty |

### 1. `config/governor.yaml` — NO EDIT NEEDED, correct by inheritance

The `alerts:` block at line 162 declares exactly six keys — `enabled`,
`cooldown_minutes`, `min_severity`, `low_cache_eff_threshold`,
`low_cache_eff_intervals`, `auto_bead`. There is **no explicit `command:` key**,
so the value comes from `default_alert_command()`. `grep -n 'br'` over the whole
file returns nothing (exit 1) — not just no `br create`, but no `br` at all.

### 2. `~/.config/claude-governor/governor.yaml` — NO EDIT NEEDED, correct by inheritance

The live config's `alerts:` block at line 135 carries the identical six keys, also
with **no explicit `command:` key**, and inherits the same default. `grep -n 'br'`
likewise returns nothing. Worth stating plainly: this is the file that would
actually be read at runtime, so "the repo config is clean" would not by itself have
closed the question. It was checked directly.

### 3. `src/` — NO EDIT NEEDED, already clean

The single `br create` in code was fixed by **8b08f68** (2026-08-03, the
`src/config.rs` default alert command) — *before* the bf-4qd6k split existed.
**78fe193** then added the regression test.

### 4. `docs/` — EDITED (2 files), by earlier children

- **1698c92** fixed `docs/research/alerts.md:185` (bf-482rd)
- **f3ba946** fixed `docs/plan/plan.md:1692` (bf-5wdrk)

Zero hits remain. These two are the only genuine edits the whole sweep produced.

### 5–6. `config/`, `scripts/` — NO EDIT NEEDED, never had a hit

`git log -S'br create' -- config/` and `-- scripts/` both return no commits;
`scripts/polish-seeder.sh:104` already used `bf create`.

## REGRESSION GUARD

Re-read first-hand at `src/config.rs`:

```rust
fn default_alert_command() -> Vec<String> {          // line 569
    vec!["bf", "create", "--type", "human"]          // (to_string() elided)
}

fn test_default_alert_command_uses_bf() {            // line 1310
    let cmd = default_alert_command();
    assert_eq!(cmd[0], "bf");
    assert_eq!(cmd, vec!["bf", "create", "--type", "human"]);
    assert_eq!(AlertConfig::default().command[0], "bf");
}
```

`AlertConfig.command` is `#[serde(default = "default_alert_command")]`. The test
pins both the first element and the full vector, and separately pins
`AlertConfig::default()`, so the deprecated `br` shim cannot silently reappear
through the default path. Since both YAML files rely on that default rather than
overriding it, this test is what actually protects them.

## OUT-OF-SCOPE `br` REFERENCES — DELIBERATELY NOT EDITED

- `.beads/issues.jsonl` still matches `br create` on 10 lines (bf-1hbmk, bf-1o5in,
  bf-47asm, bf-482rd, bf-4qd6k, bf-5ke9f, bf-5wdrk, bf-6bicx, docs-7d4, docs-ycf).
  Each is inside a bead title/description that *names the bug*. Rewriting them
  would edit the audit trail to erase the record of the thing being fixed.
- Non-`create` `br` subcommands survive as captured shell transcripts in
  `docs/pluck-query-results.md:196,206,227,235` and
  `docs/bf-wvljm-bead-inventory.md:181`. These are historical transcripts, not
  instructions to follow.

Purging every remaining `br` reference is separate work and deserves its own bead;
it is not in this umbrella's stated scope.
