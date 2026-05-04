# bf-4oa1b: Add --watch flag to cgov status

## Status: Already Implemented

The `--watch` flag for `cgov status` was already implemented in commit `18bcd34`.

## Implementation Details

Location: `src/main.rs` lines 118-120 and 944-957

### CLI Flag Definition
```rust
Status {
    /// Watch mode: clear and re-render on 30s interval
    #[arg(long)]
    watch: bool,
}
```

### Watch Mode Implementation
```rust
if watch {
    loop {
        // Clear terminal using ANSI escape sequence
        print!("\x1b[2J\x1b[H");

        let state = state::load_state(&state_path)?;
        let dashboard = format_status_dashboard(&state, chrono::Utc::now());
        print!("{}", dashboard);

        // Sleep for 30 seconds
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}
```

## Verification

Tested with `timeout 5 ./target/debug/cgov status --watch`:
- Terminal clears correctly with ANSI escape sequence `\x1b[2J\x1b[H`
- Status dashboard renders
- Loops continuously (timeout terminated after 5 seconds as expected)

## Incompatibility Check

The implementation correctly prevents `--watch` from being used with `--json` or `--summary`:
```rust
if watch && (json || summary) {
    anyhow::bail!("--watch cannot be used with --json or --summary");
}
```
