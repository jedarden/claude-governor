# Pluck Debug Flags Investigation - Bead bf-9gjr8i

**Date:** 2026-08-03  
**Workspace:** `/home/coding/claude-governor`  
**Task:** Investigate Pluck's help to identify debug flags

## Summary

Pluck is the NEEDLE bead discovery system accessed via `bf ready`. Investigation revealed no built-in command-line debug flags, but Rust environment variables provide debug logging capabilities.

## Pluck Command Reference

**Command:** `bf ready` (bead-forge CLI)  
**Purpose:** Show ready (unblocked) beads for worker assignment

### Available Options

```bash
bf ready [OPTIONS]

Options:
      --limit <LIMIT>          Limit results (0 = unlimited, default 10)
  -w, --workspace <WORKSPACE>   Workspace directory (defaults to current directory's .beads/)
      --format <FORMAT>        Output format (text, json, toon) [default: text]
      --json                   Output JSON (alias for --format json)
      --no-auto-flush          Disable automatic SQLite→JSONL flush
      --envelope               Wrap JSON output in standard envelope
  -h, --help                   Print help
```

## Debug Flags Found

### ❌ No Built-in Command-line Debug Flags

The following flags do **NOT** exist in bead-forge 0.4.0:
- `--verbose` or `-v`
- `--debug` or `-d` 
- `--trace` or `-t`

### ✅ Rust Environment Variables (Working)

**Primary debug method:**
```bash
RUST_LOG=debug bf ready
```

**Available levels:**
- `RUST_LOG=error` - Error messages only
- `RUST_LOG=warn` - Warnings and errors
- `RUST_LOG=info` - Informational messages
- `RUST_LOG=debug` - Debug messages (recommended for troubleshooting)
- `RUST_LOG=trace` - Trace messages (most verbose)

**Module-specific logging:**
```bash
RUST_LOG=bead_forge=debug bf ready
RUST_LOG=bead_forge=debug,sled=debug bf ready  # Includes database logging
```

## Configuration

**No debug-related config options** in `~/.beads/config.yaml`:
```yaml
issue_prefixes: ["bf"]
default_priority: 2
default_type: task
claim_ttl_minutes: 30
```

## Acceptance Criteria

- [x] **Pluck help output examined:** ✅ Reviewed `bf --help`, `bf ready --help`, `bf config --help`
- [x] **Correct debug/verbose flag identified:** ✅ Found `RUST_LOG=debug` environment variable
- [x] **Flag documented for next step:** ✅ Documented in this note

## Recommendations

For debugging Pluck queries in the next step (bead bf-4c4ip), use:

```bash
RUST_LOG=debug bf ready --limit 0 --format json
```

This provides:
- Full result set (`--limit 0`)
- Structured output (`--format json`)  
- Debug logging (`RUST_LOG=debug`)

## Related Beads

- **bf-4c4ip:** Run Pluck with verbose debug output (next step)
- **bf-28oar:** Verify and log Pluck query construction with exact filters
