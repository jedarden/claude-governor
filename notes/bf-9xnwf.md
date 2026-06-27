# Bead bf-9xnwf: Per-Entrypoint Token Metrics Visibility

## Status: Already Implemented

This work was completed in commit `a3d8b04` on 2026-06-27.

## What Was Implemented

The bead required exposing per-entrypoint token metrics in `cgov status` after the collector fix (bf-al0zl) that separated CLI vs SDK-CLI sessions.

### Implementation Details

1. **collector.rs - Dual UsageRecord streams**
   - Lines 949-958 separate `cli` (subscription) from `sdk-cli` (credits) during aggregation
   - Both streams are collected in parallel for visibility

2. **FleetRecord struct - New fields**
   - `cli_tokens: u64` - Subscription tokens burned
   - `cli_cost: f64` - Subscription USD cost
   - `sdk_tokens: u64` - Credit tokens (informational only)
   - `sdk_cost: f64` - Credit USD cost (informational only)

3. **status_display.rs - Billing breakdown section**
   - Lines 241-270 display the breakdown
   - Shows subscription tokens with burn rate (tok/5h)
   - Shows credit tokens marked as informational (not in quota windows)

4. **Governor forecasting**
   - Uses CLI-only burn rate for subscription quota decisions
   - SDK-CLI tokens are informational only

## Design Rationale

- **Subscription (CLI) tokens**: Used for quota forecasting and governor scaling decisions
- **Credits (SDK-CLI) tokens**: Informational visibility only, not included in quota windows
- Both are tracked to give operators complete visibility into billing breakdown

## Files Modified in Original Implementation

- `src/collector.rs` - Separation logic and struct fields
- `src/governor.rs` - Extract cli/sdk fields from fleet JSONL
- `src/state.rs` - FleetAggregate struct updated with billing fields
- `src/status_display.rs` - Billing breakdown UI and JSON output

## Verification

All requirements from bead bf-9xnwf were satisfied in the original commit:
- ✓ Parallel UsageRecord collection (subscription + credit)
- ✓ FleetRecord struct fields added
- ✓ Status display 'Billing breakdown' section
- ✓ Forecasting uses CLI-only burn rate
