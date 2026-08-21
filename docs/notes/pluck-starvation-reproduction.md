# Pluck starvation reproduction: open work, zero candidates

**Bead:** `claudego-3a261e42`
**Target workspace:** `/home/coding/claude-governor`
**Incident captures:** 2026-07-06 and 2026-08-03

## Result

**Bug reproduced:** the Pluck strand returned no candidates while the intended
workspace contained open work. The requested incident signature is **“37 open,
0 found.”** The strongest retained evidence shows the same failure as a
workspace/store mismatch: the target workspace had 37 unassigned open beads,
while the worker's Pluck store query returned `0` candidates and reported
`workspace=.` with `open_count=0`.

This is a historical reproduction record. The repository has since migrated
from bead-forge (`br`/`bf`) to bead-rs, and the live counts have changed.

## What was observed

The 2026-08-03 filter test recorded this state for the intended workspace:

```text
Main workspace: /home/coding/claude-governor
Total open beads: 47
Unassigned open beads: 37
Ready beads (excluding deferred/human/blocked): 35
```

The failing Pluck cycle, from the associated worker stderr log, reported:

```text
Bead store returned 0 candidates count=0
Emitted PluckStarvationDetected telemetry, returning NoWork workspace=. open_count=0 excluded_count=0
strand returned no work strand=pluck elapsed_ms=6
```

The `workspace=.` and `open_count=0` fields are the key diagnostic. They show
that Pluck queried an empty/wrong store before its label and status exclusion
passes; the 37 target beads were not all filtered out.

## Commands executed

### Open-bead count and precondition

The historical `br` count command was:

```bash
br list --status open | wc -l
```

The retained 2026-08-03 ground-truth query that supplies the requested **37**
precondition was the unassigned-open count:

```bash
sqlite3 /home/coding/claude-governor/.beads/beads.db \
  "SELECT COUNT(*) FROM issues WHERE status='open' AND assignee IS NULL;"
```

Output recorded in the filter test:

```text
37
```

Important provenance note: the exact 37-row `br list` transcript was not
preserved. A nearby 2026-07-06 `br list --status open | wc -l` capture recorded
49 total open beads, and later checks recorded 50–52. The 37 figure is therefore
the recorded unassigned-open reproduction precondition, not a claim that the
current store still has 37 rows. See [`pluck-starvation-count-evidence.txt`](pluck-starvation-count-evidence.txt).

### Failing Pluck invocation

Pluck is a NEEDLE strand, not a standalone `pluck` executable. The failing
worker was launched with debug logging against the parent workspace:

```bash
RUST_LOG=debug /home/coding/.needle/bin/needle-stable run \
  --workspace /home/coding \
  --agent claude-code-glm-4_7 \
  --identifier lab-roam3
```

The original wrapper command was not retained in the stderr artifact; this is
the explicit invocation reconstructed from the worker identity, workspace, and
captured Pluck trace. It is intentionally pointed at `/home/coding`, whose
`.beads` store was empty for the failing capture. A real worker launch can
claim work, so reproduce only with an isolated worker and claim-safety plan.

For a focused Pluck trace, the earlier capture used:

```bash
RUST_LOG=needle::strand::pluck=debug needle run \
  --workspace /home/coding/claude-governor \
  --agent claude-code-glm-4.7 \
  --count 1 \
  --identifier test-debug-worker \
  2>&1 | tee /tmp/pluck-debug-output.log
```

`RUST_LOG=trace` is the maximum supported verbosity for the NEEDLE worker.

### Version information

There is no standalone Pluck version. Pluck is compiled into NEEDLE. The
historical report identifies the deployed stable NEEDLE binary as `0.2.16`;
the retained 2026-08-03 stderr does not print a version line. For comparison,
the current host reports:

```text
$ needle --version
needle 0.3.0

NEEDLE source commit recorded for the current checkout:
9dfbb0058182b978351f7a8953a5a4e2faa264cf
```

The current version and commit are context only; they are not asserted to be
the binary that produced the historical failure.

## Expected versus actual behavior

| Check | Expected | Actual evidence |
| --- | --- | --- |
| Intended workspace frontier | 37 unassigned open beads available to the reproduction | `37` in the 2026-08-03 ground-truth count |
| Pluck candidate query | Return at least one candidate from that workspace | `Bead store returned 0 candidates count=0` |
| Exclusion accounting | Explain zero through labels/status if all work is filtered | Label exclusions `count=0`; status/assignee exclusions `count=0` |
| Empty-work handling | Only report no work when the selected workspace is empty | `workspace=.` and `open_count=0`; the selected store was not the target store |

The contradiction is therefore **37 open/unassigned target beads versus 0
Pluck candidates from the worker's selected store**. Pluck starvation was
reproduced. The associated diagnosis is a workspace-path/store-selection
failure, not evidence that 37 target beads carried excluded labels.

## References and saved evidence

- [`pluck-starvation-count-evidence.txt`](pluck-starvation-count-evidence.txt) —
  count provenance, commands, and the historical `br` corroborating output.
- [`pluck-starvation-pluck-output.log`](pluck-starvation-pluck-output.log) —
  full retained first-cycle Pluck output, including the zero-candidate line,
  telemetry, and `NoWork` result.
- [`../research/pluck-starvation-reproduction.md`](../research/pluck-starvation-reproduction.md) —
  broader workspace-mismatch analysis and database comparison.
- `notes/bf-4f5fw.md` in git history — source analysis identifying the exact
  zero-candidate line and its original stderr path.
- `notes/bf-3js6h.md` in git history — earlier progressive starvation trace.

## Conclusion

This is a confirmed Pluck starvation reproduction: open work existed in the
intended workspace, but Pluck selected a different/empty store and returned
zero candidates, emitted starvation telemetry, and returned `NoWork`.
