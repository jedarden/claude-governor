# Import Scan Results for Bead bf-3xomr1

## Scan Summary

Scanned all 17 Rust source files in `src/` for three specific imports: `std::fs`, `Read`, and `chrono::Duration`.

## Files Containing `std::fs` Import (12 files)

| File | Import Statement |
|------|-----------------|
| `src/alerts.rs` | `use std::fs::OpenOptions;` |
| `src/calibrator.rs` | `use std::fs::{File, OpenOptions};` |
| `src/collector.rs` | `use std::fs::{self, OpenOptions};` |
| `src/config.rs` | `use std::fs;` |
| `src/db.rs` | `use std::fs;` |
| `src/doctor.rs` | `use std::fs;` |
| `src/main.rs` | `use std::fs;` |
| `src/narrator.rs` | `use std::fs::{File, OpenOptions};` |
| `src/poller.rs` | `use std::fs::{self, File};` |
| `src/schedule.rs` | `use std::fs;` |
| `src/state.rs` | `use std::fs;` |
| `src/worker.rs` | `use std::fs;` |

## Files Containing `Read` Import (1 file)

| File | Import Statement |
|------|-----------------|
| `src/collector.rs` | `use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};` |

**Note:** Other files import `std::io` items (BufRead, BufReader, Write, etc.) but do not import the `Read` trait specifically.

## Files Containing `chrono::Duration` Import (2 files)

| File | Import Statement |
|------|-----------------|
| `src/simulator.rs` | `use chrono::{DateTime, Duration, Utc};` |
| `src/worker.rs` | `use chrono::{DateTime, Duration as ChronoDuration, Utc};` |

**Note:** Several files (governor.rs, burn_rate.rs, schedule.rs, status_display.rs, snapshot_fixtures.rs, state.rs) use `chrono::Duration` in code (e.g., `chrono::Duration::hours(5)`) without a top-level `use chrono::Duration` import—they invoke it via the full path. The `use chrono::Duration;` statements in governor.rs (lines 3431, 3471, 9402, 9626) are scoped within test functions, not top-level imports.

## Files with None of These Imports (5 files)

- `src/lib.rs`
- `src/capacity_summary.rs`
- `src/pricing.rs`
- `src/status_display.rs`
- `src/snapshot_fixtures.rs`

## Verification

All findings are based on actual top-level `use` statements in each file. No false positives included—only genuine import declarations.
