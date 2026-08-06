# bf-10l6i — alerts.command state of both governor.yaml files

Bookkeeping only. No test suite run. Findings below are in a form that can be
pasted directly into the bf-1hbmk summary.

## Findings

**`/home/coding/claude-governor/config/governor.yaml`** (repo config)
- Explicit `alerts.command` key present: **no**
- First element: **n/a** — the key is absent, so the value comes from
  `default_alert_command()` in `src/config.rs:569`, whose first element is `bf`
  (full vector `["bf", "create", "--type", "human"]`).
- Needed an edit for the `br` -> `bf` fix: **no — already correct by
  inheritance.** The `alerts:` block (line 162) contains only `enabled`,
  `cooldown_minutes`, `min_severity`, `low_cache_eff_threshold`,
  `low_cache_eff_intervals`, and `auto_bead`. No `br` vector anywhere in the
  file.

**`~/.config/claude-governor/governor.yaml`** (user config)
- Explicit `alerts.command` key present: **no**
- First element: **n/a** — absent, so it inherits the same `bf` default from
  `src/config.rs:569`.
- Needed an edit for the `br` -> `bf` fix: **no — already correct by
  inheritance.** The `alerts:` block (line 135) carries the identical six keys.
  No `br` vector anywhere in the file.

## `br` vector check

Neither file contains a `br ...` vector, so **no edit was made to either file**.
Grep for `command` and for a standalone `br` token returned zero hits in both.

## Why inheritance is safe here

`AlertConfig.command` is `#[serde(default = "default_alert_command")]`
(`src/config.rs:534-535`), and both `default_alert_command()` and
`AlertConfig::default()` return `bf` as element 0. The regression guard
`test_default_alert_command_uses_bf` (`src/config.rs:1310`) asserts
`cmd[0] == "bf"` and pins the full vector, so the deprecated `br` shim cannot
silently reappear via the default path.
