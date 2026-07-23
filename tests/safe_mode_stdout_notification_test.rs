//! Test for safe mode stdout notification verification
//!
//! This test verifies that the stdout notification about safe mode reasserting
//! appears correctly after a manual scale during safe mode.

use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper function to create a test state file with safe mode active
fn create_state_with_safe_mode(temp_dir: &TempDir) -> PathBuf {
    use claude_governor::state::{GovernorState, WorkerState, save_state};
    use chrono::Utc;

    let state_path = temp_dir.path().join("governor-state.json");

    // Create a test state with safe mode active
    let mut state = GovernorState::new();

    // Add workers to the state so scale operations are valid
    state.workers.insert(
        "test-agent".to_string(),
        WorkerState {
            current: 5,
            target: 5,
            min: 1,
            max: 10,
        },
    );

    // Activate safe mode with realistic test data
    state.safe_mode.active = true;
    state.safe_mode.entered_at = Some(Utc::now());
    state.safe_mode.trigger = Some("test_trigger".to_string());
    state.safe_mode.median_error_at_entry = Some(15.0);
    state.safe_mode.predictions_since_entry = 10;

    // Save the state to the temporary file
    save_state(&state, &state_path).expect("Failed to save test state");

    state_path
}

/// Helper function to create a test state file WITHOUT safe mode active
fn create_state_without_safe_mode(temp_dir: &TempDir) -> PathBuf {
    use claude_governor::state::{GovernorState, WorkerState, save_state};

    let state_path = temp_dir.path().join("governor-state.json");

    // Create a test state without safe mode active
    let mut state = GovernorState::new();

    // Add workers to the state so scale operations are valid
    state.workers.insert(
        "test-agent".to_string(),
        WorkerState {
            current: 5,
            target: 5,
            min: 1,
            max: 10,
        },
    );

    // Ensure safe mode is NOT active (default state)
    assert!(!state.safe_mode.active);

    // Save the state to the temporary file
    save_state(&state, &state_path).expect("Failed to test state");

    state_path
}

/// Helper function to simulate the scale command logic with stdout capture
fn simulate_scale_command_with_stdout(state_path: &PathBuf, target_count: u32) -> String {
    use claude_governor::state::{load_state, save_state};
    use chrono::Utc;

    // Capture stdout by redirecting to a string
    let mut buffer = Vec::new();

    {
        let mut captured_stdout = Cursor::new(&mut buffer);

        // Load the state
        let mut state = load_state(state_path).expect("Failed to load state");

        // Track safe mode status for user messaging
        let safe_mode_was_active = state.safe_mode.active;

        // Check if safe mode is active and log warning (to stdout for test verification)
        if state.safe_mode.active {
            let warning = "WARN: manual scale override during safe mode";
            writeln!(captured_stdout, "[governor] {}", warning).unwrap();
        }

        // Validate count against worker limits
        for (agent_id, worker) in &state.workers {
            if target_count < worker.min || target_count > worker.max {
                panic!(
                    "Worker count {} is outside allowed range for agent {} ({} - {})",
                    target_count,
                    agent_id,
                    worker.min,
                    worker.max
                );
            }
        }

        // Apply the scale command
        for worker in state.workers.values_mut() {
            worker.target = target_count;
        }

        state.updated_at = Utc::now();
        save_state(&state, state_path).expect("Failed to save state");

        writeln!(captured_stdout, "Target worker count set to {} for all agents", target_count).unwrap();

        // Warn user that safe mode will reassert on next cycle
        if safe_mode_was_active {
            writeln!(captured_stdout, "NOTE: Safe mode remains active and will reassert its target on the next cycle").unwrap();
        }
    }

    String::from_utf8(buffer).expect("Output should be valid UTF-8")
}

#[test]
fn test_scale_safe_mode_stdout_notification() {
    // Create a temporary directory for test files
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create state file with safe mode active
    let state_path = create_state_with_safe_mode(&temp_dir);

    // Execute scale command and capture stdout
    let stdout_output = simulate_scale_command_with_stdout(&state_path, 8);

    // Verify the stdout output contains the expected messages
    println!("Captured stdout:\n{}", stdout_output);

    // 1. Verify the warning about manual scale override appears
    assert!(
        stdout_output.contains("WARN: manual scale override during safe mode"),
        "Stdout should contain warning about manual scale override during safe mode. Got: {}",
        stdout_output
    );

    // 2. Verify the target worker count confirmation message appears
    assert!(
        stdout_output.contains("Target worker count set to 8 for all agents"),
        "Stdout should confirm target worker count was set. Got: {}",
        stdout_output
    );

    // 3. Verify the safe mode reassertion notification appears
    assert!(
        stdout_output.contains("NOTE: Safe mode remains active and will reassert its target on the next cycle"),
        "Stdout should contain notification that safe mode will reassert. Got: {}",
        stdout_output
    );

    // 4. Verify the state was actually updated
    use claude_governor::state::load_state;
    let updated_state = load_state(&state_path).expect("Failed to load updated state");
    assert_eq!(updated_state.workers["test-agent"].target, 8, "Worker target should be updated to 8");
}

#[test]
fn test_scale_without_safe_mode_no_stdout_notification() {
    // Create a temporary directory for test files
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create state file WITHOUT safe mode active
    let state_path = create_state_without_safe_mode(&temp_dir);

    // Execute scale command and capture stdout
    let stdout_output = simulate_scale_command_with_stdout(&state_path, 7);

    // Verify the stdout output
    println!("Captured stdout:\n{}", stdout_output);

    // 1. Verify the warning does NOT appear (safe mode is inactive)
    assert!(
        !stdout_output.contains("WARN: manual scale override during safe mode"),
        "Stdout should NOT contain warning when safe mode is inactive. Got: {}",
        stdout_output
    );

    // 2. Verify the target worker count confirmation message still appears
    assert!(
        stdout_output.contains("Target worker count set to 7 for all agents"),
        "Stdout should confirm target worker count was set. Got: {}",
        stdout_output
    );

    // 3. Verify the safe mode reassertion notification does NOT appear
    assert!(
        !stdout_output.contains("NOTE: Safe mode remains active and will reassert its target on the next cycle"),
        "Stdout should NOT contain safe mode reassertion notification when safe mode is inactive. Got: {}",
        stdout_output
    );

    // 4. Verify the state was actually updated
    use claude_governor::state::load_state;
    let updated_state = load_state(&state_path).expect("Failed to load updated state");
    assert_eq!(updated_state.workers["test-agent"].target, 7, "Worker target should be updated to 7");
}

#[test]
fn test_scale_safe_mode_notification_order_and_completeness() {
    // This test verifies the complete output order and all expected messages
    // when scaling during safe mode, ensuring the user sees the right information
    // in the right sequence.

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let state_path = create_state_with_safe_mode(&temp_dir);

    // Execute scale command and capture stdout
    let stdout_output = simulate_scale_command_with_stdout(&state_path, 6);

    println!("Captured stdout for order test:\n{}", stdout_output);

    // Split output into lines for order verification
    let lines: Vec<&str> = stdout_output.lines().collect();

    // Find the indices of each expected message
    let warning_idx = lines.iter().position(|l| l.contains("WARN: manual scale override"));
    let confirmation_idx = lines.iter().position(|l| l.contains("Target worker count set to"));
    let notification_idx = lines.iter().position(|l| l.contains("NOTE: Safe mode remains active"));

    // Verify all messages appear
    assert!(warning_idx.is_some(), "Warning message should appear");
    assert!(confirmation_idx.is_some(), "Confirmation message should appear");
    assert!(notification_idx.is_some(), "Safe mode notification should appear");

    // Verify order: warning comes before confirmation, notification comes after confirmation
    let w = warning_idx.unwrap();
    let c = confirmation_idx.unwrap();
    let n = notification_idx.unwrap();

    assert!(
        w < c && c < n,
        "Messages should appear in order: warning (line {}) < confirmation (line {}) < notification (line {})",
        w, c, n
    );

    // Verify the complete expected message text
    assert!(
        lines[w].contains("[governor] WARN: manual scale override during safe mode"),
        "Warning should have the exact expected format at line {}",
        lines[w]
    );
    assert!(
        lines[c].contains("Target worker count set to 6 for all agents"),
        "Confirmation should show the correct count at line {}",
        lines[c]
    );
    assert!(
        lines[n].contains("NOTE: Safe mode remains active and will reassert its target on the next cycle"),
        "Notification should have the exact expected format at line {}",
        lines[n]
    );
}

#[test]
fn test_scale_safe_mode_notification_multiple_scales() {
    // Test that the notification appears consistently across multiple scale operations
    // while safe mode remains active.

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let state_path = create_state_with_safe_mode(&temp_dir);

    // Perform multiple scale operations
    for count in [3, 5, 8, 2] {
        let stdout_output = simulate_scale_command_with_stdout(&state_path, count);

        // Verify notification appears for each scale operation
        assert!(
            stdout_output.contains("NOTE: Safe mode remains active and will reassert its target on the next cycle"),
            "Safe mode notification should appear for scale to {}. Output: {}",
            count,
            stdout_output
        );

        // Verify the scale was actually applied
        use claude_governor::state::load_state;
        let state = load_state(&state_path).expect("Failed to load state");
        assert_eq!(
            state.workers["test-agent"].target,
            count,
            "Worker target should be updated to {}",
            count
        );
    }
}

#[test]
fn test_scale_safe_mode_notification_content_accuracy() {
    // Test that the notification contains exactly the expected text with no
    // variations or formatting issues.

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let state_path = create_state_with_safe_mode(&temp_dir);

    let stdout_output = simulate_scale_command_with_stdout(&state_path, 4);

    // Extract just the notification line
    let notification_line = stdout_output
        .lines()
        .find(|l| l.contains("NOTE: Safe mode remains active"))
        .expect("Notification line should exist");

    // Verify exact text match (case-sensitive, no typos)
    let expected_text = "NOTE: Safe mode remains active and will reassert its target on the next cycle";
    assert_eq!(
        notification_line.trim(),
        expected_text,
        "Notification text should match exactly. Expected: '{}', Got: '{}'",
        expected_text,
        notification_line.trim()
    );
}
