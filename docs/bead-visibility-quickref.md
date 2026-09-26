# Bead visibility quick reference

This page is the short operational companion to
[`docs/bead-visibility-troubleshooting.md`](bead-visibility-troubleshooting.md).
The current workspace uses NEEDLE Pluck with the `bead-rs` backend.

## Current configuration

Verified 2026-09-25 against `/home/coding/.config/needle/config.yaml` and
needle 0.6.14:

```yaml
workspace:
  default: /home/coding/claude-governor
  home: /home/coding/.needle        # NEEDLE state, not a bead store

strands:
  pluck:
    exclude_labels: []              # empty -> built-in default set applies
    split_after_failures: 3
    persistent_starvation_records: true
```

`exclude_labels` entries are exact, case-sensitive strings. They do not support
globs, `%`, regular expressions, or prefix matching. An omitted or empty list
makes PluckStrand substitute NEEDLE's built-in default set — `deferred`,
`human`, `blocked`, `escalation`, `alert` as of needle 0.6.14; a non-empty
list replaces that default set, so repeat the defaults when adding a custom
label. This deployment's effective exclusion set is therefore the five
defaults, not any explicitly configured list.

## Authoritative verification

Documentation snapshots of live configuration drift. When any claim on this
page disagrees with what you observe, do not reconcile from memory — run the
one authoritative check:

```bash
scripts/verify-pluck-config.sh
```

It verifies the backend binding, the active config path, the resolved default
workspace, the bead-rs store layout, the CLI output contract, and the live
`strands.pluck` values, and exits non-zero on any mismatch. The rest of this
page summarizes what it reports.

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

There is no positive label requirement: labels such as `documentation` or `rust` do not
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
