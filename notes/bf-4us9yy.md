# Scan Results: open_db and baseline_snapshot Imports

Task: Scan all Rust source files in src/ for `open_db` and `baseline_snapshot` imports.

## Results

### Files containing `open_db` import:
- `src/burn_rate.rs` (4 occurrences at lines 499, 3716, 3828, 3903)
  - Various forms: standalone and combined with other db imports

### Files containing `baseline_snapshot` import:
- `src/governor.rs` (7 occurrences at lines 3010, 3067, 3113, 3147, 3275, 3430, 3470)
  - All within test modules
  - Various forms: standalone and combined with other fixture imports

### Excluded (commented examples, not actual imports):
- `src/snapshot_fixtures.rs` contains 3 commented documentation examples (`/// use ...`)
  - These are doc-comment examples, not live imports
