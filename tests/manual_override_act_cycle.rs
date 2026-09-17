//! End-to-end act-cycle coverage for the `cgov scale` manual-override contract.
//!
//! These tests use a fake tmux/bf environment so the real `run_act_cycle`
//! path is exercised without touching the operator's worker fleet.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Duration, Utc};
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{run_act_cycle, ScalingDecision};
use claude_governor::narrator::read_last_decisions_from_path;
use claude_governor::state::{self, GovernorState, ManualOverride, WorkerState};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    path: String,
    decisions: Option<String>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.path);
        match &self.decisions {
            Some(value) => std::env::set_var("CGOV_DECISIONS_PATH", value),
            None => std::env::remove_var("CGOV_DECISIONS_PATH"),
        }
    }
}

fn fake_environment(dir: &TempDir, sessions: &str) -> (EnvGuard, PathBuf, PathBuf) {
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let sessions_path = dir.path().join("sessions");
    std::fs::write(&sessions_path, sessions).unwrap();
    let decisions_path = dir.path().join("decisions.jsonl");

    let tmux = format!(
        "#!/bin/sh\ncase \"$1\" in\n  list-sessions) [ -f '{sessions}' ] && cat '{sessions}';;\n  has-session) [ -s '{sessions}' ];;\n  send-keys) : > '{sessions}';;\n  *) :;;\nesac\n",
        sessions = sessions_path.display()
    );
    write_executable(&bin.join("tmux"), &tmux);
    write_executable(&bin.join("bf"), "#!/bin/sh\necho 'bf-fake0001 ready'\n");
    write_executable(&bin.join("launch-stub"), "#!/bin/sh\nexit 0\n");

    let old_path = std::env::var("PATH").unwrap_or_default();
    let old_decisions = std::env::var("CGOV_DECISIONS_PATH").ok();
    std::env::set_var("PATH", format!("{}:{}", bin.display(), old_path));
    std::env::set_var("CGOV_DECISIONS_PATH", &decisions_path);

    (
        EnvGuard {
            path: old_path,
            decisions: old_decisions,
        },
        sessions_path,
        decisions_path,
    )
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn agent(
    name: &str,
    min_workers: u32,
    max_workers: u32,
    subscription: bool,
    workspace: Option<&Path>,
) -> AgentConfig {
    let launch_cmd = match workspace {
        Some(path) => format!(
            "launch-stub --workspace {} --agent {}",
            path.display(),
            name
        ),
        None => "launch-stub".to_string(),
    };
    serde_json::from_value(serde_json::json!({
        "launch_cmd": launch_cmd,
        "session_pattern": format!("{name}-*"),
        "heartbeat_dir": "/tmp/cgov-manual-override-no-heartbeats",
        "min_workers": min_workers,
        "max_workers": max_workers,
        "subscription": subscription,
    }))
    .unwrap()
}

fn governor_config(agents: &HashMap<String, AgentConfig>) -> GovernorConfig {
    GovernorConfig {
        pricing: PricingConfig {
            models: HashMap::new(),
        },
        sprint: Default::default(),
        daemon: Default::default(),
        alerts: AlertConfig {
            enabled: false,
            ..Default::default()
        },
        composite_risk: Default::default(),
        cone_scaling: Default::default(),
        agents: agents.clone(),
        credentials_path: None,
    }
}

fn comfortable_state(safe_target: u32) -> GovernorState {
    let mut state = GovernorState::new();
    let mut five = state::WindowForecast::default();
    five.current_utilization = 10.0;
    five.hours_remaining = 100.0;
    five.safe_worker_count = Some(safe_target);
    five.safe_worker_count_p75 = Some(safe_target);
    five.cone_ratio = 0.0;
    let mut seven = five.clone();
    seven.hours_remaining = 100.0;
    let mut weekly = seven.clone();
    weekly.hours_remaining = 100.0;
    state.capacity_forecast = state::CapacityForecast {
        five_hour: five,
        seven_day: seven,
        weekly_scoped: weekly,
        binding_window: "five_hour".to_string(),
        ..Default::default()
    };
    state
}

fn override_record(target: u32, expires_at: Option<chrono::DateTime<Utc>>) -> ManualOverride {
    ManualOverride {
        target,
        set_at: Utc::now() - Duration::minutes(1),
        expires_at,
        source: "cli".to_string(),
    }
}

fn run_act(
    path: &Path,
    dry_run: bool,
    agents: &HashMap<String, AgentConfig>,
    config: &GovernorConfig,
) -> ScalingDecision {
    run_act_cycle(
        path,
        dry_run,
        5.0,
        10,
        10,
        90.0,
        &AlertConfig {
            enabled: false,
            ..Default::default()
        },
        agents,
        0,
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        config,
        Utc::now(),
    )
    .unwrap()
}

fn decision_context(dir: &TempDir) -> serde_json::Value {
    let entries = read_last_decisions_from_path(10, &dir.path().join("decisions.jsonl")).unwrap();
    entries
        .first()
        .and_then(|entry| entry.context.clone())
        .expect("act cycle should record a decision context")
}

#[test]
fn active_pin_wins_over_sprint_and_hysteresis_and_is_audited() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) = fake_environment(&dir, "pool-1\n");

    let mut agents = HashMap::new();
    agents.insert(
        "pool".to_string(),
        agent("pool", 0, 8, true, Some(dir.path())),
    );
    let config = governor_config(&agents);
    let mut state = comfortable_state(6);
    state.capacity_forecast.five_hour.current_utilization = 45.0;
    state.capacity_forecast.five_hour.hours_remaining = 1.5;
    state.capacity_forecast.seven_day.current_utilization = 45.0;
    state.capacity_forecast.weekly_scoped.current_utilization = 45.0;
    state.workers.insert(
        "pool".to_string(),
        WorkerState {
            current: 1,
            target: 1,
            min: 0,
            max: 8,
        },
    );
    state.manual_override = Some(override_record(0, None));
    let state_path = dir.path().join("governor-state.json");
    state::save_state(&state, &state_path).unwrap();

    let decision = run_act(&state_path, true, &agents, &config);
    assert_eq!(decision, ScalingDecision::ScaleDown(1));
    let after = state::load_state(&state_path).unwrap();
    assert_eq!(after.workers["pool"].target, 0);
    assert!(
        after.manual_override.is_some(),
        "a hold-until-clear pin persists"
    );

    let context = decision_context(&dir);
    assert_eq!(context["decision_source"], "manual_override");
    assert_eq!(context["computed_target"], 6);
    assert_eq!(context["effective_target"], 0);
    assert_eq!(context["sprint_boost"], false);
}

#[test]
fn bounds_and_per_agent_floors_apply_under_a_clamped_pin() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) = fake_environment(&dir, "");

    let mut agents = HashMap::new();
    agents.insert("sonnet".to_string(), agent("sonnet", 0, 4, false, None));
    agents.insert("opus".to_string(), agent("opus", 1, 2, false, None));
    let config = governor_config(&agents);
    let mut state = comfortable_state(4);
    state.workers.insert(
        "sonnet".to_string(),
        WorkerState {
            current: 0,
            target: 0,
            min: 0,
            max: 4,
        },
    );
    state.workers.insert(
        "opus".to_string(),
        WorkerState {
            current: 0,
            target: 0,
            min: 1,
            max: 2,
        },
    );
    // This is a raw state pin written before the fleet max was reduced. The
    // act cycle must clamp it to the current aggregate envelope [0, 4].
    state.manual_override = Some(override_record(99, None));
    let state_path = dir.path().join("governor-state.json");
    state::save_state(&state, &state_path).unwrap();

    assert_eq!(
        run_act(&state_path, true, &agents, &config),
        ScalingDecision::ScaleUp(4),
        "context: {}",
        decision_context(&dir)
    );
    let after = state::load_state(&state_path).unwrap();
    let sonnet = &after.workers["sonnet"];
    let opus = &after.workers["opus"];
    assert_eq!(sonnet.target + opus.target, 4);
    assert!((0..=4).contains(&sonnet.target));
    assert!((1..=2).contains(&opus.target));
    assert_eq!(after.manual_override.unwrap().target, 99);
    assert_eq!(decision_context(&dir)["effective_target"], 4);
}

#[test]
fn emergency_brake_forces_zero_then_the_stored_pin_resumes() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, sessions_path, _decisions) = fake_environment(&dir, "pool-1\n");

    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), agent("pool", 0, 8, false, None));
    let config = governor_config(&agents);
    let mut state = comfortable_state(4);
    state.capacity_forecast.five_hour.current_utilization = 98.0;
    state.workers.insert(
        "pool".to_string(),
        WorkerState {
            current: 1,
            target: 1,
            min: 0,
            max: 8,
        },
    );
    state.manual_override = Some(override_record(3, Some(Utc::now() + Duration::hours(1))));
    let state_path = dir.path().join("governor-state.json");
    state::save_state(&state, &state_path).unwrap();

    assert_eq!(
        run_act(&state_path, true, &agents, &config),
        ScalingDecision::EmergencyBrake
    );
    let braked = state::load_state(&state_path).unwrap();
    assert_eq!(braked.workers["pool"].target, 0);
    assert_eq!(braked.manual_override.as_ref().unwrap().target, 3);
    assert_eq!(decision_context(&dir)["decision_source"], "emergency_brake");

    // The first run was dry-run, so the fake live session is still present.
    std::fs::write(&sessions_path, "pool-1\n").unwrap();
    let mut resumed = braked;
    resumed.capacity_forecast.five_hour.current_utilization = 10.0;
    state::save_state(&resumed, &state_path).unwrap();
    assert_eq!(
        run_act(&state_path, true, &agents, &config),
        ScalingDecision::ScaleUp(2)
    );
    assert_eq!(decision_context(&dir)["decision_source"], "manual_override");
}

#[test]
fn expiry_returns_to_computed_targets_and_clearing_is_not_reversed_by_act_save() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) = fake_environment(&dir, "");

    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), agent("pool", 0, 8, false, None));
    let config = governor_config(&agents);
    let mut state = comfortable_state(2);
    state.workers.insert(
        "pool".to_string(),
        WorkerState {
            current: 0,
            target: 0,
            min: 0,
            max: 8,
        },
    );
    state.manual_override = Some(override_record(6, Some(Utc::now() - Duration::seconds(1))));
    let state_path = dir.path().join("governor-state.json");
    state::save_state(&state, &state_path).unwrap();

    assert_eq!(
        run_act(&state_path, true, &agents, &config),
        ScalingDecision::ScaleUp(2)
    );
    let after = state::load_state(&state_path).unwrap();
    assert!(after.manual_override.is_none());
    assert_eq!(decision_context(&dir)["decision_source"], "computed_target");
}

#[test]
fn no_change_still_reconciles_per_agent_allocation_for_a_pin() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) = fake_environment(&dir, "sonnet-1\nsonnet-2\n");

    let mut agents = HashMap::new();
    agents.insert("sonnet".to_string(), agent("sonnet", 0, 4, false, None));
    agents.insert("opus".to_string(), agent("opus", 1, 2, false, None));
    let config = governor_config(&agents);
    let mut state = comfortable_state(2);
    state.workers.insert(
        "sonnet".to_string(),
        WorkerState {
            current: 2,
            target: 2,
            min: 0,
            max: 4,
        },
    );
    state.workers.insert(
        "opus".to_string(),
        WorkerState {
            current: 0,
            target: 0,
            min: 1,
            max: 2,
        },
    );
    state.manual_override = Some(override_record(2, None));
    let state_path = dir.path().join("governor-state.json");
    state::save_state(&state, &state_path).unwrap();

    assert_eq!(
        run_act(&state_path, false, &agents, &config),
        ScalingDecision::NoChange
    );
    let after = state::load_state(&state_path).unwrap();
    assert_eq!(after.workers["sonnet"].target, 1);
    assert_eq!(after.workers["opus"].target, 1);
    assert_eq!(decision_context(&dir)["allocation_reconciled"], true);
}
