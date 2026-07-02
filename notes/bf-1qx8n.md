# Governor Module Structure and Test Patterns

## Overview

The `governor.rs` module is the core capacity management and scaling logic for claude-governor. It contains ~5000 lines of code and handles emergency brake detection, sprint management, scaling decisions, and the main daemon loop.

## Governor Module Structure

### Key Public Types

#### Core State Types

1. **`UsageSnapshot`** (line 65)
   - Holds utilization percentages for all three usage windows
   - Methods: `new()`, `from_windows()`, `get()`

2. **`EmergencyBrake`** (line 101)
   - Records which window triggered the brake and at what utilization
   - Fields: `triggered_window`, `utilization_pct`

3. **`Agent`** (line 111)
   - Represents a tracked agent for scaling purposes
   - Fields: `id`, `workers`, `is_idle`

4. **`WindowContext`** (line 124)
   - Context for sprint eligibility evaluation
   - Fields: window name, hours_remaining, headroom_pct, cutoff_risk, safe_worker_count, has_backlog, cone_ratio

5. **`SprintState`** (line 143)
   - Tracks active underutilization recovery sprint
   - Fields: worker_id, target_workers, window, original_workers, sprint_expires_at, normal_max_workers

6. **`GovernorState`** (line 160)
   - Main state container for all governor tracking
   - Fields: emergency_brake_active, agents (HashMap), emergency_brake (Option), sprint (Option)

#### Decision Types

7. **`ScalingDecision`** (line 576)
   - Enum representing scaling actions
   - Variants: `NoChange`, `ScaleUp(u32)`, `ScaleDown(u32)`, `EmergencyBrake`

### Key Methods in GovernorState

- **`add_agent()`** - Add or update an agent in the tracked set
- **`scale_all_to_zero()`** - Emergency shutdown: set all agents to 0 workers
- **`check_emergency_brake()`** - Check if any window >= 98%, trigger brake if so
- **`clear_emergency_brake()`** - Clear brake if all windows below 98%
- **`update_emergency_brake()`** - Combined check+clear in one call
- **`apply_sprint()`** - Apply sprint trigger, boost agent workers
- **`clear_sprint()`** - Clear sprint, restore original worker count
- **`check_sprint_end()`** - Check if sprint should end
- **`is_sprint_active()`** - Check if sprint is currently active
- **`sprint_eligible()`** - Check if window is eligible for end-of-window capacity sprint
- **`check_eow_sprint_end()`** - Check if end-of-window sprint should end
- **`compute_sprint_max_workers()`** - Compute effective max workers during sprint

### Key Public Functions

- **`calculate_window_pct_delta()`** (line 634) - Calculate percentage deltas between consecutive API polls
- **`apportion_delta()`** (line 666) - Apportion total delta to specific session based on USD weight
- **`compute_target_workers()`** (line 1435) - Compute target worker count from capacity forecast
- **`apply_scaling()`** (line 1569) - Apply scaling decision with hysteresis band
- **`compute_pre_scale_target()`** (line 1638) - Compute effective target for upcoming multiplier transition
- **`update_safe_mode_from_calibration()`** (line 1707) - Update safe mode state from calibration stats
- **`run_governor_cycle()`** (line 1813) - Main daemon loop: poll -> schedule -> burn_rate -> target -> scale -> alert -> write_state
- **`run_daemon()`** (line 3214) - Run the governor daemon

### Module Constants

- `EMERGENCY_BRAKE_THRESHOLD: f64 = 98.0` (line 38)
- `SAFE_MODE_ENTRY_ERROR_THRESHOLD: f64 = 15.0` (line 41)
- `SAFE_MODE_EXIT_ERROR_THRESHOLD: f64 = 8.0` (line 44)
- `SAFE_MODE_MIN_SAMPLES: u32 = 5` (line 47)
- `SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT: u32 = 3` (line 50)
- `SAFE_MODE_CEILING_REDUCTION: f64 = 5.0` (line 53)
- `SAFE_MODE_HYSTERESIS_MULTIPLIER: f64 = 2.0` (line 56)
- `WINDOW_FIVE_HOUR: &str = "five_hour"` (line 59)
- `WINDOW_SEVEN_DAY: &str = "seven_day"` (line 60)
- `WINDOW_SEVEN_DAY_SONNET: &str = "seven_day_sonnet"` (line 61)

## Test Infrastructure in governor.rs

### Test Modules

The governor module has **two** test modules:

1. **`mod window_delta_tests`** (line 675) - Tests for window delta calculation helpers
2. **`mod tests`** (line 3317) - Main tests for governor state and behavior

### Test Pattern Examples from the Codebase

#### Pattern 1: Helper Functions with Documentation (state.rs)

Uses builder functions to create complex test states:

```rust
/// Build a fully populated GovernorState for round-trip testing
fn full_state() -> GovernorState {
    // ... create complex test state ...
}

#[test]
fn round_trip_full_state() {
    let state = full_state();
    let json = serde_json::to_string(&state).unwrap();
    let loaded: GovernorState = serde_json::from_str(&json).unwrap();
    // ... assertions ...
}
```

#### Pattern 2: Builder Helpers (burn_rate.rs)

Simple focused helpers for test data:

```rust
/// Helper: build a WindowUtilization for a named window
fn win(name: &str, pct_delta: Option<f64>, current: f64, previous: f64) -> WindowUtilization {
    WindowUtilization {
        window: name.to_string(),
        pct_delta,
        current_utilization: current,
        previous_utilization: previous,
    }
}
```

#### Pattern 3: TempDir for File Operations (worker.rs)

Uses tempfile for isolated file testing:

```rust
fn test_config(dir: &TempDir) -> WorkerConfig {
    WorkerConfig {
        launch_cmd: "echo 'would launch {id}'".to_string(),
        heartbeat_dir: dir.path().join("heartbeats"),
        graceful_timeout_secs: 2,
        session_prefix: "test-worker".to_string(),
    }
}

#[test]
fn count_heartbeat_files_counts_json() {
    let temp = TempDir::new().unwrap();
    let config = test_config(&temp);
    fs::create_dir_all(&config.heartbeat_dir).unwrap();
    // ... create test files ...
    let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
    assert_eq!(count, 2);
}
```

#### Pattern 4: Exhaustive Case Testing (burn_rate.rs)

Tests multiple scenarios around boundary conditions:

```rust
#[test]
fn guard_skip_short_interval() {
    let record = basic_record(Some(2.0));
    let rates = compute_instance_burn(&record, 1.0 / 60.0);
    assert!(rates.is_empty());
}

#[test]
fn guard_exact_two_minutes_passes() {
    let record = basic_record(Some(2.0));
    let rates = compute_instance_burn(&record, 2.0 / 60.0);
    assert_eq!(rates.len(), 1);
}

#[test]
fn guard_under_two_minutes_rejects() {
    let record = basic_record(Some(2.0));
    let rates = compute_instance_burn(&record, 1.999 / 60.0);
    assert!(rates.is_empty());
}
```

#### Pattern 5: Documented Test Helpers (governor.rs window_delta_tests)

Well-documented public test helpers:

```rust
/// Create a WindowPctSnapshot with specified utilization percentages.
///
/// Helper function to create WindowPctSnapshot instances with custom values
/// for testing delta calculations and other window percentage operations.
///
/// # Arguments
/// - `five_hour`: 5-hour window utilization percentage
/// - `seven_day`: 7-day window utilization percentage (all models)
/// - `seven_day_sonnet`: 7-day window utilization percentage (Sonnet only)
///
/// # Returns
/// A WindowPctSnapshot struct with the specified values.
///
/// # Example
/// ```rust
/// use crate::governor::window_delta_tests::make_window_pct_snapshot;
///
/// let snapshot = make_window_pct_snapshot(25.5, 45.0, 38.2);
/// assert_eq!(snapshot.five_hour, 25.5);
/// ```
pub fn make_window_pct_snapshot(
    five_hour: f64,
    seven_day: f64,
    seven_day_sonnet: f64,
) -> crate::db::WindowPctSnapshot {
    crate::db::WindowPctSnapshot {
        five_hour,
        seven_day,
        seven_day_sonnet,
    }
}
```

#### Pattern 6: Descriptive Test Names with Scenarios (governor.rs tests)

Clear test names that describe the scenario:

```rust
#[test]
fn test_97_9_pct_no_brake() {
    let mut state = governor_with_agents();
    let usage = make_usage_snapshot(97.9, 50.0, 50.0);
    let result = state.check_emergency_brake(&usage);
    assert!(result.is_none());
    assert!(!state.emergency_brake_active);
}

#[test]
fn test_98_0_pct_brake_triggers() {
    let mut state = governor_with_agents();
    let usage = make_usage_snapshot(98.0, 50.0, 50.0);
    let result = state.check_emergency_brake(&usage);
    assert!(result.is_some());
    let brake = result.unwrap();
    assert_eq!(brake.triggered_window, WINDOW_FIVE_HOUR);
}

#[test]
fn test_brake_scales_all_agents_to_zero() {
    let mut state = governor_with_agents();
    let usage = make_usage_snapshot(50.0, 98.5, 50.0);
    let _ = state.check_emergency_brake(&usage);
    for agent in state.agents.values() {
        assert_eq!(agent.workers, 0, "Agent {} should have 0 workers", agent.id);
    }
}
```

### Files with Existing Test Infrastructure

The codebase has extensive test coverage across these modules:
- `governor.rs` - 2 test modules (window_delta_tests, tests)
- `state.rs` - Round-trip serialization tests
- `burn_rate.rs` - Burn rate computation with guard tests
- `schedule.rs` - Peak/off-peak detection
- `worker.rs` - Worker scaling with TempDir fixtures
- `alerts.rs` - Alert condition checking
- `config.rs` - Configuration parsing
- `db.rs` - Database operations
- `poller.rs` - API polling
- `calibrator.rs` - Prediction calibration
- `collector.rs` - Token usage collection
- `pricing.rs` - Pricing calculations
- `capacity_summary.rs` - Capacity forecasting
- `simulator.rs` - Usage simulation
- `doctor.rs` - Health checks
- `narrator.rs` - Status narration
- `status_display.rs` - Display formatting
- `main.rs` - CLI integration tests

## Key Learnings for Test Development

1. **Use builder/helper functions** - Create complex test data with well-documented helpers
2. **Document test helpers** - Include doc comments explaining purpose, arguments, returns, and examples
3. **Use descriptive test names** - Names like `test_97_9_pct_no_brake` clearly indicate the scenario
4. **Test edge cases** - Boundary values (exact thresholds, just below/above)
5. **Use TempDir for file tests** - Isolate filesystem operations
6. **Test public APIs** - Focus on public functions and types
7. **Follow Rust conventions** - Use `#[cfg(test)]` and `mod tests` pattern
8. **Include assertions with context** - Add custom messages to assert macros for debugging
