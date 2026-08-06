# bf-2bmvq — Verify alerts.command in live governor.yaml

**Result: already-correct — no edit required.**

## Scope

`~/.config/claude-governor/governor.yaml` only (alerts block, ~line 135).

## Findings

The `alerts:` block in the live user config (lines 135-143) contains:

```yaml
alerts:
  enabled: true
  cooldown_minutes: 60
  min_severity: warning
  low_cache_eff_threshold: 0.30
  low_cache_eff_intervals: 5
  auto_bead: false
```

No `command:` key is present. A repo-wide check of the file confirms this:

```
$ grep -n "command" ~/.config/claude-governor/governor.yaml
(no matches, exit 1)
```

Therefore the file cannot carry a stale `br` alert command. The field is
populated at deserialize time by `#[serde(default = "default_alert_command")]`
(`src/config.rs:534`), which returns `["bf", "create", "--type", "human"]`
(`src/config.rs:569`). That default is pinned by the regression test
`test_default_alert_command_uses_bf` (`src/config.rs:1310`).

Additionally, `auto_bead: false` in the live config means the alert command is
not executed at all in the current deployment — alerts are logged to
`governor.log` only.

## Close note

Already-correct: no `alerts.command` key exists in the live config; the file
inherits the `src/config.rs` `bf` default and needs no edit.
