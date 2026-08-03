# Pluck Visibility Gap Analysis — Bead bf-27w2y

**Date:** 2026-08-03
**Task:** Analyze visibility gaps and root causes
**Workspace:** /home/coding/claude-governor

---

## Executive Summary

Pluck's default filter configuration is working as designed, but there is a **critical architectural issue**: investigation beads that are assigned to diagnose visibility problems become permanently invisible to the work-selector itself because they acquire the same `deferred` label that Pluck filters out. This creates a paradox where beads investigating why work is invisible are themselves invisible.

**Key Finding:** 1 open bead (`bf-156nn7`) is currently hidden by the `deferred` filter, but the deeper issue is that 4 investigation beads (bf-1y51s, bf-3js6h, bf-54ppq, bf-5dsgv) have already cycled through this pattern — they were created to investigate Pluck starvation, acquired `deferred` labels after failing, and are now permanently excluded from the work-selector.

---

## Current State (2026-08-03)

### Database Statistics

| Category | Count | Notes |
|----------|-------|-------|
| Total open beads (status=open) | 18-20 | Exact count varies between database views |
| Visible to Pluck | 17 | After applying default filters |
| Hidden by deferred filter | 1 | bf-156nn7 |
| Hidden by human filter | 0 | No beads currently use this label |
| Blocked beads (status=blocked) | ~75 | Per bead bf-2mwvej description |

### Currently Hidden Bead

**bf-156nn7:** "config/claude-governor.service still ships MemoryMax=512M — the exact value that caused the July 2026 OOM crash-loop"
- Status: `open`
- Labels: `["deferred", "failure-count:1"]`
- **Hidden by:** `deferred` label in Pluck's DEFAULT_EXCLUDE_LABELS
- **Root cause:** Acquired `deferred` label after previous failure cycles

---

## Root Cause Analysis

### 1. Pluck's Default Filter Configuration

**Source:** `/home/coding/NEEDLE/src/strand/pluck.rs:13` (compiled constant)
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];
```

**Behavior:**
- When `PluckStrand::new(vec![])` is called with empty exclude_labels, defaults are applied
- Custom exclude_labels override defaults completely (not merged)
- Filtering is applied twice: once via bead store query, once defensively in the strand

**Workspace:** `/home/coding/claude-governor`
**Bead store:** `/home/coding/claude-governor/.beads/beads.db`

### 2. The Structural Paradox

Per bead **bf-2mwvej** (OPS-GATED: 4 Pluck-investigation beads are structurally unresolvable):

> "bf-1y51s, bf-3js6h, bf-54ppq, bf-5dsgv (4 of the 7 currently-open beads in this workspace) all investigate NEEDLE own hardcoded work-selection filter (a compiled-in constant excluding beads labeled deferred, human, or blocked, documented in this repo docs/plan/pluck-configuration.md) - a constant that lives in a completely different repo (NEEDLE, src/strand/pluck.rs) and cannot be changed by any bead or worker operating inside claude-governor.
>
> All 4 already carry the deferred label themselves (with failure-count up to 11), which means NEEDLE own filter now permanently excludes them from ever being claimed again - an investigation into why beads are invisible to the work-selector produced beads that are now invisible to the work-selector, for the exact same reason."

**The cycle:**
1. Worker cannot find work → creates investigation bead
2. Investigation bead fails to fix the problem (because it's outside this repo's scope)
3. Bead gets `deferred` label added after failures
4. `deferred` label excludes the bead from Pluck → permanently invisible
5. Pattern repeats → new investigation beads created

### 3. Cross-Repo Configuration Issue

The root configuration lives in **NEEDLE repo** (`/home/coding/NEEDLE/src/strand/pluck.rs`), not in claude-governor:
- DEFAULT_EXCLUDE_LABELS is a compiled constant
- Changing it requires recompiling NEEDLE
- No runtime override exists in current deployment
- Workers in claude-governor workspace cannot modify NEEDLE's binary

---

## Specific Configuration Settings Causing Hidden Beads

### Setting 1: DEFAULT_EXCLUDE_LABELS

**Location:** `/home/coding/NEEDLE/src/strand/pluck.rs:13`
**Value:** `&["deferred", "human", "blocked"]`
**Effect:** Excludes any bead with these labels from work selection

**Hidden beads:**
- `bf-156nn7` (open, deferred) — **Currently hidden**
- `bf-1y51s, bf-3js6h, bf-54ppq, bf-5dsgv` (mentioned in bf-2mwvej) — **Already cycled to hidden**

### Setting 2: No Custom exclude_labels Override

**Location:** `~/.needle/config.yaml`
**Current:** No custom override configured
**Effect:** Uses hardcoded defaults

**Impact:** Cannot adjust filter behavior without editing NEEDLE source and recompiling

---

## Proposed Configuration Fixes

### Option 1: Remove `deferred` from DEFAULT_EXCLUDE_LABELS (NEEDLE repo change)

**Location:** `/home/coding/NEEDLE/src/strand/pluck.rs`
**Change:**
```rust
// Before:
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked"];

// After:
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["human", "blocked"];
```

**Rationale:**
- `deferred` was intended for manual "do this later" tagging
- But when workers auto-defer failed investigation beads, it creates permanent starvation
- `human` and `blocked` are still needed (human intervention, dependency gating)

**Pros:**
- Allows investigation beads to remain visible even after failures
- Prevents the structural paradox

**Cons:**
- May allow genuinely deferred work to be claimed when it shouldn't be
- Could expose long-stuck beads to repeated failure cycles

### Option 2: Add Runtime exclude_labels Override (NEEDLE repo change)

**Location:** `~/.needle/config.yaml` + NEEDLE code to read it

**Config addition:**
```yaml
strands:
  pluck:
    exclude_labels: ["human", "blocked"]  # Runtime override of defaults
```

**Rationale:**
- Allows per-workspace customization
- Can override defaults without recompiling
- claude-governor can specify its own filter policy

**Pros:**
- Flexible per-workspace configuration
- No recompilation needed
- claude-governor can opt-out of `deferred` filtering

**Cons:**
- Requires NEEDLE code changes to read config
- Still a cross-repo dependency

### Option 3: Change Auto-Deferred Behavior (bead-forge/bf CLI change)

**Location:** Auto-failure logic in NEEDLE's worker strand
**Change:** Stop automatically adding `deferred` label to failed investigation beads

**Rationale:**
- Investigation beads failing due to cross-repo issues shouldn't be deferred
- Reserve `deferred` for manual human decisions, not automatic cycles

**Pros:**
- Prevents the paradox at its source
- Keeps `deferred` meaningful (manual deferral only)

**Cons:**
- Still allows `deferred` to hide beads if humans apply it manually
- Doesn't address the filter itself

### Option 4: Workspace-Level Classification (NEW label strategy)

**Proposal:** Introduce workspace-specific labels that NEEDLE doesn't filter by default:

```yaml
# New proposed labels for claude-governor workspace:
cgov-deferred    # Internal deferral (NOT filtered by Pluck)
external-blocked # Blocked by external dependency (NOT filtered)
ops-gated        # Requires human ops intervention (NOT filtered)
```

**Rationale:**
- Distinguishes workspace-specific deferral from global `deferred`
- Allows internal classification without triggering NEEDLE's global filters

**Pros:**
- Works within existing NEEDLE filters
- claude-governor can manage its own classification
- Doesn't require cross-repo changes

**Cons:**
- Adds label complexity
- Requires updating bead creation patterns

---

## Recommended Action Path

### Immediate (claude-governor workspace)

1. **Document the structural paradox** — This analysis ✅
2. **Close the cycling investigation beads** — Per bf-2mwvej recommendation:
   - Close bf-1y51s, bf-3js6h, bf-54ppq, bf-5dsgv and their ~30 blocked ancestors as out-of-scope
   - File Pluck starvation issue directly against NEEDLE repo

3. **Remove `deferred` label from bf-156nn7** — Make it visible again:
   ```bash
   bf update bf-156nn7 --labels-remove deferred
   ```

### Short-term (NEEDLE repo)

4. **Implement Option 2** — Add runtime exclude_labels override:
   - Modify NEEDLE to read `strands.pluck.exclude_labels` from config
   - Default to compiled DEFAULT_EXCLUDE_LABELS if not specified
   - Allows claude-governor to opt-out of `deferred` filtering

### Long-term (Architecture)

5. **Implement Option 4** — Workspace-specific classification:
   - Standard practice: use workspace-prefixed labels for local concerns
   - Keep global labels (`deferred`, `human`, `blocked`) for cross-cutting concerns
   - Document in NEEDLE and bead-forge best practices

---

## Appendix: Investigation Bead History

Per bead **bf-2mwvej**, this is not the first cycle:

> "This is not a first attempt: bf-4xsc6 (identify root cause of bead invisibility), bf-302de (fix Pluck configuration), and bf-4vuwg (verify Pluck bead discovery works) were all already closed as Completed in an earlier cycle, yet the identical starvation symptom recurred and spawned this new round of 30-plus split-child beads (plus a still-blocked bf-3jo4t, starvation alert for beads invisible to worker). Something about the earlier fix did not hold, or only covered a narrower case than the one recurring now."

**Pattern:**
- Cycle 1: bf-4xsc6, bf-302de, bf-4vuwg (closed as Completed) → symptom recurred
- Cycle 2: bf-1y51s, bf-3js6h, bf-54ppq, bf-5dsgv (deferred, invisible) → 30+ split-child beads spawned
- Current: bf-156nn7 (deferred, 1 hidden bead) + ongoing systemic issue

---

## Conclusion

The visibility gap is **not a configuration error** — Pluck's filters are working as designed. The root cause is a **structural architectural mismatch**:

1. Filters live in NEEDLE repo (compiled constants)
2. Workers operate in claude-governor repo (cannot change filters)
3. Investigation beads acquire `deferred` label automatically
4. `deferred` label excludes beads from Pluck → permanent starvation

**The fix requires cross-repo coordination** — either NEEDLE adds runtime override capability, or bead-forge changes auto-defer behavior, or workspaces adopt local labeling conventions. Without addressing the architectural root cause, investigation beads will continue cycling into invisibility.
