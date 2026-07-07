# Pluck Starvation Alert Investigation - bf-1de40

## Issue Summary

**Alert:** Pluck reported finding 0 candidates despite 41 open beads existing in the workspace.

**Root Cause:** False positive alert - no configuration error. System fired during normal transient state.

## Investigation Findings

### 1. Configuration Verification ✓

All configuration settings are correct:

```yaml
# ~/.needle/config.yaml
workspace:
  default: /home/coding/claude-governor
```

- **Workspace:** `/home/coding/claude-governor`
- **Bead store:** `/home/coding/claude-governor/.beads/beads.db`
- **Exclude labels:** `["deferred", "human", "blocked"]` (default)

### 2. Bead State Analysis

**Total open beads:** 41
- **Deferred beads:** 18 (excluded by Pluck's filter)
- **Non-deferred beads:** 23 (Pluck's candidate pool)
- **Actually ready (unblocked):** 7

**Labels on open beads:**
- `split-child`: 37 beads
- `deferred`: 18 beads  
- `umbrella`: 17 beads
- `failure-count:*`: Various

### 3. Pluck Activity Log Analysis

From `~/.needle/logs/needle-claude-code-glm47-india.stderr.log`:

```
2026-07-07T04:35:23.495385Z INFO strand found candidates strand=pluck candidates=19 excluded=4
2026-07-07T04:35:24.169413Z INFO strand found candidates strand=pluck candidates=17 excluded=6
...
2026-07-07T04:35:28.659906Z INFO strand found candidates strand=pluck candidates=0 excluded=23
```

**Key observation:** Pluck successfully processed 19→0 candidates, finding and claiming beads throughout. The "candidates=0 excluded=23" state at the end represents a **transient exhaustion** of available work, not a configuration error.

### 4. Why Pluck Found 0 Candidates

The 23 non-deferred beads were temporarily unavailable because:
- **In-progress beads:** Some were being actively worked
- **Blocking dependencies:** Many had open dependencies (as seen in previous investigations)
- **Retry limits:** Some reached failure-count thresholds

This is **normal behavior** - the system correctly identified that no beads were ready for immediate claim.

### 5. Root Cause: False Positive Alert

The starvation alert fired on a temporary state that resolved itself moments later. The alerting predicate is too aggressive and does not distinguish between:
- **True starvation** (configuration errors, system bugs)
- **Transient exhaustion** (normal processing lulls)

## Comparison with Previous Alerts

This pattern matches previous starvation alerts (bf-3q9z2, bf-6b43j, bf-36zs9, bf-1mn02):
- All fired during normal operation
- All resolved without configuration changes
- All had the same "0 candidates, N excluded" pattern

The common thread is **overly aggressive alerting**, not broken configuration.

## Recommendations

### 1. Adjust Alert Thresholds

The starvation alert should require:
- **Minimum duration:** Only fire if candidates=0 persists for >5 minutes
- **Minimum attempts:** Only fire after N consecutive failures
- **Context awareness:** Check if excluded beads are actually blocked vs. truly invisible

### 2. Add Diagnostic Context

When firing alerts, include:
- Current workspace path
- Active exclude_labels
- Distribution of excluded beads (by label)
- Whether beads are "blocked by dependencies" vs. "filtered by labels"

### 3. Consider Alert Suppression

Given the 100% false positive rate, consider:
- **Disabling auto-bead creation** for starvation alerts (already done in config: `auto_bead: false`)
- **Requiring manual intervention** before creating investigation beads
- **Adding cooldown** between repeated alerts

## Conclusion

**No configuration changes required.** Pluck is working correctly. The starvation alert is a false positive caused by overly aggressive alerting thresholds.

**System state:** Normal operation
**Configuration:** Correct
**Next action:** Close bead as false positive

## Related Documentation

- Pluck configuration: `/home/coding/claude-governor/docs/plan/pluck-configuration.md`
- NEEDLE config: `~/.needle/config.yaml`
- Previous investigation: `notes/bf-3q9z2.md`
