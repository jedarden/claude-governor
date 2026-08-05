//! State-file behavior backing the governor cycle.
//!
//! The cycle's own behavior — poll called, polled data applied to state,
//! emergency brake at 98%, state written, poll failures absorbed — is verified
//! in `governor::mock_poller_tests` (`src/governor.rs`), which drives the real
//! `run_governor_cycle` against `MockPoller`. Those tests cannot live here:
//! `MockPoller` is `#[cfg(test)]`, so it is not reachable from an integration
//! test binary.
//!
//! What remains here is the state persistence layer the cycle depends on:
//! loading, writing, the fresh-state fallback, and directory creation. These
//! call `state::load_state` / `state::save_state` directly, so each assertion
//! is about production behavior rather than a re-implementation of the cycle.

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use claude_governor::governor::{
    UsageSnapshot, WINDOW_FIVE_HOUR, WINDOW_SEVEN_DAY, WINDOW_WEEKLY_SCOPED,
};
use claude_governor::state::{self, GovernorState, WorkerState};

/// Create a test state file with worker configuration
fn create_test_state_file(
    temp_dir: &TempDir,
    current: u32,
    target: u32,
    min: u32,
    max: u32,
) -> PathBuf {
    let state_path = temp_dir.path().join("governor-state.json");

    let mut state = GovernorState::new();
    state.workers.insert(
        "test-agent".to_string(),
        WorkerState {
            current,
            target,
            min,
            max,
        },
    );

    let json = serde_json::to_string_pretty(&state).expect("Failed to serialize state");
    fs::write(&state_path, json).expect("Failed to write state file");

    state_path
}

/// Verify state is loaded correctly from disk
#[test]
fn test_state_loaded_correctly() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let state_path = create_test_state_file(&temp_dir, 5, 5, 1, 10);

    // Load the state
    let loaded_state = state::load_state(&state_path).expect("Failed to load state");

    // Verify state is loaded with correct values
    assert_eq!(loaded_state.workers.len(), 1, "State should have 1 worker");
    assert_eq!(
        loaded_state.workers["test-agent"].current, 5,
        "Current workers should be 5"
    );
    assert_eq!(
        loaded_state.workers["test-agent"].target, 5,
        "Target workers should be 5"
    );
    assert_eq!(
        loaded_state.workers["test-agent"].min, 1,
        "Min workers should be 1"
    );
    assert_eq!(
        loaded_state.workers["test-agent"].max, 10,
        "Max workers should be 10"
    );
}

/// Verify state is written correctly to disk
#[test]
fn test_state_written_correctly() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let state_path = temp_dir.path().join("governor-state.json");

    // Create and save a state
    let mut state = GovernorState::new();
    state.workers.insert(
        "test-agent".to_string(),
        WorkerState {
            current: 8,
            target: 8,
            min: 2,
            max: 15,
        },
    );
    state.usage.five_hour_pct = 75.0;
    state.usage.weekly_scoped_pct = 68.0;
    state.usage.all_models_pct = 72.0;

    state::save_state(&state, &state_path).expect("Failed to save state");

    // Verify file was created
    assert!(state_path.exists(), "State file should exist after save");

    // Load and verify the written state
    let loaded_state = state::load_state(&state_path).expect("Failed to load state");

    assert_eq!(
        loaded_state.workers["test-agent"].current, 8,
        "Current workers should match saved value"
    );
    assert_eq!(
        loaded_state.workers["test-agent"].target, 8,
        "Target workers should match saved value"
    );
    assert_eq!(
        loaded_state.workers["test-agent"].min, 2,
        "Min workers should match saved value"
    );
    assert_eq!(
        loaded_state.workers["test-agent"].max, 15,
        "Max workers should match saved value"
    );
    assert_eq!(
        loaded_state.usage.five_hour_pct, 75.0,
        "5-hour utilization should match saved value"
    );
    assert_eq!(
        loaded_state.usage.weekly_scoped_pct, 68.0,
        "Weekly-scoped utilization should match saved value"
    );
    assert_eq!(
        loaded_state.usage.all_models_pct, 72.0,
        "All models utilization should match saved value"
    );
}

/// Verify polled window percentages map onto the right snapshot keys.
///
/// The cycle builds its forecast from a `UsageSnapshot`; mixing up the window
/// keys here would silently forecast against the wrong limit, so the three
/// values are kept distinct.
#[test]
fn test_window_percentages_map_to_snapshot_keys() {
    let snapshot = UsageSnapshot::from_windows(65.0, 70.0, 68.0);

    assert_eq!(
        snapshot.get(WINDOW_FIVE_HOUR),
        Some(65.0),
        "5-hour utilization should be 65%"
    );
    assert_eq!(
        snapshot.get(WINDOW_SEVEN_DAY),
        Some(70.0),
        "7-day utilization should be 70%"
    );
    assert_eq!(
        snapshot.get(WINDOW_WEEKLY_SCOPED),
        Some(68.0),
        "weekly-scoped utilization should be 68%"
    );
}

/// Verify state load creates new state when file doesn't exist
#[test]
fn test_state_load_creates_new_state_when_missing() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Attempt to load from a non-existent file
    let nonexistent_path = temp_dir.path().join("nonexistent-state.json");

    let load_result = state::load_state(&nonexistent_path);

    // Verify load succeeds with a new default state (not an error)
    assert!(
        load_result.is_ok(),
        "Load from non-existent path should return Ok with new state"
    );

    let loaded_state = load_result.unwrap();
    assert_eq!(
        loaded_state.workers.len(),
        0,
        "New state should have no workers"
    );
    assert_eq!(
        loaded_state.usage.five_hour_pct, 0.0,
        "New state should have zero utilization"
    );
}

/// Verify state write succeeds even when parent directory doesn't exist
#[test]
fn test_state_write_creates_parent_directories() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create state
    let mut state = GovernorState::new();
    state.workers.insert(
        "test-agent".to_string(),
        WorkerState {
            current: 3,
            target: 3,
            min: 1,
            max: 10,
        },
    );

    // Attempt to save to a path with nonexistent parent directories
    let nested_path = temp_dir
        .path()
        .join("level1")
        .join("level2")
        .join("state.json");

    let save_result = state::save_state(&state, &nested_path);

    // Verify save succeeds (creates parent directories automatically)
    assert!(
        save_result.is_ok(),
        "Save should succeed by creating parent directories"
    );

    // Verify the file was actually created
    assert!(
        nested_path.exists(),
        "State file should exist at nested path"
    );

    // Load and verify the state
    let loaded_state = state::load_state(&nested_path).expect("Failed to load state");
    assert_eq!(loaded_state.workers.len(), 1, "State should have 1 worker");
    assert_eq!(
        loaded_state.workers["test-agent"].current, 3,
        "Worker data should persist"
    );
}
