# Pluck Configuration Investigation Report

**Status:** consolidated findings
**Updated:** 2026-08-20
**Target workspace:** `/home/coding/claude-governor`

## Executive summary

Pluck does not select from every bead in a workspace. It asks the configured bead
backend for the ready frontier, removes excluded labels and non-claimable records,
then sorts the remaining candidates. The configuration has several legitimate
ways to make a bead invisible:

- Pluck can be pointed at a different workspace and therefore a different
  `.beads` database.
- The ready query excludes non-open, assigned, manually blocked, or dependency-
  blocked beads.
- `exclude_labels` removes beads carrying any configured label.
- A stale open assignee can keep a bead out of the ready frontier until it is
  cleared.

The original “open beads are not visible” incident was a workspace-selection
failure, not an over-broad label filter. Historical tests returned candidates
from `/home/coding/claude-governor/.beads` but zero candidates from the wrong
`/home/coding/.beads` database. The old default was subsequently changed, but
the currently authoritative global config now points at `/home/coding/aide-de-camp`,
which is again not this workspace. A worker started without an explicit workspace
can therefore reproduce the same class of failure.

There is also a separate configuration-load failure in the installed `needle`
CLI: `needle config` currently rejects
`/home/coding/.config/needle/config.yaml:139` (`telemetry.otlp_sink.tls: "none"`)
because that binary expects the structured TLS form. A worker using that same
binary may fail before Pluck initializes; this must be fixed or version-aligned
before runtime findings can be trusted.

## Evidence from the three child investigations

The supplied `bf-*` IDs are historical bead-forge IDs. They are not present as
live IDs after this workspace's 2026-08-14 bead-rs migration, so their findings
were recovered from the committed investigation history:

| Child bead | Finding | Historical artifact |
|---|---|---|
| `bf-6b65h` | Documented `exclude_labels`, the empty-list fallback, custom override behavior, and the distinction between labels and status. | Commit `00c870e` (`notes/bf-6b65h.md`) |
| `bf-22ks5` | Verified that configured workspace paths and databases were accessible, while also documenting multi-workspace discovery and the then-current default `/home/coding`. | Commit `9aa1888` (`notes/bf-22ks5.md`) |
| `bf-2ur41` | Documented the filter pipeline, stale-assignee risk, label lifecycle risk, and historical filter impact. | Commit `d69b78a` (`notes/bf-2ur41.md`) |

Later root-cause testing is also relevant: commit `3a70f2f` demonstrated that
the full filter combination returned ready beads from the correct database and
zero from the wrong database. The historical counts below are retained with
their dates because the workspace has since migrated to bead-rs and its live
queue has changed.

## Configuration sources and precedence

The current NEEDLE source resolves configuration in this order:

1. built-in defaults;
2. `/home/coding/.config/needle/config.yaml` (global file);
3. `<workspace>/.needle.yaml` (limited workspace overrides);
4. `NEEDLE_*` environment overrides;
5. CLI overrides, including `needle run --workspace`.

The loader is implemented in `/home/coding/NEEDLE/src/config/mod.rs` and reads
the global file from `.config/needle/config.yaml`. The current source does not
allow a workspace `.needle.yaml` to override `strands.pluck`; its workspace
override type supports agent settings, selected auxiliary strands, prompts,
verification/gates, workspace labels, and bead backend binding.

### Files observed

| File or setting | Current value | Visibility effect |
|---|---|---|
| `/home/coding/.config/needle/config.yaml` → `workspace.default` | `/home/coding/aide-de-camp` | **High risk:** the home Pluck store is not `claude-governor` unless the worker is launched with an explicit workspace. |
| `/home/coding/.config/needle/config.yaml` → `telemetry.otlp_sink.tls` | String `"none"` | **Configuration-load failure in installed CLI:** `needle config` rejects this value as the wrong type at line 139. |
| `/home/coding/.config/needle/config.yaml` → `workspace.home` | `/home/coding/.needle` | Stores NEEDLE state/heartbeats and optional starvation records; it is not the target bead database. |
| `/home/coding/.config/needle/config.yaml` → `strands.explore` | enabled; `workspaces: []`; `workspace_root: /home/coding/` | Allows recursive discovery of `.beads` workspaces. The exclusion file still controls which discovered roots are skipped. |
| `/home/coding/.config/needle/explore-excluded` | Does not list `claude-governor` as observed on 2026-08-20 | If `claude-governor` is added here, Explore cannot find it. |
| `/home/coding/claude-governor/.needle.yaml` | `bead_cli.backend: bead-rs` | Selects the backend; no Pluck override is present. |
| `/home/coding/claude-governor/.beads/config.json` | bead-rs workspace metadata (`prefix: claudego`) | Identifies the live bead-rs store; it does not define Pluck label filters. |
| `/home/coding/.needle/config.yaml` | `workspace.default: /home/coding/claude-governor`, older schema/content | **Not read by the current loader.** It is a stale duplicate and can create false confidence during troubleshooting. |

The global config's `workspace.default` is a home-store selection, not a
restriction on Explore. Explore can still discover other repositories when it
is enabled, but this does not make an incorrectly selected home database
correct. Workers load configuration at startup, so a change requires a worker
restart.

## Configured Pluck settings

The authoritative global config declares the following Pluck values. They become
runtime-effective only after the configuration-load error described above is
resolved:

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

### `exclude_labels`

The configured list has four exact, case-sensitive labels:

| Label | Effect |
|---|---|
| `deferred` | Removes postponed work from Pluck candidates. |
| `human` | Removes work reserved for human intervention. |
| `blocked` | Removes work marked as blocked; dependencies are also checked by the ready query. |
| `starvation-alert` | Removes alert beads from normal work selection. This is an explicit deployment setting, not the current source default. |

The source fallback in `/home/coding/NEEDLE/src/strand/pluck.rs:21` is:

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

An empty `exclude_labels` vector activates that fallback. A non-empty configured
vector replaces the fallback; it is not merged with it. Consequently, an empty
list is not a way to disable all exclusions. The current global file is
non-empty, so its four labels will be effective once the file parses.

The list applies to the `labels` field only. `open`, `in_progress`, and `closed`
are status values, not labels; status filtering comes from the ready query and
the defensive Pluck filter.

### `split_after_failures`

The current value is `3`. When the first sorted candidate carries a
`failure-count:N` label at or above the threshold, Pluck returns a split
instruction instead of the normal bead-found result. This can delay processing
of that bead, but it is not a database visibility filter. A value of `0`
disables the split trigger.

Candidates are sorted by priority, failure count, creation time, and ID. The
failure-count tie-breaker prevents a repeatedly failing bead from monopolizing
the first position.

### `persistent_starvation_records`

The current value is `false`, matching the source default. Pluck still emits
starvation telemetry when it has no candidates. When enabled, it additionally
writes records under the NEEDLE home state directory
(`/home/coding/.needle/state/starvation-records.jsonl`); this setting affects
diagnostics, not candidate visibility.

## Candidate filter pipeline

The effective pipeline is:

```text
configured home workspace
  → configured bead backend's ready frontier
  → backend label/ID filters
  → Pluck defensive label filter
  → Pluck status and assignee guard
  → deterministic sort
  → split check or BeadFound
```

### 1. Workspace and backend

Pluck opens the `.beads` store for the worker's resolved home workspace. The
CLI workspace is canonicalized when supplied. If the selected workspace has no
valid `.beads` directory, Pluck returns `Skipped(no_home_store)` rather than
searching this repository by accident.

The current target binds `bead-rs` in `.needle.yaml`. The post-migration live
store is `.beads/beads.db`, with the durable checkpoint under
`.beads/checkpoint/`; older `bf`/`br` commands and their old schema should not be
used to diagnose this store.

### 2. Ready-frontier constraints

The bead-rs `ready` operation is the main visibility gate. It returns work that
is ready to claim, which means, at minimum:

- base status is `open`;
- the bead is unassigned;
- it is not manually blocked;
- it has no unresolved blocking dependency.

The Pluck `Filters` object supplies `assignee: None`, the configured
`exclude_labels`, and an empty `exclude_ids` set. The backend also removes any
configured excluded IDs, although Pluck currently supplies none.

### 3. Defensive label filter

Pluck repeats the label exclusion after the backend returns candidates. This
protects against backend output that omits or mishandles label data and prevents
a select/claim/retry loop. Any returned bead carrying one of the four effective
labels is removed again.

### 4. Status and stale-assignee guard

Pluck removes an `in_progress` bead and removes an `open` bead with any assignee.
The latter is intentionally conservative: an open bead with a dead worker's
assignee is still invisible until the assignment is cleared. This is the main
filter-level starvation risk identified by `bf-2ur41`.

### 5. Metadata and time fields

The old bead-forge investigation notes discussed `ephemeral`, `pinned`,
`is_template`, `defer_until`, and `due_at` as possible or historical query
criteria. They are not active Pluck settings in the current bead-rs path shown
above. In particular, Pluck does not implement a time-based reactivation of a
`deferred` label; label lifecycle remains an operational responsibility.

## Which settings can make beads invisible?

| Setting or state | Can hide a bead? | Assessment |
|---|---:|---|
| Wrong `workspace.default` or missing `--workspace` | Yes, potentially all beads in the intended repository | **Primary incident cause.** The worker queries another valid but empty/smaller `.beads` store. |
| `explore.workspace_root` / `explore-excluded` | Yes for discovered workspaces | Current root includes `/home/coding`; `claude-governor` is not currently excluded. |
| `exclude_labels` | Yes for matching labels | Intended behavior. `starvation-alert` is an extra explicit exclusion in the current global config. |
| Empty `exclude_labels` | Yes, by activating the three-label source fallback | Common configuration misunderstanding. |
| `status != open` | Yes | Ready-frontier behavior, not a Pluck bug. |
| Assigned open bead or `in_progress` status | Yes | Stale assignees can cause durable invisibility. |
| Manual block or unresolved dependency | Yes | Ready-frontier behavior. |
| `exclude_ids` | Yes in principle | Currently empty, so it is not contributing. |
| `split_after_failures` | Delays normal processing, not query visibility | Only applies after candidates have been found and sorted. |
| `persistent_starvation_records` | No | Changes diagnostics only. |
| `.beads/config.json` lifecycle metadata | Not directly | Backend selection is relevant; bead-rs metadata itself is not an exclude-label setting. |

## Root cause of the historical invisibility

The decisive evidence was a database comparison from the later investigation:

| Database queried | Historical result |
|---|---:|
| `/home/coding/claude-governor/.beads/beads.db` | 36 ready candidates under the full filter combination |
| `/home/coding/.beads/beads.db` | 0 open beads and 0 candidates |

The failure sequence was:

1. NEEDLE/Pluck resolved a relative or incorrect default workspace to a parent
   or other workspace.
2. The worker opened that workspace's valid `.beads` database.
3. The ready query correctly found no open/claimable records there.
4. Pluck reported no candidates, making the beads in
   `/home/coding/claude-governor/.beads` appear invisible.

This explains why debug output showed “no beads excluded”: there were no beads
from the wrong database on which the label filter could operate. Systematic
filter tests on the correct database did not produce a zero-candidate result.

The `bf-22ks5` finding that `/home/coding` was accessible was therefore not
evidence that it was the right workspace. Accessibility and correctness are
different checks. A later historical change set the old global default to
`/home/coding/claude-governor`, but the current authoritative file has drifted
to `/home/coding/aide-de-camp`; that live mismatch must be treated as an
unresolved configuration risk.

## Recommendations

1. **Make the active config parse with the installed worker.** Resolve the
   `telemetry.otlp_sink.tls` schema mismatch at line 139 (use the format
   supported by the installed `needle`, or deploy a matching binary/config
   pair), then verify with `needle config`. This is a prerequisite for
   trusting any Pluck runtime behavior.

2. **Align the active default with the intended worker home.** For workers
   serving this repository, set `workspace.default` in
   `/home/coding/.config/needle/config.yaml` to
   `/home/coding/claude-governor`, or always launch with
   `needle run --workspace /home/coding/claude-governor`. Restart workers after
   changing it.

3. **Remove configuration ambiguity.** Treat `/home/coding/.config/needle/` as
   the active configuration location and archive or clearly mark
   `/home/coding/.needle/config.yaml` as legacy. Verify the resolved value with
   `needle config`/the running worker rather than reading the duplicate file.

4. **Keep label policy intentional.** Retain `deferred`, `human`, and `blocked`
   if those labels mean “do not dispatch automatically.” Review whether
   `starvation-alert` should remain hidden; remove it from the explicit global
   list only if alert beads are meant to be ordinary worker tasks. Do not use an
   empty list expecting no exclusions.

5. **Recover stale assignments.** Monitor open beads with assignees, clear
   assignments left by dead workers, and ensure the mend/heartbeat recovery
   path is operating. A label audit should also remove `deferred` or `blocked`
   labels when their reason no longer applies.

6. **Protect discovery.** Keep `explore.workspace_root` broad enough to include
   the target, keep `claude-governor` out of `explore-excluded`, and use an
   explicit `explore.workspaces` list when strict repository scope is required.

7. **Use the current backend for verification.** From the target workspace,
   compare `bead list --status open` with `bead list --ready`, then inspect the
   worker's resolved workspace and Pluck telemetry. Do not validate a bead-rs
   store with old `bf`/`br` commands.

## Verification snapshot

On 2026-08-20, the live bead-rs store in this repository reported 75 open beads
and 16 beads on the ready frontier. These are queue-state counts, not a claim
that all 16 survive the active four-label Pluck filter. The database and backend
were readable, the target `.needle.yaml` selected bead-rs, and the target was
not listed in the current Explore exclusion file.

The historical child counts (1,208 total beads, 21 open, 20 claimable, and 81
filtered in one bf-era analysis) should be used only as dated investigation
evidence, not as current operational metrics.

## Conclusion

The Pluck filter settings are restrictive by design but function correctly on
the intended database. Labels, readiness, dependencies, manual blocks, and
stale assignees can hide individual beads; a workspace mismatch can hide the
entire queue. The root cause of the investigated “open beads are not visible”
incident was the latter: Pluck queried the wrong workspace database. The active
configuration still has a workspace-default mismatch, so aligning that path and
removing the duplicate-config ambiguity are the required configuration fixes.
