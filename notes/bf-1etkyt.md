# Bead bf-1etkyt: _observe subcommand scaffold verification

## Status: COMPLETE (Already implemented)

The `_observe` subcommand scaffold was already fully implemented in the codebase.

## Verification

All acceptance criteria met:

1. ✅ `run_internal_observe_command()` function exists at line 1655
2. ✅ Wired in main.rs match arm at lines 1202-1204
3. ✅ Function loads config (`GovernorConfig::load()`) and state_path (`default_state_path()`) successfully
4. ✅ TODO comment indicates what logic needs to be extracted (lines 1659-1664)
5. ✅ "cgov _observe" runs without errors — outputs: `[observe] Observe cycle not yet implemented`
6. ✅ No actual observe logic yet — just scaffold (function only logs a message)

## Implementation Details

The function is at `src/main.rs:1655-1668`:

```rust
fn run_internal_observe_command() -> Result<()> {
    let _config = GovernorConfig::load()?;
    let _state_path = default_state_path();

    // TODO: Implement observe cycle logic
    // This should:
    // 1. Poll usage data from Anthropic API
    // 2. Compute capacity forecast
    // 3. Calibrate predictions
    // 4. Write state with updated forecast and calibration

    log::info!("[observe] Observe cycle not yet implemented");
    Ok(())
}
```

The match arm that calls it is at lines 1202-1204:
```rust
Commands::_Observe {} => {
    run_internal_observe_command()?;
}
```

## Command Tested

```bash
$ cgov _observe
[2026-08-04T03:21:59Z INFO  cgov] [observe] Observe cycle not yet implemented
```

The scaffold is ready for the actual observe logic to be extracted from the governor daemon loop in future beads.
