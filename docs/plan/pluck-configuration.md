# Pluck filter and label settings

**Status:** authoritative current-state reference
**Verified:** 2026-08-21
**Scope:** NEEDLE Pluck using this workspace's `bead-rs` backend

This document records the complete Pluck configuration and candidate-filter
inventory. It supersedes the older `bf`/`br`-era SQL examples in this
repository. The target workspace's `.needle.yaml` selects `bead-rs`; it does
not define a second Pluck configuration.

## Effective configuration

The configured global configuration is `/home/coding/.config/needle/config.yaml`.
Its complete `strands.pluck` section is:

```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
      - starvation-alert
    split_after_failures: 3
    persistent_starvation_records: false
```

The target workspace contains only this backend binding:

```yaml
bead_cli:
  backend: bead-rs
```

### Runtime loadability

The filter values above are present in the global file, but the installed
`needle 0.3.0 config` command currently fails before Pluck initializes because
it rejects the unrelated `telemetry.otlp_sink.tls: none` value at line 133.
The configuration must be schema-fixed or version-aligned before a worker can
use these values at runtime; this does not change the filter inventory below.

There are exactly three configurable Pluck keys in NEEDLE's
`PluckConfig`:

| Key | Effective value | Candidate visibility? | Meaning |
|---|---:|---:|---|
| `strands.pluck.exclude_labels` | `deferred`, `human`, `blocked`, `starvation-alert` | Yes | Excludes a bead when any exact label matches. |
| `strands.pluck.split_after_failures` | `3` | No | Changes the result for the first sorted candidate when its `failure-count:N` label reaches the threshold. `0` disables splitting. |
| `strands.pluck.persistent_starvation_records` | `false` | No | Controls whether no-candidate diagnostics are also appended to NEEDLE's starvation-record file. |

No other `strands.pluck` keys, status selectors, required-label selectors, or
metadata selectors are configured for this workspace.

### Configuration precedence and workspace selection

NEEDLE loads built-in defaults, the global config, workspace-supported
overrides, environment overrides, and CLI overrides. Pluck is constructed from
the resolved `strands.pluck` values when the worker starts.

The global `workspace.default` currently points to
`/home/coding/aide-de-camp`, while this repository is
`/home/coding/claude-governor`. That path selects the bead store that Pluck
opens; it is not itself a label filter. A worker serving this repository must
use an explicit workspace override or an aligned default. The NEEDLE home path
(`/home/coding/.needle`) stores worker state and diagnostics; it is not the
target `.beads` database.

## Complete `exclude_labels` inventory

### Active target configuration

These are the four labels currently passed into Pluck from the active global
config:

| Exact label | Effect |
|---|---|
| `deferred` | Excludes postponed work. |
| `human` | Excludes work reserved for human handling. |
| `blocked` | Excludes beads carrying the blocked marker. This is a label match, separate from bead-rs manual blocking and effective status. |
| `starvation-alert` | Excludes starvation-alert beads from normal work selection. This is an explicit deployment label, not a built-in Pluck default. |

Matching is exact and case-sensitive. The implementation uses string
membership (`contains`); there is no wildcard, prefix, regular-expression, or
case-folded matching. A bead with multiple labels is excluded if it has at
least one matching label.

### Built-in fallback

NEEDLE defines this fallback in
`/home/coding/NEEDLE/src/strand/pluck.rs`:

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

An omitted or empty `exclude_labels` vector is replaced by those three labels.
A non-empty configured list replaces the fallback; it is not merged with it.
Therefore:

- `exclude_labels: []` does **not** disable label filtering;
- a custom non-empty list must repeat `deferred`, `human`, or `blocked` if
  those defaults should remain excluded;
- the active four-label list is the effective list for this deployment.

Labels such as `failure-count:N`, `cycling`, `polish`, or `documentation` are
not excluded by Pluck unless an operator adds their exact names to
`exclude_labels`. `failure-count:N` has separate split and ordering behavior,
described below.

## Complete candidate-filter inventory

Pluck selection has multiple layers. The backend's ready query is the primary
visibility gate; Pluck then applies label and defensive guards.

### 1. Workspace and store

Pluck queries the resolved workspace's configured bead store. For this target,
NEEDLE opens the `.beads` store selected by `bead-rs` and runs:

```text
bead list --ready --json --limit 999999
```

If the resolved workspace has no valid `.beads` store, Pluck returns
`Skipped(no_home_store)`. It does not search upward for this repository's
database.

### 2. Bead-rs `--ready` frontier

The current bead-rs implementation defines a ready issue as all of the
following:

| Criterion | Required value | Notes |
|---|---|---|
| Base status | `open` | `in_progress`, `deferred`, and `closed` do not enter the ready frontier. |
| Assignee | `NULL` / absent | Assigned open beads are not ready. |
| Manual block | `false`, `0`, or `NULL` | A manual block is an effective `blocked` state. |
| Blocking dependencies | None unfinished | A dependency of kind `blocks` hides the bead while its blocker has any base status other than `closed`. `relates_to` is not a blocking dependency. |

The ready query orders its result by `priority ASC`, `created_at ASC`, and
`id ASC`. Pluck requests a limit of `999999`, so the limit is effectively an
implementation ceiling rather than a normal queue filter.

The ready frontier has no filter for labels, pinned state, ephemeral state,
templates, due dates, `defer_until`, or required labels. Labels are included in
the JSON projection and filtered by NEEDLE after the backend returns them.

### 3. NEEDLE `Filters` passed to the store

Pluck constructs the complete runtime filter object as follows:

```rust
Filters {
    assignee: None,
    exclude_labels: self.exclude_labels.clone(),
    exclude_ids: HashSet::new(),
}
```

| Runtime field | Current value | Effect |
|---|---|---|
| `Filters.assignee` | `None` | Pluck does not add an assignee equality filter in the store adapter. The bead-rs `--ready` query already requires no assignee. |
| `Filters.exclude_labels` | The four active labels above | The CLI store adapter removes any returned bead carrying one of these exact labels. |
| `Filters.exclude_ids` | Empty set | Pluck supplies no configured ID exclusions. |

The store adapter applies `exclude_labels` and `exclude_ids` after parsing the
backend output. This is followed by Pluck's own defensive label pass, so label
exclusion is intentionally performed twice: once in the store adapter and once
in `PluckStrand` to protect against a backend that omits or mishandles labels.

### 4. Pluck status and stale-assignee guard

After the store returns candidates, Pluck removes:

- a bead whose status is `in_progress`;
- an `open` bead that still has an assignee.

Normally the ready frontier has already removed both cases. The second pass is
a defensive guard against stale or inconsistent backend output. It also means
that an open bead assigned to a dead worker remains invisible until its
assignee is cleared.

Pluck does not independently filter `closed` or `deferred` candidates after
the ready query; those statuses should never be returned by `--ready`.

### 5. Worker-local ID exclusions

The worker maintains a transient exclusion set outside `strands.pluck`. The
`StrandRunner` removes IDs from selected candidates when they are currently
excluded, including IDs that recently lost a claim race or produced a claim
error. Race-lost IDs have a 30-second TTL; other retry exclusions are cleared
as the worker's retry state resets. This set is not persisted configuration and
is not part of `exclude_labels`.

## Labels versus statuses

Labels and statuses are separate fields:

| Concept | Current behavior |
|---|---|
| `deferred` label | Excluded by the active `exclude_labels` list. |
| `deferred` base status | Not eligible for `--ready`, even without the label. |
| `blocked` label | Excluded by the active `exclude_labels` list. |
| Manual block | Not a label; `manual_blocked` prevents `--ready` and is shown as effective status `blocked`. |
| Unfinished `blocks` dependency | Not a label; prevents `--ready`. |
| `in_progress` base status | Not eligible for `--ready`; also removed by Pluck's defensive status guard. |
| `closed` base status | Not eligible for `--ready`; a closed blocker is considered finished. |
| `failure-count:N` label | Not an exclusion label. It affects Pluck ordering and may trigger splitting. |

There is no active Pluck filter that requires a bead to have a positive label
such as `polish`, `rust`, or `documentation`.

## Ordering and failure-count behavior

After filtering, Pluck sorts candidates deterministically by:

```text
priority ASC → failure count ASC → created_at ASC → id ASC
```

The failure count is the maximum valid integer in any `failure-count:N` label;
missing or malformed values count as zero. This ordering prevents a repeatedly
failing bead from monopolizing the first slot at the same priority.

With `split_after_failures: 3`, if the first sorted candidate has a failure
count of at least three, Pluck returns a `Split` result instead of a normal
bead-found result. This changes dispatch behavior after filtering; it does not
make the bead disappear from the ready query. Setting the threshold to `0`
disables this check.

If a threshold-reaching candidate's title or body matches NEEDLE-internal
configuration patterns (including Pluck or `exclude_labels` investigation),
NEEDLE skips that split candidate and evaluates the queue again. This is a
content-based safety guard, not a configurable label filter.

## Diagnostics setting

When the filtered candidate list is empty, Pluck emits starvation telemetry with
the store-returned count, exclusion count, and exclusion reasons. With the
current `persistent_starvation_records: false`, it does not write a persistent
record. If enabled, records are written to NEEDLE's own
`<workspace.home>/state/starvation-records.jsonl`, never to the target
repository's `.beads` store.

## Troubleshooting checklist

To distinguish a filter result from a wrong workspace or empty ready frontier:

```bash
# Confirm the target backend binding.
sed -n '1,40p' /home/coding/claude-governor/.needle.yaml

# Inspect the active Pluck values.
sed -n '/^strands:/,/^[^[:space:]]/p' /home/coding/.config/needle/config.yaml

# Compare all open issues with the ready frontier in the target workspace.
bead list --status open --json --limit 999999
bead list --ready --json --limit 999999
```

Use the current `bead` CLI for the bead-rs store. Do not use historical `bf`
or `br` SQL/schema examples as evidence about the current ready predicate.
