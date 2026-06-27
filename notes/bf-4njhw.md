# Bead bf-4njhw: Safe-mode logging documentation

## Task Completed

Located and documented the current implementation of the safe-mode warning in `run_scale_command`.

## Findings

### Location
- **File**: `src/main.rs`
- **Function**: `run_scale_command`
- **Line**: 550

### Current Implementation
```rust
// Check if safe mode is active
if state.safe_mode.active {
    log::warn!("[governor] WARN: manual scale override during safe mode");
}
```

### Logging Framework
- **Crate**: `log` (standard Rust logging facade)
- **Backend**: `env_logger` (initialized in `main()` at line 918)
- **Level**: `LevelFilter` (Debug if verbose, Info otherwise)

### [governor] Prefix Pattern
The `[governor]` prefix is used consistently throughout the codebase for governor-related log messages. Examples from `src/governor.rs`:

```rust
log::info!("[governor] === cycle start at {} ===", now.to_rfc3339());
log::info!("[governor] polled usage: sonnet={:.1}%, all_models={:.1}%, 5h={:.1}%",
log::warn!("[governor] poll failed, keeping previous usage data: {}", e);
log::warn!("[governor] EMERGENCY BRAKE: scaling all to 0");
log::info!("[governor] scaling up by {} workers", n);
```

### Context
The safe-mode warning is triggered when a user manually runs `cgov scale` while safe mode is active. Safe mode activates when:
- Median absolute error exceeds `SAFE_MODE_ENTRY_ERROR_THRESHOLD` (15.0 pct points)
- Emergency brake is triggered (98% utilization threshold)

The warning logs at WARN level to indicate a manual override during a conservative operating mode.

## Next Steps
This information enables the next bead to update the log message correctly.
