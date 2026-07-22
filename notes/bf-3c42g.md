# Task bf-3c42g: Exclude Orphans from Worker Counting and Shutdown Selection

## Status: Already Implemented

All requirements for this task are already fully implemented in `src/worker.rs`. The orphan exclusion logic was implemented in previous beads and is working correctly.

## Implementation Details

### 1. count_workers() Excludes Orphans (Line 124-136)
```rust
pub fn count_workers(config: &WorkerConfig) -> WorkerCount {
    let heartbeat_count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
    let (tmux_count, sessions) = count_tmux_sessions(&config.session_prefix);
    WorkerCount {
        heartbeat_count,
        tmux_count,
        consistent: heartbeat_count == tmux_count,
        sessions,
    }
}
```
- Calls `count_heartbeat_files()` → `read_heartbeats()`
- `read_heartbeats()` removes orphaned heartbeat files (stale + session doesn't exist in tmux)
- Returns count of only valid heartbeats

### 2. find_workers_to_stop() Excludes Orphans (Line 379-401)
```rust
fn find_workers_to_stop(n: usize, config: &WorkerConfig) -> Vec<String> {
    let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);
    // ... sort and selection logic
}
```
- Calls `read_heartbeats()` which removes orphans before sorting
- Never returns a worker with a dead tmux session

### 3. Sort Order Preserved (Line 383-394)
Workers sorted by:
1. Idle workers first (is_idle = true)
2. Then by heartbeat age (oldest first)
- Orphans filtered out before sorting takes effect
- Fresh (< 60s) heartbeats unchanged behavior

### 4. Consistent Flag Recovery
- Before cleanup: `heartbeat_count` includes orphans, `tmux_count` doesn't
- After cleanup: `heartbeat_count` excludes orphans, equals `tmux_count`
- `consistent = heartbeat_count == tmux_count` becomes true

## Test Coverage (All Passing)

✓ `test_count_workers_consistent_after_cleanup` - Verifies consistent recovers to true
✓ `test_find_workers_to_stop_excludes_stale` - Verifies orphans excluded from shutdown
✓ `test_stale_heartbeat_dead_session_removed` - Verifies orphan removal
✓ `test_mixed_stale_and_fresh_heartbeats` - Verifies mixed handling
✓ `test_fresh_heartbeat_unchanged_behavior` - Verifies fresh heartbeats unchanged
✓ All other worker tests pass (50 tests total)

## Orphan Detection Logic (read_heartbeats, Line 414-498)

1. Read all heartbeat files from heartbeat_dir
2. For each heartbeat:
   - Check if it matches session_prefix
   - Calculate age: `now - timestamp`
   - If age > STALE_HEARTBEAT_THRESHOLD (60s):
     - Check if session exists in tmux
     - If NOT: remove orphaned heartbeat file
     - If YES: treat as executing (is_idle = false)
   - Add to returned HashMap
3. Return only valid heartbeats

## Acceptance Criteria Met

- ✓ count_workers().consistent recovers to true after orphaned heartbeats are cleaned up
- ✓ find_workers_to_stop() never returns a worker with a dead tmux session
- ✓ Unit tests: orphans excluded from counts
- ✓ Unit tests: orphans excluded from shutdown candidates
- ✓ No behavior change for fresh (<60s) heartbeats
- ✓ cargo test passes (527 tests, 0 failed)

## Conclusion

The implementation is complete and working correctly. All acceptance criteria are met. No code changes needed.
