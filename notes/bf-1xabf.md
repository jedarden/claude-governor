# Bead Count Verification - bf-1xabf

**Date:** 2026-07-06 19:05:35 EDT

## Task
Verify Pluck workspace has exactly 37 open beads.

## Finding
The workspace has **50 open beads**, not 37 as expected.

## Verification Commands
```bash
$ br list --status open | wc -l
50
```

## Complete Output
```
[bf-21swe] Verify safe-mode warning message fix works correctly - open (P2)
[bf-g7tl4] Write stdout notification verification test - open (P2)
[bf-5enwf] Run full verification and regression check - open (P2)
[bf-38oc5] Implement stale-heartbeat handling per plan: 60s threshold, tmux verification, orphan file removal - open (P2)
[bf-42ovy] Implement governor-side p5h/p7d/p7ds annotation of collector records (unblocks empirical promotion validation) - open (P2)
[bf-en75g] Remove orphaned heartbeat files for dead tmux sessions - open (P2)
[bf-3c42g] Exclude orphans from worker counting and shutdown selection - open (P2)
[bf-3g4ew] Implement governor-side window delta computation from API snapshots - open (P2)
[bf-5vhsh] Implement SQLite annotation with session apportioning - open (P2)
[bf-1zz0c] Add guard conditions for window delta annotation - open (P2)
[bf-5y6qi] Add unit tests and verify integration with downstream features - open (P2)
[bf-3z0vo] Implement window delta computation in governor cycle - open (P2)
[bf-s8mea] Add INFO level logging for computed window deltas - open (P2)
[bf-g9mg9] Write unit tests for snapshot delta computation - open (P2)
[bf-knxi6] Handle first poll when no previous snapshot exists - open (P2)
[bf-37w5k] Write unit test for consecutive snapshot delta computation - open (P2)
[bf-5vhv2] Add basic governor cycle test infrastructure - open (P2)
[bf-5pl4o] Write consecutive snapshots test - open (P2)
[bf-4t780] Add delta population assertions - open (P2)
[bf-1b7wv] Add delta value verification - open (P2)
[bf-375k6] Write basic governor cycle smoke test - open (P2)
[bf-4bzt9] Add governor cycle behavior verification tests - open (P2)
[bf-8oevv] Write test for first poll handling - open (P2)
[bf-4bce1] Add explicit Option pattern matching for snapshot handling - open (P2)
[bf-2em2u] Implement conditional delta computation with proper state storage - open (P2)
[bf-rkrd5] Verify first poll handling with tests - open (P2)
[bf-3tglb] Implement proper Option pattern matching structure - open (P2)
[bf-9mtsa] Initialize delta fields for first poll case - open (P2)
[bf-1gscj] Run and verify first poll test suite - open (P2)
[bf-3t7xa] Verify delta computation location - open (P2)
[bf-67zna] Document completion and verify tests - open (P2)
[bf-1row2] Verify calculate_window_pct_delta call is inside the Some-Some block - open (P2)
[bf-64r1k] Verify state delta assignments are inside the Some-Some block - open (P2)
[bf-53tr7] Update promotion references in plan.md - open (P2)
[bf-18y8i] Fix minor issues in plan.md - open (P2)
[bf-54ppq] Investigate Pluck configuration settings - open (P2)
[bf-3js6h] Reproduce Pluck starvation issue - open (P2)
[bf-4xsc6] Identify root cause of bead invisibility - open (P2)
[bf-302de] Fix Pluck configuration - open (P2)
[bf-4vuwg] Verify Pluck bead discovery works - open (P2)
[bf-1i11d] Investigate Pluck configuration and workspace path - open (P2)
[bf-1y51s] Diagnose configuration filter and exclude_labels issues - open (P2)
[bf-3suxt] Fix Pluck configuration to make beads visible - open (P2)
[bf-1hga0] Verify Pluck finds beads after configuration fix - open (P2)
[bf-v34ij] Investigate Pluck configuration for bead discovery - open (P2)
[bf-1c2y5] Identify specific configuration blocking bead discovery - open (P2)
[bf-52ljx] Apply configuration fix to enable bead discovery - open (P2)
[bf-5dsgv] Investigate Pluck configuration and bead visibility settings - open (P2)
[bf-2q36k] cgov scale: log correct safe-mode warning message per spec - open (P3)
[bf-9ky36] Update plan.md stale sections: file layout, config paths, module list, expired promotion references - open (P3)
```

## Current Workspace State
- **Total open beads:** 50
- **Expected:** 37
- **Discrepancy:** +13 beads
- **Workspace:** /home/coding/claude-governor

## Historical Context
According to previous verification (bf-5n8hp):
- Expected count: 37 beads
- Previous actual count: 51 beads
- Current actual count: 50 beads

The workspace has been consistently higher than the expected 37 beads for multiple verification cycles.

## Conclusion
**FAILED** - The workspace does not have 37 open beads as expected. It currently has 50 open beads.

The expected count of 37 beads appears to be outdated or incorrect. The workspace has maintained 50-51 open beads across multiple verification attempts.

## Recommendation
Update any documentation or test conditions that reference "37 open beads" to reflect the current actual count of approximately 50 beads.
