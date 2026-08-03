# Safety Branches for Git History Surgery

**Created:** 2026-08-03
**Bead:** bf-5mxydp

## Backup Branches

### Lab Clone
- **Location:** `/home/coding/claude-governor` (host: 100.81.129.38, hostname: lab)
- **Backup Branch:** `backup-lab-before-reconcile-20260803`
- **Origin:** GitHub (`https://github.com/jedarden/claude-governor.git`)
- **Current State:** Long chain of docs commits documenting test-verification results and a Children 1-3 revert-then-restore cycle (dated 2026-07-27)

### Reference Clone
- **Location:** `/tmp/claude-governor-reference` (temporary clone)
- **Backup Branch:** `backup-ref-before-reconcile-20260803`
- **Origin:** Forgejo (`https://git.ardenone.com/jedarden/claude-governor.git`)
- **Current State:** chore/seeder commit about default polish-queue push behavior (canonical)

## Purpose

These safety branches preserve the current state of both clones before any git history reconciliation. The lab clone has diverged from the canonical Forgejo repository, and these backup branches provide a rollback point if the reconciliation process encounters issues.

## Next Steps

The reconciliation process should:
1. Identify the divergence point between the two commit histories
2. Create a merge commit (never force-push, per standing policy)
3. Repoint lab origin to Forgejo once reconciled
4. Verify that both histories are preserved in the merge

## Verification

Both branches created successfully:
- ✓ `backup-lab-before-reconcile-20260803` on lab clone
- ✓ `backup-ref-before-reconcile-20260803` on reference clone
