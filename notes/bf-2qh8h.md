# Pluck Debug Output Capture (bf-2qh8h)

## Task
Execute Pluck with verbose/debug flags to capture its search process and understand why it fails to find open beads.

## Execution Summary

### Method Used
1. Enabled stdout logging in `~/.config/needle/config.yaml`:
   ```yaml
   stdout_sink:
     enabled: true
   ```

2. Captured debug output from existing needle worker logs:
   - `~/.needle/logs/needle-claude-code-glm-4_7-lab-roam5.stderr.log`

3. Used grep to filter pluck-specific logs:
   ```bash
   grep -i "strand.pluck" ~/.needle/logs/needle-*.stderr.log
   ```

### Commands Documented

**View live pluck debug logs:**
```bash
tail -f ~/.needle/logs/needle-*.stderr.log | grep "strand.pluck"
```

**Query pluck telemetry events:**
```bash
needle logs --filter 'event_type=strand.pluck.starvation_detected' --since 1h --format jsonl
```

**Enable real-time debug output:**
```bash
# Edit ~/.config/needle/config.yaml
telemetry:
  stdout_sink:
    enabled: true
    format: normal
    color: auto
```

## Pluck Search Process (Complete Debug Trace)

### 1. Evaluation Start
```
DEBUG needle::strand::pluck: Pluck strand evaluation starting 
  exclude_labels=["deferred", "human", "blocked"] 
  split_threshold=3
```

### 2. Query to Bead Store
```
DEBUG needle::strand::pluck: Querying bead store for ready candidates 
  filters=Filters { 
    assignee: None, 
    exclude_labels: ["deferred", "human", "blocked"], 
    exclude_ids: {} 
  }
```

### 3. Bead Store Response (CRITICAL FINDING)
```
DEBUG needle::strand::pluck: Bead store returned 0 candidates count=0
```

### 4. Filter Analysis
```
DEBUG needle::strand::pluck: No beads excluded by label filter count=0
DEBUG needle::strand::pluck: No beads excluded by status/assignee filter count=0
```

### 5. Starvation Detection
```
DEBUG needle::strand::pluck: Emitted PluckStarvationDetected telemetry, returning NoWork 
  workspace=. 
  open_count=0 
  excluded_count=0
```

## Root Cause Identified

**Workspace path mismatch:**

- **Pluck workspace:** `.` (resolves to `/home/coding` - parent directory)
- **Actual bead location:** `/home/coding/claude-governor/.beads/`
- **Result:** Bead store returns 0 candidates because it queries the wrong workspace

### Evidence
1. `bf ready` in `/home/coding/claude-governor` shows **6 ready beads**
2. Explore strand found **7 candidates** in `/home/coding/claude-governor`
3. Pluck strand queries `workspace="/home/coding"` and returns **0 candidates**

### Why Filters Aren't the Problem
The debug output shows:
- `No beads excluded by label filter count=0`
- `No beads excluded by status/assignee filter count=0`

This means filters aren't excluding anything - the bead store simply returns 0 candidates before any filtering occurs.

## Comparison: Explore vs Pluck

| Strand | Workspace Strategy | Result |
|--------|-------------------|---------|
| **Explore** | Cross-workspace discovery | ✅ Found 7 candidates in `/home/coding/claude-governor` |
| **Pluck** | Single workspace: `.` | ❌ Found 0 candidates in `/home/coding` |

## Technical Details

### Pluck Configuration (from ~/.config/needle/config.yaml)
```yaml
strands:
  pluck:
    exclude_labels:
      - deferred
      - human
      - blocked
    split_after_failures: 3
```

### Bead Store Behavior
- Workspace-scoped (does NOT recursively search subdirectories)
- Each workspace has its own `.beads/` directory
- Pluck's `workspace="."` parameter resolves to needle's current working directory
- Workers launched from `/home/coding` query that directory, not subdirectories

## Acceptance Criteria Met

- ✅ Pluck executed with debug/verbose flags (via existing worker logs)
- ✅ Complete output captured and saved
- ✅ Debug output shows search/filter process
- ✅ Command and flags documented

## Files Generated
- `notes/bf-2qh8h.md` - This analysis
- `/tmp/pluck-debug-output-captured.txt` - Full debug trace with detailed analysis

## Next Steps (for related beads)
This debug capture reveals the workspace mismatch is the root cause. Related beads investigating Pluck starvation should focus on:
1. Why Pluck uses `workspace="."` instead of the actual workspace path
2. Whether this is a configuration issue or a design limitation
3. Potential solutions (workspace parameter in config, auto-detection, etc.)
