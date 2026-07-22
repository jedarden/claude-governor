# Bead bf-en75g: Remove orphaned heartbeat files for dead tmux sessions

## Status: ALREADY IMPLEMENTED

This bead's requirements have already been fully implemented in previous child bead work.

## Implementation Verification

The orphaned heartbeat file removal is implemented in `src/worker.rs` in the `read_heartbeats()` function (lines 456-468):

```rust
if is_stale {
    // Stale heartbeat — verify against tmux
    let session_exists = tmux_sessions_set.contains(&hb.session);

    if !session_exists {
        // Session no longer exists, remove orphaned heartbeat file
        log::info!(
            "[worker] removing stale heartbeat for session {} (session not in tmux, age={}s)",
            hb.session,
            age.num_seconds()
        );
        let _ = fs::remove_file(&path);
        continue;
    }
    // ...
}
```

## Acceptance Criteria Met

✅ **Orphaned heartbeat files deleted**: When heartbeat is stale AND tmux session no longer exists, file is removed using `std::fs::remove_file()`

✅ **INFO level logging**: Removal is logged at INFO level with session_id and age:
```rust
log::info!(
    "[worker] removing stale heartbeat for session {} (session not in tmux, age={}s)",
    hb.session,
    age.num_seconds()
);
```

✅ **Excluded from heartbeat count**: The `continue;` statement on line 468 excludes removed heartbeats from the returned HashMap

✅ **Unit test exists**: `test_stale_heartbeat_dead_session_removed()` (lines 751-785) creates a stale heartbeat file and verifies the file is deleted when the tmux session doesn't exist

✅ **cargo test passes**: All 18 worker tests pass, including:
- `test_stale_heartbeat_dead_session_removed` - main test for this feature
- `test_mixed_stale_and_fresh_heartbeats` - verifies cleanup in mixed scenarios
- `test_count_workers_consistent_after_cleanup` - verifies consistency after cleanup
- `test_find_workers_to_stop_excludes_stale` - verifies stale heartbeats excluded from scale operations

## Implementation Location

- **File**: `src/worker.rs`
- **Function**: `read_heartbeats()` (lines 414-498)
- **Orphan removal logic**: Lines 456-468
- **Tests**: Lines 751-785 (primary test), plus related integration tests

## Related Features

This feature works in conjunction with:
- Staleness detection (60-second threshold)
- Tmux session verification via `count_tmux_sessions()`
- Worker count consistency checks via `count_workers()`
