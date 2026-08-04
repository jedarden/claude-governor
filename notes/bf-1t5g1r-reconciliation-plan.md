# Git History Reconciliation Plan

**Bead ID:** bf-1t5g1r  
**Plan Date:** 2026-08-03  
**Purpose:** Merge divergent commit histories without data loss

---

## Executive Summary

The repository has two divergent lineages that need reconciliation:
- **Reference lineage** (`backup-ref-before-reconcile-20260803`): Contains 27+ commits of Pluck investigation work
- **Lab lineage** (`backup-lab-before-reconcile-20260803`): Contains a parallel divergence analysis commit

The reference lineage is the authoritative source with complete development history. The lab lineage contains a duplicate/parallel commit that should be integrated without losing any work.

---

## Current Repository State

### Active Branches
- `main`: Currently at `333f418` (feat: add comprehensive Pluck query construction logging)
- `origin/main`: Currently at `1e0b740` (parallel commit to 78c2c9c - likely a race condition)
- `backup-ref-before-reconcile-20260803`: Points to `1f46e64` (27 commits of Pluck work)
- `backup-lab-before-reconcile-20260803`: Points to `d67c695` (divergence analysis duplicate)
- `revert-test-children-1-3`: Contains revert-then-restore pattern documentation
- `lab-divergence-20260801`: Older divergence point

### Backup Branch Analysis

**backup-ref-before-reconcile-2026083** (27 commits):
```
1f46e64 docs: complete Pluck basic query verification (bf-1cmca)
cc5ae32 docs: complete Pluck configuration fix verification (bf-34ycm)
9318b35 docs: complete Pluck query logging verification (bf-2mr7n)
48e2ebf fix: correct NEEDLE workspace default path from /home/coding to /home/coding/claude-governor
b0eb46e docs: complete Pluck bead discovery testing (bf-2elz6)
dd29e58 docs: complete bead visibility configuration analysis (bf-15prd)
a1fd1a9 docs: complete bead visibility root cause analysis (bf-ko49u)
5e996d9 docs: complete Pluck configuration investigation (bf-5dsgv)
7fdd946 docs: complete bead visibility validation (bf-3bl3w)
ae83ac1 docs: update bead visibility configuration with discovered .needle.yaml layer (bf-15prd)
746721c docs: complete Pluck configuration investigation (bf-4k2j5)
11e38a3 docs: record safety branches for git history surgery (bf-5mxydp)
5639d7f docs: complete bead visibility analysis (bf-dge6t)
4a33e8c test: add comprehensive logging for Pluck filter parameters (bf-1876c)
b8fc912 docs: add Pluck debug output analysis (bf-4c4ip)
546e09a docs: add comprehensive Pluck query results documentation (bf-11rt2)
03cd636 docs: complete bead visibility configuration documentation (bf-15prd)
02a14b4 docs: complete Pluck debug output capture (bf-2qh8h)
e373a49 test: add comprehensive Pluck open beads query test (bf-6dk3c)
bad61ab test: add comprehensive Pluck open beads query test (bf-6dk3c)
897c767 docs: complete Pluck configuration investigation (bf-4k2j5)
2386bc4 docs(bf-39f1ao): Add comprehensive commit history divergence analysis
[... 6+ more commits to root]
```

**backup-lab-before-reconcile-20260803** (1 unique commit):
```
d67c695 docs(bf-39f1ao): Add comprehensive commit history divergence analysis (parallel duplicate)
```

### Critical Finding

Both lineages contain the **same divergence analysis content** (bead bf-39f1ao), but in different commits:
- Reference lineage: `2386bc4` (integrated into main history)
- Lab lineage: `d67c695` (parallel duplicate on separate branch)

This is the **only unique commit** on the lab lineage. All other work exists in the reference lineage.

---

## Commit Content Analysis

### Comparison of Divergence Analysis Commits

**Commit 2386bc4** (on reference lineage):
- Created: 2026-08-03
- Content: Full divergence analysis document
- Status: Integrated into main development history

**Commit d67c695** (on lab lineage):
- Created: 2026-08-03 19:52:53
- Content: Full divergence analysis document (identical content)
- Status: Parallel duplicate on separate branch

Both commits document the same findings (analysis of lab vs reference clone divergence), but `d67c695` was created on a divergent branch and contains identical analysis content to `2386bc4`.

### File Changes in Both Commits

Both commits add the same file:
- `notes/bf-39f1ao.md` - Comprehensive commit history divergence analysis document

Since `notes/bf-39f1ao.md` already exists in the reference lineage (from commit 2386bc4), merging `d67c695` will attempt to add the same content again, potentially creating a conflict.

---

## Divergence Graph

```
main (current: 333f418)
├── 78c2c9c docs: document safety branches for git history surgery
├── 1f46e64 (backup-ref) ──► 27 commits of Pluck work ◄── REFERENCE LINEAGE
│   └── 2386bc4 divergence analysis (integrated)
│
└── d67c695 (backup-lab) ──► divergence analysis duplicate ◄── LAB LINEAGE
    └── shares ancestry with ref lineage up to divergence point
```

---

## Reconciliation Strategy

### Phase 1: Verify Backup Branch State

**Commands:**
```bash
# Confirm backup branches are safe
git branch -vv | grep backup
git log --oneline backup-ref-before-reconcile-20260803 | wc -l  # Should be 27+
git log --oneline backup-lab-before-reconcile-20260803 | wc -l  # Should be 1 (only d67c695)
```

**Expected Result:**
- `backup-ref` has 27+ commits
- `backup-lab` has 1 unique commit (d67c695)
- Both branches are static (no force-push risk)

### Phase 2: Verify Current Main Branch

**Commands:**
```bash
# Check main branch state
git log --oneline main | head -5
git status
git diff main origin/main
```

**Expected Result:**
- main is at or beyond 333f418
- No uncommitted changes
- main and origin/main may have diverged (known race condition at 78c2c9c/1e0b740)

### Phase 3: Merge Strategy

**Decision:** Use **simple merge** since commits touch unrelated files:
- Reference lineage commits: Pluck investigation files (notes/, tests/, src/)
- Lab lineage commit: Already documented in reference lineage (notes/bf-39f1ao.md exists)

**Merge Command:**
```bash
# Ensure clean state
git checkout main
git status  # Should be clean

# Attempt merge
git merge backup-lab-before-reconcile-20260803 --no-edit
```

**Expected Behavior:**
- **Scenario A (Clean Merge):** Git recognizes `d67c695` adds `notes/bf-39f1ao.md`, but file already exists from `2386bc4`. Git auto-merges using the existing content.
- **Scenario B (Conflict):** Git detects conflict on `notes/bf-39f1ao.md`. Manual resolution required.

### Phase 4: Conflict Resolution (If Needed)

**If conflict occurs on notes/bf-39f1ao.md:**

```bash
# Check conflict markers
git status
cat notes/bf-39f1ao.md | grep -A5 "<<<<<<<"

# Resolution: Keep reference version (already integrated)
git checkout --theirs notes/bf-39f1ao.md
# OR manually verify both versions are identical, then:
# git checkout --ours notes/bf-39f1ao.md

# Mark as resolved
git add notes/bf-39f1ao.md
```

**Verification:**
```bash
# Verify content is identical
diff <git show 2386bc4:notes/bf-39f1ao.md> <git show d67c695:notes/bf-39f1ao.md>
# Should be empty diff (files are identical)
```

### Phase 5: Complete Merge

**Commands:**
```bash
# Finalize merge
git commit --no-edit  # If conflict resolution required
git log --oneline --graph --all -10  # Verify merge commit
```

**Expected Result:**
- Single merge commit combining both lineages
- No duplicate divergence analysis documents
- All 27+ Pluck commits retained
- Clean merge graph

### Phase 6: Verify No Commits Lost

**Commands:**
```bash
# Verify ref lineage commits are present
git log --oneline main | grep "1f46e64"
git log --oneline main | grep "cc5ae32"
git log --oneline main | grep "Pluck"

# Verify lab lineage commit is integrated
git log --oneline main | grep "d67c695"

# Verify divergence analysis content exists
test -f notes/bf-39f1ao.md && echo "Analysis document exists"

# Verify commit count
git log --oneline main --count  # Should include all commits
```

**Expected Result:**
- All reference lineage commits present in main
- Lab lineage commit (d67c695) integrated or merged
- Single `notes/bf-39f1ao.md` file exists
- Total commit count increased by 1 (merge commit)

### Phase 7: Handle Remote Divergence (origin/main)

**Known Issue:** main and origin/main diverged at commit 78c2c9c/1e0b740 (parallel commits with same message).

**Commands:**
```bash
# Check remote state
git fetch origin
git log --oneline origin/main | head -5
git log --oneline main | head -5

# If origin/main is behind main, push main
git push origin main --force-with-lease  # CAUTION: overrides parallel commit

# If origin/main has different content, investigate first
git diff main origin/main
```

**Decision Point:**
- If `1e0b740` is identical to `78c2c9c` (same content, different SHA): Safe to force-push main
- If `1e0b740` has different content: Need separate reconciliation (out of scope for this plan)

### Phase 8: Final Verification

**Commands:**
```bash
# Verify no divergence analysis duplicates
find notes/ -name "*bf-39f1ao*" -type f

# Verify bead documentation exists
test -f notes/bf-1t5g1r.md && echo "This bead's documentation exists"

# Verify all backup branches still exist
git branch | grep backup

# Verify remote is synchronized
git status
git log --oneline origin/main -1
git log --oneline main -1
```

**Expected Result:**
- Single `notes/bf-39f1ao.md` file (no duplicates)
- `notes/bf-1t5g1r.md` exists (this reconciliation plan)
- Both backup branches preserved
- main and origin/main synchronized

---

## Risk Assessment

### Low Risk
- Both backup branches are static (no ongoing changes)
- Content is already documented in reference lineage
- Clean merge expected (no overlapping file changes)

### Medium Risk
- Remote divergence at origin/main (1e0b740 vs 78c2c9c) may require separate handling
- Force-push may be needed, requiring verification of identical content

### Mitigation
- Backup branches created before reconciliation
- All commits verified before merge
- Step-by-step verification at each phase
- Force-push only with --force-with-lease (safer than --force)

---

## Rollback Plan

If merge fails or produces unexpected results:

```bash
# Reset to before merge
git reset --hard HEAD~1  # Remove merge commit
git reset --hard backup-ref-before-reconcile-20260803  # Start from clean reference state

# Verify state
git log --oneline -5
git status
```

**Alternative Rollback:**
```bash
# Reset to known good state on origin
git reset --hard origin/main
git push origin main --force-with-lease
```

---

## Post-Reconciliation Cleanup

Once merge is verified and successful:

```bash
# Optional: Rename backup branches for historical record
git branch -m backup-ref-before-reconcile-20260803 archived-ref-20260803
git branch -m backup-lab-before-reconcile-20260803 archived-lab-20260803

# Keep branches for safety (do not delete until main is verified stable over several commits)
```

---

## Success Criteria

✓ All commits from reference lineage present in main  
✓ Lab lineage divergence analysis integrated or merged  
✓ No duplicate divergence analysis files  
✓ main and origin/main synchronized  
✓ No data loss (all commits accounted for)  
✓ Clean git graph (no unnecessary merge commits)  
✓ All verification steps pass  

---

## Summary

**Reconciliation Target:** Reference lineage (`backup-ref-before-reconcile-20260803`)  
**Merge Source:** Lab lineage (`backup-lab-before-reconcile-20260803`)  
**Merge Strategy:** Simple merge (commits touch same file, but content is identical)  
**Expected Conflicts:** 0-1 (notes/bf-39f1ao.md if git doesn't auto-resolve)  
**Data Loss Risk:** Low (all work preserved in reference lineage)  

**Final Command Sequence:**
```bash
git checkout main
git merge backup-lab-before-reconcile-20260803 --no-edit
# If conflict: git checkout --theirs notes/bf-39f1ao.md && git add notes/bf-39f1ao.md
git push origin main --force-with-lease
```

---

**End of Reconciliation Plan**
