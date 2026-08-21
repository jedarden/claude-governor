# Pluck starvation reproduction report

**Bead:** `claudego-f5ec1051`

**Target workspace:** `/home/coding/claude-governor`

**Incident evidence:** 2026-07-06 and 2026-08-03

**Report completed:** 2026-08-20

## Result

The documented failure is the **“37 open, 0 found”** Pluck starvation
condition: work was expected in the target workspace, but Pluck returned zero
candidates and emitted starvation telemetry.

The strongest captured 2026-08-03 evidence shows that this was a workspace
selection failure, not a label-filter failure. Pluck queried `/home/coding`,
whose `.beads` store had five total issues and zero open issues, while the
intended `/home/coding/claude-governor` store had 1,208 issues, 21 open issues,
and 10 ready beads at the time of the capture.

The number 37 is the incident’s expected/precondition count recorded in the
task and analysis. It is not a durable count of the database: nearby
verification runs measured 49–52 open beads, and the current bead-rs store is
different again. The exact bug statement remains: **open work existed in the
intended workspace, while Pluck found 0 in the workspace it actually queried.**

## Environment

### Incident environment

| Item | Value |
| --- | --- |
| Host | Hetzner EX44, Linux 6.12.63 |
| Pluck | NEEDLE `pluck` strand, compiled into NEEDLE |
| NEEDLE binary | `~/.needle/bin/needle-stable` |
| NEEDLE version | `0.2.16` was the deployed stable version documented during this incident; the captured log itself does not print a version |
| Bead backend | Historical bead-forge (`bf`/`br` compatible) |
| Intended workspace | `/home/coding/claude-governor` |
| Workspace used by failing worker | `/home/coding` |
| Intended database | `/home/coding/claude-governor/.beads/beads.db` |
| Database actually queried | `/home/coding/.beads/beads.db` |
| Pluck labels | `deferred`, `human`, `blocked` |
| Pluck split threshold | `3` |

### Current verification context

The repository has since migrated to bead-rs:

```text
needle 0.3.0
NEEDLE commit: 9dfbb0058182b978351f7a8953a5a4e2faa264cf
target commit: dd60860fae6690d373b5fc2031ea97d9f9b88d29
backend: bead-rs
current target store: 60 open, 13 ready
```

The active global config currently defaults to `/home/coding/aide-de-camp`,
not this repository. A worker serving this repository therefore needs an
explicit `--workspace /home/coding/claude-governor`; a relative or unrelated
home path can reproduce the same class of failure.

## Exact commands

The command that exercised the Pluck runtime in the incident was an explicit
NEEDLE worker launch against the wrong home workspace:

```bash
RUST_LOG=debug /home/coding/.needle/bin/needle-stable run \
  --workspace /home/coding \
  --agent claude-code-glm-4_7 \
  --identifier lab-roam3
```

The original shell wrapper was not retained in the stderr artifact; the
command above is the explicit reproduction invocation reconstructed from the
worker identity, workspace, and retained Pluck trace. It is intentionally
pointed at `/home/coding` to reproduce the wrong-store condition. Do not run it
against a live production queue without an isolated worker/home and a claim
safety plan: a real NEEDLE worker can claim beads.

The historical command-level cross-check was:

```bash
RUST_LOG=debug bf ready --limit 0 --format json > pluck-debug-output.txt 2>&1
```

The direct database checks used to compare the two stores were:

```bash
sqlite3 /home/coding/.beads/beads.db \
  "SELECT COUNT(*) FROM issues WHERE status='open';"

sqlite3 /home/coding/claude-governor/.beads/beads.db \
  "SELECT COUNT(*) FROM issues WHERE status='open';"

bf ready --limit 0 --format json | jq -r '.[].id'
```

For the current bead-rs backend, use these read-only commands instead of the
historical `bf`/`br` commands:

```bash
cd /home/coding/claude-governor
bead list --status open --json --limit 999999 | jq -s 'length'
bead list --ready --json --limit 999999 | jq -s 'length'
```

## Full captured Pluck debug output

The following is the complete retained Pluck sequence for the first starvation
cycle, reproduced verbatim from the 2026-08-03 analysis of
`/home/coding/.needle/logs/needle-claude-code-glm-4_7-lab-roam3.stderr.log`
(lines 123–131):

```text
2026-08-03T23:04:53.870031Z DEBUG ...strand.pluck{...}: needle::strand::pluck: Pluck strand evaluation starting exclude_labels=["deferred", "human", "blocked"] split_threshold=3
2026-08-03T23:04:53.870042Z DEBUG ...strand.pluck{...}: needle::strand::pluck: Querying bead store for ready candidates filters=Filters { assignee: None, exclude_labels: ["deferred", "human", "blocked"], exclude_ids: {} }
2026-08-03T23:04:53.873069Z DEBUG ...strand.pluck{...}: needle::strand::pluck: Bead store returned 0 candidates count=0
2026-08-03T23:04:53.873094Z DEBUG ...strand.pluck{...}: needle::strand::pluck: No beads excluded by label filter count=0
2026-08-03T23:04:53.873098Z DEBUG ...strand.pluck{...}: needle::strand::pluck: No beads excluded by status/assignee filter count=0
2026-08-03T23:04:53.876267Z DEBUG ...strand.pluck{...}: needle::telemetry: telemetry event event_type=strand.pluck.starvation_detected seq=21
2026-08-03T23:04:53.876291Z DEBUG ...strand.pluck{...}: needle::strand::pluck: Emitted PluckStarvationDetected telemetry, returning NoWork workspace=. open_count=0 excluded_count=0
2026-08-03T23:04:53.876304Z INFO ...strand.pluck{...}: needle::strand: strand returned no work strand=pluck elapsed_ms=6
```

The important combination is `Bead store returned 0 candidates` plus
`workspace=. open_count=0`. Pluck did not receive 37 candidates and filter
them all out; it opened the wrong store, which contained no open work.

## Independent historical starvation trace

An earlier 2026-07-06 run captured the same failure as progressive candidate
loss. Its direct count was 49 open beads, not 37, which is why the counts must
be labeled by capture date:

```text
$ br list --status open | wc -l
49

2026-07-06T12:43:05.404136Z  INFO ... strand found candidates strand=pluck candidates=6 excluded=17 elapsed_ms=2
2026-07-06T12:43:05.814821Z  INFO ... strand found candidates strand=pluck candidates=5 excluded=18 elapsed_ms=2
2026-07-06T12:43:05.927291Z  INFO ... strand found candidates strand=pluck candidates=4 excluded=19 elapsed_ms=2
2026-07-06T12:43:06.139230Z  INFO ... strand found candidates strand=pluck candidates=3 excluded=20 elapsed_ms=2
2026-07-06T12:43:06.551055Z  INFO ... strand found candidates strand=pluck candidates=2 excluded=21 elapsed_ms=2
2026-07-06T12:43:06.662621Z  INFO ... strand found candidates strand=pluck candidates=1 excluded=22 elapsed_ms=2
2026-07-06T12:43:06.874733Z  INFO ... strand found candidates strand=pluck candidates=0 excluded=23 elapsed_ms=2
2026-07-06T12:43:07.095944Z  INFO ... strand found candidates strand=explore candidates=1 excluded=0 elapsed_ms=62
```

This trace demonstrates that Pluck can drain its candidate set to zero while
Explore still finds work. It is corroborating evidence for starvation, but it
does not by itself prove that all 49 open beads were eligible at every instant;
claims, assignees, dependencies, and concurrent workers can change during the
sequence.

## Database comparison from the incident

The 2026-08-03 investigation recorded this state:

| Query target | Total issues | Open issues | Ready/claimable |
| --- | ---: | ---: | ---: |
| `/home/coding/.beads/beads.db` (worker’s store) | 5 | 0 | 0 |
| `/home/coding/claude-governor/.beads/beads.db` (intended store) | 1,208 | 21 | 10 |

The Pluck debug line reported `workspace=.` because the worker was running
with `/home/coding` as its home. In that process, `.` resolved to the wrong
store. The absence of label and status exclusions (`count=0` for both) is
consistent with an empty wrong store and inconsistent with the hypothesis that
37 target beads were all removed by Pluck labels.

## Diagnosis

1. The target repository contained open/ready work.
2. The worker’s Pluck home was `/home/coding`, not the target repository.
3. Pluck queried `/home/coding/.beads/` and received zero candidates.
4. Pluck emitted starvation telemetry and returned `NoWork`.
5. Explore could still find work because it scans discovered workspaces
   independently; its immediate `candidates=1` result confirms that the fleet
   was not globally empty.

Therefore the reproduced bug is a workspace-path mismatch causing Pluck
starvation. The historical label-filter investigations are separate symptoms
and should not be used to explain this particular `workspace=.` trace.

## References

- `/home/coding/NEEDLE/src/strand/pluck.rs` — Pluck implementation and default
  excluded labels.
- [`pluck-configuration.md`](../plan/pluck-configuration.md) — current
  bead-rs filter contract.
- [`pluck-workspace-paths.md`](../pluck-workspace-paths.md) — workspace
  selection and precedence.
- `notes/bf-3js6h.md` in git history — original 2026-07-06 progressive
  starvation trace.
- `notes/bf-4f5fw.md` in git history — 2026-08-03 zero-candidate trace and
  wrong-store comparison.
