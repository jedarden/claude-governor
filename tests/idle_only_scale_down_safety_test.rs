//! End-to-end idle-only scale-down safety on a live mixed busy+idle fleet
//! (claudego-c400daf1).
//!
//! The bead's four clauses, mapped to where each is already pinned and what
//! this file adds:
//!
//! 1. **Scale-down never terminates active tasks while idle capacity can
//!    absorb the cut** — pinned live by `graceful_idle_scaling_test.rs`
//!    (`scale_down_signals_only_idle_workers_regression`,
//!    `mixed_active_and_idle_scale_down_is_capped_idle_only_and_recorded`).
//!    This file re-exercises that guarantee under a **computed zero** target
//!    (`safe_worker_count = Some(0)`, a duty-cycle withdrawal) rather than a
//!    nonzero one, where the existing live coverage was only the brake arm.
//! 2. **Hysteresis damps scale-down** — pinned by
//!    `hysteresis_hold_touches_no_worker_regression` and the brake file's
//!    exact band boundaries. Re-pinned here as the suppressed Hold a
//!    just-below-threshold window produces on the live mixed fleet.
//! 3. **Per-cycle caps bound every move** — pinned by
//!    `scale_down_honors_per_cycle_cap_regression` and its consecutive-cycle
//!    variant. The computed-zero withdrawal here paces under the same cap,
//!    one idle worker per cycle.
//! 4. **The emergency brake fires only for a real usage window at or above
//!    98%** — the decision-level distinction is pinned by
//!    `emergency_brake_distinction_test.rs`
//!    (`a_computed_zero_runs_the_graceful_path_through_the_real_cycle`,
//!    `a_real_98_window_runs_the_brake_through_the_real_cycle`), but those
//!    run `dry_run = true` against a homogeneous pool: nobody is signalled,
//!    so the busy/idle dimension is invisible to them. This file composes
//!    the two arms on **byte-identical live mixed fleets** — real sessions,
//!    real SIGINTs and kill-sessions through a fake tmux — so the "only" in
//!    the clause is a controlled contrast:
//!
//!    - window at exactly `EMERGENCY_BRAKE_THRESHOLD` → the brake kills every
//!      session including the busy ones, bypassing idle selection, band, and
//!      caps, and engages safe mode;
//!    - the same fleet at `THRESHOLD - 0.01` → nobody is touched while the
//!      band damps, and with the band removed the withdrawal is graceful,
//!      idle-only, cap-paced: the busy workers lose their sessions only after
//!      the idle pool is exhausted, one per cycle, oldest heartbeat first,
//!      and always by graceful SIGINT — never a brake kill.
//!
//! Harness adapted from `graceful_idle_scaling_test.rs` (fake tmux that
//! records every call and marks signalled sessions dead). Tests in this
//! binary swap `PATH` / `CGOV_DECISIONS_PATH`, so every test holds
//! [`ENV_LOCK`] for its whole body.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{Duration as ChronoDuration, Utc};
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{run_act_cycle, ScalingDecision, EMERGENCY_BRAKE_THRESHOLD};
use claude_governor::narrator::{read_last_decisions_from_path, ScaleAction};
use claude_governor::state;

/// Serializes every test that swaps PATH / CGOV_DECISIONS_PATH (tests in one
/// binary share a process and run in threads).
static ENV_LOCK: Mutex<()> = Mutex::new(());
/// The PATH this process launched with, captured before any test swaps it.
static ORIG_PATH: OnceLock<String> = OnceLock::new();

/// Session pattern prefix. The census counts only sessions matching
/// `{PREFIX}-*`, so both the tmux session name and the heartbeat's `session`
/// field must carry it.
const PREFIX: &str = "cgbrake";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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
/// - `has-session -t <s>` succeeds only for a session that is live (listed)
///   and not already stopped;
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

/// Launch stub: no-op; these tests only scale down.
fn install_launch_stub(bin_dir: &Path) {
    write_executable(bin_dir, "launch-stub", "exit 0");
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

/// Register a worker: live tmux session + heartbeat. The tmux session and
/// the heartbeat's `session` field both carry the full `{PREFIX}-<name>`
/// name — the census counts only sessions matching the agent's `{PREFIX}-*`
/// pattern, so a bare name would be invisible to the cycle.
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
/// other two windows are roomier and below the brake threshold, so they can
/// never bind over it or fire the brake.
///
/// `safe_worker_count = Some(0)` is the duty-cycle withdrawal verdict: the
/// fleet's honest pace-ahead computation, with NO usage window at or above
/// the brake threshold.
fn withdrawal_state(binding_utilization: f64) -> state::GovernorState {
    fn seed_window(win: &mut state::WindowForecast, utilization: f64, safe: Option<u32>) {
        win.current_utilization = utilization;
        win.hours_remaining = 40.0;
        win.cutoff_risk = false;
        win.safe_worker_count = safe;
        win.safe_worker_count_p75 = safe;
        win.cone_ratio = 0.0; // narrow cone → p50 estimate selected
    }

    let mut s = state::GovernorState::new();
    s.capacity_forecast.binding_window = "five_hour".to_string();
    seed_window(
        &mut s.capacity_forecast.five_hour,
        binding_utilization,
        Some(0),
    );
    seed_window(&mut s.capacity_forecast.seven_day, 50.0, None);
    seed_window(&mut s.capacity_forecast.weekly_scoped, 45.0, None);
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

/// One-agent fleet map over the fixtures built in [`harness`]. Built by
/// deserialization so this file compiles against trees that add optional
/// `AgentConfig` fields (serde tolerates both).
fn single_agent(launch_cmd: String, heartbeat_dir: &Path) -> HashMap<String, AgentConfig> {
    let agent = serde_json::from_value(serde_json::json!({
        "launch_cmd": launch_cmd,
        "session_pattern": format!("{PREFIX}-*"),
        "heartbeat_dir": heartbeat_dir.to_string_lossy(),
        "min_workers": 0,
        "max_workers": 10,
        "subscription": false,
    }))
    .expect("agent config fixture should deserialize");
    HashMap::from([("pool".to_string(), agent)])
}

/// The mixed fleet every test runs against: three idle workers and two busy
/// ones whose heartbeats are the OLDER ones, so an age-first candidate sort
/// would interrupt active tasks — idle status must dominate age.
const FLEET: [(&str, i64, bool); 5] = [
    ("busy-oldest", 50, false),
    ("busy-young", 49, false),
    ("idle-oldest", 40, true),
    ("idle-mid", 10, true),
    ("idle-newest", 5, true),
];

/// The full tmux session name for a fleet member.
fn session(name: &str) -> String {
    format!("{PREFIX}-{name}")
}

/// Everything one test needs: an env dir with the fakes on PATH, the mixed
/// fleet seeded, the state file, and the log paths. Created under the env
/// lock (the caller holds it).
struct Harness {
    _env: TempDir,
    state_path: PathBuf,
    sessions_file: PathBuf,
    calls_log: PathBuf,
    hb_dir: PathBuf,
    decisions_path: PathBuf,
}

/// Build the harness and its one-agent fleet map together; the agent config
/// embeds the env's launch stub and heartbeat dir.
fn harness() -> (Harness, HashMap<String, AgentConfig>) {
    let env = TempDir::new().expect("temp env dir");
    let root = env.path().to_path_buf();
    let bin_dir = root.join("bin");
    std::fs::create_dir(&bin_dir).expect("bin dir");

    let sessions_file = root.join("sessions.txt");
    std::fs::write(&sessions_file, "").expect("empty sessions file");
    let calls_log = root.join("tmux-calls.log");
    std::fs::write(&calls_log, "").expect("empty calls log");
    let stopped_file = root.join("stopped.txt");
    std::fs::write(&stopped_file, "").expect("empty stopped file");

    install_fake_tmux(&bin_dir, &sessions_file, &calls_log, &stopped_file);
    install_quiet_bf(&bin_dir);
    install_launch_stub(&bin_dir);

    let orig = ORIG_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), orig));
    let decisions_path = root.join("decisions.jsonl");
    std::env::set_var("CGOV_DECISIONS_PATH", &decisions_path);

    let launch_cmd = format!(
        "{} --workspace {} cgbrake-scaled {}",
        bin_dir.join("launch-stub").display(),
        root.display(),
        root.join("launches.log").display(),
    );

    let hb_dir = root.join("heartbeats");
    for (name, age_secs, is_idle) in FLEET {
        seed_worker(&sessions_file, &hb_dir, name, age_secs, is_idle);
    }

    let h = Harness {
        _env: env,
        state_path: root.join("governor-state.json"),
        sessions_file,
        calls_log,
        hb_dir,
        decisions_path,
    };
    let agents = single_agent(launch_cmd, &h.hb_dir);
    (h, agents)
}

impl Harness {
    /// Seed the state file with a binding five_hour window at
    /// `binding_utilization` carrying `safe_worker_count = Some(0)`.
    fn write_state(&self, binding_utilization: f64) {
        let st = withdrawal_state(binding_utilization);
        std::fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&st).expect("serialize state"),
        )
        .expect("write state fixture");
    }

    /// Simulate the named workers having exited: drop their sessions from the
    /// fake tmux census the way a real tmux server drops a session whose
    /// process ended, so the NEXT cycle counts — and considers as shutdown
    /// candidates — only the survivors. Their heartbeat files stay on disk,
    /// as they do in production until the orphan sweep collects them.
    fn reap(&self, gone: &[&str]) {
        let census = std::fs::read_to_string(&self.sessions_file).expect("read sessions file");
        let survivors: Vec<&str> = census
            .lines()
            .filter(|s| !gone.contains(s))
            .collect();
        let mut body = survivors.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        std::fs::write(&self.sessions_file, body).expect("rewrite sessions file");
    }

    /// One live act cycle (dry_run = false — the executor only exists on the
    /// real path) against a freshly written state at `binding_utilization`.
    /// Caller holds [`ENV_LOCK`]; [`harness()`] already activated the fakes.
    fn cycle(
        &self,
        agents: &HashMap<String, AgentConfig>,
        binding_utilization: f64,
        hysteresis: f64,
        max_up: u32,
        max_down: u32,
    ) -> ScalingDecision {
        self.write_state(binding_utilization);
        run_act_cycle(
            &self.state_path,
            false,
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

fn sorted(names: Vec<String>) -> Vec<String> {
    let mut v = names;
    v.sort();
    v
}

/// The decisions log read back as `(action, decision_source)` pairs so tests
/// can audit every cycle's true decision source. `read_last_decisions_from_
/// path` returns newest first; callers that only use `.all()` don't care.
fn decision_audit(decisions_path: &PathBuf, expected: usize) -> Vec<(ScaleAction, String)> {
    let entries =
        read_last_decisions_from_path(expected, decisions_path).expect("read decision audit log");
    assert_eq!(
        entries.len(),
        expected,
        "every cycle must record a decision entry"
    );
    entries
        .iter()
        .map(|entry| {
            let source = entry
                .context
                .as_ref()
                .and_then(|ctx| ctx["decision_source"].as_str())
                .expect("decision entry carries a decision_source")
                .to_string();
            (entry.action, source)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Computed zero, no window at the threshold: graceful, idle-only,
//    cap-paced withdrawal on the live mixed fleet
// ---------------------------------------------------------------------------

/// `safe_worker_count = Some(0)` with NO usage window at or above the brake
/// threshold is a duty-cycle withdrawal, not an emergency. On the live path
/// with real sessions it must:
///
/// - take the capped `ScaleDown` path — one worker per cycle under
///   `max_down_per_cycle = 1`, never `EmergencyBrake`;
/// - signal only idle workers while the idle pool can absorb the cut, even
///   though the busy workers' heartbeats are the older ones;
/// - reach the busy tier only after the idle pool is exhausted, oldest
///   heartbeat first, and then by graceful SIGINT — a brake kill-session
///   must never appear anywhere in the log;
/// - audit every cycle as `decision_source = "computed_target"` with the
///   zero effective target, and leave safe mode OFF (the brake's signature).
#[test]
fn computed_zero_withdraws_idle_only_across_capped_cycles_on_the_live_mixed_fleet() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (h, agents) = harness();

    // The usage window sits far below the brake threshold: the zero target is
    // computed, not braked into.
    let d1 = h.cycle(&agents, 65.0, 0.0, 3, 1);
    assert_eq!(
        d1,
        ScalingDecision::ScaleDown(1),
        "computed zero takes the graceful capped path, not the brake"
    );
    assert_eq!(
        signalled(&h.calls_log),
        sorted(vec![session("idle-oldest")]),
        "the first cycle signals the oldest idle worker only"
    );
    assert!(killed(&h.calls_log).is_empty(), "no brake kills, ever");

    h.reap(&[session("idle-oldest").as_str()]);

    // Cycle 2: next-oldest idle worker; both busy workers still untouched.
    let d2 = h.cycle(&agents, 65.0, 0.0, 3, 1);
    assert_eq!(d2, ScalingDecision::ScaleDown(1));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(vec![session("idle-oldest"), session("idle-mid")]),
        "the second cycle still touches only idle workers, within the cap"
    );
    assert!(killed(&h.calls_log).is_empty());

    h.reap(&[session("idle-mid").as_str()]);

    // Cycle 3: last idle worker; the busy tier survives a third time.
    let d3 = h.cycle(&agents, 65.0, 0.0, 3, 1);
    assert_eq!(d3, ScalingDecision::ScaleDown(1));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(vec![
            session("idle-oldest"),
            session("idle-mid"),
            session("idle-newest"),
        ]),
        "three cycles, three idle workers, never a busy one"
    );
    assert!(killed(&h.calls_log).is_empty());

    h.reap(&[session("idle-newest").as_str()]);

    // Cycle 4: the idle pool is exhausted and two busy workers remain. The
    // withdrawal still paces (one per cycle) and reaches the busy tier
    // oldest-heartbeat-first — but by graceful SIGINT, never kill-session.
    // This is the arm the emergency brake skips entirely: the brake would
    // have killed both busy workers in cycle 1.
    let d4 = h.cycle(&agents, 65.0, 0.0, 3, 1);
    assert_eq!(d4, ScalingDecision::ScaleDown(1));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(vec![
            session("idle-oldest"),
            session("idle-mid"),
            session("idle-newest"),
            session("busy-oldest"),
        ]),
        "only after idle exhaustion does the withdrawal reach the busy tier, oldest first"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "a computed-zero withdrawal never kill-sessions anyone — the brake's signature"
    );

    // The audit log names every cycle's true source: a computed target of 0,
    // never the brake.
    let audit = decision_audit(&h.decisions_path, 4);
    assert!(
        audit
            .iter()
            .all(|(action, source)| matches!(action, ScaleAction::ScaleDown)
                && source == "computed_target"),
        "every cycle audited as a computed_target scale-down: {audit:?}"
    );

    // The bounded move is recorded in state, and safe mode stays OFF —
    // engaging it is the brake's signature, and no window crossed the
    // threshold here.
    let after = state::load_state(&h.state_path).expect("reload persisted state");
    let pool = after.workers.get("pool").expect("pool tracked in state");
    assert_eq!(pool.target, 1, "state records the bounded withdrawal target");
    assert!(
        !after.safe_mode.active,
        "a graceful withdrawal does not engage safe mode"
    );
}

// ---------------------------------------------------------------------------
// 2. A real window AT the threshold: the brake, on the identical fleet
// ---------------------------------------------------------------------------

/// The byte-identical fleet and state fixture with the binding window at
/// exactly `EMERGENCY_BRAKE_THRESHOLD`: the same computed zero now travels
/// the violent arm. The brake kills EVERY session — idle and busy — directly
/// with kill-session, bypassing idle-first selection, a 50-worker band, and a
/// zero down-cap (knobs that hold back any graceful move), and engages safe
/// mode with the emergency_brake trigger. Pins the "only" in "the brake only
/// for a real usage window at or above 98%": 0.01 utilization points are the
/// whole difference between this test and the graceful withdrawal above.
#[test]
fn a_window_at_exactly_98_brakes_the_identical_fleet_killing_even_busy_workers() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (h, agents) = harness();

    let decision = h.cycle(&agents, EMERGENCY_BRAKE_THRESHOLD, 50.0, 3, 0);

    assert_eq!(
        decision,
        ScalingDecision::EmergencyBrake,
        "a real window at the threshold takes the brake on the identical fixture"
    );
    assert_eq!(
        killed(&h.calls_log),
        sorted(vec![
            session("busy-oldest"),
            session("busy-young"),
            session("idle-oldest"),
            session("idle-mid"),
            session("idle-newest"),
        ]),
        "the brake kills every session — idle selection does not apply to it"
    );
    assert!(
        signalled(&h.calls_log).is_empty(),
        "the brake kill-sessions directly; it does not take the graceful SIGINT path"
    );

    let after = state::load_state(&h.state_path).expect("reload persisted state");
    let pool = after.workers.get("pool").expect("pool tracked in state");
    assert_eq!(pool.target, 0, "the brake zeroes the pool's target");
    assert_eq!(pool.current, 0, "the brake zeroes the pool's live count");
    assert!(
        after.safe_mode.active,
        "the brake engages safe mode so the next cycle does not instantly scale back up"
    );
    assert_eq!(
        after.safe_mode.trigger.as_deref(),
        Some("emergency_brake"),
        "safe mode records the brake as its trigger"
    );

    // The audit log names the source: the brake, not a computed target.
    let audit = decision_audit(&h.decisions_path, 1);
    assert_eq!(
        audit,
        vec![(
            ScaleAction::EmergencyBrakeEngage,
            "emergency_brake".to_string()
        )],
        "the cycle audited as an emergency brake"
    );
}

// ---------------------------------------------------------------------------
// 3. Just below the threshold: the identical knobs hold, then shed idle-only
// ---------------------------------------------------------------------------

/// `EMERGENCY_BRAKE_THRESHOLD - 0.01` on the identical fleet with the
/// identical knobs (band 50, zero down-cap): no brake, and the band damps
/// the withdrawal — NOBODY is touched. Then with the band removed the same
/// below-threshold window sheds the oldest idle worker gracefully: the two
/// arms differ by 0.01 utilization points, and below the line the fleet
/// keeps every brake-protection property (idle-first, cap-paced, no
/// kill-sessions).
#[test]
fn a_window_just_below_98_holds_the_identical_fleet_then_sheds_idle_only() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (h, agents) = harness();

    let just_below = EMERGENCY_BRAKE_THRESHOLD - 0.01;

    // Cycle 1: identical knobs to the brake test — band 50 damps the
    // scale-down, the zero down-cap would block it anyway, and there is no
    // real threshold window to bypass either. A hold touching nobody is the
    // correct outcome.
    let d1 = h.cycle(&agents, just_below, 50.0, 3, 0);
    assert_eq!(
        d1,
        ScalingDecision::NoChange,
        "just below the threshold the band damps the withdrawal — no brake"
    );
    assert!(
        signalled(&h.calls_log).is_empty() && killed(&h.calls_log).is_empty(),
        "a damped hold touches no session of either tier"
    );
    let audit1 = decision_audit(&h.decisions_path, 1);
    assert_eq!(
        audit1,
        vec![(ScaleAction::Hold, "computed_target".to_string())],
        "the suppressed hold is audited with its true (computed) source"
    );

    // Cycle 2: same below-threshold window, band removed, cap 1 — the
    // graceful withdrawal resumes and stays idle-only.
    let d2 = h.cycle(&agents, just_below, 0.0, 3, 1);
    assert_eq!(d2, ScalingDecision::ScaleDown(1));
    assert_eq!(
        signalled(&h.calls_log),
        sorted(vec![session("idle-oldest")]),
        "below the threshold the shed is the graceful idle-only path"
    );
    assert!(
        killed(&h.calls_log).is_empty(),
        "still no brake kill-sessions anywhere in the log"
    );

    let after = state::load_state(&h.state_path).expect("reload persisted state");
    assert!(!after.safe_mode.active, "no window crossed the threshold");
}
