# bf-a0l7t — Verify alerts.command absent or 'bf' in config/governor.yaml

**Result: already-correct — no edit required.**

## What was inspected

`config/governor.yaml`, the `alerts:` block at lines 162-170:

```yaml
alerts:
  enabled: true
  cooldown_minutes: 60
  min_severity: warning
  low_cache_eff_threshold: 0.30
  low_cache_eff_intervals: 5
  auto_bead: false
```

## Findings

- There is **no `command:` key** in the `alerts:` block. The field is therefore
  supplied by `#[serde(default = "default_alert_command")]` on
  `AlertConfig::command` (`src/config.rs:534`).
- `default_alert_command()` (`src/config.rs:569`) returns
  `["bf", "create", "--type", "human"]` — first element is `bf`, not the stale `br`.
- A grep for `br` across the whole of `config/governor.yaml` returned no matches,
  so no stale alert command is carried anywhere else in the file.
- Regression coverage already exists: `test_default_alert_command_uses_bf`
  (`src/config.rs:1310`) asserts `AlertConfig::default().command[0] == "bf"`.

## Conclusion

The repo-tracked config does not carry a stale `br` alert command. It inherits the
`src/config.rs` default, which is correct. No changes made to `config/governor.yaml`.
