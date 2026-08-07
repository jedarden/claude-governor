//! End-to-end test of the alert episode lifecycle against the real `bf` CLI.
//!
//! The unit tests in `src/alerts.rs` drive the lifecycle with stub commands (`echo`, `true`),
//! which proves the state machine but not that the governor can actually parse a bead id back
//! out of `bf create` or that `bf close` accepts the arguments we build. This test runs the
//! whole loop — create, refresh, auto-close — against a throwaway `bf` workspace and then
//! reads the bead back to confirm it really was closed.
//!
//! Skipped (passing) when `bf` is not on PATH, so the suite still runs in environments that
//! don't have the bead CLI installed.

use chrono::{Duration, Utc};
use claude_governor::alerts::{process_alert_episodes, AlertCondition, AlertSeverity, AlertType};
use claude_governor::config::AlertConfig;
use claude_governor::state::GovernorState;
use std::path::Path;
use std::process::Command;

fn bf_available() -> bool {
    Command::new("bf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// An alert config wired to a throwaway bead workspace.
fn config_for_workspace(dir: &Path) -> AlertConfig {
    let ws = dir.to_string_lossy().to_string();
    AlertConfig {
        enabled: true,
        auto_bead: true,
        cooldown_minutes: 60,
        min_severity: "warning".to_string(),
        // `--title` last: the alert message is appended as the final argument.
        command: vec![
            "bf".into(),
            "create".into(),
            "--json".into(),
            "--type".into(),
            "human".into(),
            "-w".into(),
            ws.clone(),
            "--title".into(),
        ],
        close_command: vec!["bf".into(), "close".into(), "-w".into(), ws.clone()],
        update_command: vec!["bf".into(), "update".into(), "-w".into(), ws],
        ..AlertConfig::default()
    }
}

fn cutoff_condition(now: chrono::DateTime<Utc>, message: &str) -> AlertCondition {
    AlertCondition::new(
        AlertType::SonnetCutoffRisk,
        message.to_string(),
        AlertSeverity::Warning,
        now,
    )
    .with_scope("weekly_scoped")
}

/// Read a bead back from the workspace as JSON.
fn show_bead(dir: &Path, id: &str) -> serde_json::Value {
    let output = Command::new("bf")
        .arg("show")
        .arg(id)
        .arg("-w")
        .arg(dir)
        .arg("--json")
        .output()
        .expect("bf show should run");
    assert!(
        output.status.success(),
        "bf show failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("bf show should emit JSON");
    // `bf show --json` returns an array of matches.
    match parsed {
        serde_json::Value::Array(mut items) => {
            assert_eq!(items.len(), 1, "expected exactly one bead for {}", id);
            items.remove(0)
        }
        other => other,
    }
}

#[test]
fn alert_episode_creates_one_bead_and_auto_closes_it() {
    if !bf_available() {
        eprintln!("skipping: `bf` not on PATH");
        return;
    }

    let temp = tempfile::TempDir::new().unwrap();
    let dir = temp.path();
    let init = Command::new("bf")
        .arg("init")
        .current_dir(dir)
        .output()
        .expect("bf init should run");
    assert!(
        init.status.success(),
        "bf init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let config = config_for_workspace(dir);
    let mut state = GovernorState::default();
    let t0 = Utc::now();

    // --- Cycle 1: condition becomes true. One bead is created and tracked. ---
    let outcome = process_alert_episodes(
        &mut state,
        &config,
        &[cutoff_condition(
            t0,
            "Seven-day Sonnet window at cutoff risk: 96.1% utilized",
        )],
        t0,
        None,
    );
    assert_eq!(
        outcome.opened,
        vec!["sonnet_cutoff_risk:weekly_scoped".to_string()]
    );

    let episode = state
        .open_alert_beads
        .get("sonnet_cutoff_risk:weekly_scoped")
        .expect("episode should be tracked");
    let bead_id = episode
        .bead_id
        .clone()
        .expect("bead id should have been parsed out of `bf create --json`");

    let bead = show_bead(dir, &bead_id);
    assert_eq!(bead["status"], "open");
    assert_eq!(bead["issue_type"], "human");
    assert!(
        bead["title"]
            .as_str()
            .unwrap()
            .contains("sonnet_cutoff_risk"),
        "title should carry the alert: {}",
        bead["title"]
    );

    // --- Cycles 2..25: condition stays true. No further beads, notes refreshed in place. ---
    for hour in 1..25 {
        let now = t0 + Duration::hours(hour);
        let outcome = process_alert_episodes(
            &mut state,
            &config,
            &[cutoff_condition(
                now,
                &format!(
                    "Seven-day Sonnet window at cutoff risk: {}% utilized",
                    96 + hour % 3
                ),
            )],
            now,
            None,
        );
        assert!(
            outcome.opened.is_empty(),
            "hour {}: a still-true condition must not create another bead",
            hour
        );
        assert_eq!(outcome.suppressed, 1);
    }

    // Exactly one human bead exists in the workspace after a full day of the condition holding.
    let list = Command::new("bf")
        .args(["list", "--json", "--type", "human", "-w"])
        .arg(dir)
        .output()
        .expect("bf list should run");
    // `bf list --json` emits one JSON object per line.
    let stdout = String::from_utf8_lossy(&list.stdout);
    let count = stdout.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        count, 1,
        "24 hours of a persistent condition must yield exactly one bead, got:\n{}",
        stdout
    );

    // The single bead was refreshed rather than duplicated.
    let bead = show_bead(dir, &bead_id);
    assert_eq!(bead["status"], "open");
    assert!(
        bead["notes"].as_str().unwrap().contains("Still active"),
        "notes should have been refreshed: {}",
        bead["notes"]
    );

    // --- Cycle 26: condition clears. The bead is auto-closed. ---
    let end = t0 + Duration::hours(25);
    let outcome = process_alert_episodes(&mut state, &config, &[], end, None);
    assert_eq!(outcome.resolved.len(), 1);
    assert!(outcome.resolved[0].closed, "close command should have run");
    assert!(
        state.open_alert_beads.is_empty(),
        "resolved episode should be dropped from state"
    );

    let bead = show_bead(dir, &bead_id);
    assert_eq!(bead["status"], "closed", "bead should be auto-closed");
    assert!(
        bead["close_reason"]
            .as_str()
            .unwrap()
            .contains("Condition cleared"),
        "close reason should explain the auto-close: {}",
        bead["close_reason"]
    );
}
