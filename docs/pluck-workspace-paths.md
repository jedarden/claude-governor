# Pluck workspace path settings

This document describes how NEEDLE's Pluck and Explore strands select bead
workspaces. The values below reflect the host configuration after the runtime
fix applied on 2026-08-21.

## Current settings

The authoritative global configuration is
`/home/coding/.config/needle/config.yaml`:

```yaml
workspace:
  default: /home/coding/claude-governor
  home: /home/coding/.needle

strands:
  explore:
    enabled: true
    workspaces: []
    workspace_root: /home/coding/
```

These values mean:

| Setting | Current value | Meaning |
| --- | --- | --- |
| `workspace.default` | `/home/coding/claude-governor` | Pluck's home workspace when a worker is started without `--workspace`. Pluck queries this directory's `.beads` store. |
| `workspace.home` | `/home/coding/.needle` | NEEDLE's own state, logs, heartbeats, and optional starvation records. It is not a bead workspace and is not the Explore scan root. |
| `strands.explore.enabled` | `true` | Allows the later Explore strand to look for work outside Pluck's home workspace. |
| `strands.explore.workspaces` | `[]` | No explicit Explore workspace paths are configured. Empty means auto-discovery mode. |
| `strands.explore.workspace_root` | `/home/coding/` | Root whose direct child directories are checked for `.beads` when `workspaces` is empty. |

There is therefore one explicitly configured Pluck home path and no explicitly
configured Explore path list. The many repositories below `/home/coding/` are
potentially discovered from the root; they are not individually listed in the
configuration.

The target repository's [`.needle.yaml`](../.needle.yaml) contains only the
backend binding:

```yaml
bead_cli:
  backend: bead-rs
```

It does not override any workspace paths. `/home/coding/claude-governor` is
now the default Pluck home. A worker started with
`--workspace /home/coding/claude-governor` remains explicit and selects the
same repository.

## Where path settings are stored and resolved

NEEDLE loads configuration in this order, with later layers taking precedence:

1. built-in defaults;
2. `~/.config/needle/config.yaml`;
3. the selected workspace's `.needle.yaml`;
4. `NEEDLE_*` environment overrides;
5. CLI overrides, including `needle run --workspace`.

The loader reads the global file from
`~/.config/needle/config.yaml` (`/home/coding/.config/needle/config.yaml` for
this user). A workspace `.needle.yaml` may override agent, prompt, selected
auxiliary-strand, validation, label, and bead-backend settings, but its
`workspace.default`, `workspace.home`, and Explore path settings are not part
of the allowed workspace override. The `--workspace` option is the normal way
to choose Pluck's home workspace for a worker.

`~/.needle/config.yaml` is an older v1 file and is not the source used by the
current loader. Its older `workspace.default` value must not be used when
diagnosing the current worker configuration.

The normal inspection commands are:

```bash
needle config --dump --show-source
needle config --get workspace.default
needle config --get strands.explore.workspace_root
needle config --get strands.explore.workspaces
```

After the runtime fix, `needle config --get workspace.default` resolves to
`/home/coding/claude-governor` and `needle doctor` reports valid configuration.
The OTLP TLS setting is now a structured mapping, so the global file is
loadable by the installed NEEDLE versions.

## How paths affect bead discovery

Each repository has an isolated bead store at `<workspace>/.beads`. A path
does not make beads globally visible: Pluck and Explore must open that specific
workspace's store.

### Pluck: one home workspace

For each worker, the resolved home path is chosen as follows:

```text
needle run --workspace PATH  →  PATH
no --workspace               →  workspace.default
```

Pluck then opens `<resolved-home>/.beads` and asks the configured bead backend
for ready, unassigned beads. If the home directory has no valid `.beads`
directory, Pluck skips with `no_home_store`; it does not search other
repositories to find a replacement home store.

An incorrect `workspace.default` is a complete visibility boundary. Before the
fix, an open bead in `/home/coding/claude-governor/.beads` was invisible when
the worker's resolved home remained `/home/coding/aide-de-camp`; the corrected
default now opens the target store.

### Explore: optional cross-workspace discovery

Explore runs later in the strand flow when it is enabled and the home queue did
not produce work. It uses two mutually exclusive modes:

- **Auto-discovery (`workspaces: []`):** the current implementation reads the
  entries directly under `workspace_root` and selects child directories that
  contain `.beads`. It does not perform arbitrary-depth recursive traversal or
  upward search. With the current root, a repository such as
  `/home/coding/claude-governor/.beads` is a candidate because
  `/home/coding/claude-governor` is a direct child of `/home/coding`.
- **Pinned discovery (`workspaces: [PATH, ...]`):** auto-discovery is disabled
  and only the listed paths are scanned. A non-empty list is an intentional
  restriction; a newly created repository is not found unless it is added to
  the list. The list is also not refreshed by auto-discovery.

Explore skips the resolved home workspace because Pluck already checked it.
For every other selected workspace it opens that workspace's `.beads` store,
queries its ready frontier, and aggregates candidates across the selected
workspaces. Thus, with the current settings, Explore can find work in other
direct-child repositories under `/home/coding/`, but it cannot compensate for
a worker that fails to start because the global config is invalid.

In auto-discovery mode, the current source refreshes the directory scan on each
Explore scan, subject to Explore's scan cadence. New direct-child repositories
can therefore be picked up without editing `workspaces`; changing the config
file itself still requires a new worker because configuration is loaded at
boot.

### Path-related visibility checklist

When a bead appears missing, check these in order:

1. Which path was resolved for the worker? Look for `--workspace` in the launch
   command; otherwise use `workspace.default`.
2. Does `<resolved-home>/.beads` exist and belong to the expected backend?
3. If relying on Explore, is `strands.explore.enabled` true, is the repository
   a direct child of `workspace_root`, and is it absent from a pinned list?
4. Is the repository itself the resolved home? Explore intentionally skips the
   home path because Pluck owns that query.
5. After path selection succeeds, check normal ready-frontier rules such as
   open status, assignment, unresolved blocking dependencies, and excluded
   labels. Those are bead filters, not workspace path settings.

The file `/home/coding/.config/needle/explore-excluded` is not read by the
current NEEDLE Explore implementation and is not an active Pluck path setting.
Likewise, `strands.weave.exclude_workspaces` belongs to Weave and does not
restrict Pluck or Explore.

## Implementation references

The behavior is implemented in the NEEDLE checkout:

- `/home/coding/NEEDLE/src/config/mod.rs` — path fields, configuration
  precedence, and workspace override restrictions.
- `/home/coding/NEEDLE/src/cli/mod.rs` — `--workspace` resolution and opening
  the home bead store.
- `/home/coding/NEEDLE/src/strand/pluck.rs` — Pluck's home-store query.
- `/home/coding/NEEDLE/src/strand/explore.rs` — auto-discovery, pinned mode,
  home-workspace exclusion, and cross-workspace queries.
