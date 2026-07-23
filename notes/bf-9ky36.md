# Bead bf-9ky36: Update plan.md stale sections

## Finding: All issues already fixed in previous commits

The issues described in this bead were already addressed by the following commits:

### Commit d06850e (2026-07-03): "Fix file layout section in plan.md"
- ✓ Changed systemd/ directory reference to config/
- ✓ Added missing src modules: capacity_summary.rs, status_display.rs, pricing.rs, db.rs, config.rs, lib.rs
- ✓ Added install.sh platform handling (linux-arm64 support)
- ✓ Added Makefile to the file tree

### Commit 9f43fc2 (2026-07-03): "Update plan.md to align with implemented artifacts"
- ✓ Expanded doctor health checks table from 12 to ~20 checks
- ✓ Clarified empty promotions.json as default (flat 1.0 multiplier)
- ✓ Added explicit default config example showing empty array
- ✓ Added detailed doctor output example
- ✓ Fixed File Layout section structure

### Commit 0c46843 (2026-07-23): "Reframe March 2026 promotion as historical example"
- ✓ Marked March 2026 promotion as historical example
- ✓ Documented empty array [] as shipped configuration and normal operating state
- ✓ Updated all references to treat it as historical, not active/current

## Current state verification

All acceptance criteria are already met:

1. ✓ plan.md file-layout tree shows config/ (not systemd/)
2. ✓ All src modules are present in the listing
3. ✓ No ~/.needle/config/governor.yaml references (grep found none)
4. ✓ March 2026 promotion described as historical example
5. ✓ Empty promotions.json [] documented as no-promo default
6. ✓ No duplicate risk-item numbering (items 1-10 are sequential)
7. ✓ Doctor section shows ~20 health checks (not 12)

## Minor observation

The plan lists `tests/fixtures.rs` but does not mention `src/snapshot_fixtures.rs`. Both files exist:
- `tests/fixtures.rs` (605 lines) - general test fixtures
- `src/snapshot_fixtures.rs` (896 lines) - snapshot delta computation fixtures

This is a very minor documentation gap and does not affect the accuracy of the plan's description of the main source modules.

## Conclusion

No changes to plan.md were required. All issues described in the bead were already resolved by commits from July 3 and July 23, 2026. The bead appears to have been created before those fixes were applied.
