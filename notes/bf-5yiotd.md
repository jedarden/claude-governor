# bf-5yiotd - Correct bf-4fnc20 status to open

## Task
With bf-2mao1t's verification confirming zero blockers, correct bf-4fnc20 dependency chain and verify the compiler-warnings chain can flow.

## Problem Identified
Per bf-2mao1t's verification (notes/bf-2mao1t.md), bf-4fnc20 had:
- **1 blocking dependency:** bf-58z77u (incorrect - backwards dependency)
- **Status:** open (correct)

The dependency `bf-4fnc20 → bf-58z77u` was backwards in the chain. The correct flow should be:
- bf-4fnc20 (Fix unused imports) → bf-58z77u (Verify after cleanup) → ...

But bf-58z77u was blocking bf-4fnc20, creating a circular dependency.

## Fix Applied
Removed the backwards dependency:
```bash
bf dep remove bf-4fnc20 bf-58z77u
```

## Verification

### Before fix
```
$ bf dep list bf-4fnc20
  bf-4fnc20 depends on bf-58z77u (blocks)
```

### After fix
```
$ bf dep list bf-4fnc20
No dependencies found for bf-4fnc20

$ bf ready | grep bf-4fnc20
[bf-4fnc20] Fix unused imports in src/ files (priority=2, impact=1, float=500)
```

### Chain verification
The compiler-warnings chain now flows correctly:
- bf-pdjq78 (leaf) → ready to work
- bf-4fnc20 → now unblocked and ready ✓
- bf-3zdrza → blocked by bf-4fnc20 (will unblock when bf-4fnc20 completes)
- bf-llq2p8 → blocked by bf-3zdrza
- ... → rest of chain up to bf-5be7lz

## Acceptance Criteria Met
- ✓ bf-4fnc20 status is open (was already open, remains open)
- ✓ bf-4fnc20 now has zero blocking dependencies
- ✓ bf ready shows bf-4fnc20 as ready to work
- ✓ Compiler-warnings chain can now flow (bf-4fnc20 unblocked)

## Impact
The fix unblocks the entire compiler-warnings cleanup chain. bf-4fnc20 is now ready to work, and once complete, it will unblock bf-3zdrza, which will unblock bf-llq2p8, and so on up to bf-5be7lz.
