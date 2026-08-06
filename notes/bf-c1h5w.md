# bf-c1h5w — Does an `alerts.command` key exist in the live governor.yaml?

## Answer

**ABSENT**

## Scope

`~/.config/claude-governor/governor.yaml` (143 lines, mtime 2026-05-03), alerts block only.
Line range from bf-31i1t: **lines 135–143**.

## Evidence

Whole-file grep for the substring `command` returns no match at all:

```
$ grep -n 'command' /home/coding/.config/claude-governor/governor.yaml
(no output — exit 1)
```

Full alerts block as read (lines 133–143):

```yaml
# Alert configuration
# Controls alert firing and cooldown behavior
alerts:
  enabled: true                     # Enable/disable alerts
  cooldown_minutes: 60              # Cooldown period between repeated alerts
  min_severity: warning             # Minimum severity level: info, warning, critical
  low_cache_eff_threshold: 0.30     # Fleet cache efficiency below this fraction triggers alert (30%)
  low_cache_eff_intervals: 5        # Number of consecutive intervals below threshold before alerting
  auto_bead: false                  # Disabled: alert predicates have 100% FP rate (docs-878a)
                                    # Alerts are logged to governor.log but do not spawn beads.
                                    # Re-enable only after FP rate < 5% over 100-alert window.
```

The six child keys under `alerts:` are `enabled` (136), `cooldown_minutes` (137),
`min_severity` (138), `low_cache_eff_threshold` (139), `low_cache_eff_intervals` (140),
`auto_bead` (141). Lines 142–143 are trailing comment continuation, not keys.
No `command:` key at any indent level within the block.

## Notes

Read-only bead — the live config was not edited.
