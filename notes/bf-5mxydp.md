# Safety Branches for Git History Surgery

**Created:** 2026-08-03
**Bead:** bf-5mxydp

## Backup Branch Names

### Reference Clone (local)
- **Branch:** `backup-ref-before-reconcile-20260803`
- **Location:** `/home/coding/claude-governor`
- **Status:** ✅ Created

### Lab Clone (100.81.129.38)
- **Branch:** `backup-lab-before-reconcile-20260803`
- **Location:** `/home/coding/claude-governor` on 100.81.129.38
- **Status:** ✅ Created

## Verification

Both branches were verified with `git branch -a | grep backup-*`:
```bash
# Local (reference)
git branch -a | grep backup-ref
# Output: backup-ref-before-reconcile-20260803

# Lab (remote)
ssh 100.81.129.38 "cd claude-governor && git branch -a | grep backup-lab"
# Output: backup-lab-before-reconcile-20260803
```

## Purpose

These safety branches preserve the current state of both clones before any git history surgery operations. If reconciliation or history rewriting goes wrong, these branches provide a clean rollback point to the pre-surgery state.
