# Pluck filter root-cause findings

**Verified:** 2026-08-21
**Target workspace:** `/home/coding/claude-governor`
**Backend:** `bead-rs`

## Applied runtime fix

The live NEEDLE configuration at `/home/coding/.config/needle/config.yaml` was
updated on 2026-08-21:

- `workspace.default` now points to `/home/coding/claude-governor` instead of
  the unrelated `/home/coding/aide-de-camp` store.
- `telemetry.otlp_sink.tls: none` is now the structured
  `{ insecure: true, ca_file: '' }` form accepted by the installed NEEDLE
  binaries.

Both `needle 0.3.0` and the active `needle 0.4.2` resolve the target workspace
after the change. `needle doctor --workspace /home/coding/claude-governor`
passes configuration, bead-store, and SQLite checks. The repository Pluck
integration test passes 2/2 and the bead-rs ready query returns 7 candidates at
verification time.

## Verdict

The historical “open beads, zero Pluck candidates” incident was caused by a
workspace/store mismatch, not by an overly broad `exclude_labels` list.
Pluck queried a different, empty store (`/home/coding/.beads/beads.db`) while
the work being counted was in
`/home/coding/claude-governor/.beads/beads.db`. The retained starvation trace
reported `workspace=.` and `open_count=0`, with zero beads attributed to label
or status/assignee exclusion.

Before the runtime fix, the global NEEDLE configuration had the same class of
risk: its `workspace.default` was `/home/coding/aide-de-camp`, not this
repository. The default is now aligned, and dedicated workers should still
prefer an explicit absolute workspace in their launch command.

The configured labels are not too broad. Matching is exact and case-sensitive;
a bead is removed only when one of its labels is exactly one of the configured
values. The current target database has one label row, attached to a closed
issue, so the four configured labels remove zero of its ready candidates.

## Evidence from the filter tests

Counts are historical snapshots and differ as the queue changed. They are
included to show the filter transition, not as a current backlog promise.

| Test/evidence | Result | Interpretation |
| --- | --- | --- |
| Historical label isolation (`bf-1y51s`) | No exclusions: 45; exclude `deferred`: 28; exclude `human`, `blocked`, or `starvation-alert` individually: 45 | Only `deferred` matched open beads in that snapshot; the configured list was not broad enough to explain zero. |
| Historical filter combinations (`pluck_filter_combinations_test`) | On the correct store: base 1,262; `open` 41; unassigned 373; labels only 1,179; `open + unassigned` 40; `open + labels` 36; unassigned + labels 369; full query 36 | All eight combinations returned candidates. No filter combination was blocking. |
| Historical workspace mismatch (`pluck_workspace_mismatch_test`) | Target store: 36 ready; parent store: 0 open and 0 ready | Selecting the parent store explains the zero result. |
| Current `pluck_db_test` | 2 tests passed; target path and `bead list --ready --json --limit 999999` verified; backend returned 7 ready candidates; one label row belongs to a closed issue | The current bead-rs path and filter configuration work. Its separate SQL simulation reports 21 because it does not model dependency readiness. |
| Current `pluck_filter_combinations_test` | Test process passed, but only 4 of 8 scenarios ran; four legacy queries failed with `no such column: status` | This test is stale for bead-rs (`base_status` replaced `status`). Its exit code does not validate the old SQL filter matrix. |
| Current `test_workspace_path_formats` | Test process passed, but no format was reported successful because its readiness query uses the removed `status` column | Absolute target, `.`, trailing-slash, and double-slash paths opened the target file; the readiness assertion is stale. |
| Current `tests/test_exclude_labels.sh` | Failed immediately with `no such column: i.status` | The shell test is also legacy `bf`/`br` SQL and cannot measure current label behavior. |

The current diagnostic tests therefore need a future bead-rs-native cleanup,
but their failures are schema incompatibilities, not evidence that labels hide
all work. The historical captured results and the current `bead list` result
agree on the root cause.

## Correct configuration

For a worker dedicated to this repository, use an absolute workspace path. Do
not rely on `.` or on a global default that points at another repository:

```yaml
# ~/.config/needle/config.yaml (target-specific example)
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
    split_after_failures: 3
    persistent_starvation_records: false
```

The target repository's `.needle.yaml` must select the current backend:

```yaml
bead_cli:
  backend: bead-rs
```

If the global config serves multiple repositories, keep its shared default and
pass the target explicitly instead:

```bash
needle run --workspace /home/coding/claude-governor --agent <agent-name>
```

Restart NEEDLE after changing configuration; workers load it at startup. The
global file is now schema-aligned with both the installed `needle 0.3.0` and
the active `needle 0.4.2`, so the Pluck settings are runtime-effective.

An empty `exclude_labels` list does not disable filtering: NEEDLE falls back to
`deferred`, `human`, and `blocked`. A non-empty list replaces that fallback,
so repeat every default label that should remain excluded when adding a custom
label.

## Working filter example

This is a non-claiming check that uses the same workspace and exact label
semantics as Pluck. It is safe to run from the repository and does not mutate
the bead store:

```bash
cd /home/coding/claude-governor
bead list --ready --json --limit 999999 \
  | jq -s '
      map(select(
        any(.labels[]?;
          . == "deferred" or
          . == "human" or
          . == "blocked" or
          . == "starvation-alert"
        ) | not
      ))
      | length
    '
```

On the verification run above, this returned `7`. The important diagnostic
comparison is:

```bash
cd /home/coding/claude-governor
bead list --status open --json --limit 999999 | jq -s 'length'   # 21
bead list --ready --json --limit 999999 | jq -s 'length'         # 7
```

If the first command reports open work but the second reports zero, inspect
assignees, manual blocks, and unfinished `blocks` dependencies before changing
labels. If the backend returns zero and Pluck reports `open_count=0`, first
verify the resolved workspace and `.beads/beads.db` path; do not conclude that
`exclude_labels` caused the starvation until the correct store has been
queried.

## References

- [`docs/plan/pluck-configuration.md`](../plan/pluck-configuration.md) —
  current filter and readiness contract.
- [`docs/research/pluck-starvation-reproduction.md`](pluck-starvation-reproduction.md) —
  retained historical starvation trace and workspace comparison.
- [`docs/notes/pluck-starvation-pluck-output.log`](../notes/pluck-starvation-pluck-output.log) —
  original zero-candidate Pluck trace.
- [`tests/pluck_db_test.rs`](../../tests/pluck_db_test.rs) — current bead-rs
  query/parameter checks.
