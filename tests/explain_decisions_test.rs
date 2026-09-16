//! Fixture-based tests for `cgov explain` — the scaling-decision viewer.
//!
//! `cgov explain` reads a JSONL audit log of scaling decisions
//! (`~/.needle/state/governor-decisions.jsonl`, written by the act cycle) and
//! renders it as human text or JSON. These tests cover the three things the
//! viewer depends on:
//!
//! 1. **Serialize** — a `DecisionEntry` round-trips through serde with the
//!    snake_case action keys the JSONL format promises.
//! 2. **Persist** — entries append to the log and read back newest-first with
//!    correct `--last N` semantics, via the same helpers the CLI uses.
//! 3. **Render** — `format_decision(s)_human` and the `--json` shape show the
//!    binding window, worker transition, trigger and rationale.
//!
//! Plus end-to-end wiring: `run_act_cycle` against a fixture state file must
//! actually append a decision entry (with computed-vs-actual context) to the
//! redirected log — the property that makes `cgov explain` show real data.

use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use tempfile::TempDir;

use claude_governor::config::{CompositeRiskConfig, ConeScalingConfig};
use claude_governor::governor::{self, ScalingDecision};
use claude_governor::narrator::{
    append_decision_to_path, default_decisions_path, format_decision_human, format_decisions_human,
    narrate_decision, read_last_decisions_from_path, DecisionContext, DecisionEntry, ScaleAction,
};
use claude_governor::state;

#[path = "fixtures.rs"]
mod fixtures;

/// The decision-log path override is process-global, so tests that touch it
/// (or run the act cycle, which consults it) serialize on this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn set_decisions_path(path: &Path) {
    std::env::set_var("CGOV_DECISIONS_PATH", path);
}

fn clear_decisions_path() {
    std::env::remove_var("CGOV_DECISIONS_PATH");
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A WindowForecast with the given numbers, everything else defaulted.
fn forecast(utilization: f64, margin_hrs: f64, hours_remaining: f64) -> state::WindowForecast {
    state::WindowForecast {
        current_utilization: utilization,
        margin_hrs,
        hours_remaining,
        binding: true,
        ..Default::default()
    }
}

/// A state whose binding window carries the given forecast.
fn state_with_binding(binding_window: &str, window: state::WindowForecast) -> state::GovernorState {
    let mut s = state::GovernorState::new();
    s.capacity_forecast = state::CapacityForecast {
        binding_window: binding_window.to_string(),
        five_hour: if binding_window == "five_hour" {
            window.clone()
        } else {
            state::WindowForecast::default()
        },
        seven_day: if binding_window == "seven_day" {
            window.clone()
        } else {
            state::WindowForecast::default()
        },
        weekly_scoped: if binding_window == "weekly_scoped" {
            window
        } else {
            state::WindowForecast::default()
        },
        ..Default::default()
    };
    s
}

/// Narrate a decision between two fixture states.
fn entry(
    action: ScaleAction,
    before: &state::GovernorState,
    after: &state::GovernorState,
    workers_before: u32,
    workers_after: u32,
    trigger: &str,
) -> DecisionEntry {
    narrate_decision(&DecisionContext {
        before,
        after,
        action,
        trigger: trigger.to_string(),
        agent_id: None,
        workers_before,
        workers_after,
    })
}

/// Write a GovernorState to disk the way the fixtures do, so act-cycle tests
/// can shape the forecast the decision will be computed from.
fn write_state_file(temp_dir: &Path, state_to_write: &state::GovernorState) -> std::path::PathBuf {
    let state_path = temp_dir.join("governor-state.json");
    let json = serde_json::to_string_pretty(state_to_write).unwrap();
    std::fs::write(&state_path, json).expect("Failed to write state fixture");
    state_path
}

/// Run one act cycle in dry-run mode (hermetic: no workers launched or killed)
/// against a fixture state, with the decision log redirected into the temp dir.
fn run_dry_act_cycle(
    temp_dir: &Path,
    state_to_write: &state::GovernorState,
    hysteresis: f64,
) -> (ScalingDecision, std::path::PathBuf) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let decisions_path = temp_dir.join("decisions.jsonl");
    set_decisions_path(&decisions_path);

    let state_path = write_state_file(temp_dir, state_to_write);
    let config = fixtures::test_governor_config_with_agents(&["test-agent"]);
    let alert_config = fixtures::test_alert_config();
    let agents = config.agents.clone();

    let decision = governor::run_act_cycle(
        &state_path,
        true, // dry_run — nothing is launched or killed
        hysteresis,
        10,   // max up per cycle
        10,   // max down per cycle
        90.0, // target ceiling
        &alert_config,
        &agents,
        30,  // pre-scale minutes
        &[], // promotions
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &config,
        Utc::now(),
    )
    .expect("act cycle should succeed");

    clear_decisions_path();
    (decision, decisions_path)
}

// ---------------------------------------------------------------------------
// 1. Serialize
// ---------------------------------------------------------------------------

#[test]
fn decision_entry_serializes_to_snake_case_jsonl_line() {
    let before = state_with_binding("weekly_scoped", forecast(60.0, 5.0, 37.5));
    let after = state_with_binding("weekly_scoped", forecast(65.0, 3.0, 37.5));
    let e = entry(
        ScaleAction::ScaleUp,
        &before,
        &after,
        2,
        3,
        "margin_hrs dropped below 5h threshold",
    );

    let line = serde_json::to_string(&e).unwrap();

    // One-line JSON object with the documented keys.
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(parsed.is_object());
    assert_eq!(parsed["action"], "scale_up");
    assert_eq!(parsed["from"], 2);
    assert_eq!(parsed["to"], 3);
    assert_eq!(parsed["binding_window"], "weekly_scoped");
    assert!(parsed.get("ts").is_some());
    assert!(parsed["reason"]
        .as_str()
        .unwrap()
        .contains("Scaled up from 2 to 3 workers"));

    // And it round-trips back into the typed entry.
    let back: DecisionEntry = serde_json::from_str(&line).unwrap();
    assert_eq!(back.action, e.action);
    assert_eq!(back.from, e.from);
    assert_eq!(back.to, e.to);
    assert_eq!(back.binding_window, e.binding_window);
    assert!((back.margin_before - e.margin_before).abs() < 1e-9);
    assert!((back.margin_after - e.margin_after).abs() < 1e-9);
    assert_eq!(back.reason, e.reason);
    assert_eq!(back.trigger, e.trigger);
    assert_eq!(back.ts, e.ts);
}

#[test]
fn every_scale_action_serializes_snake_case_and_back() {
    let actions = [
        (ScaleAction::ScaleUp, "scale_up"),
        (ScaleAction::ScaleDown, "scale_down"),
        (ScaleAction::Hold, "hold"),
        (ScaleAction::SprintActivate, "sprint_activate"),
        (ScaleAction::SprintDeactivate, "sprint_deactivate"),
        (ScaleAction::PreScale, "pre_scale"),
        (ScaleAction::EmergencyBrakeEngage, "emergency_brake_engage"),
        (
            ScaleAction::EmergencyBrakeRelease,
            "emergency_brake_release",
        ),
        (ScaleAction::PromotionTransition, "promotion_transition"),
        (
            ScaleAction::CutoffRiskTransitionSafeToRisk,
            "cutoff_risk_transition_safe_to_risk",
        ),
        (
            ScaleAction::CutoffRiskTransitionRiskToSafe,
            "cutoff_risk_transition_risk_to_safe",
        ),
        (
            ScaleAction::PredictionAccuracyScore,
            "prediction_accuracy_score",
        ),
    ];
    let before = state_with_binding("five_hour", forecast(50.0, 4.0, 2.5));
    let after = state_with_binding("five_hour", forecast(55.0, 3.5, 2.5));

    for (action, key) in actions {
        let e = entry(action, &before, &after, 1, 2, "fixture");
        let line = serde_json::to_string(&e).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["action"], key, "action {:?} key", action);
        let back: DecisionEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(back.action, action, "action {:?} round-trip", action);
    }
}

// ---------------------------------------------------------------------------
// 2. Persist
// ---------------------------------------------------------------------------

#[test]
fn decisions_persist_append_and_read_back_newest_first() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("decisions.jsonl");

    let weekly =
        |util: f64, margin: f64| state_with_binding("weekly_scoped", forecast(util, margin, 37.5));

    // Oldest -> newest: scale up, hold, emergency brake.
    let scale_up = entry(
        ScaleAction::ScaleUp,
        &weekly(60.0, 5.0),
        &weekly(65.0, 3.0),
        0,
        3,
        "target 3 > current 0 beyond hysteresis 1",
    );
    let hold = entry(
        ScaleAction::Hold,
        &weekly(65.0, 3.0),
        &weekly(65.0, 3.0),
        3,
        3,
        "hysteresis: target 3 within ±1 of current 3",
    );
    let brake = entry(
        ScaleAction::EmergencyBrakeEngage,
        &weekly(98.5, -2.0),
        &weekly(98.5, -2.0),
        3,
        0,
        "binding window 'weekly_scoped' at/above cutoff threshold; target forced to 0",
    );

    append_decision_to_path(&scale_up, &path).unwrap();
    append_decision_to_path(&hold, &path).unwrap();
    append_decision_to_path(&brake, &path).unwrap();

    // Raw file: one JSON object per line, in append order.
    let raw = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 3, "one line per decision");
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v.is_object(), "each line must be a JSON object");
    }

    // Read back newest-first.
    let all = read_last_decisions_from_path(10, &path).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].action, ScaleAction::EmergencyBrakeEngage);
    assert_eq!(all[1].action, ScaleAction::Hold);
    assert_eq!(all[2].action, ScaleAction::ScaleUp);

    // `--last N` semantics: request fewer than exist, get the newest N.
    let last2 = read_last_decisions_from_path(2, &path).unwrap();
    assert_eq!(last2.len(), 2);
    assert_eq!(last2[0].action, ScaleAction::EmergencyBrakeEngage);
    assert_eq!(last2[1].action, ScaleAction::Hold);

    // Margin data survives the round-trip: the brake entry kept its negative margin.
    assert!((all[0].margin_before - (-2.0)).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 3. Render
// ---------------------------------------------------------------------------

#[test]
fn decision_renders_binding_window_and_rationale_for_humans() {
    let before = state_with_binding("weekly_scoped", forecast(60.0, 5.0, 37.5));
    let after = state_with_binding("weekly_scoped", forecast(65.0, 3.0, 37.5));
    let e = entry(
        ScaleAction::ScaleUp,
        &before,
        &after,
        0,
        3,
        "target 3 > current 0 beyond hysteresis 1",
    );

    let rendered = format_decision_human(&e);

    assert!(rendered.contains("SCALE UP"), "action header: {}", rendered);
    assert!(
        rendered.contains("Workers: 0 -> 3"),
        "worker transition: {}",
        rendered
    );
    assert!(
        rendered.contains("Binding: weekly_scoped (margin 5.0h -> 3.0h)"),
        "binding window and margin movement: {}",
        rendered
    );
    assert!(
        rendered.contains("Trigger: target 3 > current 0 beyond hysteresis 1"),
        "trigger line: {}",
        rendered
    );
    assert!(
        rendered.contains("Scaled up from 0 to 3 workers"),
        "rationale with concrete numbers: {}",
        rendered
    );
}

#[test]
fn multiple_decisions_render_as_a_list_and_json_array() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("decisions.jsonl");

    let weekly =
        |util: f64, margin: f64| state_with_binding("weekly_scoped", forecast(util, margin, 37.5));
    append_decision_to_path(
        &entry(
            ScaleAction::ScaleUp,
            &weekly(60.0, 5.0),
            &weekly(65.0, 3.0),
            0,
            3,
            "t1",
        ),
        &path,
    )
    .unwrap();
    append_decision_to_path(
        &entry(
            ScaleAction::EmergencyBrakeEngage,
            &weekly(98.5, -2.0),
            &weekly(98.5, -2.0),
            3,
            0,
            "t2",
        ),
        &path,
    )
    .unwrap();

    let entries = read_last_decisions_from_path(10, &path).unwrap();

    // Human render: newest first, one block per decision, separators between.
    let rendered = format_decisions_human(&entries);
    assert!(
        rendered.contains("Last 2 decision(s)"),
        "header: {}",
        rendered
    );
    let brake_at = rendered
        .find("EMERGENCY BRAKE ENGAGE")
        .expect("brake block");
    let up_at = rendered.find("SCALE UP").expect("scale up block");
    assert!(brake_at < up_at, "newest decision must render first");
    assert!(rendered.contains("---"), "blocks are separated");

    // JSON render — the exact serialization `cgov explain --json` prints.
    let json = serde_json::to_string_pretty(&entries).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.is_array());
    assert_eq!(parsed.as_array().unwrap().len(), 2);
    assert_eq!(parsed[0]["action"], "emergency_brake_engage");
    assert_eq!(parsed[0]["to"], 0);
    assert_eq!(parsed[1]["action"], "scale_up");
    assert_eq!(parsed[1]["binding_window"], "weekly_scoped");
}

#[test]
fn empty_log_renders_as_no_decisions() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("does-not-exist.jsonl");

    let entries = read_last_decisions_from_path(5, &path).unwrap();
    assert!(entries.is_empty());
    assert_eq!(format_decisions_human(&entries), "No decisions recorded.\n");
}

// ---------------------------------------------------------------------------
// 4. Log path override
// ---------------------------------------------------------------------------

#[test]
fn decisions_path_honors_env_override() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let temp = TempDir::new().unwrap();
    let overridden = temp.path().join("redirected.jsonl");

    set_decisions_path(&overridden);
    assert_eq!(default_decisions_path(), overridden);

    clear_decisions_path();
    assert_eq!(
        default_decisions_path()
            .file_name()
            .and_then(|s| s.to_str()),
        Some("governor-decisions.jsonl"),
        "without the override the log lives at the default ~/.needle path"
    );
}

// ---------------------------------------------------------------------------
// 5. Act-cycle wiring (end-to-end)
// ---------------------------------------------------------------------------

/// The `create_full_state_file` shape: binding weekly_scoped at 35% with
/// safe_worker_count 7 (p75 6), one agent with bounds 0..8. Census finds no
/// tmux workers for the test pattern, so current is 0 and the act cycle must
/// compute target 7 → ScaleUp(7) past a 1.0 band.
fn scale_up_fixture_state() -> state::GovernorState {
    let mut s = state::GovernorState::new();
    s.capacity_forecast = state::CapacityForecast {
        five_hour: state::WindowForecast {
            current_utilization: 50.0,
            safe_worker_count: Some(5),
            safe_worker_count_p75: Some(4),
            ..Default::default()
        },
        seven_day: state::WindowForecast {
            current_utilization: 40.0,
            safe_worker_count: Some(6),
            safe_worker_count_p75: Some(5),
            ..Default::default()
        },
        weekly_scoped: state::WindowForecast {
            current_utilization: 35.0,
            margin_hrs: 20.0,
            hours_remaining: 100.0,
            safe_worker_count: Some(7),
            safe_worker_count_p75: Some(6),
            binding: true,
            ..Default::default()
        },
        binding_window: "weekly_scoped".to_string(),
        ..Default::default()
    };
    s.workers.insert(
        "test-agent".to_string(),
        state::WorkerState {
            current: 0,
            target: 0,
            min: 0,
            max: 8,
        },
    );
    s
}

#[test]
fn act_cycle_appends_scale_up_decision_with_computed_vs_actual_context() {
    let temp = TempDir::new().unwrap();
    let (decision, decisions_path) = run_dry_act_cycle(temp.path(), &scale_up_fixture_state(), 1.0);

    let n = match &decision {
        ScalingDecision::ScaleUp(n) => *n,
        other => panic!("fixture should produce ScaleUp, got {:?}", other),
    };
    assert!(n >= 1, "fixture target 7 vs current 0 must scale up");

    let entries = read_last_decisions_from_path(10, &decisions_path).unwrap();
    assert_eq!(entries.len(), 1, "exactly one decision recorded this cycle");

    let e = &entries[0];
    assert_eq!(e.action, ScaleAction::ScaleUp);
    assert_eq!(e.from, 0, "census found no workers");
    assert_eq!(e.to, n, "requested post-scale count");
    assert_eq!(
        e.binding_window, "weekly_scoped",
        "the binding window is recorded"
    );
    assert!(
        (e.margin_after - 20.0).abs() < 1e-9,
        "binding window margin recorded, got {}",
        e.margin_after
    );
    assert!(
        e.trigger.contains("target 7 > current 0"),
        "trigger carries the computed-vs-current rationale: {}",
        e.trigger
    );

    // Computed-vs-actual: dry-run computed a target and requested workers but
    // launched none.
    let ctx = e.context.as_ref().expect("computed-vs-actual context");
    assert_eq!(ctx["computed_target"], serde_json::json!(7));
    assert_eq!(ctx["wanted_delta"], serde_json::json!(7));
    assert_eq!(ctx["hysteresis_band"], serde_json::json!(1.0));
    assert_eq!(
        ctx["actual_launched"],
        serde_json::json!(0),
        "dry-run launches nothing"
    );
    assert_eq!(ctx["dry_run"], serde_json::json!(true));
    assert_eq!(ctx["sprint_boost"], serde_json::json!(false));

    // And the recorded entry renders for a human, exactly as `cgov explain`
    // will show it.
    let rendered = format_decisions_human(&entries);
    assert!(rendered.contains("SCALE UP"));
    assert!(rendered.contains("Binding: weekly_scoped"));
}

#[test]
fn act_cycle_records_hysteresis_suppressed_hold() {
    // Target 1, current 0, band 2.0: the wanted change is smaller than the
    // band, so the cycle holds — and that suppressed hold IS a decision the
    // log must show ("wanted 1, stayed at 0").
    let mut s = state::GovernorState::new();
    s.capacity_forecast = state::CapacityForecast {
        weekly_scoped: state::WindowForecast {
            current_utilization: 10.0,
            margin_hrs: 60.0,
            hours_remaining: 120.0,
            safe_worker_count: Some(1),
            safe_worker_count_p75: Some(1),
            binding: true,
            ..Default::default()
        },
        binding_window: "weekly_scoped".to_string(),
        ..Default::default()
    };
    s.workers.insert(
        "test-agent".to_string(),
        state::WorkerState {
            current: 0,
            target: 0,
            min: 0,
            max: 8,
        },
    );

    let temp = TempDir::new().unwrap();
    let (decision, decisions_path) = run_dry_act_cycle(temp.path(), &s, 2.0);

    assert!(
        matches!(decision, ScalingDecision::NoChange),
        "delta 1 within band 2.0 must hold, got {:?}",
        decision
    );

    let entries = read_last_decisions_from_path(10, &decisions_path).unwrap();
    assert_eq!(entries.len(), 1, "hysteresis-suppressed hold is recorded");
    assert_eq!(entries[0].action, ScaleAction::Hold);
    assert_eq!(entries[0].from, 0);
    assert_eq!(entries[0].to, 0);
    assert!(
        entries[0].trigger.contains("hysteresis"),
        "hold trigger names the band: {}",
        entries[0].trigger
    );
    let ctx = entries[0].context.as_ref().unwrap();
    assert_eq!(ctx["computed_target"], serde_json::json!(1));
    assert_eq!(ctx["wanted_delta"], serde_json::json!(1));
}

#[test]
fn act_cycle_does_not_log_at_target_holds() {
    // Target 0, current 0: a steady-state at-target hold is a no-op, not a
    // decision — recording it every loop interval would drown the log.
    let mut s = state::GovernorState::new();
    s.capacity_forecast = state::CapacityForecast {
        weekly_scoped: state::WindowForecast {
            current_utilization: 0.0,
            safe_worker_count: Some(0),
            safe_worker_count_p75: Some(0),
            binding: true,
            ..Default::default()
        },
        binding_window: "weekly_scoped".to_string(),
        ..Default::default()
    };
    s.workers.insert(
        "test-agent".to_string(),
        state::WorkerState {
            current: 0,
            target: 0,
            min: 0,
            max: 8,
        },
    );

    let temp = TempDir::new().unwrap();
    let (decision, decisions_path) = run_dry_act_cycle(temp.path(), &s, 1.0);

    assert!(matches!(decision, ScalingDecision::NoChange));
    assert!(
        !decisions_path.exists(),
        "at-target holds must not produce decision-log entries"
    );
}
