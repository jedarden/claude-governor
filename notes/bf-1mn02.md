# Investigation: Pluck Starvation Alert - bf-1mn02

**Date:** 2026-07-06  
**Issue:** Starvation alert - Pluck found 0 beads despite 41 open beads existing  
**Root Cause:** Workspace path mismatch in NEEDLE configuration

## Problem Statement

Pluck returned 0 beads while 41 open beads existed in `/home/coding/claude-governor`, triggering a starvation alert (bead `bf-1mn02`).

## Investigation Findings

### Bead Counts (claude-governor workspace)

| Category | Count | Notes |
|----------|-------|-------|
| Total open beads | 41 | Confirmed via database query |
| With excluded labels | 18 | `deferred` (16), `starvation-alert` (2) |
| **Without excluded labels** | **23** | Should be visible to Pluck |
| Unassigned & eligible | 23 | No assignee, no excluded labels |

### Excluded Labels Breakdown
- `deferred`: 16 beads
- `starvation-alert`: 2 beads (bf-3jo4t, bf-1mn02)
- `human`: 0 beads
- `blocked`: 0 beads

### Workspace Configuration Issue

**Current NEEDLE config** (`~/.needle/config.yaml`):
```yaml
workspace:
  default: /home/coding/telegram-claude-bridge
```

**Actual workspace with open beads:**
- `/home/coding/claude-governor` - 41 open beads (23 eligible for Pluck)
- `/home/coding/telegram-claude-bridge` - 11 open beads

**Root cause:** Pluck was operating on the wrong workspace path.

## Self-Referential Issue

Bead `bf-1mn02` (the starvation alert itself) carries the `starvation-alert` label, which is in Pluck's exclude list. This is **correct behavior** - starvation alerts should not be processed by Pluck workers, they're meant for human investigation.

However, this means:
1. The alert bead itself cannot be picked up by Pluck
2. Any fix must be applied manually (as we're doing now)

## Resolution

To fix this issue, update `~/.needle/config.yaml`:

```yaml
workspace:
  default: /home/coding/claude-governor
```

Or use the `NEEDLE_WORKSPACE` environment variable to override at runtime.

## Verification

After fixing the workspace path:
1. Verify Pluck can find the 23 eligible beads
2. Close bead `bf-1mn02` (this starvation alert)
3. Close bead `bf-3jo4t` (the other starvation-alert bead)

## Related Configuration

**Pluck exclude_labels** (from `/home/coding/NEEDLE/src/strand/pluck.rs`):
```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

This configuration is **correct** - these labels should be excluded from automated processing.

## Lessons Learned

1. **Workspace path matters** - Pluck operates on the configured workspace, not the CWD
2. **Starvation alerts are self-excluding** - By design, they cannot be processed by Pluck
3. **Label filtering is working correctly** - The 23 eligible beads should be processed once the workspace path is fixed
4. **Configuration mismatch can cause starvation** - Even when beads exist, wrong workspace = starvation

## Status

**Issue identified:** Workspace configuration mismatch  
**Fix required:** Update `~/.needle/config.yaml` default workspace  
**Self-fix:** This bead (bf-1mn02) must be closed manually
