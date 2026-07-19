# Per-Window Target Utilization Override Implementation

## Task Analysis

Task bf-tfuo0 requested implementation of per-window target_utilization/ceiling overrides for the Claude Governor config system. However, upon investigation, **this functionality was already fully implemented** in the codebase.

## Current Implementation Status

### ✅ Already Implemented in src/config.rs

1. **Data Structure** (lines 180-196):
   - `windows: HashMap<String, WindowOverrideConfig>` field in `DaemonConfig`
   - `WindowOverrideConfig` struct with `target_utilization: Option<f64>` field
   - Full serde deserialization support

2. **Accessor Method** (lines 247-285):
   - `get_target_ceiling_for_window(&self, window_name: &str) -> f64`
   - Returns window-specific override if configured, otherwise global default
   - Converts 0-1 range to 0-100 percentage

3. **Comprehensive Tests** (lines 949-1051):
   - `test_window_config_defaults` - fallback to global
   - `test_window_config_custom` - all windows overridden
   - `test_window_config_partial_override` - some windows overridden
   - `test_window_config_none_utilization` - missing target_utilization field
   - `test_window_config_empty_map` - empty windows HashMap

### ✅ Already Integrated in src/governor.rs

The governor correctly uses per-window overrides:
- Line 3351: `let base_target_ceiling = pricing_config.daemon.get_target_ceiling_for_window(window);`
- Line 3397: Passes `effective_target_ceiling` to `generate_window_forecast()`
- Each of the three windows (five_hour, seven_day, seven_day_sonnet) gets its specific ceiling

### ✅ Already Supported in src/burn_rate.rs

The burn rate module correctly handles per-window ceilings:
- `generate_window_forecast()` accepts `target_ceiling` parameter
- Each window's forecast uses its own ceiling value
- Safe worker calculations respect window-specific ceilings

## Changes Made for This Task

### 1. Added Documentation to config/governor.yaml

Added commented example showing how to use per-window overrides:
```yaml
# Per-window overrides available (optional):
#   windows:
#     five_hour:
#       target_utilization: 0.85  # tighter to avoid mid-task cutoff
#     seven_day:
#       target_utilization: 0.90
#     seven_day_sonnet:
#       target_utilization: 0.90
```

This documents the capability without changing shipped defaults.

### 2. Added Additional Tests to src/config.rs

- `test_window_config_five_hour_tighter`: Tests the plan's design intent (five_hour at 85%)
- `test_window_config_all_different`: Tests all three windows with distinct ceilings

## Verification

- ✅ `cargo build --release` - clean build, no errors
- ✅ `cargo test --release` - all 548 tests pass
- ✅ Config tests: 7 window config tests all pass
- ✅ Governor integration: per-window ceilings correctly used
- ✅ No defaults changed in shipped config file

## Conclusion

The per-window target utilization override functionality was **already fully implemented and operational** in the codebase. The task description evidence was outdated - the grep patterns mentioned in the task description do match the code, and the implementation is complete.

The only missing element was user-facing documentation in the shipped config file, which has now been added as comments.
