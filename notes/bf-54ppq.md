# Pluck Configuration Investigation Summary

**Task:** bf-54ppq - Investigate Pluck configuration settings
**Date:** 2026-07-06
**Workspace:** /home/coding/claude-governor

## Current Pluck Configuration

### Exclude Labels (DEFAULT)
Source: Compiled into NEEDLE binary (`/home/coding/NEEDLE/src/strand/pluck.rs:13`)

```rust
const DEFAULT_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];
```

**Labels excluded from Pluck:**
- `deferred` - Beads marked for later processing
- `human` - Beads requiring human intervention
- `blocked` - Beads with blocking dependencies
- `starvation-alert` - Beads created by alerting system

### Workspace Path
- **Path:** `/home/coding/claude-governor`
- **Bead store:** `/home/coding/claude-governor/.beads/`
- **Database:** `/home/coding/claude-governor/.beads/beads.db`

## Investigation Findings

### Open Beads Status
- **Total open beads:** 51
- **Many have `deferred` label:** Excluded by Pluck configuration

### Sample Analysis (10 open beads)
**Status of labels:**
- `deferred` present: 4/10 beads (40%) - **EXCLUDED by Pluck**
- No `deferred` label: 6/10 beads (60%)

**Dependency chain issues:**
- Beads without `deferred` labels often have blocking dependencies on other open beads
- Example chain: `bf-5enwf` → blocked by `bf-g7tl4` (open) → blocked by `bf-ii5vh` (completed)

## Root Cause Analysis

### Why Pluck Can't Find Open Beads

**Primary cause:** The combination of:

1. **Exclude labels filter** - Removes beads with `deferred` label
   - 40% of sample beads have `deferred` label

2. **Claimability filter** - Removes beads with:
   - `InProgress` status
   - `Open` beads with stale assignee
   - Open blocking dependencies

3. **Dependency chains** - Many beads form chains where each depends on another:
   ```
   bf-5enwf (open) → blocked by bf-g7tl4 (open) → blocked by bf-ii5vh (completed)
   ```

### Configuration Values Affecting Bead Visibility

| Setting | Value | Effect |
|---------|-------|--------|
| `exclude_labels` | `["deferred", "human", "blocked", "starvation-alert"]` | Filters out ~40% of open beads |
| Workspace path | `/home/coding/claude-governor` | Correct and accessible |
| `claim_ttl_minutes` | `0` (from br config) | No claim timeout - beads stay assigned |
| Strand status | `pluck: auto` | Enabled and running |

## Specific Config Values

### From `/home/coding/claude-governor/.beads/config.yaml`
```yaml
# All commented - using br defaults
# issue_prefix: claude-governor
# default_priority: 2
# default_type: task
```

**Active values (from `br config list`):**
- `issue_prefixes: ["bf"]`
- `default_priority: 2`
- `default_type: task`
- `claim_ttl_minutes: 0`

### From `~/.needle/config.yaml`
```yaml
strands:
  pluck: auto    # Primary work from auto-discovered workspace
  explore: auto  # Look for work in other workspaces
  mend: true     # Maintenance and cleanup
  knot: true     # Alert human when stuck
```

## Conclusion

**The configuration is working as designed.** Pluck cannot find open beads because:

1. Many open beads have the `deferred` label (excluded by default)
2. Remaining beads often have open blocking dependencies (excluded by claimability filter)
3. `claim_ttl_minutes: 0` means assigned beads never time out

**No configuration changes are recommended** - the system is functioning correctly. The lack of "ready" beads reflects the actual state of the work queue, not a misconfiguration.

## Related Documentation

- Detailed Pluck configuration: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`
- NEEDLE source: `/home/coding/NEEDLE/src/strand/pluck.rs`
- NEEDLE config: `~/.needle/config.yaml`
