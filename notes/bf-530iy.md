# Bead bf-530iy: Add logging for computed deltas

## Summary
Added INFO-level logging for computed window deltas in `src/governor.rs` to aid debugging.

## Changes Made

### 1. Updated first delta calculation log (line 1450)
Changed from `debug!` to `info!` and added timestamp:
- Before: `log::debug!("[governor] API window deltas: 5h={:+.3}% 7d={:+.3}% 7ds={:+.3}%", ...)`
- After: `log::info!("[governor] {} computed window deltas: 5h={:+.3}% 7d={:+.3}% 7ds={:+.3}%", ...)`

### 2. Updated second delta calculation log (line 1828)
Added timestamp for consistency:
- Before: `log::info!("[governor] API delta in {:.0}s: 5h={:+.3}% 7d={:+.3}% 7ds={:+.3}% ...", elapsed_secs, ...)`
- After: `log::info!("[governor] {} computed window deltas (in {:.0}s): 5h={:+.3}% 7d={:+.3}% 7ds={:+.3}% ...", now.to_rfc3339(), elapsed_secs, ...)`

## Acceptance Criteria Met
✅ Deltas logged at INFO level after each calculation
✅ Log messages include all three window deltas (p5h, p7d, p7ds)
✅ Log format is consistent and readable
✅ Timestamps included in both log locations
✅ `cargo test` passes (469 tests passed)

## Log Format
Both logs now follow a consistent format:
`[governor] <timestamp> computed window deltas: 5h=<value>% 7d=<value>% 7ds=<value>%`

This provides clear debugging visibility into window percentage changes computed from consecutive API poll snapshots.
