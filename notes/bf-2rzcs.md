# Bead bf-2rzcs: Staleness Detection Already Implemented

## Task
Add staleness detection to heartbeat parsing in `src/worker.rs`.

## Finding
The requested staleness detection was already fully implemented in a previous commit (c58175d "Implement stale-heartbeat handling for worker manager").

## Implementation Details

### Constant (Line 20)
```rust
const STALE_HEARTBEAT_THRESHOLD: i64 = 60; // seconds
```

### Age Computation (Line 453)
```rust
let age = now.signed_duration_since(hb.timestamp);
```

### Staleness Detection (Lines 454-479)
```rust
let is_stale = age > stale_threshold;

if is_stale {
    // Stale heartbeat — verify against tmux
    let session_exists = tmux_sessions_set.contains(&hb.session);

    if !session_exists {
        // Session no longer exists, remove orphaned heartbeat file
        log::info!(...);
        let _ = fs::remove_file(&path);
        continue;
    }

    // Session exists but heartbeat is stale — treat as executing
    log::debug!(...);
    hb.is_idle = false;
}

heartbeats.insert(hb.session.clone(), hb);
```

## Verification
- All 460 tests pass
- Comprehensive test coverage for staleness scenarios:
  - `test_stale_heartbeat_dead_session_removed`
  - `test_stale_heartbeat_live_session_retained_as_executing`
  - `test_fresh_heartbeat_unchanged_behavior`
  - `test_mixed_stale_and_fresh_heartbeats`
  - `test_stale_threshold_boundary`
  - `test_one_second_below_threshold_not_stale`

## Conclusion
The task acceptance criteria are fully met by the existing implementation. No code changes were required.
