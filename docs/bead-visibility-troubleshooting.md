# Bead visibility and starvation troubleshooting

This is the operational guide for NEEDLE Pluck in this repository. It assumes
the workspace is bound to the `bead-rs` backend in [`.needle.yaml`](../.needle.yaml).
For the complete filter inventory, see
[`docs/plan/pluck-configuration.md`](plan/pluck-configuration.md). The older
bead-forge pages in this repository are historical investigation notes; do not
use their SQL or commands to infer the current ready predicate.

## The visibility contract

Bead visibility is a pipeline, not one filter:

```text
resolved workspace
    -> <workspace>/.beads store
    -> bead list --ready
    -> exact exclude_labels match
    -> Pluck defensive status/assignee checks
    -> transient worker-local exclusions
    -> ordering and claim
```

The current target workspace is `/home/coding/claude-governor`. The active
global Pluck configuration is `/home/coding/.config/needle/config.yaml`:

```yaml
workspace:
  default: /home/coding/claude-governor
  home: /home/coding/.needle

strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
      - starvation-alert
```

`workspace.default` selects the bead workspace. `workspace.home` is NEEDLE's
state directory for logs, heartbeats, and optional diagnostics; it is not a
bead store.

## Correct `exclude_labels` patterns

`exclude_labels` is a YAML sequence of exact label strings:

```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
      - starvation-alert
```

A bead is excluded when any one of its labels exactly matches an entry. The
matching is case-sensitive and uses string membership. These are not glob,
prefix, regular-expression, or case-insensitive patterns:

```yaml
# These do not exclude every label beginning with the prefix.
- deferred*
- deferred%
- "failure-count:*"
```

Those values would only match a literal label containing `*` or `%` (and the
literal string `failure-count:*`). To exclude a label, write the complete
label, including punctuation and case. A generated label such as
`failure-count:3` is not covered by `failure-count:*`; it is handled by Pluck's
failure-count ordering and split logic instead.

The built-in fallback is only:

```text
deferred, human, blocked
```

An omitted or empty `exclude_labels` list uses that fallback; it does not mean
"exclude nothing." A non-empty configured list replaces the fallback rather
than merging with it. Therefore, a custom list must repeat the three built-in
labels when they should remain excluded:

```yaml
# Correct: retain the defaults and add a deployment-specific label.
exclude_labels:
  - deferred
  - human
  - blocked
  - starvation-alert
```

Labels such as `polish`, `documentation`, and `failure-count:3` are not
excluded unless their exact values are added. Pluck has no configured required
label list, so adding a positive label does not make a bead visible or ready.

## Workspace path best practices

Use an absolute repository path in every worker launch command:

```yaml
agents:
  worker:
    launch_cmd: "needle run --agent <agent> --workspace /home/coding/claude-governor"
```

The resolution rule is:

```text
needle run --workspace PATH  -> PATH
no --workspace               -> workspace.default
```

Pluck opens only `<resolved workspace>/.beads`; it does not search upward,
search sibling repositories, or substitute another store when the path is
wrong. A relative path such as `.` is therefore unsafe for a service or a
launcher whose current directory is not known.

Explore is separate from Pluck:

- `strands.explore.workspaces: []` enables auto-discovery of direct children
  under `strands.explore.workspace_root` that contain `.beads`.
- A non-empty `workspaces` list is a pin list and restricts Explore to those
  paths; new repositories must be added explicitly.
- Explore skips Pluck's resolved home workspace because Pluck already checked
  it.
- `strands.weave.exclude_workspaces` and old `explore-excluded` files do not
  configure the current Pluck home path.

When changing the global config, start a new worker or restart the service.
Workers load their resolved configuration at startup. For cgov-managed worker
commands, restart cgov after changing the governor configuration as well.

## Filter syntax and common mistakes

The current read-only query is:

```bash
cd /home/coding/claude-governor
bead list --ready --json --limit 999999
```

With `--json`, bead-rs emits compact JSONL: one JSON object per line, not one
JSON array. Use `jq -s` when an array is useful:

```bash
bead list --ready --json --limit 999999 | jq -s 'length'
bead list --ready --json --limit 999999 | jq -r '.id'
bead list --status open --json --limit 999999 | jq -s 'length'
```

`--limit` is a maximum record count, not a page number. The large value above
is the value Pluck uses; it is an implementation ceiling, not a visibility
override.

The `--ready` frontier requires all of the following before Pluck sees a bead:

- base status `open`;
- no assignee;
- no manual block;
- no unfinished dependency of kind `blocks`.

`relates_to` dependencies do not block readiness. A `blocked` label and a
manual block are different mechanisms, but both can make a bead unavailable.
Pluck then applies `exclude_labels` and defensive checks for stale assigned or
`in_progress` records.

Common mistakes:

| Mistake | Result | Correct check |
| --- | --- | --- |
| Counting open beads as ready beads | Assigned, manually blocked, and dependency-blocked beads are included in the count | Compare `--status open` with `--ready` |
| Using a legacy bead-forge command or SQL as current evidence | The command may use a different backend or schema | Use `bead list --ready --json` |
| Supplying `exclude_labels: []` to disable filtering | The built-in three-label fallback remains active | Configure the complete intended list; do not rely on an empty list as an escape hatch |
| Writing `deferred*` or `failure-count:*` | No wildcard matching occurs | Use the exact label value |
| Adding `polish` to a bead and expecting it to become ready | Pluck does not require positive labels | Check status, assignee, block, dependencies, and exclusions |
| Running from a parent or sibling directory | A different `.beads` store is queried | Use an absolute `--workspace` and verify the resolved store |
| Looking for an array in JSON output | JSONL has no top-level array | Pipe through `jq -s` |
| Editing config while workers keep running | Existing workers continue using their startup config | Restart workers and verify the resolved configuration |

## Starvation response procedure

Treat starvation as an evidence-gathering problem. Do not immediately remove
labels or delete dependencies; those changes can make intentionally deferred
or blocked work claimable.

### 1. Confirm the worker and configuration

```bash
needle status
needle doctor --workspace /home/coding/claude-governor
needle config --dump --show-source
sed -n '/^workspace:/,/^strands:/p' /home/coding/.config/needle/config.yaml
sed -n '/^strands:/,/^telemetry:/p' /home/coding/.config/needle/config.yaml
```

Confirm that the worker has the intended absolute workspace and that the
effective Pluck list contains the expected exact labels. If configuration is
invalid, fix the source file and restart the worker before investigating bead
data.

### 2. Compare open and ready frontiers

```bash
WORKSPACE=/home/coding/claude-governor
cd "$WORKSPACE"

bead list --status open --json --limit 999999 | jq -s 'length'
bead list --ready --json --limit 999999 | jq -s 'length'
bead list --ready --json --limit 999999 | jq -r '[.id, .priority, (.labels | join(",")), .title] | @tsv'
```

Interpret the result before changing anything:

- Open `0`, ready `0`: there is no backlog in this store.
- Open `N`, ready `N` (or close): the ready frontier is working; investigate
  worker dispatch, claim races, or worker-local retry exclusions.
- Open `N`, ready `0`: inspect the causes below. This is the useful definition
  of a candidate starvation condition.

### 3. Identify why open beads are not ready

List labels on the open set without assuming that a label explains every
missing bead:

```bash
bead list --status open --json --limit 999999 |
  jq -r '[.id, (.assignee // "<unassigned>"), (.status // "<unknown>"), (.labels | join(",")), .title] | @tsv'
```

For a suspicious ID, inspect its complete bead-rs record:

```bash
bead show BEAD_ID --json | jq '.[0]'
```

Check, in order:

1. Is the bead assigned to a live or stale worker?
2. Is `manual_blocked` true?
3. Does `blocked_by` contain an unfinished blocker of kind `blocks`?
4. Does `labels` contain one of the exact active exclusion labels?
5. Is the command querying the intended `.beads` store?

Clear a stale assignee or resolve a dependency only when the ownership and
dependency are confirmed. Remove an exclusion label only when the work is
actually eligible for automated handling. Preserve evidence in the bead
comment or incident record when changing a shared queue.

### 4. Check Pluck diagnostics and claim behavior

Pluck emits starvation telemetry with the store-returned count, filtered count,
and exclusion reasons. Query recent events with the current filter syntax:

```bash
needle logs --since 2h --filter 'event_type~strand\.pluck\.starvation_detected' --format json
needle logs --since 2h --filter 'event_type~bead\.claim\..*'
```

If the ready list is non-empty but no work is being claimed, look for claim
races, repeated claim failures, or a worker that is stuck after dispatch. A
transient worker-local exclusion can hide an ID briefly even though the bead
is still ready. Restarting a single unhealthy worker may clear that transient
state; changing global labels will not.

The current deployment sets `persistent_starvation_records: false`, so the
absence of a starvation-record file is not evidence that starvation did not
occur. Use NEEDLE telemetry and worker logs as the durable evidence source.

### 5. Verify the repair

After a deliberate repair:

```bash
cd /home/coding/claude-governor
needle doctor --workspace "$PWD"
bead list --ready --json --limit 999999 | jq -s 'length'
needle status
```

Record the before/after open and ready counts, the resolved workspace, the
configuration change, and the worker restart time. If the ready count is still
zero, return to step 3 instead of repeatedly changing labels.

## Quick symptom table

| Symptom | Most likely cause | First action |
| --- | --- | --- |
| Open count is zero | No backlog in this store | Verify the intended workspace before creating work |
| Open beads exist, ready count is zero | Assignment, manual block, unfinished blocker, or exact exclusion label | Compare open/ready JSON and inspect `bead show` |
| Ready count is positive, worker stays idle | Claim race, worker-local exclusion, or dispatch failure | Query Pluck and claim telemetry; inspect worker status |
| Every bead disappears after adding a custom label list | Custom list replaced the built-in defaults or added an exact label present on all beads | Restore the complete intended list and restart workers |
| A repository's beads are missing but another repository has work | Wrong `workspace.default` or missing `--workspace` | Use an absolute worker workspace and run `needle doctor` there |
| A new sibling repository is not found by Explore | Pinned `workspaces` list, non-direct-child path, or Explore disabled | Check Explore mode and `workspace_root` |

## Related references

- [`docs/plan/pluck-configuration.md`](plan/pluck-configuration.md) — current
  filter and candidate inventory.
- [`docs/pluck-workspace-paths.md`](pluck-workspace-paths.md) — path resolution
  and Explore behavior.
- [`docs/pluck-query-results.md`](pluck-query-results.md) — current JSONL
  result contract, followed by historical query notes.
- [`docs/research/pluck-filter-root-cause.md`](research/pluck-filter-root-cause.md)
  — incident evidence and reproduction history.
