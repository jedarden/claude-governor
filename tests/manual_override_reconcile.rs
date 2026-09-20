//! Manual-target vs computed-target reconciliation (bead claudego-c165ce55,
//! third child of claudego-e9f097c6).
//!
//! The lifecycle half of the contract (persistence, clamping, brake
//! suspension, mid-cycle CLI writes) lives in `manual_override_lifecycle.rs`,
//! the base act-cycle precedence in `manual_override_act_cycle.rs`, and the
//! schema half in `manual_override_compat.rs`. This file pins the
//! reconciliation clauses those leave open, each against a control run that
//! proves the computed-target machinery WOULD have acted without the pin:
//!
//! - an underutilization sprint that verifiably fires without a pin is
//!   suppressed wholesale by one — the boosted target never survives an
//!   active override (the pin binds exactly, not `max(pin, sprint)`);
//! - a pre-scale ramp before a losing multiplier transition is suppressed by
//!   an active pin — pre-scale is a computed-target modifier, not an
//!   authority of its own;
//! - `cgov scale --clear` returns the fleet to computed targets on the next
//!   act cycle (the act-level half of the resume clause; the resolve-level
//!   half is pinned in the lifecycle file);
//! - a state file written before `manual_override` existed — no key at all —
//!   loads through the real act cycle and reconciles as computed;
//! - a pin at exactly the sum of the pools' min_workers floors lands the
//!   fleet on the pin with every floor intact, and still sheds through a
//!   hysteresis band that would hold the same move for a computed target.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{run_act_cycle, ScalingDecision};
use claude_governor::narrator::read_last_decisions_from_path;
use claude_governor::schedule::Promotion;
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

/// Fake tmux + bf on PATH, mirroring `manual_override_act_cycle.rs`. The
/// sessions file is the tmux census: one session name per line, matched
/// against each agent's `session_pattern`. `ready_beads` is what the fake
/// `bf ready` prints — the backlog signal the underutilization sprint needs.
fn fake_environment(dir: &TempDir, sessions: &str, ready_beads: &str) -> (EnvGuard, PathBuf, PathBuf) {
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
    write_executable(
        &bin.join("bf"),
        &format!("#!/bin/sh\ncat <<'BFEOD'\n{ready_beads}BFEOD\n"),
    );
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
        "heartbeat_dir": "/tmp/cgov-manual-reconcile-no-heartbeats",
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

/// A comfortable forecast whose computed target is `safe_target`: every
/// window carries the same safe count and sits far from the brake
/// threshold. `utilization` / `hours_remaining` tune sprint eligibility
/// (sprint wants utilization < 50% and 0 < hours < 2).
fn forecast_state(safe_target: u32, utilization: f64, hours_remaining: f64) -> GovernorState {
    let mut state = GovernorState::new();
    let window = state::WindowForecast {
        current_utilization: utilization,
        hours_remaining,
        safe_worker_count: Some(safe_target),
        safe_worker_count_p75: Some(safe_target),
        cone_ratio: 0.0,
        ..Default::default()
    };
    state.capacity_forecast = state::CapacityForecast {
        five_hour: window.clone(),
        seven_day: window.clone(),
        weekly_scoped: window,
        binding_window: "five_hour".to_string(),
        ..Default::default()
    };
    state
}

fn insert_worker(state: &mut GovernorState, name: &str, current: u32, min: u32, max: u32) {
    state.workers.insert(
        name.to_string(),
        WorkerState {
            current,
            target: current,
            min,
            max,
        },
    );
}

/// What `cgov scale N` stores, hold-until-clear form (mirrors
/// `manual_override_record` in the binary with `--ttl 0`).
fn pin(target: u32, now: DateTime<Utc>) -> ManualOverride {
    ManualOverride {
        target,
        set_at: now,
        expires_at: None,
        source: "cli".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_act(
    path: &Path,
    dry_run: bool,
    agents: &HashMap<String, AgentConfig>,
    config: &GovernorConfig,
    now: DateTime<Utc>,
    hysteresis_band: f64,
    pre_scale_minutes: u64,
    promotions: &[Promotion],
) -> ScalingDecision {
    run_act_cycle(
        path,
        dry_run,
        hysteresis_band,
        10,
        10,
        90.0,
        &AlertConfig {
            enabled: false,
            ..Default::default()
        },
        agents,
        pre_scale_minutes,
        promotions,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        config,
        now,
    )
    .unwrap()
}

/// The newest decision entry's context — the act cycle's own account of what
/// it computed, what it applied, and why.
fn decision_context(dir: &TempDir) -> serde_json::Value {
    if let Ok(raw) = std::fs::read_to_string(dir.path().join("decisions.jsonl")) {
        eprintln!("=== decisions.jsonl ===\n{raw}=== end ===");
    } else {
        eprintln!("=== decisions.jsonl MISSING ===");
    }
    let entries = read_last_decisions_from_path(10, &dir.path().join("decisions.jsonl")).unwrap();
    entries
        .first()
        .and_then(|entry| entry.context.clone())
        .expect("act cycle should record a decision context")
}

fn take_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// UTC from Eastern components (March 2026 is EDT, UTC-4), the same fixture
/// shape the schedule tests use.
fn et(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
    use chrono::TimeZone;
    chrono_tz::America::New_York
        .with_ymd_and_hms(year, month, day, hour, min, 0)
        .unwrap()
        .with_timezone(&Utc)
}

fn march_2026_promo() -> Promotion {
    Promotion {
        name: "March 2026 Promo".to_string(),
        start_date: "2026-03-15".to_string(),
        end_date: "2026-03-25".to_string(),
        peak_start_hour_et: 8,
        peak_end_hour_et: 14,
        offpeak_multiplier: 2.0,
        applies_to: vec!["weekly_scoped".to_string()],
    }
}

#[test]
fn sprint_that_would_fire_is_suppressed_by_an_active_pin() {
    let _lock = take_lock();
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) = fake_environment(
        &dir,
        "pool-1\n",
        "bf-fake0001 ready\nbf-fake0002 ready\n",
    );

    let mut agents = HashMap::new();
    // Subscription + workspace + fake backlog of 2 > 1 running: sprint-eligible.
    agents.insert(
        "pool".to_string(),
        agent("pool", 0, 8, true, Some(dir.path())),
    );
    let config = governor_config(&agents);

    // Safe counts sit ABOVE the pin below (6 > 5): the per-pool
    // window-affinity cap is the tightest safe count across the pool's
    // windows, and a cap under the pin would silently bound the pool's
    // post-cycle allocation regardless of the override. Precedence is the
    // clause under test here, not allocator caps.
    let mut state = forecast_state(6, 20.0, 1.5);
    insert_worker(&mut state, "pool", 1, 0, 8);
    let path = dir.path().join("governor-state.json");
    state::save_state(&state, &path).unwrap();

    // Control, no pin: the sprint verifiably fires — computed 6 is boosted to
    // the pool max 8. This is what makes the second half non-vacuous.
    let decision = run_act(&path, true, &agents, &config, Utc::now(), 0.0, 0, &[]);
    assert_eq!(decision, ScalingDecision::ScaleUp(7), "1 -> 8 (sprint)");
    let control = decision_context(&dir);
    assert_eq!(control["decision_source"], "computed_target");
    assert_eq!(control["sprint_boost"], true);
    assert_eq!(control["computed_target"], 6);
    assert_eq!(control["effective_target"], 8);

    // Same fleet, same sprint-eligible window, plus an active pin: the boost
    // is suppressed wholesale and the fleet moves to the pin exactly — not
    // max(pin, sprint), and not the computed 6 either. The cycle still
    // records the computed target it chose not to use.
    state.manual_override = Some(pin(5, Utc::now()));
    state::save_state(&state, &path).unwrap();

    let decision = run_act(&path, true, &agents, &config, Utc::now(), 0.0, 0, &[]);
    assert_eq!(decision, ScalingDecision::ScaleUp(4), "1 -> 5 (the pin)");
    let pinned = decision_context(&dir);
    assert_eq!(pinned["decision_source"], "manual_override");
    assert_eq!(pinned["sprint_boost"], false);
    assert_eq!(pinned["computed_target"], 6);
    assert_eq!(pinned["effective_target"], 5);
    assert_eq!(pinned["manual_override_target"], 5);

    let after = state::load_state(&path).unwrap();
    assert_eq!(after.workers["pool"].target, 5);
    assert!(
        after.manual_override.is_some(),
        "the pin persists through the cycle it governed"
    );
}

#[test]
fn pre_scale_ramp_is_suppressed_by_an_active_pin() {
    let _lock = take_lock();
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) =
        fake_environment(&dir, "pool-1\npool-2\npool-3\npool-4\n", "bf-fake0001 ready\n");

    // Non-subscription pool: the sprint path stays out of the picture, so the
    // only computed-target modifier in play is the pre-scale ramp.
    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), agent("pool", 0, 8, false, None));
    let config = governor_config(&agents);

    let now = et(2026, 3, 16, 7, 35);
    let promos = vec![march_2026_promo()];
    let mut state = forecast_state(6, 10.0, 100.0);
    insert_worker(&mut state, "pool", 4, 0, 8);
    state.usage.sonnet_resets_at = (now + Duration::days(2)).to_rfc3339();
    let path = dir.path().join("governor-state.json");
    state::save_state(&state, &path).unwrap();

    // Control, no pin: 25 minutes before the promo's 2x -> 1x transition the
    // computed target 6 is pre-scaled down — post-transition safe is
    // floor(6 / 2) = 3, ramped one worker to max(3, 4 - 1) = 3 — and the
    // zero band lets the one-worker shed through.
    let decision = run_act(&path, true, &agents, &config, now, 0.0, 30, &promos);
    assert_eq!(decision, ScalingDecision::ScaleDown(1), "4 -> 3 (ramp)");
    let control = decision_context(&dir);
    assert_eq!(control["decision_source"], "computed_target");
    assert_eq!(control["computed_target"], 6);
    assert_eq!(control["effective_target"], 3);

    // Same instant, same imminent transition, plus an active pin: the ramp is
    // suppressed — pre-scale modifies a computed target and has no authority
    // over an operator's pin. The fleet moves toward 5, not toward 3.
    state.manual_override = Some(pin(5, now));
    state::save_state(&state, &path).unwrap();

    let decision = run_act(&path, true, &agents, &config, now, 0.0, 30, &promos);
    assert_eq!(decision, ScalingDecision::ScaleUp(1), "4 -> 5 (the pin)");
    let pinned = decision_context(&dir);
    assert_eq!(pinned["decision_source"], "manual_override");
    assert_eq!(pinned["computed_target"], 6);
    assert_eq!(pinned["effective_target"], 5);
    assert_eq!(pinned["manual_override_target"], 5);

    let after = state::load_state(&path).unwrap();
    assert_eq!(after.workers["pool"].target, 5);
}

#[test]
fn clearing_the_pin_restores_computed_targets_on_the_next_cycle() {
    let _lock = take_lock();
    let dir = TempDir::new().unwrap();
    let (_env, sessions_path, _decisions) = fake_environment(&dir, "pool-1\n", "bf-fake0001 ready\n");

    let mut agents = HashMap::new();
    agents.insert("pool".to_string(), agent("pool", 0, 8, false, None));
    let config = governor_config(&agents);

    let now = Utc::now();
    let mut state = forecast_state(2, 10.0, 100.0);
    insert_worker(&mut state, "pool", 1, 0, 8);
    state.manual_override = Some(pin(6, now));
    let path = dir.path().join("governor-state.json");
    state::save_state(&state, &path).unwrap();

    // While pinned, 6 binds over the computed 2.
    let decision = run_act(&path, true, &agents, &config, now, 0.0, 0, &[]);
    assert_eq!(decision, ScalingDecision::ScaleUp(5), "1 -> 6 (the pin)");
    assert_eq!(decision_context(&dir)["decision_source"], "manual_override");

    // `cgov scale --clear` — the same locked load-take-save transaction
    // run_scale_command runs (the CLI wrapper itself is untestable here: it
    // resolves the state path internally).
    state::with_state_lock(&path, || -> anyhow::Result<()> {
        let mut s = state::load_state(&path)?;
        assert!(
            s.manual_override.take().is_some(),
            "the clear must find the pin it is removing"
        );
        Ok(state::save_state(&s, &path)?)
    })
    .unwrap();

    // Next cycle: computed targets rule again — the resume clause, at the act
    // level rather than the resolve level.
    std::fs::write(&sessions_path, "pool-1\n").unwrap();
    let decision = run_act(&path, true, &agents, &config, now, 0.0, 0, &[]);
    assert_eq!(decision, ScalingDecision::ScaleUp(1), "1 -> 2 (computed)");
    let context = decision_context(&dir);
    assert_eq!(context["decision_source"], "computed_target");
    assert_eq!(context["computed_target"], 2);
    assert_eq!(context["effective_target"], 2);
    assert_eq!(context["manual_override_target"], serde_json::Value::Null);

    let after = state::load_state(&path).unwrap();
    assert!(after.manual_override.is_none(), "the clear stays cleared");
    assert_eq!(after.workers["pool"].target, 2);
}

#[test]
fn legacy_state_file_without_override_reconciles_as_computed() {
    let _lock = take_lock();
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) = fake_environment(&dir, "", "bf-fake0001 ready\n");

    // A state file in the pre-override schema (45a7be4^): no manual_override
    // key anywhere, plus the forecast a real pre-override file carried. The
    // load half (deserialize + real load_state) is pinned in
    // manual_override_compat.rs; here the file goes through a full act cycle.
    let doc = serde_json::json!({
        "updated_at": "2026-09-16T11:00:00Z",
        "usage": {
            "sonnet_pct": 0.0,
            "all_models_pct": 41.5,
            "five_hour_pct": 63.25,
            "sonnet_resets_at": "",
            "seven_day_resets_at": "2026-09-19T00:00:00Z",
            "five_hour_resets_at": "2026-09-16T15:00:00Z",
            "stale": false,
            "weekly_scoped_model": null,
            "weekly_scoped_pct": 41.5
        },
        "workers": {
            "needle-sonnet": { "current": 0, "target": 0, "min": 0, "max": 8 }
        },
        "capacity_forecast": {
            "five_hour": {
                "current_utilization": 10.0,
                "hours_remaining": 100.0,
                "safe_worker_count": 3,
                "safe_worker_count_p75": 3,
                "cone_ratio": 0.0
            },
            "seven_day": {
                "current_utilization": 10.0,
                "hours_remaining": 100.0,
                "safe_worker_count": 3,
                "safe_worker_count_p75": 3,
                "cone_ratio": 0.0
            },
            "weekly_scoped": {
                "current_utilization": 10.0,
                "hours_remaining": 100.0,
                "safe_worker_count": 3,
                "safe_worker_count_p75": 3,
                "cone_ratio": 0.0
            },
            "binding_window": "five_hour"
        },
        "alerts": [],
        "safe_mode": {
            "active": false,
            "entered_at": null,
            "trigger": null,
            "median_error_at_entry": null,
            "predictions_since_entry": 0,
            "scored_at_entry": 0
        },
        "token_refresh_failing": false,
        "p5h_delta": -3.5,
        "p7d_delta": 12.0,
        "p7ds_delta": null
    });
    let path = dir.path().join("governor-state.json");
    std::fs::write(&path, doc.to_string()).unwrap();

    let mut agents = HashMap::new();
    agents.insert(
        "needle-sonnet".to_string(),
        agent("needle-sonnet", 0, 8, false, None),
    );
    let config = governor_config(&agents);

    // The cycle reconciles as computed: safe count 3 from the file's own
    // forecast, no override consulted, source stamped computed_target.
    let decision = run_act(&path, true, &agents, &config, Utc::now(), 0.0, 0, &[]);
    assert_eq!(decision, ScalingDecision::ScaleUp(3), "0 -> 3 (computed)");
    let context = decision_context(&dir);
    assert_eq!(context["decision_source"], "computed_target");
    assert_eq!(context["computed_target"], 3);
    assert_eq!(context["effective_target"], 3);
    assert_eq!(context["manual_override_target"], serde_json::Value::Null);

    // The act-owned save adopted the legacy file: legacy values survived the
    // merge, the field this version owns landed as an explicit null, and no
    // override exists.
    let after = state::load_state(&path).unwrap();
    assert!(after.manual_override.is_none());
    assert_eq!(after.workers["needle-sonnet"].target, 3);
    assert_eq!(after.usage.five_hour_pct, 63.25, "legacy values survive");
    assert_eq!(after.p5h_delta, Some(-3.5));

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        saved["manual_override"],
        serde_json::Value::Null,
        "the cycle's save writes the current schema: the key the legacy file \
         lacked is present (null) once this version touches the file"
    );
}

#[test]
fn pin_at_the_sum_of_floors_lands_on_the_pin_with_every_floor_intact() {
    let _lock = take_lock();
    let dir = TempDir::new().unwrap();
    let (_env, _sessions, _decisions) = fake_environment(
        &dir,
        "sonnet-1\nsonnet-2\nsonnet-3\nopus-1\n",
        "bf-fake0001 ready\n",
    );

    // sonnet (min 0, max 4) + opus (min 1, max 2): aggregate envelope [0, 4],
    // sum of floors 1. A pin of 2 is inside the envelope and exactly covers
    // every floor with one spare slot.
    let mut agents = HashMap::new();
    agents.insert("sonnet".to_string(), agent("sonnet", 0, 4, false, None));
    agents.insert("opus".to_string(), agent("opus", 1, 2, false, None));
    let config = governor_config(&agents);

    // Safe counts of 2, deliberately UNDER the envelope max: the aggregate
    // computed target is the safe count clamped to the envelope, so 6 would
    // clamp to 4 and make the control an at-target hold (which the cycle
    // does not even record). 2 keeps the control a band-held shed.
    let mut state = forecast_state(2, 10.0, 100.0);
    insert_worker(&mut state, "sonnet", 3, 0, 4);
    insert_worker(&mut state, "opus", 1, 1, 2);
    let path = dir.path().join("governor-state.json");
    state::save_state(&state, &path).unwrap();

    // Control, no pin, computed 2: the same shed the pin will request is
    // HELD by the 90-worker hysteresis band — forecast noise must not shed
    // workers, so the computed path does nothing.
    let decision = run_act(&path, true, &agents, &config, Utc::now(), 90.0, 0, &[]);
    assert_eq!(decision, ScalingDecision::NoChange);
    let control = decision_context(&dir);
    assert_eq!(control["decision_source"], "computed_target");
    assert_eq!(control["effective_target"], 2);

    // Pinned 2, same band: the operator's pin bypasses the hold — a pin is a
    // deliberate move, not forecast noise — and the shed lands on the pin
    // with every floor intact. Opus (the expensive, floored pool) keeps its
    // 1; sonnet's surplus above its own floor funds the rest.
    state.manual_override = Some(pin(2, Utc::now()));
    state::save_state(&state, &path).unwrap();

    // Live run: the executor actually removes the workers this time.
    let decision = run_act(&path, false, &agents, &config, Utc::now(), 90.0, 0, &[]);
    assert_eq!(decision, ScalingDecision::ScaleDown(2), "4 -> 2 (the pin)");

    let after = state::load_state(&path).unwrap();
    let sonnet = after.workers["sonnet"].target;
    let opus = after.workers["opus"].target;
    assert_eq!(sonnet + opus, 2, "the fleet lands exactly on the pin");
    assert_eq!(opus, 1, "opus's min_workers floor survives the shed");
    assert_eq!(sonnet, 1, "the spare slot stays with the cheap pool");
    assert_eq!(
        after.manual_override.map(|ov| ov.target),
        Some(2),
        "the stored pin stays raw through the cycle it governed"
    );
}
