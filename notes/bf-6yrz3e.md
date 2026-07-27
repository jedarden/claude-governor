# Bead bf-6yrz3e: Update parent bead with fix requirements

## Summary
Updated the parent bead (bf-8uspdf) with fix requirements based on the completed investigation into the weekly_scoped sonnet_pct hard-coding bug.

## What Was Done

1. **Analyzed dependency completion**: Reviewed the just-closed bead bf-5b8khw which had completed adding inline code comments documenting all 8 locations where sonnet_pct is incorrectly hard-coded for weekly_scoped utilization.

2. **Updated parent bead (bf-8uspdf)**: Added comprehensive fix requirements to the parent bead "Locate and document the sonnet_pct hard-coding for weekly_scoped" including:
   - Investigation completion status
   - Core bug pattern explanation
   - Required changes (is_weekly_scoped_sonnet() check, use weekly_scoped_pct)
   - Specific locations requiring fixes (6 in governor_cycle_behavior_test.rs, 2 in fixtures.rs)
   - Verification steps

3. **Documented the fix approach**: The parent bead now clearly specifies:
   - Add is_weekly_scoped_sonnet() check before using sonnet_pct for weekly_scoped context
   - Replace legacy sonnet_pct references with model-agnostic weekly_scoped_pct from limits[] array
   - Run cargo test to verify model-agnostic approach works correctly

## Context
This bead was part of a larger investigation chain (bf-8uspdf → bf-6yrz3e → bf-5b8khw → bf-4sg5wa) that documented a bug where the weekly_scoped window (which is model-agnostic and can track Sonnet, Opus, Fable, etc.) was incorrectly hard-coded to always use sonnet_pct regardless of which model it was actually tracking.

## Next Steps
The parent bead bf-8uspdf now has clear fix requirements and can be unblocked to proceed with implementation.
