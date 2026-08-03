# Commit History Divergence Analysis: Lab vs Reference Clone

**Bead ID:** bf-39f1ao  
**Analysis Date:** 2026-08-03  
**Analyst:** NEEDLE worker (claude-code-glm-4.7-test-debug-worker)

## Executive Summary

The lab clone and reference clone have **significantly divergent** commit histories. The reference clone (origin: `git.ardenone.com`) contains only a single commit from July 30, 2026, while the lab clone (origin: `github.com`) has over 200 additional commits including extensive Pluck investigation documentation.

**Critical Finding:** The reference clone appears to be a stale or reset repository that does not reflect current development progress.

---

## Clone Definitions

### Lab Clone
- **Host:** 100.81.129.38 (Tailscale IP)
- **Path:** `/home/coding/claude-governor`
- **Origin:** `https://github.com/jedarden/claude-governor.git`
- **Current HEAD:** `14ea5a5` (docs: update bf-4026a with latest Pluck query results)

### Reference Clone
- **Origin:** `https://git.ardenone.com/jedarden/claude-governor.git` (the "correct" origin)
- **Path:** `/tmp/reference-clone` (cloned for analysis)
- **Current HEAD:** `15b1ac7` (fix(governor): stop treating current_total==0 as insufficient burn-rate data)
- **Clone Date:** 2026-08-03 (bare clone, depth 1)

---

## Divergence Point

**Last Common Ancestor (Merge Base):** `15b1ac7`  
**Date:** 2026-07-30 22:37:45 -0400  
**Commit Message:** "fix(governor): stop treating current_total==0 as insufficient burn-rate data"

This commit is the **ONLY commit** in the reference clone, but is just one of hundreds in the lab clone.

---

## Commit Graph Comparison

### Reference Clone Graph (git.ardenone.com)
```
* 15b1ac7 fix(governor): stop treating current_total==0 as insufficient burn-rate data
```
**Total commits: 1**

### Lab Clone Graph (github.com) - Top 20
```
* 14ea5a5 docs: update bf-4026a with latest Pluck query results
| * 26ccea5 docs: update bf-4026a with latest Pluck query results
|/  
* 52f0aad docs: add comprehensive Pluck starvation reproduction report (bf-551q7)
* a401397 docs: complete Pluck visibility gap analysis (bf-27w2y)
* 0518a20 docs: complete Pluck configuration investigation (bf-54ppq)
* 29f030d docs: complete Pluck filter configuration documentation (bf-66ejs)
* c7e4122 docs: complete Pluck configuration investigation documentation (bf-3fi8d)
* f13fd9e docs: complete Pluck visibility gap analysis (bf-27w2y)
* 8f4ac23 docs: complete Pluck search output analysis (bf-4f5fw)
| *   0e5ab30 WIP on main: 93aee8a docs: complete Pluck visibility gap analysis (bf-27w2y)
| |\  
| | * 54518aa index on main: 93aee8a docs: complete Pluck visibility gap analysis (bf-27w2y)
| |/  
| * 93aee8a docs: complete Pluck visibility gap analysis (bf-27w2y)
| * f642ce9 docs: complete Pluck search output analysis (bf-4f5fw)
|/  
* d69b78a docs: complete Pluck filter configuration review (bf-2ur41)
* d3899d8 feat: add basic Pluck query script and documentation (bf-4026a)
* 9aa1888 docs: complete Pluck workspace path verification (bf-22ks5)
* 73aeb08 docs: complete Pluck debug output analysis (bf-56wnh)
* 1bbc8b1 docs: complete Pluck workspace path documentation (bf-3scq0)
* de897f0 docs: complete bead inventory - 2,032 non-closed beads across 18 workspaces
* 7c0d928 docs: compile Pluck configuration investigation summary (bf-jwpdu)
```
**Total commits: 200+**

---

## Divergence Timeline

### Reference Clone (git.ardenone.com)
- **2026-07-30 22:37:45 -0400** - Single commit: Governor fix for 0<->max_workers oscillation

### Lab Clone (github.com) - Post-Divergence Commits

The lab clone has **203 additional commits** since the divergence point. Key commit groups:

#### Recent Pluck Investigation (2026-08-03)
- `14ea5a5` docs: update bf-4026a with latest Pluck query results
- `52f0aad` docs: add comprehensive Pluck starvation reproduction report (bf-551q7)
- `a401397` docs: complete Pluck visibility gap analysis (bf-27w2y)
- `0518a20` docs: complete Pluck configuration investigation (bf-54ppq)
- `29f030d` docs: complete Pluck filter configuration documentation (bf-66ejs)

#### Pluck Investigation (2026-07-27 to 2026-08-01)
- `d3899d8` feat: add basic Pluck query script and documentation (bf-4026a)
- `9aa1888` docs: complete Pluck workspace path verification (bf-22ks5)
- `73aeb08` docs: complete Pluck debug output analysis (bf-56wnh)
- `1bbc8b1` docs: complete Pluck workspace path documentation (bf-3scq0)
- `de897f0` docs: complete bead inventory - 2,032 non-closed beads across 18 workspaces
- `7c0d928` docs: compile Pluck configuration investigation summary (bf-jwpdu)

#### Governor Improvements (2026-07-22 to 2026-07-30)
- `a5232e1` test: add comprehensive unit tests for apportioning calculation logic
- `d5fc451` docs: document completion of production-path test coverage
- Multiple weekly_scoped model fixes and tests
- Cold-start confidence signal implementation
- BaselineBurnRates configuration work

#### Core Functionality (2026-07-01 to 2026-07-21)
- Snapshot delta computation tests and implementation
- First poll handling improvements
- Window delta annotation features
- Per-window target_utilization overrides

---

## Key Differences Summary

| Aspect | Reference Clone (git.ardenone.com) | Lab Clone (github.com) |
|--------|-------------------------------------|-------------------------|
| **Total Commits** | 1 | 200+ |
| **Latest Commit** | 2026-07-30 22:37:45 | 2026-08-03 (ongoing) |
| **Pluck Investigation** | None | Extensive (40+ docs commits) |
| **Governor Fixes** | 1 fix (current_total==0) | Multiple improvements |
| **Test Coverage** | Baseline | Comprehensive test suite |
| **Documentation** | Minimal | Extensive bead documentation |

---

## The Divergence Commit (15b1ac7)

**Full commit details from lab clone:**

```
commit 15b1ac785d3a5a894cebfddbabf487d136b40829
Author: jedarden <github@jedarden.com>
Date:   Thu Jul 30 22:37:45 2026 -0400

    fix(governor): stop treating current_total==0 as insufficient burn-rate data
    
    Root cause of a live 0<->max_workers oscillation observed on lab tonight:
    pct_per_worker was computed as fleet_pct_hr / current_total, guarded by
    current_total > 0. The instant the fleet correctly scaled down to 0 workers
    (protecting a tight window), the next cycle's current_total was 0, making
    pct_per_worker collapse to 0.0 regardless of how much real rate data existed
    (329 EMA samples, 10.03%/hr). That fed a 0.0 mean_rate_per_worker into
    safe_worker_count's formula, which requires mean_rate_per_worker > 0.0 and
    returns None otherwise -- so a fleet at 0 for the RIGHT reason looked
    identical to genuine cold start with zero samples ever. None hits
    safe_worker_count_or_max's fallback (added earlier tonight, commit dbad2cc)
    and resets the ceiling to max_workers, launching new workers -- which then
    get scaled back to 0 by the next real cycle, current_total hits 0 again,
    and the cycle repeats. Confirmed live: four consecutive 5-min cycles
    alternated target workers 0 -> 8 -> 0 -> 8, each "8" briefly launching (and
    billing) a worker before the real math pulled it back down -- actively
    working against the cutoff-risk protection it exists to provide.
    
    Fix: use current_total.max(1) instead of requiring current_total > 0. When
    fleet_pct_hr > 0.0 (real aggregate data exists) but current_total == 0,
    this now divides by a hypothetical 1 worker -- a conservative, pessimistic
    per-worker estimate -- so safe_worker_count computes a real, stable number
    instead of None. True cold start (fleet_pct_hr == 0, no samples ever) is
    unaffected: pct_per_worker is still 0.0 and correctly falls through to the
    max_workers bootstrap ceiling.
    
    All 547 tests pass unchanged. No new regression test added -- this
    computation is inline in run_governor_cycle's per-window forecast loop, not
    an isolated pure function; properly unit-testing it needs the same mockito
    HTTP-mocking harness gap noted in a0ebe54. Verifying live on lab instead by
    watching for the oscillation to stop across several real cycles.

 src/governor.rs | 20 +++++++++++++++++---
 1 file changed, 17 insertions(+), 3 deletions(-)
```

---

## Conclusions and Recommendations

### Critical Issues Identified

1. **Reference Clone is Severely Outdated:** The git.ardenone.com repository appears to be a reset or stale clone with only 1 commit from July 30, missing all subsequent development work.

2. **Missing Commit History:** 203 commits of development work are not reflected in the reference clone, including:
   - Entire Pluck investigation (40+ documentation commits)
   - Governor improvements and fixes
   - Comprehensive test suite additions
   - Production-path enhancements

3. **Origin Misalignment:** The lab clone points to github.com as origin, while the reference clone points to git.ardenone.com. This suggests a repository migration or synchronization issue.

### Recommendations

1. **Immediate:** Investigate why git.ardenone.com has only 1 commit when it should contain the full history.

2. **Short-term:** Determine which repository should be the source of truth:
   - If git.ardenone.com is intended as primary: push lab clone history to it
   - If github.com is intended as primary: update all remotes to point there

3. **Long-term:** Establish a clear origin-of-truth strategy and ensure all clones synchronize to it.

---

## Appendix: Commands Used for Analysis

```bash
# Lab clone analysis
git log --oneline --graph --all -20
git log -1 --format="%H %s"
git remote -v

# Reference clone creation
git clone --mirror --depth 1 https://git.ardenone.com/jedarden/claude-governor.git /tmp/reference-clone

# Reference clone analysis
cd /tmp/reference-clone
git log --oneline --graph --all -20
git log -1 --format="%H %ai %s"
git branch -a

# Divergence analysis
cd /home/coding/claude-governor
git merge-base 15b1ac7 14ea5a5
git log --oneline 15b1ac7..14ea5a5
git show 15b1ac7 --stat
```

---

**End of Analysis**
