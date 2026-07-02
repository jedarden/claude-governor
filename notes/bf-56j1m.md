# Bead bf-56j1m: Stale Heartbeat Verification

## Summary
This bead requested implementation of stale heartbeat verification against tmux sessions. Upon inspection, the feature has already been fully implemented in a previous change.

## Existing Implementation (src/worker.rs:414-498)

The `read_heartbeats()` function already implements all required functionality:

1. **Gets active tmux sessions** (line 436):
   ```rust
   let (_, tmux_sessions) = count_tmux_sessions(session_prefix);
   let tmux_sessions_set: std::collections::HashSet<String> =
       tmux_sessions.into_iter().collect();
   ```

2. **Detects stale heartbeats** (line 454):
   ```rust
   let is_stale = age > stale_threshold;
   ```

3. **Verifies against tmux sessions** (line 458):
   ```rust
   let session_exists = tmux_sessions_set.contains(&hb.session);
   ```

4. **Distinguishes outcomes**:
   - **Orphaned sessions** (lines 461-468): Session not in tmux → removes heartbeat file with INFO log
   - **Stale but alive** (lines 473-478): Session exists → sets `is_idle = false` with DEBUG log

5. **Logging levels**:
   - INFO: `[worker] removing stale heartbeat for session {} (session not in tmux, age={}s)`
   - DEBUG: `[worker] stale heartbeat for session {} but session exists (age={}s), treating as executing`

## Test Coverage
All 18 tests pass, including:
- `test_stale_heartbeat_dead_session_removed` - verifies orphaned heartbeat removal
- `test_stale_heartbeat_live_session_retained_as_executing` - verifies stale-but-alive handling
- `test_count_workers_consistent_after_cleanup` - verifies consistency restoration
- `test_find_workers_to_stop_excludes_stale` - verifies stale heartbeats excluded from shutdown

## Conclusion
No implementation work was required. The feature was already implemented in commit c58175d ("Implement stale-heartbeat handling for worker manager").
