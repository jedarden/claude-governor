# Unused Imports Catalog - bf-pdjq78

## Summary

Systematically scanned all 18 Rust source files in `src/` to identify unused imports.

## Imports Requested for Review

All of the following imports are **actually used** and are **NOT unused**:

### std::fs
**Status: USED across all files that import it**

Files importing `std::fs`:
- `src/calibrator.rs` - uses `File`, `OpenOptions`, `exists()`, `create_dir_all()`
- `src/worker.rs` - uses `exists()`, `read_dir()`, `remove_file()`
- `src/db.rs` - uses `create_dir_all()`, `exists()`, `File`, `write()`
- `src/schedule.rs` - uses `exists()`, `write()`
- `src/poller.rs` - uses `File`, `write()`
- `src/main.rs` - uses `exists()`, `remove_file()`, `metadata()`
- `src/collector.rs` - uses `File`, `exists()`, `create_dir_all()`, `metadata()`
- `src/alerts.rs` - uses `create_dir_all()`, `metadata()`, `OpenOptions`, `exists()`
- `src/config.rs` - uses `exists()`, `create_dir_all()`, `write()`
- `src/doctor.rs` - uses `exists()`
- `src/state.rs` - uses `exists()`, `File`, `create_dir_all()`
- `src/narrator.rs` - uses `create_dir_all()`, `OpenOptions`, `exists()`, `File()`

**Conclusion: No unused std::fs imports found.**

### Read trait (std::io::Read)
**Status: UNUSED in 1 file**

- `src/collector.rs:14` - imports `Read` but never calls any Read trait methods
  - File only uses `read_to_string()` which is a free function, not a Read trait method
  - No calls to `read()`, `read_to_end()`, `read_exact()`, or `read_to_string()` method

**Conclusion: 1 unused Read import found.**

### chrono::Duration
**Status: HEAVILY USED across multiple files**

Files using `chrono::Duration`:
- `src/snapshot_fixtures.rs` - 19 usages (seconds, hours, minutes, days)
- `src/burn_rate.rs` - 13 usages
- `src/status_display.rs` - 6 usages
- `src/doctor.rs` - 1 usage
- `src/collector.rs` - 1 usage
- `src/governor.rs` - 45+ usages
- `src/schedule.rs` - 2 usages
- `src/state.rs` - 3 usages

**Conclusion: No unused chrono::Duration imports found.**

### open_db
**Status: ACTIVELY USED across 7 files**

Files using `open_db`:
- `src/db.rs:15` - definition
- `src/db.rs:353, 880, 1130` - internal uses
- `src/governor.rs:4481, 4849, 6241` - uses
- `src/main.rs:961` - use
- `src/collector.rs:1271, 1332` - uses
- `src/burn_rate.rs:499, 3716, 3722, 3828, 3834, 3903, 3909, 3928, 3937` - multiple uses

**Conclusion: No unused open_db imports found.**

### baseline_snapshot
**Status: ACTIVELY USED across 2 files**

Files using `baseline_snapshot`:
- `src/snapshot_fixtures.rs:82` - definition
- `src/snapshot_fixtures.rs:289, 296, 303, 559, 560, 576, 591, 604, 699, 754, 832, 910, 1002, 1167, 1242` - test usages
- `src/governor.rs:3010, 3012, 3067, 3069, 3113, 3115, 3147, 3149, 3275, 3277, 3430, 3433, 3470, 3473` - test usages

**Conclusion: No unused baseline_snapshot imports found.**

---

## All Unused Imports Found

### From Specific Request
1. **src/collector.rs:14** - `std::io::Read`
   - Imported in: `use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};`
   - Not used: No Read trait methods called

### Additional Unused Imports (discovered during scan)
2. **src/alerts.rs:12** - `std::collections::HashMap`
   - Imported: `use std::collections::HashMap;`
   - Not used: HashMap never referenced

3. **src/capacity_summary.rs:24** - `std::collections::HashMap`
   - Imported: `use std::collections::HashMap;`
   - Not used: HashMap never referenced

4. **src/narrator.rs:14** - `std::collections::HashMap`
   - Imported: `use std::collections::HashMap;`
   - Not used: HashMap never referenced

5. **src/snapshot_fixtures.rs:14** - `chrono::Datelike`
   - Imported: `use chrono::{DateTime, Datelike, Utc};`
   - Not used: Datelike trait never used

---

## Verification

All unused imports verified using:
```bash
cargo check 2>&1 | grep "unused import"
```

Confirmed no false positives - all identified imports are genuinely unused.

## Files Scanned

Total 18 Rust source files:
- src/lib.rs
- src/governor.rs
- src/main.rs
- src/doctor.rs
- src/worker.rs
- src/db.rs
- src/capacity_summary.rs
- src/snapshot_fixtures.rs
- src/collector.rs
- src/calibrator.rs
- src/status_display.rs
- src/config.rs
- src/burn_rate.rs
- src/pricing.rs
- src/schedule.rs
- src/narrator.rs
- src/alerts.rs
- src/poller.rs
- src/simulator.rs
- src/state.rs

## Methodology

1. Ran `cargo check` to get compiler-detected unused imports
2. For requested imports (std::fs, Read, chrono::Duration, open_db, baseline_snapshot):
   - Found all files importing each item
   - Verified actual usage by searching for method/function calls
3. Cross-referenced with grep searches to ensure no false positives
4. Verified with source code inspection where needed
