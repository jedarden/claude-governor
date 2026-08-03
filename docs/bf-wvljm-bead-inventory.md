# Bead Inventory Report (bf-wvljm)

**Generated:** 2026-08-03
**Scope:** 18 workspaces (major active repos)
**Total non-closed beads:** 2,032

## Executive Summary

Across 18 sampled workspaces, there are **2,032 non-closed beads** distributed across:
- **1,050 blocked** (51.7%)
- **904 open** (44.5%)
- **59 in_progress** (2.9%)
- **14 other** (0.7%): done(8), completed(5), pending(4), resolved(1), ready(1)

## Workspace Distribution

### Top 10 Workspaces by Bead Count

| Workspace | Total Beads | % of Sample |
|-----------|-------------|-------------|
| pdftract | 305 | 15.0% |
| bead-forge | 242 | 11.9% |
| NEEDLE | 233 | 11.5% |
| HOOP | 222 | 10.9% |
| AgentScribe | 200 | 9.8% |
| SIGIL | 144 | 7.1% |
| vista | 98 | 4.8% |
| SEAM | 93 | 4.6% |
| claude-governor | 89 | 4.4% |
| mta-my-way | 89 | 4.4% |

## Status Analysis

### Blocked Beads by Workspace

**1,050 total blocked beads (51.7% of all non-closed)**

| Workspace | Blocked Count | % of Workspace |
|-----------|---------------|----------------|
| bead-forge | 183 | 75.6% |
| AgentScribe | 176 | 88.0% |
| NEEDLE | 129 | 55.4% |
| HOOP | 98 | 44.1% |
| vista | 77 | 78.6% |
| telegram-claude-bridge | 62 | 59.8% |
| claude-governor | 60 | 67.4% |
| pdftract | 52 | 17.0% |
| ARMOR | 46 | 71.9% |
| mta-my-way | 40 | 44.9% |

**Pattern:** AgentScribe, bead-forge, and vista have the highest blocked percentages (75%+), indicating potential dependency chain issues or stale blockers.

### Open (Ready) Beads by Workspace

**904 total open beads (44.5%)**

pdftract leads with 253 open beads (83% of its total), suggesting active healthy backlog.

### In-Progress Beads by Workspace

**59 total in_progress beads (2.9%)**

| Workspace | In-Progress |
|-----------|-------------|
| HOOP | 20 |
| NEEDLE | 9 |
| SIGIL | 9 |
| ARMOR | 8 |
| claude-governor | 5 |
| pdftract | 5 |
| spaxel | 3 |

## Label Analysis

### Visibility-Affecting Labels

These labels directly impact bead visibility and dispatch:

| Label | Count | Impact |
|-------|-------|--------|
| split-child | 156 | Indicates bead that was part of a larger bead that was split |
| cycling | 123 | Bead that repeatedly fails and gets requeued |
| deferred | 102 | Explicitly deferred from processing |
| umbrella | 42 | Parent bead grouping multiple related tasks |
| needs-human-review | 3 | Requires human intervention |
| verification-failed | 2 | Automated verification failed |

**Key Finding:** 156 beads labeled `split-child` and 123 labeled `cycling` represent systemic issues in bead management - either repeated failures or incomplete splits.

### Failure Count Labels

Labels tracking retry attempts:

| Label | Count |
|-------|-------|
| failure-count:5 | 104 |
| failure-count:1 | 28 |
| failure-count:2 | 10 |
| failure-count:4 | 7 |
| failure-count:6 | 6 |
| failure-count:7 | 3 |
| failure-count:8 | 1 |
| failure-count:9 | 1 |
| failure-count:51 | 1 |
| failure-count:55 | 1 |
| failure-count:74 | 1 |
| failure-count:283 | 1 |
| failure-count:297 | 1 |
| failure-count:301 | 1 |

**Critical Finding:** Several beads have extremely high failure counts (283, 297, 301), indicating systemic processing issues or beads that are fundamentally broken/stuck.

## Patterns and Issues

### 1. Blocked Bead Accumulation

- **51.7%** of all non-closed beads are blocked
- **AgentScribe** (88%) and **bead-forge** (76%) have highest blocked ratios
- **Claude-governor** has 60 blocked beads (67%)

**Implication:** Many workspaces have accumulated blocked beads that may never become ready due to:
- Stale dependency chains (blockers already closed but beads never updated)
- Circular dependencies
- Forgotten/abandoned beads

### 2. Failure Count Inflation

- **104 beads** with failure-count:5 (likely at dispatch limit)
- **3 beads** with extreme failure counts (283, 297, 301)
- **pattern:** High failure counts correlate with cycling label

**Implication:** Dispatch system repeatedly attempts beads that consistently fail, consuming capacity without progress.

### 3. Split-Child Bead Accumulation

- **156 split-child** beads across workspaces
- These beads may represent incomplete split operations

**Implication:** Large beads were split but parent-child relationships not properly resolved.

### 4. Workspace-Specific Issues

**claude-governor:**
- 60 blocked beads (67% of workspace)
- 5 in_progress (active investigation)
- Related to Pluck configuration investigation (beads bf-22ks5, bf-wvljm, bf-3scq0, etc.)

**bead-forge:**
- 242 total beads, 183 blocked (76%)
- Highest absolute blocked count
- May need bulk status reconciliation

## Visibility Comparison

### Expected vs Actual Visibility

Based on configuration documentation (from parent bead investigation), Pluck should discover:
- All beads in configured workspaces
- Excluding beads with `deferred` label (102 beads)
- Filtering by status based on configuration

**Actual Discovery:** 0 beads (per investigation in parent beads)

**Root Cause:** Investigation ongoing (see related beads bf-22ks5, bf-3scq0, bf-4c4ip)

## Recommendations

1. **Bulk Status Reconciliation:** Run sweep to identify blocked beads with satisfied dependencies and flip to open (as documented in bead bf-1rac5m)

2. **High Failure Count Cleanup:** Investigate and manually resolve beads with failure-count > 50

3. **Split-Child Resolution:** Audit 156 split-child beads for proper parent-child relationship closure

4. **Blocked Bead Audit:** Review high-blocked-percentage workspaces (AgentScribe, bead-forge, vista) for systemic issues

5. **Pluck Configuration Fix:** Complete ongoing investigation (bf-22ks5, bf-3scq0) to restore bead discovery

## Methodology

**Sample Size:** 18 workspaces (selected from 41 total bead workspaces)
**Sampling Method:** Direct `br list --json` queries per workspace
**Exclusions:** 23 workspaces not sampled (likely idle/archived)
**Timestamp:** 2026-08-03T23:41:20Z (bead creation timestamp)
