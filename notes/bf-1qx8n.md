# Governor Module Examination - BF-1qx8n

## Task Summary
Examine src/governor.rs to understand:
- Current module structure
- Existing governor types and methods  
- Test patterns used elsewhere in the codebase

## Governor Types and Key Methods

### Core Data Structures

1. **UsageSnapshot** (line 64-97)
   - Stores utilization percentages for all windows
   - Key methods:
     - `new()` - creates empty snapshot
     - `from_windows(five_hour, seven_day, seven_day_sonnet)` - creates from values
     - `get(window)` - gets utilization for specific window

2. **EmergencyBrake** (line 99-107)
   - Represents emergency brake event when utilization >= 98%
   - Fields: `triggered_window`, `utilization_pct`

3. **Agent** (line 109-120)
   - Represents an agent in the fleet
   - Fields: `id`, `workers`, `is_idle`

4. **WindowContext** (line 122-139)
   - Context for sprint eligibility evaluation
   - Fields: `name`, `hours_remaining`, `headroom_pct`, `cutoff_risk`, `safe_worker_count`, `has_backlog`, `cone_ratio`

5. **SprintState** (line 141-156)
   - Tracks active underutilization recovery sprint
   - Fields: `worker_id`, `target_workers`, `window`, `original_workers`, `sprint_expires_at`, `normal_max_workers`

6. **GovernorState** (line 158-562)
   - Main governor state with comprehensive methods
   - Key methods:
     - `new()` - creates new state
     - `add_agent(id, workers, is_idle)` - adds/updates agent
     - `scale_all_to_zero()` - emergency shutdown
     - `check_emergency_brake(usage)` - checks if 98% threshold breached
     - `clear_emergency_brake(usage)` - clears brake when utilization drops
     - `update_emergency_brake(usage)` - combines check and clear
     - `apply_sprint(trigger)` - applies sprint trigger
     - `clear_sprint()` - ends sprint, restores original workers
     - `check_sprint_end(usage, sprint_config)` - checks if sprint should end
     - `is_sprint_active()` - returns true if sprint active
     - `sprint_eligible(window_ctx, other_windows, config)` - checks eligibility
     - `check_eow_sprint_end(window_ctx, config, now)` - checks end-of-window sprint end
     - `compute_sprint_max_workers(normal_max, other_windows, config)` - computes max during sprint

7. **ScalingDecision** (line 574-585)
   - Enum for scaling outcomes: `NoChange`, `ScaleUp(u32)`, `ScaleDown(u32)`, `EmergencyBrake`

### Key Functions

**Delta Calculation** (line 610-672):
- `calculate_window_pct_delta(previous_snapshot, current_snapshot)` - calculates percentage changes
- `apportion_delta(total_delta, total_usd, session_total_usd)` - apportions delta to session

**Scaling Logic** (line 1298-1616):
- `distribute_workers_by_cost_priority(...)` - distributes workers by cost priority
- `compute_target_workers(state, ...)` - computes target worker count from forecast
- `apply_scaling(target, current, hysteresis_band, ...)` - applies scaling with hysteresis

**Pre-scaling** (line 1622-1691:
- `compute_pre_scale_target(now, pre_scale_minutes, ...)` - handles upcoming multiplier transitions

**Safe Mode** (line 1697-1765):
- `update_safe_mode_from_calibration(safe_mode, calibration, stats, now)` - updates safe mode from calibration

**Main Loop** (line 1810+):
- `run_governor_cycle(...)` - main daemon loop: poll -> schedule -> burn_rate -> target -> scale -> alert -> write_state

## Test Pattern Examples from Codebase

### Pattern 1: Helper-based Test Setup (from `src/db.rs` lines 871-882)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = open_db(&db_path).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }
}
```
**Characteristics:**
- Uses `#[cfg(test)]` module attribute
- Uses `tempfile::TempDir` for temp file/directory creation
- Setup function returns test dependencies
- `.unwrap()` for error handling in tests

### Pattern 2: Direct Creation and Assertion (from `src/config.rs` lines 593-623)
```rust
#[test]
fn test_parse_pricing_config() {
    let yaml = r#"
pricing:
  models:
    claude-sonnet-4-20250514:
      input_per_mtok: 3.0
      output_per_mtok: 15.0
"#;

    let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.pricing.models.len(), 1);

    let pricing = config.pricing.models.get("claude-sonnet-4-20250514").unwrap();
    assert_eq!(pricing.input_per_mtok, 3.0);
}
```
**Characteristics:**
- Uses raw string literals for test data
- Asserts on specific field values
- Chain of `.unwrap()` calls for test-only code

### Pattern 3: File-based Testing (from `src/worker.rs` lines 577-613)
```rust
#[test]
fn count_heartbeat_files_counts_json() {
    let temp = TempDir::new().unwrap();
    let config = test_config(&temp);

    fs::create_dir_all(&config.heartbeat_dir).unwrap();

    // Create test files
    let fresh_timestamp = (Utc::now() - ChronoDuration::seconds(30))
        .format("%Y-%m-%dT%H:%M:%SZ").to_string();
    fs::write(
        config.heartbeat_dir.join("test-worker-1.json"),
        format!(r#"{{"session":"test-worker-1","timestamp":"{}"}}"#, fresh_timestamp),
    ).unwrap();

    let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
    assert_eq!(count, 2);
}
```
**Characteristics:**
- Creates temp directory structure
- Writes test data to files
- Tests file reading/parsing logic

## Existing Test Infrastructure in governor.rs

### Test Module 1: window_delta_tests (lines 674-1213)

Located directly within governor module, this test module provides:

**Test Helper Functions:**
- `make_window_pct_snapshot(five_hour, seven_day, seven_day_sonnet)` (line 1098)
- `make_usage_snapshot(five_hour_pct, seven_day_pct, seven_day_sonnet_pct)` (line 1133)
- `make_usage_snapshot_with_time(taken_at, ...)` (line 1170)

**Test Coverage:**
- `test_calculate_window_pct_delta_basic` (line 679) - basic delta computation
- `test_calculate_window_pct_delta_negative_deltas` (line 697) - window reset scenarios
- `test_calculate_window_pct_delta_zero_previous` (line 715) - first poll case
- `test_apportion_delta_basic` (line 733) - USD-weighted delta apportionment
- `test_apportion_delta_zero_total_usd` (line 740) - edge case handling
- `test_consecutive_snapshots_non_zero_deltas` (line 784) - consecutive API poll simulation
- `test_identical_snapshots_zero_deltas` (line 832) - no-change scenario
- `test_first_poll_no_previous_snapshot` (line 870) - first poll handling
- `test_negative_deltas_window_reset` (line 960) - window reset negative deltas
- `test_mixed_deltas_increase_and_decrease` (line 1008) - mixed delta scenarios
- `test_delta_precision_small_changes` (line 1050) - precision testing
- `test_snapshot_helpers_create_valid_structs` (line 1189) - helper validation

### Test Module 2: tests (lines 3316+)

Located at end of file, provides:

**Test Helper Functions:**
- `make_usage_snapshot(five_hour, seven_day, seven_day_sonnet)` (line 3346)
- `make_usage_snapshot_from_map(windows)` (line 3378)
- `governor_with_agents()` (line 3402)

**Test Naming Pattern:**
- Tests use descriptive `test_<function>_<scenario>` naming
- Each test has a doc comment explaining what it tests
- Tests are focused on single behaviors

**Assertion Pattern:**
- Uses `assert_eq!` for exact value matching
- Uses `assert!` for boolean conditions
- Uses floating-point epsilon comparisons for f64 values

## Key Observations

1. **Comprehensive test infrastructure already exists** in governor.rs with multiple test modules
2. **Helper functions are preferred** for creating test data (make_usage_snapshot, make_window_pct_snapshot, etc.)
3. **tempfile crate** is used extensively for temp file/directory management
4. **Floating-point comparisons** use epsilon-based comparisons for f64 values
5. **Test modules use #[cfg(test)]** attribute to exclude from production builds
6. **Tests are co-located with source code** (not in separate tests/ directory)
7. **Governor module is large** (5000+ lines) with extensive test coverage already in place
