# Bead visibility quick reference

This page is the short operational companion to
[`docs/bead-visibility-troubleshooting.md`](bead-visibility-troubleshooting.md).
The current workspace uses NEEDLE Pluck with the `bead-rs` backend.

## Current configuration

```yaml
workspace:
  default: /home/coding/claude-governor
  home: /home/coding/.needle        # NEEDLE state, not a bead store

strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
      - starvation-alert
```

`exclude_labels` entries are exact, case-sensitive strings. They do not support
globs, `%`, regular expressions, or prefix matching. An omitted or empty list
uses the built-in fallback `deferred`, `human`, `blocked`; a non-empty list
replaces that fallback, so repeat the defaults when adding a custom label.

## Workspace rule

Always launch workers with an absolute path:

```bash
needle run --agent AGENT --workspace /home/coding/claude-governor
```

Without `--workspace`, NEEDLE uses `workspace.default`. Pluck opens only
`<resolved-workspace>/.beads`; it does not search sibling or parent stores.

## Current ready query

```bash
cd /home/coding/claude-governor
bead list --ready --json --limit 999999
```

The ready frontier requires an open, unassigned, manually unblocked bead with
no unfinished `blocks` dependency. Pluck then removes exact excluded labels
and stale assigned/`in_progress` records. `bead --json` output is JSONL, so
count it with:

```bash
bead list --ready --json --limit 999999 | jq -s 'length'
bead list --status open --json --limit 999999 | jq -s 'length'
```

There is no positive label requirement: `polish` or `documentation` does not
make a bead ready.

## Five-minute starvation check

```bash
WORKSPACE=/home/coding/claude-governor
needle doctor --workspace "$WORKSPACE"
needle config --dump --show-source
(cd "$WORKSPACE" && bead list --status open --json --limit 999999 | jq -s 'length')
(cd "$WORKSPACE" && bead list --ready --json --limit 999999 | jq -s 'length')
(cd "$WORKSPACE" && bead list --status open --json --limit 999999 |
  jq -r '[.id, (.assignee // "<unassigned>"), (.labels | join(",")), .title] | @tsv')
needle logs --since 2h --filter 'event_type~strand\.pluck\.starvation_detected' --format json
```

If open > 0 and ready = 0, inspect `bead show ID --json` for an assignee,
`manual_blocked`, unfinished `blocks` dependencies, or an exact excluded
label. If ready > 0 but no worker progresses, inspect claim and dispatch
telemetry instead of changing labels.

## Avoid these traps

| Trap | Use instead |
| --- | --- |
| A legacy bead-forge command or SQL | `bead list --ready --json` |
| `exclude_labels: []` to disable filtering | Configure the complete intended label list |
| `deferred*`, `deferred%`, or `failure-count:*` | The exact label value |
| Relative `--workspace .` in a service | An absolute workspace path |
| Treating open count as claimable count | Compare open and ready frontiers |
| Assuming Explore searches recursively | Use direct-child auto-discovery or a pinned path list |
| Editing config without restarting workers | Restart and verify the startup configuration |

For the full repair procedure and evidence checklist, see the troubleshooting
guide linked above.
