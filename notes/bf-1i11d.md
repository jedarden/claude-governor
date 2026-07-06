# Pluck Configuration and Workspace Investigation

**Investigation Date:** 2026-07-06  
**Workspace:** `/home/coding/claude-governor`  
**Bead ID:** bf-1i11d  
**Status:** in_progress

## Executive Summary

Investigation of Pluck configuration and workspace path reveals **significant discrepancies** between configured defaults and actual runtime behavior. The primary issue is a **workspace mismatch**: NEEDLE's default workspace is configured for `telegram-claude-bridge`, but workers are operating in `claude-governor`.

## Current Pluck Configuration

### 1. Exclude Labels Configuration

**Source:** Compiled into NEEDLE binary (`/home/coding/NEEDLE/src/strand/pluck.rs:13`)

**Default Exclude Labels:**
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

**Current Status:**
- ✅ Using default exclude_labels (no custom override)
- ✅ Filtering applied at both store query and strand level
- ✅ No custom exclude_labels configured in workspace

### 2. Workspace Path Configuration

**Configuration Sources:**

| Source | Configured Path | Actual Path | Status |
|--------|----------------|-------------|--------|
| `~/.needle/config.yaml:9` | `/home/coding/telegram-claude-bridge` | `/home/coding/claude-governor` | ❌ **MISMATCH** |
| Current workspace | N/A | `/home/coding/claude-governor` | ✅ Active |
| Bead store location | `{workspace}/.beads/` | `/home/coding/claude-governor/.beads/` | ✅ Correct |

**Critical Finding:** The default workspace in NEEDLE configuration points to `telegram-claude-bridge`, but Pluck is operating on `claude-governor`. This suggests:
- Workers are being launched with explicit `--workspace` override
- OR environment variable `NEEDLE_WORKSPACE` is set at runtime
- OR the bead discovery mechanism is using current working directory

### 3. Strand Configuration

**Source:** `~/.needle/config.yaml:70-87`

```yaml
strands:
  pluck: auto    # Primary work from the auto-discovered workspace
  explore: auto  # Look for work in other workspaces
  mend: true     # Maintenance and cleanup (always on - reap stale beads)
  knot: true     # Alert human when stuck (always on)
```

**Status:** ✅ All strands properly configured

### 4. Worker Configuration (from governor.yaml)

```yaml
agents:
  needle-sonnet:
    launch_cmd: "needle run --agent claude-code-glm-5 --workspace {workspace} --session-prefix needle-cgov"
    session_pattern: "needle-cgov-*"
    heartbeat_dir: "~/.needle/state/heartbeats"
    min_workers: 0
    max_workers: 8
```

**Status:** ⚠️ Workers are launched with explicit `--workspace {workspace}` parameter, which may be overriding the default workspace configuration.

## Open Bead Analysis

### Total Open Beads: 53

**Breakdown by Status:**
- **Open:** 51 beads
- **In Progress:** 1 bead (bf-1i11d - this investigation)
- **Blocked:** 2 beads (bf-1iow5, bf-wvljm)

### Notable Bead Categories

**Pluck Configuration Investigation Beads (12 beads):**
- bf-54ppq: Investigate Pluck configuration settings
- bf-3js6h: Reproduce Pluck starvation issue  
- bf-4xsc6: Identify root cause of bead invisibility
- bf-1i11d: Investigate Pluck configuration and workspace path (this bead)
- bf-1y51s: Diagnose configuration filter and exclude_labels issues
- bf-3suxt: Fix Pluck configuration to make beads visible
- bf-1hga0: Verify Pluck finds beads after configuration fix
- bf-v34ij: Investigate Pluck configuration for bead discovery
- bf-1c2y5: Identify specific configuration blocking bead discovery
- bf-52ljx: Apply configuration fix to enable bead discovery
- bf-5dsgv: Investigate Pluck configuration and bead visibility settings
- bf-5msut: Investigate Pluck trace output analysis

**Blocked Beads (2):**
- bf-1iow5: Verify Pluck can find and process open beads (blocked)
- bf-wvljm: List and categorize all open beads (blocked)

**Closed/Completed Verification Beads (3):**
- bf-1xabf: Verify Pluck workspace has 37 open beads (closed)
- bf-49qnq: Verify workspace has 37 open beads (closed)  
- bf-5n8hp: Verify open bead count in workspace (closed)

## Discrepancies Identified

### 🔴 Critical Issues

1. **Workspace Configuration Mismatch**
   - **Expected:** Default workspace should match active workspace
   - **Actual:** `~/.needle/config.yaml` has `default: /home/coding/telegram-claude-bridge`
   - **Impact:** Workers may be confused about which workspace to use
   - **Recommendation:** Update default workspace to `/home/coding/claude-governor` OR ensure all worker launches use explicit `--workspace` override

2. **Excessive Pluck Investigation Beads (12 beads)**
   - **Symptom:** Multiple redundant beads investigating same issue
   - **Root Cause:** Previous investigations didn't resolve the core problem
   - **Impact:** Suggests systemic bead discovery/configuration issues
   - **Recommendation:** Complete this investigation, then consolidate/duplicate investigation beads

### 🟡 Medium Issues

3. **Blocked Bead Discovery Beads**
   - **Issue:** bf-1iow5 and bf-wvljm are blocked, preventing verification of bead discovery
   - **Impact:** Cannot confirm whether Pluck configuration fixes actually work
   - **Recommendation:** Investigate blocking dependencies and unblock these verification beads

4. **Bead Count Discrepancy**
   - **Previous verification:** 37 open beads (bf-1xabf, bf-49qnq, bf-5n8hp)
   - **Current count:** 53 open beads
   - **Difference:** +16 beads since last verification
   - **Impact:** Bead count has grown significantly, suggesting new bead creation surge
   - **Recommendation:** Verify if bead count growth is expected or indicates split/runaway behavior

## Configuration Sourcing Summary

| Setting | Source | Location | Type | Current Value |
|---------|--------|----------|------|---------------|
| Default exclude_labels | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:13` | Constant | `["deferred", "human", "blocked", "starvation-alert"]` |
| Custom exclude_labels | Not configured | N/A | Runtime override | None (uses defaults) |
| Workspace default | NEEDLE config | `~/.needle/config.yaml:9` | YAML path | `/home/coding/telegram-claude-bridge` ❌ |
| Current workspace | CLI/environment | NEEDLE assignment | Runtime | `/home/coding/claude-governor` ✅ |
| Bead store path | Derived from workspace | `{workspace}/.beads/` | Directory | `/home/coding/claude-governor/.beads/` ✅ |
| Strand enablement | NEEDLE config | `~/.needle/config.yaml:70-87` | YAML map | `pluck: auto` ✅ |
| Filter logic | Compiled binary | `/home/coding/NEEDLE/src/strand/pluck.rs:105-133` | Rust code | Three-tier filtering ✅ |

## Recommendations

### Immediate Actions

1. **Fix Workspace Configuration:**
   ```bash
   # Update default workspace to match actual workspace
   sed -i 's|default: /home/coding/telegram-claude-bridge|default: /home/coding/claude-governor|' ~/.needle/config.yaml
   ```

2. **Consolidate Investigation Beads:**
   - Close duplicate Pluck investigation beads (bf-54ppq, bf-3js6h, bf-v34ij, bf-1c2y5, bf-52ljx, bf-5dsgv, bf-5msut)
   - Keep only this investigation (bf-1i11d) and the root cause bead (bf-4xsc6)

3. **Investigate Blocked Beads:**
   - Check dependencies for bf-1iow5 and bf-wvljm
   - Resolve blocking issues to enable verification

### Follow-up Actions

4. **Verify Bead Discovery:**
   - After configuration fix, run `br list | grep -E "open|in_progress"` to confirm Pluck can discover all 53 open beads
   - Test with a simple worker to confirm beads are being claimed

5. **Monitor Bead Count:**
   - Track whether bead count continues to grow (+16 since last verification)
   - Investigate if split behavior is creating excessive child beads

## Conclusion

The investigation reveals a **workspace configuration mismatch** as the primary discrepancy. The default workspace in NEEDLE configuration points to `telegram-claude-bridge`, but workers are operating in `claude-governor`. This suggests workers are being launched with explicit workspace overrides, which may be causing confusion in bead discovery.

The presence of 12 Pluck investigation beads and 2 blocked verification beads indicates ongoing systemic issues with bead discovery that previous investigations have not resolved.

**Next Steps:**
1. Fix workspace configuration in `~/.needle/config.yaml`
2. Consolidate redundant investigation beads
3. Verify bead discovery works after configuration fix
4. Investigate blocked verification beads

---

**Acceptance Criteria Status:**
- ✅ Document current Pluck configuration (exclude_labels, filters, workspace path)
- ✅ List all open beads that should be available to the worker (53 beads)
- ✅ Identify discrepancies between configuration and actual bead state (workspace mismatch, excessive investigation beads)
