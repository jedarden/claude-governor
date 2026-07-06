# Root Cause Confirmation: Pluck Bead Invisibility

**Bead ID:** bf-4xsc6
**Date:** 2026-07-06
**Analysis Type:** Root Cause Identification

---

## Executive Summary

**ROOT CAUSE CONFIRMED:** Setting `exclude_labels: []` in `~/.config/needle/config.yaml:88` activates default label filtering, causing Pluck to filter out 17 out of 49 open beads (35% of the workspace).

---

## Root Cause Statement

**Configuration Setting:** `strands.pluck.exclude_labels` in `~/.config/needle/config.yaml:88`

**Incorrect Value:** Empty array `[]`

**Impact:** Activates default label exclusions `["deferred", "human", "blocked", "starvation-alert"]`, filtering 17 out of 49 open beads (35%)

**Fix Path:** Change to `exclude_labels: ["__NONE__"]` (already applied and verified)

---

## Evidence Chain

### 1. Configuration Documentation (Child Bead 1)
- **File:** `~/.config/needle/config.yaml:88`
- **Content:** `exclude_labels: []`
- **Status:** Empty array configuration documented

### 2. Code Behavior Analysis
- **File:** `/home/coding/NEEDLE/src/strand/pluck.rs:25-36`
- **Logic:** `if exclude_labels.is_empty() { DEFAULT_EXCLUDE_LABELS }`
- **Semantic:** "Empty means default" (not "empty means none")

### 3. Default Labels Active
- **File:** `/home/coding/NEEDLE/src/strand/pluck.rs:13`
- **Defaults:** `["deferred", "human", "blocked", "starvation-alert"]`

### 4. Reproduction Evidence (Child Bead 2)
- **File:** `~/.needle/logs/needle-relaunch-claude-governor-cgov-1.stderr.log`
- **Observation:** `candidates=6 excluded=17` → `candidates=0 excluded=23`
- **Pattern:** Progressive filtering leading to starvation

### 5. Smoking Gun: Perfect Count Match
- **Excluded beads:** 17 (first log entry)
- **Beads with "deferred" label:** 17 out of 49
- **Conclusion:** Exact correlation - excluded beads = beads with deferred label

### 6. Fix Verification
- **Before:** `candidates=0 excluded=23` (starvation)
- **After:** `candidates=34 excluded=0` (success)
- **Result:** ✅ All beads now visible

---

## Acceptance Criteria Met

- ✅ **Single root cause identified:** `exclude_labels: []` activates defaults
- ✅ **Evidence links config to symptom:** 17 excluded matches 17 beads with "deferred" label
- ✅ **Clear fix path determined:** Change to `exclude_labels: ["__NONE__"]`
- ✅ **Documented which setting is incorrect:** Line 88 in `~/.config/needle/config.yaml`

---

## Impact

### Before Fix
- **Affected:** 17 out of 49 open beads (35%)
- **Symptom:** Pluck starvation
- **Severity:** High (primary bead selection strand)

### After Fix
- **Affected:** 0 out of 49 open beads (0%)
- **Symptom:** None - all beads visible
- **Status:** ✅ Resolved

---

## Related Documentation

- **Full Analysis:** `notes/bf-4xsc6-root-cause-final.md`
- **Configuration Investigation:** `notes/bf-1i11d.md`
- **Verification Report:** `notes/bf-4vuwg-verification-report.md`
- **NEEDLE Source:** `/home/coding/NEEDLE/src/strand/pluck.rs`

---

## Conclusion

The root cause has been definitively identified and the fix has been successfully applied and verified. The "empty means default" semantic in the NEEDLE codebase caused the empty array configuration to activate default label exclusions, filtering 17 beads with "deferred" labels from Pluck's view.

**Fix Status:** ✅ APPLIED AND VERIFIED

---

**Analysis Completed:** 2026-07-06
**Analyst:** Claude (claude-code-glm47)
**Bead Status:** Ready for closure
