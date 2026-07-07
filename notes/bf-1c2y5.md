# Specific Configuration Blocking Bead Discovery

**Bead ID:** bf-1c2y5  
**Date:** 2026-07-06  
**Workspace:** `/home/coding/claude-governor`

## Executive Summary

**The specific configuration setting blocking Pluck's bead discovery was `workspace.default` in `~/.needle/config.yaml`, which was misconfigured to point to the wrong workspace directory.**

## Root Cause Configuration

### Configuration File
`~/.needle/config.yaml`

### Specific Setting
```yaml
workspace:
  default: /home/coding/telegram-claude-bridge  # ❌ WRONG (pre-fix)
  default: /home/coding/claude-governor        # ✅ CORRECT (post-fix)
```

## Why This Blocked Bead Discovery

### Mechanism of Failure

1. **Pluck's Workspace Resolution**
   - Pluck reads the `workspace.default` setting to determine which workspace to search
   - When set to `/home/coding/telegram-claude-bridge`, Pluck searched in the wrong directory
   - The wrong workspace had fewer beads (11 total vs 43 in the correct workspace)

2. **Bead Store Mismatch**
   - Wrong workspace: `/home/coding/telegram-claude-bridge/.beads/beads.db`
   - Correct workspace: `/home/coding/claude-governor/.beads/beads.db`
   - Pluck was reading from an entirely different bead database

3. **Resulting Symptom**
   - Pluck would query the wrong bead store
   - The wrong store had 11 open beads (most not matching Pluck's filters)
   - The correct store had 43 open beads with 3 ready beads matching filters

### Why This Wasn't Immediately Obvious

- Both workspace directories existed and were valid
- Both had `.beads/` directories with valid databases
- Pluck's filtering logic was correct - it was just looking in the wrong place
- The error manifested as "0 beads found" rather than an explicit error message

## The Fix Applied

**Configuration Change in `~/.needle/config.yaml`:**
```diff
 workspace:
-  default: /home/coding/telegram-claude-bridge
+  default: /home/coding/claude-governor
```

## Verification Results

### Pre-Fix State (June 2026)
- **Workspace path:** `/home/coding/telegram-claude-bridge` ❌
- **Open beads in wrong workspace:** 11
- **Pluck results:** Searching in incorrect workspace, 0 matching beads
- **Symptom:** Starvation alerts, workers unable to find work

### Post-Fix State (July 2026)
- **Workspace path:** `/home/coding/claude-governor` ✅
- **Open beads in correct workspace:** 43
- **Pluck results:** 3 ready beads found ✅
- **Workers active:** 7 heartbeats detected ✅

## How the Fix Was Verified

### Bead bf-2c8i6 (Workspace Access Verification)
- ✅ Workspace directory exists and is readable
- ✅ `.beads/` directory exists
- ✅ `br ready --json` executes successfully
- ✅ Returns valid JSON with bead data

### Bead bf-1hga0 (Post-Fix Verification)
- ✅ Pluck returns 3 ready beads (> 0)
- ✅ Workers can claim and process beads (7 active)
- ✅ No starvation alerts in governor logs
- ✅ Configuration fix restored functionality

## Technical Details

### Pluck's Workspace Resolution Flow

1. **Read Configuration:**
   ```rust
   let workspace = config.workspace.default;
   // Reads from ~/.needle/config.yaml
   ```

2. **Resolve Bead Store:**
   ```rust
   let bead_store = format!("{}/.beads/beads.db", workspace);
   // If wrong, reads from completely different database
   ```

3. **Apply Filters:**
   ```rust
   // Filters are correct - just applied to wrong dataset
   let ready = store.beads()
       .filter(|b| b.status == Status::Open)
       .filter(|b| !has_excluded_labels(b))
       .filter(|b| !is_assigned(b))
       .collect();
   ```

### Why Other Configurations Were NOT the Problem

| Configuration | Value | Status | Why It Wasn't the Issue |
|--------------|-------|--------|------------------------|
| `exclude_labels` | `["deferred", "human", "blocked", "starvation-alert"]` | ✅ Correct | Filters were appropriate, only excluded 18 deferred beads |
| `strands.pluck` | `auto` | ✅ Correct | Strand was enabled properly |
| `workspace.path` | Derived from `default` | ❌ WRONG | Derived from wrong `default` value |
| Filter logic | Three-tier filtering | ✅ Correct | Logic was sound, applied to wrong data |

## Lessons Learned

1. **Workspace Path Validation**
   - Need to validate `workspace.default` matches the actual working directory
   - Consider adding a validation step on NEEDLE startup

2. **Symptom vs Root Cause**
   - Symptom: "0 beads found" suggested filter problem
   - Root cause: Workspace misconfiguration caused data source problem
   - Takeaway: Verify data source location before debugging filters

3. **Diagnostic Strategy**
   - Started with filter investigation (bf-v34ij)
   - Moved to workspace access verification (bf-2c8i6)
   - Found configuration issue (bf-3suxt)
   - Verified fix restored functionality (bf-1hga0)

## Related Beads

- **bf-v34ij:** Investigated Pluck configuration for bead discovery (found filters correct)
- **bf-2c8i6:** Verified Pluck workspace access (verified accessibility)
- **bf-3suxt:** Applied the configuration fix (changed workspace.default)
- **bf-1hga0:** Verified Pluck finds beads after configuration fix (confirmed working)
- **bf-1c2y5:** This bead - Identified specific configuration blocking discovery (this analysis)

## Conclusion

**The specific configuration setting was `workspace.default` in `~/.needle/config.yaml`.** Setting it to `/home/coding/telegram-claude-bridge` caused Pluck to search in the wrong workspace, where it found 0 matching beads. The fix was to change it to `/home/coding/claude-governor`, after which Pluck successfully found 3 ready beads and workers resumed normal operation.
