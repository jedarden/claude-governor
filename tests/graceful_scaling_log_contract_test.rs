//! Runtime proof that the graceful-scaling guarantee is auditable from the
//! log stream (claudego-f10af2f7).
//!
//! WHO gets removed is pinned end-to-end in `tests/graceful_idle_scaling_test.rs`,
//! `tests/idle_only_scale_down_safety_test.rs`, `tests/scale_down_ordering.rs`,
//! and `tests/emergency_brake_distinction_test.rs` — those read the fake tmux
//! call log and the decision audit log. What nothing asserted until now is the
//! half an operator actually tails: `journalctl --user -u claude-governor`
//! (CLAUDE.md §3's documented triage path) must show, on its own, that a
//! graceful scale-down removed only idle workers (`graceful=N, forced=0`) and
//! that an emergency brake bypassed idle selection (`scaling all to 0`,
//! `killed N worker sessions`) — without a line naming an active worker as
//! interrupted. This binary drives real live cycles and asserts on the
//! records a captured logger received.
//!
//! It lives in its own integration-test binary because it must own the
//! process-global `log` logger — the same reasoning as
//! `tests/delta_logging_runtime_test.rs`, whose capture plumbing is reused
//! here.
//!
//! The documented contract being pinned (docs/hysteresis-and-smooth-scaling.md,
//! docs/plan/plan.md §6 and §9):
//!
//! - **Graceful scale-down is idle-only** — with enough idle capacity to
//!   cover the cut, every removed worker is an idle one and the log's
//!   `forced=` count is 0; active workers are never interrupted, and no log
//!   record even names them.
//! - **The emergency brake is the documented exception** — a window at 98%
//!   kills every session, idle and active, directly (`kill-session`, not the
//!   graceful SIGINT path), and the log stream says so at every layer: the
//!   decision line naming the window and percentage, the executor's
//!   `source=emergency_brake` line, the killed count, the reconcile summary,
//!   and the decision audit entry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{Duration as ChronoDuration, Utc};
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{run_act_cycle, ScalingDecision};
use claude_governor::narrator::{read_last_decisions_from_path, ScaleAction};
use claude_governor::state;

// ---------------------------------------------------------------------------
// Log capture — the seam under test
// ---------------------------------------------------------------------------

/// Captured (level, message) pairs from the governor's logging.
static TEST_LOGS: OnceLock<Mutex<Vec<(log::Level, String)>>> = OnceLock::new();

struct TestLogger;

impl log::Log for TestLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        let logs = TEST_LOGS.get_or_init(|| Mutex::new(Vec::new()));
        logs.lock()
            .unwrap()
            .push((record.level(), format!("{}", record.args())));
    }
    fn flush(&self) {}
}

static TEST_LOGGER: TestLogger = TestLogger;

/// Serializes whole test bodies against the shared captured-log buffer and
/// the process environment (both tests swap PATH / CGOV_DECISIONS_PATH).
static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

fn init_logger() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&TEST_LOGGER).expect("this binary owns the global logger");
        // INFO carries the graceful scale-down summary lines; anything
        // stricter would let this test pass for the wrong reason.
        log::set_max_level(log::LevelFilter::Info);
    });
}

/// Records captured at or after `from` whose message contains `pattern`.
fn records_since(from: usize, pattern: &str) -> Vec<(log::Level, String)> {
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .iter()
        .skip(from)
        .filter(|(_, msg)| msg.contains(pattern))
        .cloned()
        .collect()
}

fn record_count() -> usize {
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .len()
}

/// Assert exactly one captured record (at or after `from`) contains `pattern`
/// and return it — the cycle emits each contract line once, and a second copy
/// would mean a replayed or duplicated decision.
fn one_record(from: usize, pattern: &str) -> (log::Level, String) {
    let hits = records_since(from, pattern);
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one log record containing {pattern:?}, got {}: {hits:?}",
        hits.len()
    );
    hits.into_iter().next().expect("non-empty")
}

// ---------------------------------------------------------------------------
// Hermetic fixtures — the fake tmux fleet from tests/graceful_idle_scaling_test.rs
// ---------------------------------------------------------------------------

/// Session prefix: the agent's `cgscale-*` pattern trims to this, and every
/// fixture session name starts with it so the fake tmux census counts them.
const PREFIX: &str = "cgscale";

fn write_executable(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{}\n", body)).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod script");
}

/// `tmux` stands in for the live fleet: `list-sessions` prints the census,
/// `send-keys`/`kill-session` mark the target dead and record the call, and
/// every invocation lands in the calls log.
fn install_fake_tmux(bin_dir: &Path, sessions_file: &Path, calls_log: &Path, stopped_file: &Path) {
    write_executable(
        bin_dir,
        "tmux",
        &format!(
            "printf '%s\\n' \"$*\" >> '{calls}'\ncase \"$1\" in\n\
             \x20 list-sessions) cat '{sessions}' 2>/dev/null; exit 0;;\n\
             \x20 has-session)\n\
             \x20   if grep -Fxq \"$3\" '{stopped}' 2>/dev/null; then exit 1; fi\n\
             \x20   if grep -Fxq \"$3\" '{sessions}' 2>/dev/null; then exit 0; fi\n\
             \x20   exit 1;;\n\
             \x20 send-keys|kill-session) printf '%s\\n' \"$3\" >> '{stopped}'; exit 0;;\n\
             esac\nexit 0",
            calls = calls_log.display(),
            sessions = sessions_file.display(),
            stopped = stopped_file.display(),
        ),
    );
}

fn install_quiet_bf(bin_dir: &Path) {
    write_executable(bin_dir, "bf", "exit 0");
}

fn install_launch_stub(bin_dir: &Path) {
    write_executable(bin_dir, "launch-stub", "echo \"$3\" >> \"$4\"");
}

fn launch_cmd_for(bin_dir: &Path, env: &Path, log: &Path) -> String {
    format!(
        "{} --workspace {} log-scaled {}",
        bin_dir.join("launch-stub").display(),
        env.display(),
        log.display()
    )
}

fn agent_config(launch_cmd: String, heartbeat_dir: &Path, max_workers: u32) -> AgentConfig {
    serde_json::from_value(serde_json::json!({
        "launch_cmd": launch_cmd,
        "session_pattern": format!("{PREFIX}-*"),
        "heartbeat_dir": heartbeat_dir.to_string_lossy(),
        "min_workers": 0,
        "max_workers": max_workers,
        "subscription": false,
    }))
    .expect("agent config fixture should deserialize")
}

fn write_heartbeat(hb_dir: &Path, session: &str, age_secs: i64, is_idle: bool) {
    std::fs::create_dir_all(hb_dir).expect("heartbeat dir");
    let heartbeat = serde_json::json!({
        "session": session,
        "timestamp": (Utc::now() - ChronoDuration::seconds(age_secs)).to_rfc3339(),
        "is_idle": is_idle,
        "current_task": if is_idle { None } else { Some(format!("task-{session}")) },
        "model": "claude-sonnet",
    });
    std::fs::write(
        hb_dir.join(format!("{session}.json")),
        heartbeat.to_string(),
    )
    .expect("write heartbeat");
}

fn seed_worker(sessions_file: &Path, hb_dir: &Path, name: &str, age_secs: i64, is_idle: bool) {
    use std::io::Write;
    let session = format!("{PREFIX}-{name}");
    let mut sessions = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sessions_file)
        .expect("open sessions file");
    writeln!(sessions, "{session}").expect("append session");
    write_heartbeat(hb_dir, &session, age_secs, is_idle);
}

/// Narrow-cone five_hour binding window; the other two windows never bind.
fn seeded_state(binding_utilization: f64, binding_safe: Option<u32>) -> state::GovernorState {
    fn seed(win: &mut state::WindowForecast, utilization: f64, safe: Option<u32>) {
        win.current_utilization = utilization;
        win.hours_remaining = 40.0;
        win.cutoff_risk = false;
        win.safe_worker_count = safe;
        win.safe_worker_count_p75 = safe;
        win.cone_ratio = 0.0; // narrow cone → p50 estimate selected
    }

    let mut s = state::GovernorState::new();
    s.capacity_forecast.binding_window = "five_hour".to_string();
    seed(
        &mut s.capacity_forecast.five_hour,
        binding_utilization,
        binding_safe,
    );
    seed(&mut s.capacity_forecast.seven_day, 50.0, None);
    seed(&mut s.capacity_forecast.weekly_scoped, 45.0, None);
    s
}

fn governor_config() -> GovernorConfig {
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
        agents: Default::default(),
        credentials_path: None,
    }
}

fn single_agent(launch_cmd: String, heartbeat_dir: &Path) -> HashMap<String, AgentConfig> {
    let mut agents = HashMap::new();
    agents.insert(
        "pool".to_string(),
        agent_config(launch_cmd, heartbeat_dir, 10),
    );
    agents
}

struct Harness {
    _env: Option<TempDir>,
    state_path: PathBuf,
    sessions_file: PathBuf,
    calls_log: PathBuf,
    launch_log: PathBuf,
    hb_dir: PathBuf,
}

fn harness() -> Harness {
    init_logger();
    let env = TempDir::new().expect("temp env dir");
    let bin_dir = env.path().join("bin");
    std::fs::create_dir(&bin_dir).expect("bin dir");

    let sessions_file = env.path().join("sessions.txt");
    std::fs::write(&sessions_file, "").expect("empty sessions file");
    let calls_log = env.path().join("tmux-calls.log");
    std::fs::write(&calls_log, "").expect("empty calls log");
    let stopped_file = env.path().join("stopped.txt");
    std::fs::write(&stopped_file, "").expect("empty stopped file");
    let launch_log = env.path().join("launches.log");
    std::fs::write(&launch_log, "").expect("empty launch log");

    install_fake_tmux(&bin_dir, &sessions_file, &calls_log, &stopped_file);
    install_quiet_bf(&bin_dir);
    install_launch_stub(&bin_dir);

    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    std::env::set_var("CGOV_DECISIONS_PATH", env.path().join("decisions.jsonl"));

    let state_path = env.path().join("governor-state.json");
    let hb_dir = env.path().join("heartbeats");

    let keep_env = std::env::var("CGOIDLE_KEEP_ENV").is_ok();
    if keep_env {
        eprintln!("CGOIDLE env dir: {}", env.path().display());
    }
    let env = if keep_env {
        std::mem::forget(env);
        None
    } else {
        Some(env)
    };

    Harness {
        _env: env,
        state_path,
        sessions_file,
        calls_log,
        launch_log,
        hb_dir,
    }
}

impl Harness {
    fn launch_cmd(&self) -> String {
        let env = self._env.as_ref().expect("harness env dir");
        launch_cmd_for(&env.path().join("bin"), env.path(), &self.launch_log)
    }

    fn worker(&self, name: &str, age_secs: i64, is_idle: bool) {
        seed_worker(&self.sessions_file, &self.hb_dir, name, age_secs, is_idle);
    }

    /// Write `state` to the harness state file as the cycle's input.
    fn seed_state(&self, state: &state::GovernorState) {
        std::fs::write(
            &self.state_path,
            serde_json::to_string_pretty(state).expect("serialize state"),
        )
        .expect("write state fixture");
    }
}

/// Sessions touched by `verb` (`send-keys` = graceful SIGINT, `kill-session`
/// = brake), parsed from the fake tmux calls log. Sorted for comparison.
fn sessions_from_calls(log: &Path, verb: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.first() == Some(&verb) && fields.get(1) == Some(&"-t") {
                fields.get(2).map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

fn signalled(log: &Path) -> Vec<String> {
    sessions_from_calls(log, "send-keys")
}

fn killed(log: &Path) -> Vec<String> {
    sessions_from_calls(log, "kill-session")
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

/// Run a real act cycle against the harness state with the given policy
/// knobs. Caller holds [`TEST_ENV_LOCK`]; `harness()` already activated the
/// fakes.
fn act_cycle(
    h: &Harness,
    agents: &HashMap<String, AgentConfig>,
    hysteresis: f64,
    max_up: u32,
    max_down: u32,
) -> ScalingDecision {
    run_act_cycle(
        &h.state_path,
        false, // dry_run — the executor and its log lines only exist on the real path
        hysteresis,
        max_up,
        max_down,
        90.0, // target ceiling
        &AlertConfig {
            enabled: false,
            ..Default::default()
        },
        agents,
        0, // pre_scale_minutes (disabled)
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &governor_config(),
        Utc::now(),
    )
    .expect("act cycle should run")
}

// ---------------------------------------------------------------------------
// 1. Graceful scale-down: the log stream alone shows idle-only removal
// ---------------------------------------------------------------------------

/// Five workers run — three idle, two busy with the OLDER heartbeats — and
/// the cut is two. The log stream an operator tails must carry the whole
/// idle-only story by itself: the graceful summary (`gracefully scaling down
/// by 2 workers`), the per-pool line with `graceful=2, forced=0` (nothing was
/// force-killed, so no active worker was interrupted), the fleet total
/// (`2 graceful, 0 force-killed`), the worker-level SIGINT and
/// graceful-completion lines, the reconcile summary naming the pool stop —
/// and, negatively, no `force-killing session` line and no record naming
/// either busy worker at all. The tmux calls log cross-checks WHO was
/// signalled, since the log stream records counts rather than names.
#[test]
fn graceful_scale_down_log_stream_shows_idle_only_removal_with_actives_untouched() {
    let _guard = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("idle-old", 5, true);
    h.worker("idle-new", 4, true);
    h.worker("idle-youngest", 3, true);
    // Busy workers carry the older heartbeats: the age-first trap.
    h.worker("busy-a", 50, false);
    h.worker("busy-b", 49, false);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    let state = seeded_state(65.0, Some(3));
    h.seed_state(&state);

    let marker = record_count();
    let decision = act_cycle(&h, &agents, 1.0, 10, 10);
    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(2),
        "a surplus of 2 beyond the band sheds exactly 2"
    );

    // The decision layer names the graceful move…
    let (level, _) = one_record(marker, "gracefully scaling down by 2 workers");
    assert_eq!(level, log::Level::Info);
    // …the executor layer proves the cut landed entirely on graceful removals.
    let (_, line) = one_record(
        marker,
        "scaled down pool agent: 5 -> 3 workers (removed: 2, graceful=2, forced=0)",
    );
    assert!(
        line.contains("forced=0"),
        "forced=0 is the log-level statement that no active worker was interrupted"
    );
    one_record(marker, "total scaled down: 2 graceful, 0 force-killed");
    // The worker layer names the mechanics: SIGINT delivered, all targeted
    // workers exited before the graceful timeout.
    one_record(marker, "sent SIGINT to 2/2 workers");
    one_record(marker, "workers shut down gracefully");
    // The reconcile summary is the line CLAUDE.md §3 triage greps for.
    let (_, reconcile) = one_record(marker, "reconcile: decision=ScaleDown(2)");
    assert!(
        reconcile.contains("fleet 5 -> 3") && reconcile.contains("stopped 2"),
        "the reconcile line must carry the fleet move: {reconcile}"
    );
    assert!(
        reconcile.contains("pool stop 2 (5 -> 3)"),
        "the reconcile line must name the pool's stop: {reconcile}"
    );

    // Negative half: an interrupted active worker would surface as a
    // force-kill or by name. Neither appears anywhere in the stream.
    assert!(
        records_since(marker, "force-killing session").is_empty(),
        "a fully idle-covered cut never escalates to a force-kill"
    );
    for busy in ["cgscale-busy-a", "cgscale-busy-b"] {
        assert!(
            records_since(marker, busy).is_empty(),
            "no log record may name active worker {busy} — it was never touched"
        );
    }

    // WHO was signalled is recorded by tmux, not the log stream: exactly the
    // two OLDEST idle workers; the youngest idle survives; no kill-session.
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgscale-idle-old", "cgscale-idle-new"]),
        "the idle tier absorbs the cut, oldest first; actives keep running"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "a graceful scale-down never kill-sessions"
    );

    // The decision audit log carries the same story for `cgov explain`.
    let decisions_path =
        PathBuf::from(std::env::var("CGOV_DECISIONS_PATH").expect("decisions env"));
    let decisions = read_last_decisions_from_path(1, &decisions_path)
        .expect("read scale-down decision audit log");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].action, ScaleAction::ScaleDown);
    assert_eq!((decisions[0].from, decisions[0].to), (5, 3));
}

// ---------------------------------------------------------------------------
// 2. Emergency brake: the log stream alone shows the bypass of idle selection
// ---------------------------------------------------------------------------

/// The same mixed fleet, with a window AT the 98% threshold. The documented
/// brake (plan.md §9) kills every worker regardless of idle state, and every
/// layer of the log stream must say so: the decision line names the window
/// and its percentage, the executor names `source=emergency_brake`, the
/// killed count covers the whole fleet, the reconcile summary lands the fleet
/// at 0 — and the graceful path is absent (no SIGINT, no force-kill line).
/// State zeroes and safe mode engages with the brake trigger.
#[test]
fn emergency_brake_log_stream_shows_idle_selection_bypassed() {
    let _guard = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("idle-a", 5, true);
    h.worker("idle-b", 4, true);
    h.worker("busy-a", 50, false);
    h.worker("busy-b", 49, false);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    let mut state = seeded_state(65.0, Some(2));
    state.capacity_forecast.five_hour.current_utilization = 98.0;
    h.seed_state(&state);

    let marker = record_count();
    let decision = act_cycle(&h, &agents, 50.0, 0, 0);
    assert_eq!(decision, ScalingDecision::EmergencyBrake);

    // Decision layer: the brake names the breaching window and percentage.
    let (level, line) = one_record(marker, "EMERGENCY BRAKE: five_hour at 98.0% >= 98%");
    assert_eq!(level, log::Level::Warn, "the brake is a warning: {line}");
    // Executor layer: the brake names its source and its bypass of idle
    // selection — every session goes, straight to kill-session.
    let (_, line) = one_record(
        marker,
        "EMERGENCY BRAKE (source=emergency_brake): scaling all to 0",
    );
    assert!(line.contains("scaling all to 0"));
    one_record(marker, "killed 4 worker sessions");
    // Reconcile summary: the fleet lands at 0 through the pool stop.
    let (_, reconcile) = one_record(marker, "reconcile: decision=EmergencyBrake");
    assert!(
        reconcile.contains("fleet 4 -> 0") && reconcile.contains("stopped 4"),
        "the reconcile line must carry the brake's fleet move: {reconcile}"
    );
    assert!(
        reconcile.contains("pool stop 4 (4 -> 0)"),
        "the reconcile line must name the pool's stop: {reconcile}"
    );

    // Negative half: the brake does not travel the graceful path at all.
    assert!(
        records_since(marker, "sent SIGINT").is_empty(),
        "the brake kill-sessions directly; it never takes the graceful SIGINT path"
    );
    assert!(
        records_since(marker, "force-killing session").is_empty(),
        "brake kills are direct kill-sessions, not the graceful escalation"
    );
    assert!(
        records_since(marker, "gracefully scaling down").is_empty(),
        "the brake is not a graceful scale-down and must not be logged as one"
    );

    // tmux records WHO died: all four sessions, idle AND active — the
    // documented bypass of idle-only selection.
    assert_eq!(
        killed(&h.calls_log),
        sorted(&[
            "cgscale-idle-a",
            "cgscale-idle-b",
            "cgscale-busy-a",
            "cgscale-busy-b"
        ]),
        "the brake kills every session — idle selection does not apply to it"
    );
    assert!(
        signalled(&h.calls_log).is_empty(),
        "no graceful SIGINT was sent to anyone"
    );

    // State zeroes and safe mode engages so the next cycle cannot instantly
    // scale back up.
    let after = state::load_state(&h.state_path).expect("reload state");
    let pool = after.workers.get("pool").expect("pool tracked in state");
    assert_eq!(pool.current, 0, "the brake zeroes the pool's live count");
    assert_eq!(pool.target, 0, "the brake zeroes the pool's target");
    assert!(
        after.safe_mode.active,
        "the brake engages safe mode against an instant scale-back"
    );
    assert_eq!(
        after.safe_mode.trigger.as_deref(),
        Some("emergency_brake"),
        "safe mode records what engaged it"
    );

    // The decision audit log carries the engagement for `cgov explain`.
    let decisions_path =
        PathBuf::from(std::env::var("CGOV_DECISIONS_PATH").expect("decisions env"));
    let decisions =
        read_last_decisions_from_path(1, &decisions_path).expect("read brake decision audit log");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].action, ScaleAction::EmergencyBrakeEngage);
    assert_eq!((decisions[0].from, decisions[0].to), (4, 0));
    assert!(
        decisions[0].reason.contains("EMERGENCY BRAKE ENGAGED"),
        "the audit reason names the brake engagement: {}",
        decisions[0].reason
    );
}
