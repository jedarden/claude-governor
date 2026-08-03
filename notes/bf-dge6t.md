# Bead Visibility Analysis

## Executive Summary

- **Total beads in workspace:** 1,208
- **Open beads (status != "closed"):** 75
- **Beads visible to worker (`bf ready`):** 9
- **Beads hidden from worker:** 66 (49 blocked + 7 open with dependencies + 2 done + 7 in_progress + 1 other)

## Status Breakdown

### All Open Beads (75 total)
- `blocked`: 49 beads
- `done`: 2 beads  
- `in_progress`: 7 beads
- `open` (no dependencies): 17 beads

### Ready vs Invisible Gap

From the 17 `open` status beads:
- **9 visible to worker** (shown by `bf ready`)
- **7 invisible** (status=open but have dependencies)
- **1 other** (likely filtering issue)

## Invisible Beads That Should Be Visible

### The 7 Hidden Open Beads

These beads have `status: open` but are **not shown by `bf ready`** because they have active dependencies:

1. **bf-1y51s** - "Diagnose configuration filter and exclude_labels issues"
   - Dependencies: bf-40yby (blocked), bf-81ukr (?)
   - **Hidden reason:** Blocked by unresolved dependency chain

2. **bf-2h8n23** - "Fix remaining compiler warnings"
   - Dependencies: bf-4te4ib (blocked)
   - **Hidden reason:** Directly blocked by unresolved bf-4te4ib

3. **bf-3js6h** - "Reproduce Pluck starvation issue"
   - Dependencies: bf-5jral (blocked), bf-551q7 (closed)
   - **Hidden reason:** Blocked by bf-5jral

4. **bf-3ww0k4** - "Verify clean build with clippy"
   - Dependencies: bf-2h8n23 (open, not ready)
   - **Hidden reason:** Blocked by bead that is itself blocked (circular dependency chain)

5. **bf-3zdrza** - "Fix unused variables in src/ and tests/"
   - Dependencies: bf-4fnc20 (in_progress)
   - **Hidden reason:** Blocked by in-progress dependency

6. **bf-5be7lz** - "Eliminate compiler warnings"
   - Dependencies: bf-64cczk (closed), bf-3ww0k4 (open, not ready)
   - **Hidden reason:** Blocked by bead that is itself blocked

7. **bf-5dsgv** - "Investigate Pluck configuration and bead visibility settings"
   - Dependencies: bf-dge6t (in_progress, current bead), bf-27w2y (closed)
   - **Hidden reason:** Blocked by current bead

## Dependency Chain Analysis

### Critical Blocking Chains

1. **Compiler warning cleanup chain:**
   - bf-4te4ib (blocked) → blocks → bf-2h8n23 → blocks → bf-3ww0k4 → blocks → bf-5be7lz
   - **Impact:** 4 beads held up by one blocked bead

2. **Pluck investigation chain:**
   - bf-40yby (blocked) → blocks → bf-1y51s
   - **Impact:** 2 beads held up

3. **Import cleanup chain:**
   - bf-4fnc20 (in_progress) → blocks → bf-3zdrza
   - **Impact:** Will unblock when bf-4fnc20 completes

## Root Causes of Invisibility

### 1. **Dependency System Hiding Open Beads** (Primary Cause)
Beads with `status: open` but non-empty dependency lists are **not shown by `bf ready`** even though they're not technically "blocked" status. This is the main source of invisibility.

### 2. **Blocked Status Cascade** (Secondary Cause)
49 beads have `status: blocked` because their dependencies are unresolved. These are never shown to workers.

### 3. **Circular Dependency Chains** (Tertiary Cause)
Some beads form chains where:
- A is blocked by B
- B is blocked by C  
- C is blocked by A (or similar circular pattern)

This creates deadlocks that prevent the entire chain from becoming visible.

## Configuration Issues

The issue is **not** with NEEDLE adapter filters (none found) but with:
1. **Bead-forge/br visibility logic**: `bf ready` filters out beads with unresolved dependencies
2. **Dependency management**: Open beads with dependencies are treated as blocked for worker visibility

## Recommendations

1. **Resolve blocking beads first:** Complete bf-4te4ib and bf-40yby to unlock chains
2. **Break circular dependencies:** Re-architect chains like bf-2h8n23 → bf-3ww0k4 → bf-5be7lz
3. **Consider dependency visibility:** Make open-but-blocked beads visible with a marker
4. **Track dependency health:** Monitor chains deeper than 3 beads as deadlock risks

## Worker Visibility by Type

```
Worker can see (9):     open status + no dependencies
Worker cannot see (66):
  - 49 blocked status
  -  7 open with dependencies  
  -  7 in_progress (assigned to other workers)
  -  2 done status
  -  1 other (likely filtering artifact)
```

