# Bead bf-45jp8: Add explicit first-poll skip logic with else block

## Status: Already Completed

The work described in this bead was already completed in commit d9c67c2 for bead bf-3trgh on 2026-07-02.

## Implementation Details

Location: `src/governor.rs` lines 2034-2040

```rust
} else {
    // First poll: prev_snapshot is None, cannot compute delta
    // Ensure delta fields remain at default (0.0) - no update needed
    log::debug!(
        "[governor] first poll detected (no previous snapshot), skipping delta computation"
    );
}
```

## Acceptance Criteria - All Met

- ✅ Code has explicit else block handling None case
- ✅ Clear comments explain first-poll skip
- ✅ Code compiles without errors (verified with cargo check)
- ✅ No panics or unwrap() in the None path
- ✅ Delta fields are NOT initialized (as specified - control flow only)

## Verification

```bash
cargo check  # Compiles successfully
```

The implementation correctly handles the first-poll case where `previous_api_snapshot` is None, allowing the governor cycle to run without crashing or attempting to compute deltas from a single snapshot.
