//! Regression tests for graceful idle-only scale-down and asymmetric
//! hysteresis (claudego-00024dfd).
//!
//! Five properties, each named in the bead description. The worker-selection
//! properties are pinned end-to-end through `run_act_cycle` with
//! `dry_run = false`, so the assertions cover the full chain — decision,
//! cost-priority distribution, and the executor's SIGINTs — rather than only
//! the pure decision function. tmux resolves to a fake that serves a fixed
//! `list-sessions` census, records every call, and marks signalled sessions
//! dead for `has-session`, so a signalled worker shuts down gracefully and
//! the calls log is the observable record of exactly who was touched.
//!
//! 1. **Scale-down sheds idle workers only** — with enough idle capacity to
//!    cover the cut, no active worker is ever signalled, even when the active
//!    workers' heartbeats are the older ones (an age-first sort would
//!    interrupt them; idle status must dominate age).
//! 2. **A stale heartbeat is a working worker** — a live session whose stale
//!    heartbeat still claims `is_idle: true` is treated as executing and left
//!    running while a fresh idle worker absorbs the cut.
//! 3. **Per-cycle caps bound every move** — a 3-worker shed with
//!    `max_down_per_cycle = 2` signals exactly the two oldest idle workers
//!    and leaves the rest for the next cycle. The next cycle is pinned too:
//!    it sheds the next-oldest survivors, again within the cap, and never
//!    re-signals a worker it already stopped.
//! 4. **Hysteresis is asymmetric** — a 1-worker surplus holds at any band
//!    while the same-size deficit always closes, and a hold touches no
//!    worker at all.
//! 5. **The emergency brake overrides everything** — any window at 98% kills
//!    every session, idle and active, with the band at 50 and
//!    `max_down_per_cycle = 0`: neither the idle-first selection, the band,
//!    nor the caps can hold the brake back.
//!
//! The complement of 1 is pinned too: when the idle pool cannot cover the
//! cut, active workers are shed last and oldest-heartbeat-first, so the
//! youngest active worker survives longest.
//!
//! Section 8 pins the ledger shed order (claudego-f80857a2) on the LIVE
//! path (claudego-ea621a6d): with ledger evidence present the exhaustion
//! shed consumes the worst verified-closure yield per dollar first; with the
//! ledger absent — a silent directory or a failed read — every pool ranks
//! Unknown and the shed is byte-for-byte the pre-ledger cost-per-hour
//! order; and whatever the pool-level order says, a pool's own shed still
//! takes its idle workers before its busy ones.
//!
//! Section 9 pins the reclaim path's failure and timeout edges
//! (claudego-b24eb184): a worker that ignores the graceful SIGINT is
//! force-killed only after `graceful_timeout_secs` — busy workers still never
//! touched; a failed SIGINT escalates to the same force-kill; a failed
//! force-kill is reported in `ScaleDownResult` rather than crashing or
//! hanging; a tmux that cannot answer sheds nobody and sweeps nothing; and
//! the accounting (signaled / graceful / force_killed) adds up for a mixed
//! outcome. Section 10 pins the role dimension of repeated scaling cycles:
//! busy workers spared by one cycle are legitimate candidates the moment
//! their own heartbeat flips idle, across a down → up → down sequence.
//!
//! Every test that swaps PATH / CGOV_DECISIONS_PATH holds [`ENV_LOCK`] for
//! its whole body (tests in one binary share a process and run in threads).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{Duration as ChronoDuration, Utc};
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{apply_scaling, run_act_cycle, ScalingDecision};
use claude_governor::narrator::{read_last_decisions_from_path, ScaleAction};
use claude_governor::state;
use claude_governor::worker::{scale_down_graceful, WorkerConfig};

/// Serializes every test that swaps PATH / CGOV_DECISIONS_PATH.
static ENV_LOCK: Mutex<()> = Mutex::new(());
/// The PATH this process was launched with, captured before any test swaps it.
static ORIG_PATH: OnceLock<String> = OnceLock::new();

// ---------------------------------------------------------------------------
// Hermetic fixtures
// ---------------------------------------------------------------------------

/// Session prefix: the pattern `cgidle-*` trims to this, and every fixture
/// session name starts with it so the fake tmux census counts them.
const PREFIX: &str = "cgidle";

fn write_executable(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{}\n", body)).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod script");
}

/// `tmux` stands in for the live fleet:
///
/// - `list-sessions` prints the sessions file (the census);
/// - `send-keys -t <s> C-c` (the graceful SIGINT) records `<s>` in the
///   stopped file, so the worker shuts down and later `has-session` probes
///   report it gone;
/// - `kill-session -t <s>` (the emergency brake) records `<s>` the same way;
/// - `has-session -t <s>` succeeds only for a session that is live
///   (listed) and not already stopped;
/// - every call is appended to the calls log, which is what the assertions
///   read to prove exactly which sessions were touched.
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

/// `bf ready` prints nothing: no backlog anywhere, so the underutilization
/// sprint can never fire and inflate a target behind the test's back.
fn install_quiet_bf(bin_dir: &Path) {
    write_executable(bin_dir, "bf", "exit 0");
}

/// Launch stub: appends the tag argument to the launch log so tests can
/// prove no worker was launched where none was expected.
fn install_launch_stub(bin_dir: &Path) {
    write_executable(bin_dir, "launch-stub", "echo \"$3\" >> \"$4\"");
}

fn launch_cmd_for(bin_dir: &Path, env: &Path, log: &Path) -> String {
    format!(
        "{} --workspace {} idle-scaled {}",
        bin_dir.join("launch-stub").display(),
        env.display(),
        log.display()
    )
}

/// An agent config built by deserialization so this file compiles against
/// trees that add optional `AgentConfig` fields (serde tolerates both).
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

/// Heartbeat `age_secs` old for `session`; idle workers carry no task, busy
/// ones name one (mirroring what a working worker writes).
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

/// Register a worker: live tmux session + heartbeat. The tmux session and the
/// heartbeat's `session` field both carry the full `cgidle-<name>` name — the
/// census counts only sessions matching the agent's `cgidle-*` pattern, so a
/// bare name would be invisible to the cycle.
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

/// Narrow-cone five_hour binding window selecting the p50 safe count; the
/// other two windows are roomier and can never bind over it.
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

fn disabled_alerts() -> AlertConfig {
    AlertConfig {
        enabled: false,
        ..Default::default()
    }
}

fn governor_config() -> GovernorConfig {
    GovernorConfig {
        pricing: PricingConfig {
            models: HashMap::new(),
        },
        sprint: Default::default(),
        daemon: Default::default(),
        alerts: disabled_alerts(),
        composite_risk: Default::default(),
        cone_scaling: Default::default(),
        agents: Default::default(),
        credentials_path: None,
    }
}

/// One-agent fleet map over the fixtures built in [`harness`].
fn single_agent(launch_cmd: String, heartbeat_dir: &Path) -> HashMap<String, AgentConfig> {
    let mut agents = HashMap::new();
    agents.insert(
        "pool".to_string(),
        agent_config(launch_cmd, heartbeat_dir, 10),
    );
    agents
}

/// Everything one test needs: an env dir with the fakes on PATH, the state
/// file, and the log paths. Created under the env lock (the caller holds it).
struct Harness {
    /// `None` when `CGOIDLE_KEEP_ENV` leaked the TempDir for debugging — the
    /// env dir is then expected to outlive the test on purpose.
    _env: Option<TempDir>,
    state_path: PathBuf,
    sessions_file: PathBuf,
    calls_log: PathBuf,
    launch_log: PathBuf,
    hb_dir: PathBuf,
}

fn harness() -> Harness {
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

    let orig = ORIG_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), orig));
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

    /// Simulate the named workers having exited: drop their sessions from the
    /// fake tmux census the way a real tmux server drops a session whose
    /// process ended, so the NEXT cycle counts — and considers as shutdown
    /// candidates — only the survivors. Their heartbeat files stay on disk,
    /// as they do in production until the orphan sweep collects them.
    fn reap(&self, sessions: &[&str]) {
        let census = std::fs::read_to_string(&self.sessions_file).expect("read sessions file");
        let survivors: Vec<&str> = census.lines().filter(|s| !sessions.contains(s)).collect();
        let mut body = survivors.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        std::fs::write(&self.sessions_file, body).expect("rewrite sessions file");
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
/// knobs. Caller holds [`ENV_LOCK`]; `harness()` already activated the fakes.
#[allow(clippy::too_many_arguments)]
fn act_cycle(
    h: &Harness,
    agents: &HashMap<String, AgentConfig>,
    hysteresis: f64,
    max_up: u32,
    max_down: u32,
) -> ScalingDecision {
    run_act_cycle(
        &h.state_path,
        false, // dry_run — the executor only exists on the real path
        hysteresis,
        max_up,
        max_down,
        90.0, // target ceiling
        &disabled_alerts(),
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

/// Seed the harness state file with a binding window targeting `safe`
/// workers, then run a real act cycle.
fn cycle_with_target(
    h: &Harness,
    agents: &HashMap<String, AgentConfig>,
    safe: u32,
    hysteresis: f64,
    max_up: u32,
    max_down: u32,
) -> ScalingDecision {
    let state = seeded_state(65.0, Some(safe));
    std::fs::write(
        &h.state_path,
        serde_json::to_string_pretty(&state).expect("serialize state"),
    )
    .expect("write state fixture");
    act_cycle(h, agents, hysteresis, max_up, max_down)
}

// ---------------------------------------------------------------------------
// 1. Scale-down sheds idle workers only
// ---------------------------------------------------------------------------

/// Four workers run; the cut is two. The two idle workers absorb it and both
/// busy workers keep running — even though the busy workers' heartbeats are
/// the OLDER ones. An age-first candidate sort would signal a busy worker
/// here; idle status must dominate heartbeat age.
#[test]
fn scale_down_signals_only_idle_workers_regression() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("idle-a", 5, true);
    h.worker("idle-b", 4, true);
    // Busy workers carry the older heartbeats: the trap for an age-first sort.
    h.worker("busy-a", 50, false);
    h.worker("busy-b", 49, false);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    let decision = cycle_with_target(&h, &agents, 2, 1.0, 10, 10);

    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(2),
        "surplus of 2 beyond band 1 must shed exactly 2"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-idle-a", "cgidle-idle-b"]),
        "exactly the idle workers absorb the cut"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "a graceful scale-down never kill-sessions"
    );
}

// ---------------------------------------------------------------------------
// 2. A stale heartbeat is a working worker
// ---------------------------------------------------------------------------

/// A live session whose stale heartbeat still claims `is_idle: true` must be
/// treated as executing: the freshness cutoff exists because an outdated idle
/// status is exactly what must not drive a shutdown decision. Shedding one of
/// two workers therefore hits the FRESH idle worker, never the stale one —
/// even though the stale heartbeat is the older one and claims to be idle.
#[test]
fn stale_heartbeat_live_worker_is_treated_as_executing_regression() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    // Claims idle, but 5 minutes without a heartbeat: unknown, not idle.
    h.worker("stale", 300, true);
    h.worker("fresh", 5, true);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    let decision = cycle_with_target(&h, &agents, 1, 0.0, 10, 10);

    assert_eq!(decision, ScalingDecision::ScaleDown(1));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-fresh"]),
        "the stale-heartbeat session is treated as executing; the fresh idle worker absorbs the cut"
    );
}

// ---------------------------------------------------------------------------
// 3. Per-cycle caps bound every move
// ---------------------------------------------------------------------------

/// Four idle workers run; the target sheds three but `max_down_per_cycle` is
/// two. The decision is capped at 2, and the executor signals exactly the two
/// OLDEST idle workers (heartbeat age orders candidates within the idle
/// tier), leaving the younger pair for the next cycle — the cap bounds the
/// move without changing who is shed first.
#[test]
fn scale_down_honors_per_cycle_cap_regression() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("old", 40, true);
    h.worker("older-still", 30, true);
    h.worker("young", 5, true);
    h.worker("youngest", 4, true);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    let decision = cycle_with_target(&h, &agents, 1, 0.5, 10, 2);

    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(2),
        "a wanted shed of 3 is capped at max_down_per_cycle = 2"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-old", "cgidle-older-still"]),
        "the cap signals only the oldest idle workers; the rest wait for the next cycle"
    );
}

/// The cap binds on EVERY cycle, not just the first. Five idle workers, two
/// cuts: the first cycle sheds the two oldest (wanted 3, cap 2), the signalled
/// workers exit, and the next cycle — facing the three survivors with the
/// target one lower still — sheds the next-oldest pair, again capped at 2.
/// The continuation proves what a first-cycle test cannot: the next cycle
/// resumes from the RIGHT candidates (the ones the cap left running, oldest
/// first, and never re-signals a worker it already stopped), and the cap
/// bounds that cycle too — "no cycle ever removes more than the cap allows".
#[test]
fn scale_down_honors_per_cycle_cap_on_every_consecutive_cycle_regression() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("ancient", 40, true);
    h.worker("old", 30, true);
    h.worker("middle", 20, true);
    h.worker("young", 5, true);
    h.worker("youngest", 4, true);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);

    // Cycle one: five idle workers, safe count 2 — a wanted shed of 3, capped
    // at 2, landing on the two oldest.
    let first = cycle_with_target(&h, &agents, 2, 0.5, 10, 2);
    assert_eq!(
        first,
        ScalingDecision::ScaleDown(2),
        "a wanted shed of 3 is capped at max_down_per_cycle = 2 in the first cycle"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-ancient", "cgidle-old"]),
        "cycle one sheds exactly the two oldest idle workers"
    );

    // The two signalled workers exit; their tmux sessions end.
    h.reap(&["cgidle-ancient", "cgidle-old"]);

    // Cycle two: three survivors, safe count 1 — a wanted shed of 2, again at
    // the cap. It must fall on the next-oldest live workers, not restart from
    // the top of the departed age order, and not exceed the cap either way.
    let second = cycle_with_target(&h, &agents, 1, 0.5, 10, 2);
    assert_eq!(
        second,
        ScalingDecision::ScaleDown(2),
        "the next cycle sheds the remainder, again capped at max_down_per_cycle = 2"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&[
            "cgidle-ancient",
            "cgidle-old",
            "cgidle-middle",
            "cgidle-young"
        ]),
        "cycle two takes the next-oldest live workers: no session is signalled twice \
         and the youngest survives for the cycle after"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "a capped graceful scale-down never kill-sessions, in any cycle"
    );
}

/// A mixed fleet must scale down gracefully across cycles: the first cut is
/// capped even though more workers are wanted, and both cuts choose idle
/// workers while active workers remain live. The persisted target and audit
/// entries make the governor's bounded decision observable beyond tmux calls.
#[test]
fn mixed_active_and_idle_scale_down_is_capped_idle_only_and_recorded() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    // Make active workers older than the idle workers so an age-only sort would
    // interrupt work. The idle tier must win before heartbeat age is compared.
    h.worker("idle-old", 40, true);
    h.worker("idle-new", 10, true);
    h.worker("idle-youngest", 5, true);
    h.worker("active-old", 50, false);
    h.worker("active-young", 49, false);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);

    // The target is two, so three workers must eventually leave. The first
    // cycle is capped at two and removes only the two oldest idle workers.
    let first = cycle_with_target(&h, &agents, 2, 0.0, 10, 2);
    assert_eq!(first, ScalingDecision::ScaleDown(2));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-idle-old", "cgidle-idle-new"]),
        "the first cycle honors the cap and signals idle workers only"
    );

    // Let the signalled sessions exit. The second cycle still has one idle and
    // both active workers; it should remove the last idle worker, not either
    // active session, and remain within the same per-cycle cap.
    h.reap(&["cgidle-idle-old", "cgidle-idle-new"]);
    let second = cycle_with_target(&h, &agents, 2, 0.0, 10, 2);
    assert_eq!(second, ScalingDecision::ScaleDown(1));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&[
            "cgidle-idle-old",
            "cgidle-idle-new",
            "cgidle-idle-youngest"
        ]),
        "both cycles remove only idle workers and never signal active workers"
    );
    assert!(killed(&h.calls_log).is_empty());

    // State exposes the live census taken at the start of the second cycle
    // and the bounded target selected for it: three workers entered the cycle
    // (two active plus one idle), and the target was two.
    let after = state::load_state(&h.state_path).expect("reload persisted state");
    let pool = after.workers.get("pool").expect("pool tracked in state");
    assert_eq!(
        pool.current, 3,
        "the second cycle census saw three live sessions"
    );
    assert_eq!(pool.target, 2, "state records the bounded scale-down target");

    // The decision log records both bounded requests and their actual graceful
    // outcomes, making the idle-only scale-down auditable by cgov explain.
    let decisions_path = h.state_path.parent().unwrap().join("decisions.jsonl");
    let decisions = read_last_decisions_from_path(2, &decisions_path)
        .expect("read scale-down decision audit log");
    assert_eq!(decisions.len(), 2);
    assert!(
        decisions
            .iter()
            .all(|entry| entry.action == ScaleAction::ScaleDown)
    );
    assert_eq!((decisions[0].from, decisions[0].to), (3, 2));
    assert_eq!((decisions[1].from, decisions[1].to), (5, 3));
    assert_eq!(
        decisions[0].context.as_ref().unwrap()["actual_removed"],
        serde_json::json!(1)
    );
    assert_eq!(
        decisions[1].context.as_ref().unwrap()["actual_removed"],
        serde_json::json!(2)
    );
    for entry in decisions {
        assert_eq!(
            entry.context.as_ref().unwrap()["max_down_per_cycle"],
            serde_json::json!(2)
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Hysteresis is asymmetric — and a hold touches no one
// ---------------------------------------------------------------------------

/// The mirror property: the SAME 1-worker delta gets opposite treatment by
/// direction. A 1-worker surplus is within every band's cushion and holds; a
/// 1-worker deficit closes immediately at every band. A symmetric band would
/// either hold the deficit (stranding the fleet below target) or shed on the
/// surplus (churn).
#[test]
fn hysteresis_is_asymmetric_one_worker_mirror_regression() {
    for band in [1.0f64, 2.0, 5.0, 10.0] {
        for current in [3u32, 5, 9] {
            let down = apply_scaling(current - 1, current, band, 10, 10, false);
            assert_eq!(
                down,
                ScalingDecision::NoChange,
                "1-worker surplus at current {} under band {} holds (the down-side cushion)",
                current,
                band
            );
            let up = apply_scaling(current + 1, current, band, 10, 10, false);
            assert_eq!(
                up,
                ScalingDecision::ScaleUp(1),
                "the same-size deficit at current {} under band {} closes immediately",
                current,
                band
            );
        }
    }
}

/// End-to-end complement: a surplus inside the band is a NoChange cycle and
/// touches no worker at all — no SIGINTs, no kills, no launches. A hold that
/// fidgeted with the fleet would interrupt active tasks just as surely as a
/// bad scale-down would.
#[test]
fn hysteresis_hold_touches_no_worker_regression() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("idle-a", 5, true);
    h.worker("idle-b", 4, true);
    h.worker("busy-a", 20, false);
    h.worker("busy-b", 19, false);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    // Target 3 vs current 4: a 1-worker surplus, inside band 2.0.
    let decision = cycle_with_target(&h, &agents, 3, 2.0, 10, 10);

    assert_eq!(
        decision,
        ScalingDecision::NoChange,
        "a surplus within the band holds"
    );
    assert!(
        signalled(&h.calls_log).is_empty(),
        "a hold never signals a worker"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "a hold never kills a worker"
    );
    assert!(
        std::fs::read_to_string(&h.launch_log)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "a hold launches nothing"
    );
}

// ---------------------------------------------------------------------------
// 5. The emergency brake overrides everything
// ---------------------------------------------------------------------------

/// The binding window slams into the 98% brake threshold with four workers
/// running — two of them mid-task. The brake kills every session, idle and
/// active: it does not walk the idle-first graceful selection. And it fires
/// with the band at 50 and `max_down_per_cycle = 0` — neither the cushion
/// nor the caps can hold it back (a scale-down that had to respect either
/// could never reach zero).
#[test]
fn emergency_brake_kills_all_workers_bypassing_idle_selection_and_caps_regression() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("idle-a", 5, true);
    h.worker("idle-b", 4, true);
    h.worker("busy-a", 20, false);
    h.worker("busy-b", 19, false);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);

    // State with the binding window AT the brake threshold.
    let mut state = seeded_state(65.0, Some(2));
    state.capacity_forecast.five_hour.current_utilization = 98.0;
    std::fs::write(
        &h.state_path,
        serde_json::to_string_pretty(&state).expect("serialize state"),
    )
    .expect("write state fixture");

    // Band 50 would cushion any normal scale-down; a 0 down-cap would block
    // one entirely. The brake is decided before either applies.
    let decision = act_cycle(&h, &agents, 50.0, 0, 0);

    assert_eq!(decision, ScalingDecision::EmergencyBrake);
    assert_eq!(
        killed(&h.calls_log),
        sorted(&[
            "cgidle-idle-a",
            "cgidle-idle-b",
            "cgidle-busy-a",
            "cgidle-busy-b"
        ]),
        "the brake kills every session — idle selection does not apply to it"
    );
    assert!(
        signalled(&h.calls_log).is_empty(),
        "the brake kill-sessions directly; it does not take the graceful SIGINT path"
    );

    // The persisted state records the stop and engages safe mode.
    let after = state::load_state(&h.state_path).expect("reload state");
    let pool = after.workers.get("pool").expect("pool tracked in state");
    assert_eq!(pool.current, 0, "the brake zeroes the pool's live count");
    assert_eq!(pool.target, 0, "the brake zeroes the pool's target");
    assert!(
        after.safe_mode.active,
        "the brake engages safe mode so the next cycle does not instantly scale back up"
    );
}

// ---------------------------------------------------------------------------
// 6. Complement: the idle pool exhausted — actives shed last, oldest first
// ---------------------------------------------------------------------------

/// Three workers run, only one idle; the cut is two. The idle worker goes
/// first, then the OLDER of the two active workers; the youngest active
/// worker survives. Active workers are a last resort, taken in heartbeat-age
/// order — the documented graceful-degradation contract for the case where
/// idle capacity cannot cover the cut.
#[test]
fn scale_down_sheds_actives_last_and_oldest_first_when_idle_pool_exhausted_regression() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("act-young", 4, false);
    h.worker("act-old", 40, false);
    h.worker("idle-mid", 20, true);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    let decision = cycle_with_target(&h, &agents, 1, 0.0, 10, 10);

    assert_eq!(decision, ScalingDecision::ScaleDown(2));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-idle-mid", "cgidle-act-old"]),
        "idle first, then the oldest active; the youngest active worker survives"
    );
}

// ---------------------------------------------------------------------------
// 7. Window-affinity shed — a pool above its own ceiling sheds on a hold
//    cycle, paced (claudego-ad125e03)
// ---------------------------------------------------------------------------

/// An agent config for the two-pool matrix: explicit session pattern, window
/// affinity declaration, and a launch_cmd whose `--agent` name keys the
/// per-model burn rate that makes its cost deterministic.
fn affinity_agent_config(
    launch_cmd: String,
    session_pattern: &str,
    heartbeat_dir: &Path,
    max_workers: u32,
    windows: &[&str],
) -> AgentConfig {
    serde_json::from_value(serde_json::json!({
        "launch_cmd": launch_cmd,
        "session_pattern": session_pattern,
        "heartbeat_dir": heartbeat_dir.to_string_lossy(),
        "min_workers": 0,
        "max_workers": max_workers,
        "subscription": false,
        "windows": windows,
    }))
    .expect("agent config fixture should deserialize")
}

/// Register a worker for a named pool: the full session name goes into the
/// shared fake-tmux census and its heartbeat into that pool's heartbeat dir,
/// so each pool's census (pattern-filtered) counts only its own workers.
fn seed_pool_worker(
    sessions_file: &Path,
    hb_dir: &Path,
    session: &str,
    age_secs: i64,
    is_idle: bool,
) {
    use std::io::Write;
    let mut sessions = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sessions_file)
        .expect("open sessions file");
    writeln!(sessions, "{session}").expect("append session");
    write_heartbeat(hb_dir, session, age_secs, is_idle);
}

/// The affinity-shed forecast: five_hour supports 13, the binding seven_day
/// is roomy at 16, and weekly_scoped — the premium window only pool B
/// consumes — says ZERO workers are affordable. Per-model burn rates make
/// pool B the expensive pool, so the down-pass sheds it first, deterministically.
fn seeded_affinity_shed_state() -> state::GovernorState {
    fn seed(win: &mut state::WindowForecast, utilization: f64, safe: u32) {
        win.current_utilization = utilization;
        win.hours_remaining = 40.0;
        win.cutoff_risk = false;
        win.safe_worker_count = Some(safe);
        win.safe_worker_count_p75 = Some(safe);
        win.cone_ratio = 0.0; // narrow cone → p50 estimate selected
    }

    let mut s = state::GovernorState::new();
    s.capacity_forecast.binding_window = "seven_day".to_string();
    seed(&mut s.capacity_forecast.five_hour, 65.0, 13);
    seed(&mut s.capacity_forecast.seven_day, 60.0, 16);
    seed(&mut s.capacity_forecast.weekly_scoped, 45.0, 0);
    s.burn_rate.by_model.insert(
        "cgaff-a".to_string(),
        state::ModelBurnRate {
            pct_per_worker_per_hour: 5.0,
            dollars_per_worker_per_hour: 10.5,
            samples: 10,
        },
    );
    s.burn_rate.by_model.insert(
        "cgaff-b".to_string(),
        state::ModelBurnRate {
            pct_per_worker_per_hour: 10.0,
            dollars_per_worker_per_hour: 22.5,
            samples: 10,
        },
    );
    s
}

/// Re-seed the affinity-shed state, then run a real act cycle against it.
/// Caller holds [`ENV_LOCK`]; `harness()` already activated the fakes.
fn affinity_shed_cycle(h: &Harness, agents: &HashMap<String, AgentConfig>) -> ScalingDecision {
    std::fs::write(
        &h.state_path,
        serde_json::to_string_pretty(&seeded_affinity_shed_state()).expect("serialize state"),
    )
    .expect("write state fixture");
    act_cycle(h, agents, 0.5, 10, 2)
}

/// The bead's worked example, end to end (claudego-ad125e03). Pool A
/// consumes five_hour only: safe 13, running 13 — AT its ceiling, no
/// headroom. Pool B consumes all three windows: weekly_scoped says 0, so its
/// ceiling is 0 while 3 of its workers run. The binding seven_day window
/// (safe 16) equals the running total of 16 — a hold the aggregate
/// authorized no move on — and there is no pool with headroom to re-home
/// B's workers onto, so the distribution's give-back handed them straight
/// back, cycle after cycle, forever.
///
/// The aggregate now supplies the missing authorization, and the move is
/// indistinguishable from any other shed: capped at `max_down_per_cycle` = 2
/// (then 1 — not the full excess of 3 at once), landed only on the
/// over-ceiling pool, never on pool A, and CONVERGED — once the excess is
/// gone the bound returns None, the roomy binding window is free to want its
/// growth back, and the affinity caps deny it without shedding anyone.
#[test]
fn affinity_shed_sheds_an_over_ceiling_pool_on_a_hold_cycle_paced_and_converged() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    let env = h.state_path.parent().unwrap().to_path_buf();
    let hb_a = env.join("heartbeats-a");
    let hb_b = env.join("heartbeats-b");
    let stub = env.join("bin").join("launch-stub");
    // `--agent <name>` keys the burn rate seeded in the state fixture.
    let launch_a = format!(
        "{} --agent cgaff-a --workspace {} idle-scaled {}",
        stub.display(),
        env.display(),
        h.launch_log.display()
    );
    let launch_b = format!(
        "{} --agent cgaff-b --workspace {} idle-scaled {}",
        stub.display(),
        env.display(),
        h.launch_log.display()
    );

    for i in 1..=13 {
        seed_pool_worker(
            &h.sessions_file,
            &hb_a,
            &format!("cgaff-a-w{i:02}"),
            10 + i,
            true,
        );
    }
    seed_pool_worker(&h.sessions_file, &hb_b, "cgaff-b-old", 30, true);
    seed_pool_worker(&h.sessions_file, &hb_b, "cgaff-b-mid", 20, true);
    seed_pool_worker(&h.sessions_file, &hb_b, "cgaff-b-young", 5, true);

    let mut agents = HashMap::new();
    agents.insert(
        "pool-a".to_string(),
        affinity_agent_config(launch_a, "cgaff-a-*", &hb_a, 16, &["five_hour"]),
    );
    agents.insert(
        "pool-b".to_string(),
        affinity_agent_config(
            launch_b,
            "cgaff-b-*",
            &hb_b,
            4,
            &["five_hour", "seven_day", "weekly_scoped"],
        ),
    );

    // Cycle one: the hold would keep all 16 workers; the shed bound lowers
    // the target to the supported 13, and the wanted shed of 3 arrives paced
    // at max_down_per_cycle = 2, on pool B's two oldest idle workers.
    let first = affinity_shed_cycle(&h, &agents);
    assert_eq!(
        first,
        ScalingDecision::ScaleDown(2),
        "the aggregate sheds its over-ceiling pool on a hold cycle, paced at the cap"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgaff-b-old", "cgaff-b-mid"]),
        "pool B absorbs the cut; pool A at its own ceiling is never touched"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "a paced affinity shed takes the graceful path"
    );

    // Cycle two: pool B's last worker is still above a ceiling of 0. The
    // remaining excess of 1 sheds — the hold no longer protects it.
    h.reap(&["cgaff-b-old", "cgaff-b-mid"]);
    let second = affinity_shed_cycle(&h, &agents);
    assert_eq!(
        second,
        ScalingDecision::ScaleDown(1),
        "the shed resumes at the new excess, still paced like any down-move"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgaff-b-old", "cgaff-b-mid", "cgaff-b-young"]),
        "exactly pool B's three workers are shed, oldest first"
    );

    // Cycle three: the excess is gone, so the bound returns None — the shed
    // exists only to shed. The roomy binding window now wants its growth
    // back (safe 16 vs 13 running) and the decision says so, but every pool
    // is at or under its own ceiling, so the distribution refuses to grow:
    // nothing launches, nothing is signalled, pool B stays at 0.
    h.reap(&["cgaff-b-young"]);
    let third = affinity_shed_cycle(&h, &agents);
    assert!(
        matches!(third, ScalingDecision::ScaleUp(_)),
        "with the excess gone the shed stops; the binding window wants growth again"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgaff-b-old", "cgaff-b-mid", "cgaff-b-young"]),
        "the converged cycle sheds nobody new"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "the affinity caps never kill"
    );
    assert!(
        std::fs::read_to_string(&h.launch_log)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "growth past a pool's own ceiling is denied absolutely: nothing launches"
    );
}

// ---------------------------------------------------------------------------
// 8. The ledger shed order is live (claudego-ea621a6d)
// ---------------------------------------------------------------------------

/// One `attempt.resolved` ledger row, shaped the way a NEEDLE worker appends
/// it to `~/.needle/logs/*.jsonl`. The worker id and workspace must look
/// real: rows whose worker id ends in `-test-worker` or whose workspace is
/// `.` are fixture invocations by ADR-030 and every ledger consumer skips
/// them — a fixture row here would silently contribute no economics.
fn write_attempt_row(
    ledger_dir: &Path,
    worker_id: &str,
    adapter: &str,
    outcome: &str,
    cost_usd: f64,
) {
    use std::io::Write;
    std::fs::create_dir_all(ledger_dir).expect("ledger dir");
    let row = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(),
        "event_type": "attempt.resolved",
        "worker_id": worker_id,
        "session_id": "leadgen-fixture",
        "bead_id": "claudego-ea621a6d",
        "workspace": ledger_dir.parent().unwrap_or(ledger_dir).to_string_lossy(),
        "data": {
            "adapter": adapter,
            "outcome": outcome,
            "costed": true,
            "estimated_cost_usd": cost_usd,
        },
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_dir.join("attempts.jsonl"))
        .expect("open ledger file");
    writeln!(file, "{row}").expect("append ledger row");
}

/// An adapter's measured economics as distinct ledger rows: `verified` rows
/// resolving `verified_success` and `failed` rows resolving `work_failure`,
/// each costing `cost_per_row` dollars — so the adapter's cost per verified
/// closure is `cost_per_row * (verified + failed) / verified`.
fn seed_adapter_economics(
    ledger_dir: &Path,
    adapter: &str,
    verified: u64,
    failed: u64,
    cost_per_row: f64,
) {
    for i in 0..verified {
        write_attempt_row(
            ledger_dir,
            &format!("leadgen-{adapter}-{i}"),
            adapter,
            "verified_success",
            cost_per_row,
        );
    }
    for i in 0..failed {
        write_attempt_row(
            ledger_dir,
            &format!("leadgen-{adapter}-f{i}"),
            adapter,
            "work_failure",
            cost_per_row,
        );
    }
}

/// Launch command and config for one pool of the ledger fleet. The
/// `--agent` value is BOTH the burn-rate key that pins the pool's hourly
/// cost AND the ledger adapter key the shed order ranks — the same wiring
/// the production pools use.
fn ledger_pool_config(
    env: &Path,
    launch_log: &Path,
    hb_dir: &Path,
    session_prefix: &str,
    adapter: &str,
) -> AgentConfig {
    let launch_cmd = format!(
        "{} --agent {} --workspace {} idle-scaled {}",
        env.join("bin").join("launch-stub").display(),
        adapter,
        env.display(),
        launch_log.display()
    );
    affinity_agent_config(launch_cmd, &format!("{session_prefix}-*"), hb_dir, 8, &["five_hour"])
}

/// The exhaustion-shed forecast: five_hour binds at `safe`, the other two
/// windows carry no safe count (they cannot bind over it or cap a pool), and
/// per-model burn rates pin every pool's hourly cost deterministically —
/// the pre-ledger shed order is therefore known before any ledger is read.
fn seeded_ledger_shed_state(safe: u32) -> state::GovernorState {
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
    seed(&mut s.capacity_forecast.five_hour, 65.0, Some(safe));
    seed(&mut s.capacity_forecast.seven_day, 50.0, None);
    seed(&mut s.capacity_forecast.weekly_scoped, 45.0, None);
    for (adapter, dollars_per_worker_hour) in [
        ("lg-none", 20.0),
        ("lg-cheap", 5.0),
        ("lg-mid", 15.0),
        ("lg-exp", 30.0),
        ("lg-worst", 20.0),
        ("lg-guarded", 30.0),
        ("lg-busy", 20.0),
        ("lg-idle-rich", 30.0),
    ] {
        s.burn_rate.by_model.insert(
            adapter.to_string(),
            state::ModelBurnRate {
                pct_per_worker_per_hour: 1.0,
                dollars_per_worker_per_hour: dollars_per_worker_hour,
                samples: 24,
            },
        );
    }
    s
}

/// Re-seed the ledger-shed state, then run a real act cycle against it.
/// Caller holds [`ENV_LOCK`] and has already pointed `CGOV_LEDGER_LOGS_DIR`
/// where the cycle should read the ledger from.
fn ledger_shed_cycle(
    h: &Harness,
    agents: &HashMap<String, AgentConfig>,
    safe: u32,
) -> ScalingDecision {
    std::fs::write(
        &h.state_path,
        serde_json::to_string_pretty(&seeded_ledger_shed_state(safe)).expect("serialize state"),
    )
    .expect("write state fixture");
    act_cycle(h, agents, 0.0, 10, 10)
}

/// The four-pool ledger fleet, two idle workers per pool (the pool's own
/// session prefixes never prefix-collide — the fake tmux census filters by
/// prefix, so `shd-m-*` would count `shd-mid-*` sessions and double-book
/// the pool). Hourly costs: exp $30 > none $20 > mid $15 > cheap $5, so the
/// pre-ledger order sheds exp → none → mid → cheap.
fn four_pool_ledger_fleet(h: &Harness) -> HashMap<String, AgentConfig> {
    let env = h.state_path.parent().unwrap().to_path_buf();
    let mut agents = HashMap::new();
    for (pool, prefix, adapter) in [
        ("pool-none", "shd-none", "lg-none"),
        ("pool-cheap", "shd-cheap", "lg-cheap"),
        ("pool-mid", "shd-mid", "lg-mid"),
        ("pool-exp", "shd-exp", "lg-exp"),
    ] {
        let hb = env.join(format!("hb-{prefix}"));
        agents.insert(
            pool.to_string(),
            ledger_pool_config(&env, &h.launch_log, &hb, prefix, adapter),
        );
        h.worker_in(&hb, &format!("{prefix}-w1"), 20, true);
        h.worker_in(&hb, &format!("{prefix}-w2"), 10, true);
    }
    agents
}

/// The ledger's verdict on the four fleet adapters, written as real rows:
/// none proved waste (5 attempts, nothing verified), cheap proved worst
/// value ($100 per verified closure), mid middling ($15), exp the best
/// ($1). The yield order (none → cheap → mid → exp) is the REVERSE of the
/// cost order for the measured pools — the two orders cannot be confused.
fn seed_four_pool_evidence(ledger_dir: &Path) {
    seed_adapter_economics(ledger_dir, "lg-none", 0, 5, 10.0);
    seed_adapter_economics(ledger_dir, "lg-cheap", 2, 0, 100.0);
    seed_adapter_economics(ledger_dir, "lg-mid", 1, 0, 15.0);
    seed_adapter_economics(ledger_dir, "lg-exp", 10, 0, 1.0);
}

impl Harness {
    /// [`Harness::worker`] against an explicit heartbeat dir — the ledger
    /// fleet runs one heartbeat dir per pool, like the affinity fleet.
    fn worker_in(&self, hb_dir: &Path, session: &str, age_secs: i64, is_idle: bool) {
        use std::io::Write;
        let mut sessions = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.sessions_file)
            .expect("open sessions file");
        writeln!(sessions, "{session}").expect("append session");
        write_heartbeat(hb_dir, session, age_secs, is_idle);
    }
}

/// Property 1, live (claudego-ea621a6d): with ledger evidence present the
/// exhaustion shed follows worst verified-closure yield per dollar — the
/// proven-waste pool sheds first, then the worst-yield-per-dollar measured
/// pool, and the pool the ledger can point at keeps its workers even though
/// it is the most EXPENSIVE pool the pre-ledger order would have shed first.
///
/// Eight idle workers, shed six. Ledger order: lg-none (nothing verified,
/// sheds before anything measured), lg-cheap ($100/closure), lg-mid
/// ($15/closure) — lg-exp ($1/closure) survives untouched. The pre-ledger
/// cost order predicts the disjoint set exp+none+mid and would have kept
/// cheap: passing this test requires the ledger path, not the cost sort.
#[test]
fn ledger_shed_follows_worst_yield_per_dollar_through_the_live_cycle() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();
    let env = h.state_path.parent().unwrap().to_path_buf();
    let ledger_dir = env.join("ledger");
    seed_four_pool_evidence(&ledger_dir);
    std::env::set_var("CGOV_LEDGER_LOGS_DIR", ledger_dir.display().to_string());

    let agents = four_pool_ledger_fleet(&h);
    let decision = ledger_shed_cycle(&h, &agents, 2);

    std::env::remove_var("CGOV_LEDGER_LOGS_DIR");

    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(6),
        "8 workers against a safe count of 2 sheds exactly 6"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&[
            "shd-none-w1", "shd-none-w2", // proven waste sheds first
            "shd-cheap-w1", "shd-cheap-w2", // worst yield per dollar next
            "shd-mid-w1", "shd-mid-w2", // then middling yield
        ]),
        "the shed follows worst verified-closure yield per dollar: none, cheap, mid"
    );
    assert!(
        !signalled(&h.calls_log).iter().any(|s| s.starts_with("shd-exp")),
        "the best-yield pool keeps both workers although it is the most \
         expensive pool — the cost order would have shed it first"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "a ledger-ordered graceful shed never kill-sessions"
    );
}

/// Property 2, live (claudego-ea621a6d): with the ledger absent every pool
/// ranks Unknown and the shed is byte-for-byte the pre-ledger
/// cost-per-hour order. Two shapes of absence go through the live cycle:
/// a ledger directory with no evidence (the read succeeds, the map is
/// empty) and an unreadable ledger path (`read_cycle_ledger_yields`
/// returns None on the read failure). Both must produce the SAME shed —
/// the cost order exp ($30) → none ($20) → mid ($15), cheap ($5) surviving —
/// which is the disjoint complement of the ledger-ordered shed above.
#[test]
fn without_ledger_evidence_the_live_shed_is_todays_cost_order() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let expected_cost_order = sorted(&[
        "shd-exp-w1", "shd-exp-w2", // most expensive sheds first
        "shd-none-w1", "shd-none-w2", // then $20
        "shd-mid-w1", "shd-mid-w2", // then $15; cheap ($5) survives
    ]);

    // Shape one: the ledger directory exists but carries no in-window rows —
    // every pool ranks Unknown and the tiebreak is the whole order.
    let h = harness();
    let env = h.state_path.parent().unwrap().to_path_buf();
    std::env::set_var(
        "CGOV_LEDGER_LOGS_DIR",
        env.join("empty-ledger").display().to_string(),
    );
    let agents = four_pool_ledger_fleet(&h);
    let decision = ledger_shed_cycle(&h, &agents, 2);
    std::env::remove_var("CGOV_LEDGER_LOGS_DIR");

    assert_eq!(decision, ScalingDecision::ScaleDown(6));
    assert_eq!(
        signalled(&h.calls_log),
        expected_cost_order,
        "no ledger evidence: the shed is exactly the pre-ledger cost-per-hour order"
    );
    assert!(
        !signalled(&h.calls_log).iter().any(|s| s.starts_with("shd-cheap")),
        "the cheap pool survives although the ledger would have shed it first"
    );
    assert!(killed(&h.calls_log).is_empty());

    // Shape two: the ledger path is unreadable — read_ledger_yield fails and
    // read_cycle_ledger_yields degrades to None, the documented
    // broken-ledger behaviour. The shed must not move by a single session.
    let h2 = harness();
    let env2 = h2.state_path.parent().unwrap().to_path_buf();
    let not_a_dir = env2.join("ledger-unreadable");
    std::fs::write(&not_a_dir, "a regular file, not a directory").expect("write blocker file");
    std::env::set_var("CGOV_LEDGER_LOGS_DIR", not_a_dir.display().to_string());
    let agents2 = four_pool_ledger_fleet(&h2);
    let decision2 = ledger_shed_cycle(&h2, &agents2, 2);
    std::env::remove_var("CGOV_LEDGER_LOGS_DIR");

    assert_eq!(decision2, ScalingDecision::ScaleDown(6));
    assert_eq!(
        signalled(&h2.calls_log),
        expected_cost_order,
        "a failed ledger read degrades to the identical pre-ledger cost order"
    );
    assert!(killed(&h2.calls_log).is_empty());
}

/// Property 3, live (claudego-ea621a6d), the prohibition: the pool the
/// ledger order selects sheds its IDLE worker, never its busy one — even
/// when the busy worker carries the OLDER heartbeat (the age-first trap).
/// The selected pool is the cheap NoVerified one; the protected pool is the
/// most expensive, so the cost order would have shed the protected pool and
/// this test cannot pass through the cost sort either.
#[test]
fn worst_ledger_pool_sheds_its_idle_worker_never_its_busy_one() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();
    let env = h.state_path.parent().unwrap().to_path_buf();
    let ledger_dir = env.join("ledger");
    seed_adapter_economics(&ledger_dir, "lg-worst", 0, 5, 10.0);
    seed_adapter_economics(&ledger_dir, "lg-guarded", 40, 0, 1.0);
    std::env::set_var("CGOV_LEDGER_LOGS_DIR", ledger_dir.display().to_string());

    let hb_worst = env.join("hb-shd-w");
    let hb_guarded = env.join("hb-shd-p");
    h.worker_in(&hb_worst, "shd-w-idle", 20, true);
    // The busy worker's heartbeat is the OLDER one — an age-first candidate
    // sort inside the pool would signal it.
    h.worker_in(&hb_worst, "shd-w-busy", 40, false);
    h.worker_in(&hb_guarded, "shd-p-idle-a", 15, true);
    h.worker_in(&hb_guarded, "shd-p-idle-b", 5, true);

    let mut agents = HashMap::new();
    agents.insert(
        "pool-worst".to_string(),
        ledger_pool_config(&env, &h.launch_log, &hb_worst, "shd-w", "lg-worst"),
    );
    agents.insert(
        "pool-guarded".to_string(),
        ledger_pool_config(&env, &h.launch_log, &hb_guarded, "shd-p", "lg-guarded"),
    );

    let decision = ledger_shed_cycle(&h, &agents, 3);
    std::env::remove_var("CGOV_LEDGER_LOGS_DIR");

    assert_eq!(
        decision,
        ScalingDecision::ScaleDown(1),
        "4 workers against a safe count of 3 sheds exactly 1"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["shd-w-idle"]),
        "the ledger-selected pool sheds its idle worker; the older busy worker \
         and the expensive-but-verified pool are untouched"
    );
    assert!(killed(&h.calls_log).is_empty());
}

/// Property 3, live (claudego-ea621a6d), the sanctioned complement: a busy
/// worker IS shed when the pool-level ledger order selects its pool and that
/// pool has no idle candidate — and it is the pool's OLDEST busy worker.
/// The idle workers of the lower-ranked pool survive, although a global
/// idle-first sort would have taken them: the ledger decides WHICH pool
/// bleeds, and only within that pool does idle-first decide who.
#[test]
fn busy_worker_sheds_only_when_no_idle_candidate_exists_in_its_pool() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();
    let env = h.state_path.parent().unwrap().to_path_buf();
    let ledger_dir = env.join("ledger");
    seed_adapter_economics(&ledger_dir, "lg-busy", 0, 5, 10.0);
    seed_adapter_economics(&ledger_dir, "lg-idle-rich", 40, 0, 1.0);
    std::env::set_var("CGOV_LEDGER_LOGS_DIR", ledger_dir.display().to_string());

    let hb_busy = env.join("hb-shd-b");
    let hb_rich = env.join("hb-shd-i");
    h.worker_in(&hb_busy, "shd-b-old", 40, false);
    h.worker_in(&hb_busy, "shd-b-young", 30, false);
    h.worker_in(&hb_rich, "shd-i-idle-a", 20, true);
    h.worker_in(&hb_rich, "shd-i-idle-b", 5, true);

    let mut agents = HashMap::new();
    agents.insert(
        "pool-busy".to_string(),
        ledger_pool_config(&env, &h.launch_log, &hb_busy, "shd-b", "lg-busy"),
    );
    agents.insert(
        "pool-rich".to_string(),
        ledger_pool_config(&env, &h.launch_log, &hb_rich, "shd-i", "lg-idle-rich"),
    );

    let decision = ledger_shed_cycle(&h, &agents, 3);
    std::env::remove_var("CGOV_LEDGER_LOGS_DIR");

    assert_eq!(decision, ScalingDecision::ScaleDown(1));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["shd-b-old"]),
        "the ledger-selected pool has no idle candidate, so its oldest busy \
         worker sheds; the idle workers of the protected pool survive"
    );
    assert!(killed(&h.calls_log).is_empty());
}

// ---------------------------------------------------------------------------
// 9. The reclaim path's failure and timeout edges (claudego-b24eb184)
// ---------------------------------------------------------------------------

/// A second `tmux` fake, layered over the plain one by overwriting `bin/tmux`
/// after [`harness`]. With both control files empty it behaves identically to
/// [`install_fake_tmux`]; the tests below arm it:
///
/// - a session named in the **immune** file receives `send-keys C-c`, logs
///   the call, and KEEPS RUNNING — a worker that ignores the graceful
///   shutdown request, the shape the timeout exists for;
/// - a verb named in the **faults** file exits 1 after logging — a reclaim
///   failure (`send-keys` cannot reach the session, `kill-session` cannot
///   kill it, or `list-sessions` cannot enumerate at all).
///
/// `has-session` still reads the stopped/sessions files, so a force-killed
/// immune session is dead to every later probe.
fn install_fault_injecting_tmux(
    bin_dir: &Path,
    sessions_file: &Path,
    calls_log: &Path,
    stopped_file: &Path,
    immune_file: &Path,
    faults_file: &Path,
) {
    write_executable(
        bin_dir,
        "tmux",
        &format!(
            "printf '%s\\n' \"$*\" >> '{calls}'\n\
             case \"$1\" in\n\
             \x20 list-sessions)\n\
             \x20   if grep -Fxq 'list-sessions' '{faults}' 2>/dev/null; then exit 1; fi\n\
             \x20   cat '{sessions}' 2>/dev/null; exit 0;;\n\
             \x20 has-session)\n\
             \x20   if grep -Fxq \"$3\" '{stopped}' 2>/dev/null; then exit 1; fi\n\
             \x20   if grep -Fxq \"$3\" '{sessions}' 2>/dev/null; then exit 0; fi\n\
             \x20   exit 1;;\n\
             \x20 send-keys)\n\
             \x20   if grep -Fxq 'send-keys' '{faults}' 2>/dev/null; then exit 1; fi\n\
             \x20   if grep -Fxq \"$3\" '{immune}' 2>/dev/null; then exit 0; fi\n\
             \x20   printf '%s\\n' \"$3\" >> '{stopped}'; exit 0;;\n\
             \x20 kill-session)\n\
             \x20   if grep -Fxq 'kill-session' '{faults}' 2>/dev/null; then exit 1; fi\n\
             \x20   printf '%s\\n' \"$3\" >> '{stopped}'; exit 0;;\n\
             esac\nexit 0",
            calls = calls_log.display(),
            sessions = sessions_file.display(),
            stopped = stopped_file.display(),
            immune = immune_file.display(),
            faults = faults_file.display(),
        ),
    );
}

/// Re-arm [`harness`]'s tmux with the fault-injecting fake and return the
/// paths of its two control files (immune first, faults second).
fn arm_fault_injecting_tmux(h: &Harness) -> (PathBuf, PathBuf) {
    let env = h.state_path.parent().unwrap().to_path_buf();
    let bin = env.join("bin");
    let immune = env.join("sigint-immune.txt");
    let faults = env.join("tmux-faults.txt");
    std::fs::write(&immune, "").expect("empty immune list");
    std::fs::write(&faults, "").expect("empty faults list");
    install_fault_injecting_tmux(
        &bin,
        &h.sessions_file,
        &h.calls_log,
        &env.join("stopped.txt"),
        &immune,
        &faults,
    );
    (immune, faults)
}

impl Harness {
    fn bin_dir(&self) -> PathBuf {
        self.state_path.parent().unwrap().join("bin")
    }
}

/// A `WorkerConfig` shaped like `WorkerConfig::from_agent_config`'s product —
/// same launch_cmd family, same heartbeat dir, same session prefix (note
/// `session_prefix()` also strips the trailing `-`, so production runs
/// `cgidle`, not `cgidle-`) — with the graceful timeout under the test's
/// control. The integration tests keep the production 30 s; the direct
/// reclaim tests shorten it to keep the suite fast while exercising the
/// identical loop.
fn direct_worker_config(h: &Harness, graceful_timeout_secs: u64) -> WorkerConfig {
    let env = h._env.as_ref().expect("harness env dir");
    WorkerConfig {
        launch_cmd: launch_cmd_for(&env.path().join("bin"), env.path(), &h.launch_log),
        heartbeat_dir: h.hb_dir.clone(),
        graceful_timeout_secs,
        session_prefix: PREFIX.to_string(),
    }
}

/// The documented guarantee's teeth (claudego-b24eb184): a worker that
/// receives the graceful SIGINT and IGNORES it is not left running forever and
/// not killed on the spot — it is force-killed only after the production
/// `graceful_timeout_secs` (30, hard-coded by `WorkerConfig::from_agent_config`
/// — this test pays that wall clock on purpose). The worker that honoured the
/// request shuts down inside the window and is never kill-sessions'd, and the
/// mid-task workers are untouched by either verb: the force-kill is scoped to
/// the worker that was asked to stop and didn't, never widened to the busy.
///
/// The window itself is pinned by two load-robust observations — the cycle's
/// wall clock reaches the full 30 s (a deleted or zeroed wait would
/// force-kill instantly and produce the identical calls log), and the
/// executor's 2 s liveness poll of the stubborn session runs at least ten
/// ticks. Both can only grow under a loaded box, so neither bound flakes.
#[test]
fn worker_ignoring_sigint_is_force_killed_after_the_timeout_busy_workers_never_touched() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();
    let (immune, _faults) = arm_fault_injecting_tmux(&h);

    std::fs::write(&immune, "cgidle-idle-stubborn\n").expect("arm immune list");

    h.worker("idle-stubborn", 5, true);
    h.worker("idle-quick", 4, true);
    h.worker("busy-a", 20, false);
    h.worker("busy-b", 19, false);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);
    let started = std::time::Instant::now();
    let decision = cycle_with_target(&h, &agents, 2, 1.0, 10, 10);

    assert_eq!(decision, ScalingDecision::ScaleDown(2));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-idle-quick", "cgidle-idle-stubborn"]),
        "both idle workers receive the graceful SIGINT first"
    );
    assert_eq!(
        killed(&h.calls_log),
        sorted(&["cgidle-idle-stubborn"]),
        "only the worker that ignored the request is force-killed, and only \
         after the timeout window closed"
    );
    assert!(
        started.elapsed().as_secs() >= 28,
        "the force-kill landed at {:?}, before the production 30 s graceful \
         window had been waited out",
        started.elapsed()
    );
    let stubborn_probes = sessions_from_calls(&h.calls_log, "has-session")
        .iter()
        .filter(|s| *s == "cgidle-idle-stubborn")
        .count();
    assert!(
        stubborn_probes >= 10,
        "the executor probed the stubborn session only {} times — the 2 s \
         liveness poll did not run the 30 s window out",
        stubborn_probes
    );
    assert!(
        !signalled(&h.calls_log)
            .iter()
            .any(|s| s.starts_with("cgidle-busy")),
        "a mid-task worker is never even asked to stop, let alone force-killed"
    );
    assert!(
        !killed(&h.calls_log)
            .iter()
            .any(|s| s.starts_with("cgidle-busy")),
        "the timeout force-kill never widens to a busy worker"
    );
}

/// A reclaim failure on the graceful leg does not abandon the worker: the
/// `send-keys` that cannot reach the session is logged as failed
/// (`result.signaled` stays 0), the worker is still awaited for the full
/// window, and at the timeout it is force-killed — escalation, not a silently
/// half-shed fleet. The direct `scale_down_graceful` call is the same executor
/// the ScaleDown arm invokes; the shortened timeout keeps the loop at one tick.
#[test]
fn a_failed_sigint_still_escalates_to_force_kill_after_the_timeout() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();
    let (_immune, faults) = arm_fault_injecting_tmux(&h);
    std::fs::write(&faults, "send-keys\n").expect("fault send-keys");

    h.worker("solo", 5, true);

    let result = scale_down_graceful(1, &direct_worker_config(&h, 2), false);

    assert_eq!(result.targeted, 1);
    assert_eq!(
        result.signaled, 0,
        "the failed send-keys is not counted as delivered"
    );
    assert_eq!(
        result.graceful, 0,
        "a worker that was never signalled cannot shut down gracefully"
    );
    assert_eq!(
        result.force_killed, 1,
        "the un-reclaimed worker escalates to the force-kill at the timeout"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-solo"]),
        "the SIGINT was attempted (and logged) even though tmux rejected it"
    );
    assert_eq!(
        killed(&h.calls_log),
        sorted(&["cgidle-solo"]),
        "the escalation kill-session is attempted once the window closes"
    );
}

/// The mirror reclaim failure: the force-kill itself cannot reach the session.
/// The attempt is logged, `result.force_killed` stays 0 — the accounting must
/// report what actually happened, not what was attempted — and the loop
/// terminates at the timeout instead of hanging on a worker it cannot reap.
#[test]
fn a_failed_force_kill_is_reported_and_terminates_at_the_timeout() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();
    let (immune, faults) = arm_fault_injecting_tmux(&h);
    std::fs::write(&immune, "cgidle-solo\n").expect("arm immune list");
    std::fs::write(&faults, "kill-session\n").expect("fault kill-session");

    h.worker("solo", 5, true);

    let result = scale_down_graceful(1, &direct_worker_config(&h, 2), false);

    assert_eq!(result.signaled, 1, "the graceful SIGINT was delivered");
    assert_eq!(result.graceful, 0, "the immune worker never shut down");
    assert_eq!(
        result.force_killed, 0,
        "a kill-session tmux rejected is not counted as killed"
    );
    assert_eq!(
        killed(&h.calls_log),
        sorted(&["cgidle-solo"]),
        "the force-kill was attempted even though it failed"
    );
}

/// A shed that lands on one compliant and one stubborn worker accounts for
/// both outcomes in one `ScaleDownResult` — and the busy worker that made the
/// cut possible appears in neither verb's log.
#[test]
fn a_mixed_shed_reports_graceful_and_forced_and_never_touches_the_busy() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();
    let (immune, _faults) = arm_fault_injecting_tmux(&h);
    std::fs::write(&immune, "cgidle-stubborn\n").expect("arm immune list");

    h.worker("quick", 5, true);
    h.worker("stubborn", 4, true);
    h.worker("busy", 20, false);

    let result = scale_down_graceful(2, &direct_worker_config(&h, 2), false);

    assert_eq!(result.targeted, 2);
    assert_eq!(
        result.signaled, 2,
        "both idle candidates were asked to stop"
    );
    assert_eq!(
        result.graceful, 1,
        "the compliant worker shut down inside the window"
    );
    assert_eq!(
        result.force_killed, 1,
        "the stubborn worker was force-killed at the timeout"
    );
    assert_eq!(
        result.sessions,
        vec!["cgidle-quick".to_string(), "cgidle-stubborn".to_string()],
        "the shed list names the two idle workers, idle-status ordered"
    );
    assert!(
        !signalled(&h.calls_log).contains(&"cgidle-busy".to_string()),
        "the busy worker absorbs nothing"
    );
}

/// The failure mode that must shed NOBODY: tmux cannot be consulted at all
/// (the binary does not resolve on PATH). Selection refuses to guess —
/// "signalling a session we failed to enumerate is how the wrong worker gets
/// interrupted" — so nothing is signalled, nothing is killed, and even the
/// STALE heartbeat is left on disk, because with no census "no live session"
/// is unknowable rather than false. The contrast shape answers with an empty
/// census (`list-sessions` fails the way real tmux does when it has no
/// sessions): liveness is then KNOWABLE, the stale heartbeat is swept as an
/// orphan, the fresh one is kept, and still nobody is signalled — an answer,
/// even an empty one, is what licenses acting on the fleet.
#[test]
fn a_tmux_that_cannot_answer_sheds_nobody_and_sweeps_nothing() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // Shape one: no tmux anywhere on PATH — spawn fails, liveness unknowable.
    let h = harness();
    let bin = h.bin_dir();
    std::fs::remove_file(bin.join("tmux")).expect("remove the fake tmux");
    std::env::set_var("PATH", &bin); // bin only: no inherited PATH to find a real tmux in

    h.worker("stale-idle", 300, true); // would be swept if tmux answered
    h.worker("fresh-idle", 5, true);

    let result = scale_down_graceful(2, &direct_worker_config(&h, 2), false);

    assert_eq!(result.signaled, 0);
    assert_eq!(result.graceful, 0);
    assert_eq!(result.force_killed, 0);
    assert!(
        result.sessions.is_empty(),
        "an unconsultable tmux sheds nobody"
    );
    assert!(killed(&h.calls_log).is_empty());
    assert!(
        h.hb_dir.join("cgidle-stale-idle.json").exists(),
        "with liveness unknowable even the stale heartbeat is kept, not swept"
    );
    assert!(h.hb_dir.join("cgidle-fresh-idle.json").exists());

    // Shape two: tmux answers, but its census is empty (the way real tmux
    // reports "no sessions"): exit 1, not a spawn failure. Liveness is now
    // knowable — every heartbeat is an orphan or a survivor with no session.
    let h2 = harness();
    let (_immune2, faults2) = arm_fault_injecting_tmux(&h2);
    std::fs::write(&faults2, "list-sessions\n").expect("fault list-sessions");

    h2.worker("stale-idle", 300, true);
    h2.worker("fresh-idle", 5, true);

    let result2 = scale_down_graceful(2, &direct_worker_config(&h2, 2), false);

    assert_eq!(result2.signaled, 0);
    assert!(result2.sessions.is_empty());
    assert!(signalled(&h2.calls_log).is_empty());
    assert!(killed(&h2.calls_log).is_empty());
    assert!(
        !h2.hb_dir.join("cgidle-stale-idle.json").exists(),
        "an answerable tmux, even with an empty census, licenses the orphan sweep"
    );
    assert!(
        h2.hb_dir.join("cgidle-fresh-idle.json").exists(),
        "a fresh heartbeat is never swept, whatever the census says"
    );
}

// ---------------------------------------------------------------------------
// 10. Role changes across repeated scaling cycles (claudego-b24eb184)
// ---------------------------------------------------------------------------

/// Busy-protection is per-cycle heartbeat state, not a sticky pardon. The same
/// pair of workers runs through a full down → up → down sequence: cycle one
/// spares them mid-task while the idle pool absorbs the cut; demand returns
/// and the fleet grows back; then the pair — now idle, their heartbeats
/// flipped — is the shed's first choice, while freshly launched busy workers
/// inherit exactly the protection the pair just gave up. No cycle ever
/// signals a worker that was busy at that cycle's census, and no already-dead
/// session is signalled twice.
#[test]
fn busy_workers_spared_in_one_cycle_are_shed_once_idle_in_a_later_cycle() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let h = harness();

    h.worker("busy-a", 50, false);
    h.worker("busy-b", 49, false);
    h.worker("idle-a", 5, true);
    h.worker("idle-b", 4, true);

    let agents = single_agent(h.launch_cmd(), &h.hb_dir);

    // Cycle one: cut 4 → 2. The idle pair absorbs it; the mid-task pair survives.
    let first = cycle_with_target(&h, &agents, 2, 1.0, 10, 10);
    assert_eq!(first, ScalingDecision::ScaleDown(2));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&["cgidle-idle-a", "cgidle-idle-b"]),
        "the busy pair is never asked to stop while it holds a task"
    );
    h.reap(&["cgidle-idle-a", "cgidle-idle-b"]);

    // The survivors finish their tasks: their heartbeats flip busy → idle.
    write_heartbeat(&h.hb_dir, "cgidle-busy-a", 8, true);
    write_heartbeat(&h.hb_dir, "cgidle-busy-b", 6, true);

    // Cycle two: demand returns, target 4 against 2 running — the fleet grows.
    let second = cycle_with_target(&h, &agents, 4, 0.5, 10, 10);
    assert_eq!(second, ScalingDecision::ScaleUp(2));
    assert_eq!(
        std::fs::read_to_string(&h.launch_log)
            .expect("read launch log")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
        2,
        "the up-move really launches"
    );

    // The two launches register as new, mid-task workers.
    h.worker("new-a", 3, false);
    h.worker("new-b", 2, false);

    // Cycle three: cut 4 → 2 again. Yesterday's protected pair is now the
    // idle pool and sheds first; the brand-new busy workers inherit the
    // protection, and the workers shed in cycle one are gone, not re-signalled.
    let third = cycle_with_target(&h, &agents, 2, 0.5, 10, 10);
    assert_eq!(third, ScalingDecision::ScaleDown(2));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(&[
            "cgidle-idle-a",
            "cgidle-idle-b",
            "cgidle-busy-a",
            "cgidle-busy-b"
        ]),
        "the flipped pair sheds exactly once each; the new busy workers are untouched"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "every cycle of the sequence takes the graceful path"
    );
}
