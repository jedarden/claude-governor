# Pluck bead-visibility configuration map

**Verified:** 2026-08-21
**Target workspace:** `/home/coding/claude-governor`
**Backend selected by the target:** `bead-rs`

This is the current configuration map for the NEEDLE Pluck strand. It replaces
the older `bf`/`br`-era descriptions in this repository. “Visibility” means
“can enter Pluck’s candidate list”; it does not mean that a bead is present in
the database or that a worker successfully claims it.

## Short answer

There are two separate visibility paths:

1. **Home Pluck** resolves one workspace, invokes its configured bead CLI, and
   filters the returned ready beads. Its configurable label filter is the
   global `strands.pluck.exclude_labels` list.
2. **Explore** can scan additional workspaces. Its global discovery settings
   determine which repositories are visited, while the current Explore
   implementation uses its own hard-coded exclusion list (`deferred`,
   `human`, `blocked`) rather than the global Pluck list.

The files that participate are:

| Location | Format | What it controls | Directly filters a bead? |
|---|---|---|---|
| `~/.config/needle/config.yaml` | YAML | Home workspace selection, Pluck labels, and Explore workspace discovery | `strands.pluck.exclude_labels`: yes; workspace/discovery keys: indirectly |
| `<workspace>/.needle.yaml` | YAML | Backend binding and optional backend executable path for that workspace | No label filter; a wrong/missing binding can prevent the store from opening |
| `<workspace>/.beads/config.json` | JSON | bead-rs workspace identity and optional checkpoint policy | No; it is required for workspace discovery, but does not select individual beads |
| `<workspace>/.beads/beads.db` | SQLite | Live bead data and the fields used by `bead list --ready` | Yes, through bead state, assignees, blocks, dependencies, and labels |

The last row is live data rather than a configuration file, but it must be in
the map: it is the authoritative source for the backend’s ready frontier.

## Runtime pipeline

For a normal `needle run`, the effective path is:

```text
defaults
  -> ~/.config/needle/config.yaml
  -> <resolved workspace>/.needle.yaml (supported workspace overrides)
  -> supported NEEDLE_* environment overrides
  -> CLI workspace override (-w/--workspace)
  -> open the configured bead-rs store in that workspace
  -> bead list --ready --json --limit 999999
  -> NEEDLE exact label/status/assignee guards
  -> Pluck ordering and claim attempt
```

NEEDLE loads configuration once at startup. Editing a file does not change an
already-running worker; restart the worker after a configuration change.

## 1. Global NEEDLE configuration

### Location and format

`/home/coding/.config/needle/config.yaml` (normally addressed as
`~/.config/needle/config.yaml`) is a YAML document loaded for every NEEDLE
workspace. The relevant sections are:

```yaml
workspace:
  default: /path/to/home-workspace
  home: /path/to/needle-state

strands:
  pluck:
    exclude_labels: [deferred, human, blocked]
    split_after_failures: 3
    persistent_starvation_records: false
  explore:
    enabled: true
    workspaces: []
    workspace_root: /path/to/search-root
```

### Settings that affect visibility

`workspace.default` selects the home workspace when `needle run` has no
`--workspace` argument. This is store selection, not a bead filter.

`strands.pluck.exclude_labels` is a list of exact, case-sensitive label names.
The store adapter and Pluck’s defensive pass remove a bead if any label is in
this list. A non-empty configured list replaces the built-in list; it is not
merged with it.

`strands.explore.enabled` controls whether the additional-workspace scan runs.
`strands.explore.workspaces` explicitly lists repositories to scan. When it is
empty, Explore discovers repositories under `strands.explore.workspace_root`.
The home workspace is skipped by Explore because Home Pluck already queried it.
These settings therefore control which other bead databases can contribute
work, not the labels excluded from Home Pluck.

`workspace.home` is not a bead-store path. It contains NEEDLE state, logs,
heartbeats, and optional starvation records. Changing it does not change which
`.beads` database Pluck opens.

`strands.pluck.split_after_failures` changes whether a selected candidate
produces a split result after repeated failures. It does not remove the bead
from the backend ready query. `persistent_starvation_records` only controls
diagnostic persistence and does not filter candidates.

### Current configured values

The relevant values currently present in the global file are:

```yaml
workspace:
  default: /home/coding/aide-de-camp
  home: /home/coding/.needle
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
      - starvation-alert
    split_after_failures: 3
    persistent_starvation_records: false
  explore:
    enabled: true
    workspaces: []
    workspace_root: /home/coding/
```

There is an operational caveat: installed `needle 0.3.0` currently rejects
this file before resolving any Pluck settings because
`telemetry.otlp_sink.tls: none` is a string where that binary expects an
object. Until that unrelated schema mismatch is fixed or the binary is
aligned, a worker cannot use this configuration normally. The values above
are the file’s configured values, not a claim that the currently installed
binary has successfully loaded them.

## 2. Workspace NEEDLE configuration

### Location and format

Each candidate workspace may contain `<workspace>/.needle.yaml`, a YAML
document. NEEDLE applies only the workspace fields supported by its loader;
the file is not an unrestricted overlay of the global configuration.

The target file is:

```yaml
bead_cli:
  backend: bead-rs
```

The supported backend fields are:

```yaml
bead_cli:
  backend: bead-rs       # or another supported backend name
  path: /absolute/path   # optional executable override
```

`bead_cli.backend` is important to visibility because it selects the command
dialect and store adapter. For this target, NEEDLE invokes the bead-rs
descriptor, which runs `bead list --ready --json --limit 999999` with the
workspace as its current directory. An absent/`auto` binding is not a
configuration for the current authoritative store-opening path; a wrong
binding or executable can produce a store error or query the wrong backend.

The current loader does **not** allow this file to override:

- `strands.pluck.exclude_labels` or other Pluck fields;
- `strands.explore.enabled`, `workspaces`, or `workspace_root`;
- `workspace.default` or `workspace.home`.

`workspace.labels` can be supplied for workspace metadata/skill routing, but it
does not filter beads. The same `.needle.yaml` lookup is performed for each
Explore workspace, so every discovered repository needs a compatible backend
binding.

## 3. Bead-rs workspace metadata

### Location and format

`<workspace>/.beads/config.json` is a committed JSON document. In the target it
currently contains workspace identity metadata of this form:

```json
{
  "created_at": "...",
  "prefix": "claudego",
  "uuid": "...",
  "version": 1
}
```

Bead-rs discovers this file by walking upward from its current directory. It
then opens the sibling `<workspace>/.beads/beads.db` and verifies the native
workspace row. Therefore, absence of this file, or an uninitialized database,
prevents Pluck from seeing the workspace at all. The identity fields do not
hide individual beads.

Current bead-rs also recognizes an optional `checkpoint` object in this JSON,
for example `checkpoint.mode` and `checkpoint.thresholds`. Those settings
control checkpoint publication/flush layout, not `bead list --ready`; they are
not candidate-visibility filters.

## 4. Live bead-rs database state

### Location and format

`<workspace>/.beads/beads.db` is a SQLite database. It is gitignored and is the
live source queried by bead-rs. The ready query in the current bead-rs backend
requires all of the following:

| Database state | Ready requirement |
|---|---|
| `issues.base_status` | `open` |
| `issues.assignee` | `NULL` / unassigned |
| `issues.manual_blocked` | `NULL` or `0` |
| blocking dependencies | No unfinished dependency of kind `blocks` |

A blocker is unfinished when its `base_status` is anything other than
`closed`. Labels are returned with the issue and are then filtered by NEEDLE;
the bead-rs ready SQL itself does not apply the Pluck exclusion-label list.

NEEDLE supplies `--limit 999999`, so the limit is effectively an implementation
ceiling rather than an intended visibility setting. Priority and creation time
affect ordering after filtering, not whether an otherwise ready bead exists.

## 5. Built-in and transient sources (not configuration files)

These sources explain visibility behavior but are not operator-editable config
files:

- NEEDLE’s compiled fallback in `src/strand/pluck.rs` is
  `deferred`, `human`, and `blocked`. It is used when the resolved
  `exclude_labels` list is empty. A custom non-empty list replaces, rather than
  extends, this fallback.
- The current Explore implementation has a separate compiled exclusion list
  containing those same three labels. It does not read the global fourth
  label, `starvation-alert`.
- Pluck creates an empty transient `exclude_ids` set. The worker can add IDs
  temporarily after claim races or retry failures, but this is runtime state,
  not a configuration file and is not persistent policy.
- Pluck also defensively removes returned `in_progress` beads and open beads
  that still have an assignee. Normally bead-rs `--ready` has already removed
  them.

## Precedence and relationships

### General NEEDLE precedence

For fields that a layer supports, later sources win in this order:

1. built-in defaults;
2. global `~/.config/needle/config.yaml`;
3. the resolved workspace’s `.needle.yaml` supported overrides;
4. supported `NEEDLE_*` environment overrides;
5. CLI overrides, highest priority.

The precedence is field-specific, not a promise that every key is accepted at
every layer:

- `strands.pluck.exclude_labels` is currently configurable from the global
  file. The workspace file, environment whitelist, and run CLI do not provide
  a Pluck-label override. If the global list is absent or empty, the compiled
  three-label fallback applies.
- `workspace.default` comes from the global file, can be overridden by
  `NEEDLE_WORKSPACE__DEFAULT`, and is superseded by `needle run
  --workspace/-w`. The CLI-selected path is the Home Pluck store.
- Explore `enabled` and `workspace_root` can be overridden by their supported
  `NEEDLE_STRANDS__EXPLORE__...` variables. `workspaces` is configured in the
  global YAML; there is no corresponding Pluck-label environment override.
- `bead_cli.backend` and `bead_cli.path` are selected from the resolved
  workspace configuration. The target’s `.needle.yaml` binding is therefore
  the authority for this repository’s backend; the bead database does not
  override it.

### What wins at each filtering stage

The sources are sequential gates, not competing copies of one SQL query:

```text
workspace path selection
  -> .needle.yaml backend selection
  -> .beads/config.json discovery and identity check
  -> .beads/beads.db bead-rs ready frontier
  -> Home Pluck exclude_labels and defensive guards
```

For Explore, global workspace discovery is inserted before the per-workspace
`.needle.yaml` and `.beads` checks. A repository that is not discovered cannot
contribute a bead, even if its own bead is ready. A repository that is
discovered but has no valid backend binding or initialized bead-rs workspace
is skipped or errors. A bead that reaches the ready frontier can still be
removed by the relevant label list.

## Files explicitly not in the visibility path

- `.beads/checkpoint/*.jsonl` and checkpoint pointer files are durable sync /
  recovery artifacts. NEEDLE does not query them for candidates; the live
  SQLite database is authoritative at runtime.
- `~/.config/needle/adapters/*.yaml` controls agent process commands and
  prompt/transport behavior after a bead is selected, not candidate
  visibility.
- `~/.config/claude-governor/governor.yaml` controls worker counts and launch
  commands. It can determine whether a NEEDLE worker is running, but it does
  not alter Pluck’s candidate filters.
- `~/.config/needle/explore-excluded` is not read by the current Explore source
  inspected for this deployment. It is a historical entry in older notes, not
  an active configuration file.
- `.beads/config.yaml`, `~/.config/beads/config.yaml`, and old `bf`/`br`
  policy files belong to other/legacy implementations and are not read by the
  target’s `bead` 0.1.3 invocation.

## Verification pointers

The map was checked against these implementations:

- NEEDLE: `src/config/mod.rs` (layering and supported workspace overrides),
  `src/strand/pluck.rs` (label/default/defensive filtering),
  `src/strand/explore.rs` (workspace scan and its separate label list),
  `src/bead_store/cli_store.rs` (post-query filters), and
  `src/bead_store/mod.rs` (workspace/backend opening).
- bead-rs: `src/store/mod.rs` (config discovery and database path) and
  `src/service/issues.rs` (the `--ready` predicate).

Useful read-only checks from the target workspace are:

```bash
sed -n '1,40p' /home/coding/claude-governor/.needle.yaml
sed -n '25,52p' ~/.config/needle/config.yaml
bead list --ready --json --limit 20
needle config --get strands.pluck.exclude_labels
```

The last two commands validate runtime state and loadability respectively;
they are expected to fail or return no candidates when the workspace database
is uninitialized or the global config has the schema mismatch described above.
