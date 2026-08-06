# bf-1hbmk — Final sweep: alerts.command contains no 'br'

**Date:** 2026-08-06
**Verdict:** No file in scope needed an edit. All acceptance criteria verified independently, not assumed.

## Which files needed edits vs. were already correct

| Target | Needed an edit? | Why |
|---|---|---|
| `config/governor.yaml` | **No — already correct by inheritance** | No explicit `alerts.command` key |
| `~/.config/claude-governor/governor.yaml` | **No — already correct by inheritance** | No explicit `alerts.command` key |
| `src/` | **No — already clean** | Sole code hit fixed earlier by `8b08f68` |
| `docs/` | **Edited, but by earlier children** | `1698c92` (bf-482rd), `f3ba946` (bf-5wdrk) |
| `config/` | **No — never had a hit** | `git log -S'br create' -- config/` → no commits |
| `scripts/` | **No — never had a hit** | `scripts/polish-seeder.sh:104` already used `bf create` |

## Criterion 1 — alerts.command in both configs

Neither config declares an explicit `alerts.command` key, so both inherit the `bf` default.

- `config/governor.yaml` — `alerts:` block at line 162 holds exactly six keys:
  `enabled`, `cooldown_minutes`, `min_severity`, `low_cache_eff_threshold`,
  `low_cache_eff_intervals`, `auto_bead`. No `command`.
- `~/.config/claude-governor/governor.yaml` — `alerts:` block at line 135 carries the
  identical six keys. No `command`.
- `grep -nE '\bbr\b'` over both files → 0 hits (exit 1). No stale `br` vector to correct.

Inherited value comes from `src/config.rs:569`:

```rust
fn default_alert_command() -> Vec<String> {
    vec!["bf", "create", "--type", "human"]  // (String-ified)
}
```

wired in via `#[serde(default = "default_alert_command")]` on `AlertConfig.command`
(`src/config.rs:534-535`) and also returned by `AlertConfig::default()` (`src/config.rs:605`).

## Criterion 2 — grep gate

```
grep -rn 'br create' src/ docs/ config/ scripts/     → 0 hits, exit 1
grep -rnE '\bbr[[:space:]]+create' src/ docs/ config/ scripts/  → 0 hits, exit 1
```

The whitespace-tolerant variant was run as well so a `br  create` with irregular spacing
could not slip past the literal grep. Children bf-482rd and bf-5wdrk cleared the docs hits.

## Criterion 3 — cargo test

Run as `~/.cargo/bin/cargo test` (absolute path — the `cargo` wrapper on PATH discards
stderr and exits 0 even on failure, so it cannot be trusted as a gate).

**PASS.** 18 test targets, all `test result: ok.` with `0 failed`:

- lib suite: 750 passed, 0 failed
- 16 integration targets: 0 failed
- doc-tests: 17 passed, 0 failed, 5 ignored

Totals: **849 passed, 0 failed, 8 ignored.** Failure-pattern scan
(`FAILED|panicked|N failed`) → 0 matches. No follow-up bead filed.

Pre-existing build warnings (unused imports/vars in test modules; 3
unnecessary-parentheses warnings at `src/governor.rs:7213`/`7219`) are out of scope.

## Regression guard

`test_default_alert_command_uses_bf` (`src/config.rs:1310`) asserts `cmd[0] == "bf"` and
pins the full vector `["bf", "create", "--type", "human"]`. Since both the serde default
and `AlertConfig::default()` route through `default_alert_command()`, the deprecated `br`
shim cannot silently reappear through the default path.

## Out-of-scope `br` references, deliberately not edited

- `.beads/issues.jsonl` matches `br create` on 10 lines, each inside a bead title or
  description (bf-1hbmk, bf-1o5in, bf-47asm, bf-482rd, bf-4qd6k, bf-5ke9f, bf-5wdrk,
  bf-6bicx, docs-7d4, docs-ycf). These are historical records naming the bug; rewriting
  them would edit the audit trail.
- Non-`create` `br` subcommands survive as captured shell transcripts in
  `docs/pluck-query-results.md:196,206,227,235` and `docs/bf-wvljm-bead-inventory.md:181`.
  Purging every `br` reference is separate work deserving its own bead.

Assembled from children bf-5ke9f, bf-10l6i, bf-6bicx (see `notes/bf-3g1ns.md`), with every
gate re-run first-hand for this close.
