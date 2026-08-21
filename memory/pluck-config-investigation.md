---
name: pluck-config-investigation
description: Current and historical Pluck workspace, filter, and connectivity findings
metadata:
  type: project
  bead_id: claudego-a4646171
  verified: 2026-08-21T11:37:09Z
---

# Pluck Configuration Investigation Summary

**Target workspace:** /home/coding/claude-governor
**Backend:** bead-rs (selected by the target .needle.yaml)
**Verification date:** 2026-08-21

## Executive summary

The target bead store is healthy and readable. The primary confirmed cause of
the historical “open beads but Pluck finds no work” incident was workspace
selection: the worker queried a different home store than the target repository.
The runtime fix aligned the global default with
/home/coding/claude-governor, so workers without an explicit workspace now
select the target store.

The same fix also replaced the incompatible
telemetry.otlp_sink.tls: "none" string with the structured
`{ insecure: true, ca_file: '' }` form. Both the installed NEEDLE 0.3.0 and
active 0.4.2 binaries now load the global configuration successfully.

The child-investigation snapshot found 48 open issues and 11 in bead-rs’s
actual ready frontier. A later read-only check at 2026-08-21T11:37:09Z found
26 open and 7 ready; counts are expected to change as workers claim and close
beads. The database has one `deferred` label row, attached to a closed issue,
so no open issue is currently removed by `exclude_labels`. In that follow-up,
19 open issues were held by unfinished `blocks` dependencies, not labels.

The implementation-phase verification found 21 open issues and 7 ready
candidates. The repository Pluck integration test passed 2/2, and
`needle doctor --workspace /home/coding/claude-governor` passed all required
configuration, backend, and SQLite checks (with one unrelated stale-heartbeat
warning).

## Workspace path findings

### Pre-fix configured paths (investigation snapshot)

The following values record the state observed before the implementation fix in
/home/coding/.config/needle/config.yaml:

    workspace:
      default: /home/coding/aide-de-camp
      home: /home/coding/.needle

    strands:
      explore:
        enabled: true
        workspaces: []
        workspace_root: /home/coding/

| Setting | Value | Effect |
|---|---|---|
| workspace.default | /home/coding/aide-de-camp | Pluck home when no CLI workspace is supplied. |
| workspace.home | /home/coding/.needle | NEEDLE state, logs, heartbeats, and optional starvation records; not a bead store. |
| strands.explore.enabled | true | Enables later cross-workspace discovery. |
| strands.explore.workspaces | [] | Auto-discover direct child directories under workspace_root. |
| strands.explore.workspace_root | /home/coding/ | Explore scan root. |

The target .needle.yaml contains only:

    bead_cli:
      backend: bead-rs

It does not override workspace paths. A worker started without
--workspace /home/coding/claude-governor therefore selects the configured
aide-de-camp store. Pluck opens only <resolved-workspace>/.beads; it does
not search upward or substitute another repository when the home store is
missing. A missing home store produces Skipped(no_home_store).

Explore can discover /home/coding/claude-governor because it is a direct child
of /home/coding/, but Explore is a separate later strand and cannot compensate
for a worker that fails global configuration loading.

Configuration precedence is: built-in defaults, global config, supported
workspace overrides, environment overrides, then CLI overrides. The CLI
--workspace is the normal explicit selection mechanism; configuration is read
when a worker starts.

### Store comparison at child-investigation verification time

| Store | Database size | Total issues | Open issues | Ready issues |
|---|---:|---:|---:|---:|
| Target /home/coding/claude-governor/.beads/beads.db | 2,179,072 bytes | 1,142 | 48 | 11 |
| Configured default /home/coding/aide-de-camp/.beads/beads.db | 4,374,528 bytes | 1,388 | 21 | 3 |
| Historical /home/coding/.beads/beads.db | absent now | — | — | — |

The configured default is a valid, nonempty store, but it is the wrong store
for this repository. The historical starvation capture used /home/coding,
which at that time contained five issues and zero open issues.

## Filter and label settings

The configured strands.pluck section is:

    strands:
      pluck:
        exclude_labels:
          - deferred
          - human
          - blocked
          - starvation-alert
        split_after_failures: 3
        persistent_starvation_records: false

NEEDLE’s PluckConfig has exactly these three configurable keys:

| Key | Value | Visibility effect |
|---|---:|---|
| exclude_labels | deferred, human, blocked, starvation-alert | Excludes a candidate with any exact matching label. |
| split_after_failures | 3 | Splits instead of dispatching when the first sorted candidate has at least three failures. |
| persistent_starvation_records | false | Emits starvation telemetry but does not append a persistent starvation record. |

Label matching is exact and case-sensitive. There is no wildcard, prefix,
regular-expression, or case-folded matching. A nonempty custom list replaces
the compiled fallback; it does not merge with it. If the key is omitted or
empty, the built-in fallback is deferred, human, and blocked.

There are no active required-label filters. Labels such as polish, rust,
documentation, and failure-count:N are not excluded unless explicitly
listed. failure-count:N is used separately for ordering and split behavior.

### Candidate pipeline

1. Pluck resolves one home workspace and opens its .beads store.
2. The bead-rs ready query requires base_status = open, no assignee,
   manual_blocked = 0 (or null), and no unfinished blocks dependency.
   relates_to dependencies do not block readiness.
3. The store adapter applies the four exact exclude_labels and an empty
   configured ID-exclusion set.
4. Pluck defensively removes in_progress and still-assigned open beads.
5. Candidates are ordered by priority ASC, failure count, created_at ASC,
   and id ASC. Failure count is the maximum valid integer in any
   failure-count:N label.
6. The worker also maintains transient ID exclusions for claim races/retries;
   these are not persistent Pluck configuration.

The ready frontier has no independent filters for pinned, ephemeral, template,
due date, defer_until, or positive labels. Manual blocking and unfinished
blocking dependencies are state/graph rules, not labels.

### Current target-store impact

Read-only checks on 2026-08-21 found:

- 48 open issues, all unassigned.
- 0 open manually-blocked issues.
- 0 label rows and 0 labeled issues; therefore none are excluded by the four
  configured labels.
- 37 open issues covered by 41 unfinished blocks dependency rows.
- 11 ready issues returned by bead list --ready --json --limit 999999.

The 11 ready issues are the authoritative current candidate count. A simple
status/assignee/label SQL query returns 48 because it omits dependency
readiness; that simplified result must not be called the bead-rs ready count.

### Follow-up live sanity check

At 2026-08-21T11:37:09Z, read-only checks of the same target store found:

- 26 open issues, all unassigned.
- 0 open manually-blocked issues.
- 1 label row overall (`deferred` on a closed issue), and 0 open labeled
  issues; therefore no open issue was excluded by the four configured labels.
- 19 open issues with unfinished `blocks` dependencies.
- 7 issues returned by `bead list --ready --json --limit 999999`.
- SQLite `PRAGMA integrity_check` still returned `ok`.

This follow-up confirms that the earlier 48/11 values are a valid point-in-time
snapshot, not a fixed invariant. Use the ready-frontier command for the live
candidate count.

## Connectivity and verification results

### Direct store checks

All checks were read-only:

- sqlite3 -readonly .beads/beads.db "PRAGMA integrity_check;" -> ok.
- Expected tables issues, labels, events, and dependencies exist.
- The current bead-rs schema uses issues.base_status and dependency columns
  blocked_issue_id / blocker_issue_id; it does not have the old status column.
- bead list --status open --json --limit 999999 -> 48 records.
- bead list --ready --json --limit 999999 -> 11 records.
- bead doctor passed database integrity, schema validity, dependency graph,
  ready-frontier, comments-integrity, and backup-generation checks. It reported
  one checkpoint-freshness warning because the live checkpoint was dirty
  (covered=1511, current=1512); this is bead-state freshness, not database
  corruption.

### Repository connectivity test

Command:

    ~/.cargo/bin/cargo test --test pluck_db_test -- --nocapture

Result: pass, 1 test, 0 failed. It verified:

- database file exists;
- SQLite connection succeeds;
- PRAGMA integrity_check succeeds;
- expected schema tables exist;
- database totals: 1,142 issues, 48 open, 0 labeled issues;
- the constructed Pluck-style query executes successfully.

Important limitation: this test’s simulated query uses only the historical
three labels (deferred, human, blocked) and omits dependency readiness, so
it reports 48 “claimable” issues. That is a connectivity/query-construction
pass, not the authoritative current ready-frontier result of 11.

### Historical diagnostic-test caveats

The following tests exit successfully but contain stale bf/br-era SQL or
non-failing diagnostics:

- pluck_filter_combinations_test reports no such column: status for its
  status-based scenarios; only four of eight scenarios execute.
- test_workspace_path_formats connects to valid paths but its old
  status-based count and ready queries fail with the same schema error.
- pluck_workspace_mismatch_test confirms the target path is accessible and
  /home/coding/.beads/beads.db is absent now, but its printed counts and
  “36 ready” conclusion are historical hard-coded text, not current results.

These tests should be migrated to base_status, current dependency columns,
and bead list --ready semantics before being used as regression gates.

### Global config resolution before the fix

needle --version reports needle 0.3.0. Both needle config --get
workspace.default and needle config --dump --show-source fail before
resolution with:

    telemetry.otlp_sink.tls: invalid type: string "none",
    expected struct OtlpTlsConfig

This was the pre-fix state: workspace/filter values were inspected directly
from the global YAML, but runtime config loading was blocked. After the fix,
`needle config --get workspace.default` resolves to
`/home/coding/claude-governor`; the structured OTLP TLS mapping is accepted by
both installed NEEDLE versions.

## Historical investigation findings

These findings explain the original incident but use the retired bead-forge
backend and must not be mixed with the current bead-rs counts.

### 2026-08-03 zero-candidate trace

The strongest retained Pluck trace showed:

    Querying bead store ... exclude_labels=["deferred", "human", "blocked"]
    Bead store returned 0 candidates count=0
    No beads excluded by label filter count=0
    No beads excluded by status/assignee filter count=0
    NoWork workspace=. open_count=0 excluded_count=0

At that capture, /home/coding/.beads had 5 total and 0 open issues, while
/home/coding/claude-governor/.beads had 1,208 total, 21 open, and 10 ready.
This is evidence of querying the wrong store, not evidence that 37 target beads
were all removed by labels. The “37 open, 0 found” wording is an incident
precondition; nearby captures measured 49–52 open and counts changed as workers
claimed work.

### 2026-07-06 progressive starvation trace

An older trace showed Pluck candidates falling from 6 to 0 while Explore found
one candidate. It corroborates starvation and concurrent queue changes, but does
not prove that all 49 open issues were eligible at every instant.

### 2026-08-04 deferred-label observation

The old pluck-debug-complete-output.txt recorded a bead-forge defect where
bf-156nn7 carried deferred but appeared in bf ready (41 open, 5 deferred,
expected 5 versus actual 6). That report used the old schema, blocked_cache,
and bf/br commands. It is retained as historical evidence of a separate
label-query problem; it is not a current bead-rs finding. The current target
labels table is empty and current readiness is dependency-driven.

## Issues and recommendations

1. Completed: restore runtime config loading by using the structured OTLP TLS
   value accepted by NEEDLE 0.3.0 and 0.4.2.
2. Completed: align Pluck’s default home workspace with
   /home/coding/claude-governor. Dedicated launch commands should still use the
   explicit absolute --workspace /home/coding/claude-governor.
3. Use the ready frontier for health checks. Report open, assigned, manual
   blocked, unfinished dependency-blocked, label-excluded, and ready counts
   separately. The pre-fix snapshot was 48 open, 0 label exclusions, 37
   dependency-blocked, and 11 ready; implementation verification later found
   21 open and 7 ready.
4. Refresh investigation tests and historical docs. Replace status with
   base_status, use bead-rs dependency columns, include all four configured
   labels, and assert query failures instead of printing them and passing.
5. Review the 37 dependency-blocked issues. Confirm their blockers are
   intentional and close or repair stale dependency chains where appropriate.
6. Enable persistent starvation records temporarily if needed. The current
   setting emits telemetry but does not retain a JSONL record under
   /home/coding/.needle/state/starvation-records.jsonl.

## Source references

- docs/pluck-workspace-paths.md — path resolution and Explore behavior.
- docs/plan/pluck-configuration.md — current filter contract and bead-rs
  readiness rules.
- docs/research/pluck-starvation-reproduction.md — historical wrong-store
  reproduction.
- tests/pluck_db_test.rs — connectivity and query-construction test.
- /home/coding/NEEDLE/src/strand/pluck.rs — Pluck implementation and compiled
  defaults.
- /home/coding/.config/needle/config.yaml — host-level configuration inspected
  for this report.
