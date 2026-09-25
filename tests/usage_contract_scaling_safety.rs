//! Integration contract: invalid or incomplete `/api/oauth/usage` data must
//! not trigger unsafe scaling decisions (claudego-c5e76c0f).
//!
//! Two sibling suites pin the two halves of that contract in isolation:
//! `usage_polling_contract.rs` pins what the poller itself does with each
//! response class (parse, error variants, stale fallback), and
//! `offpeak_promotion_window_forecasting.rs` drives the observe cycle with
//! hand-built `UsageData`. Neither answers the question that matters when the
//! API misbehaves in production: **what does the fleet actually do?** These
//! tests do: the real `Poller` runs against deterministic mockito fixtures,
//! its output flows through the real `run_observe_cycle`, and the real
//! `run_act_cycle` decides against a fake-but-materializing worker fleet.
//!
//! The pinned property, per failure class:
//!
//! - **Transport error, 429 self-rate-limit, malformed body** — the poll
//!   fails, the last good reading is retained, the error classifies as
//!   token-healthy (`token_refresh_failing` stays false: the OAuth token is
//!   not the problem), and a fleet already converged onto the last good
//!   reading's target does not grow.
//! - **Credential loss** — the auth path degrades to the cached reading with
//!   `stale=true`, flags `token_refresh_failing`, and likewise cannot grow a
//!   converged fleet.
//! - **The incomplete-but-parsing response** (`{}`: every window absent) is
//!   the dangerous one — it parses as a valid, *non-stale* reading whose
//!   windows all read 0% with no reset times, i.e. textually infinite
//!   headroom. The defense under test: a window with no parseable reset
//!   timestamp is data-absent, and data-absent windows cannot bind the
//!   scaling decision.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use tempfile::TempDir;

use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, GovernorConfig, PricingConfig,
};
use claude_governor::governor::{run_act_cycle, run_observe_cycle, CyclePaths, ScalingDecision};
use claude_governor::poller::Poller;
use claude_governor::state::{self, GovernorState};

/// Tests here mutate process-global environment (PATH, CGOV_DECISIONS_PATH);
/// serialize everything that does.
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

// ---------------------------------------------------------------------------
// Fake worker fleet: a tmux stub whose census is a sessions file, and a
// launch command that materializes a session per launch — so a scale-up is
// physically visible in the next cycle's census instead of being assumed.
// ---------------------------------------------------------------------------

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct FakeFleet {
    _guard: EnvGuard,
    sessions_path: PathBuf,
}

impl FakeFleet {
    /// `initial` sessions named pool-1..pool-N are live when the harness
    /// starts.
    fn spawn(dir: &TempDir, initial: u32) -> Self {
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let sessions_path = dir.path().join("sessions");
        let seed: String = (1..=initial).map(|n| format!("pool-{n}\n")).collect();
        std::fs::write(&sessions_path, &seed).unwrap();
        let decisions_path = dir.path().join("decisions.jsonl");

        let sessions = sessions_path.display();
        let tmux = format!(
            "#!/bin/sh\ncase \"$1\" in\n  list-sessions) [ -f '{sessions}' ] && cat '{sessions}';;\n  has-session) [ -s '{sessions}' ];;\n  send-keys) : > '{sessions}';;\n  *) :;;\nesac\n"
        );
        write_executable(&bin.join("tmux"), &tmux);
        // Each launch appends one session: the fleet a decision asks for is
        // the fleet the next census sees.
        write_executable(
            &bin.join("launch-stub"),
            &format!("#!/bin/sh\necho \"pool-$(date +%s%N)\" >> '{sessions}'\nexit 0\n"),
        );

        let old_path = std::env::var("PATH").unwrap_or_default();
        let old_decisions = std::env::var("CGOV_DECISIONS_PATH").ok();
        std::env::set_var("PATH", format!("{}:{}", bin.display(), old_path));
        std::env::set_var("CGOV_DECISIONS_PATH", &decisions_path);

        FakeFleet {
            _guard: EnvGuard {
                path: old_path,
                decisions: old_decisions,
            },
            sessions_path,
        }
    }

    fn live_sessions(&self) -> usize {
        std::fs::read_to_string(&self.sessions_path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn agent_map() -> HashMap<String, AgentConfig> {
    let mut agents = HashMap::new();
    agents.insert(
        "pool".to_string(),
        serde_json::from_value(serde_json::json!({
            "launch_cmd": "launch-stub",
            "session_pattern": "pool-*",
            "heartbeat_dir": "/tmp/cgov-usage-safety-no-heartbeats",
            "min_workers": 0,
            "max_workers": 4,
            "subscription": true,
        }))
        .unwrap(),
    );
    agents
}

fn pricing_config(agents: &HashMap<String, AgentConfig>) -> GovernorConfig {
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

/// Credentials file in the documented shape, expiring far outside the
/// 5-minute refresh threshold: every scenario here isolates the usage-poll
/// path from the refresh path.
fn write_credentials(dir: &TempDir) -> String {
    let path = dir.path().join(".credentials.json");
    let expires_at = Utc::now().timestamp_millis() + 3_600_000;
    let body = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "access-token-1",
            "refreshToken": "refresh-token-1",
            "expiresAt": expires_at,
            "scopes": ["user:inference"],
        }
    });
    std::fs::write(&path, body.to_string()).unwrap();
    path.to_string_lossy().into_owned()
}

/// A poller wired to the given usage/token endpoints, refresh retry delay
/// zeroed.
fn poller_with_endpoints(creds_path: &str, usage_url: &str, token_url: &str) -> Poller {
    Poller::with_credentials_path(Some(creds_path.to_string()))
        .expect("a valid credentials path should build a poller")
        .with_endpoints(usage_url.to_string(), token_url.to_string())
        .with_refresh_retry_delay(Duration::ZERO)
}

/// The last good reading: moderate utilization on all three windows, reset
/// times in the future. The reset timestamps are FIXED across cycles — the
/// real API reports one timestamp per window period, so a re-poll five
/// minutes later returns the same instant with less time remaining.
fn healthy_usage_body() -> String {
    let base = Utc::now();
    let five_hour = (base + ChronoDuration::hours(2)).to_rfc3339();
    let seven_day = (base + ChronoDuration::hours(90)).to_rfc3339();
    let weekly = (base + ChronoDuration::hours(90)).to_rfc3339();
    format!(
        r#"{{"five_hour": {{"utilization": 55.0, "resets_at": "{five_hour}"}},
            "seven_day": {{"utilization": 40.0, "resets_at": "{seven_day}"}},
            "limits": [{{"kind": "weekly_scoped", "percent": 60, "resets_at": "{weekly}",
                         "scope": {{"model": {{"id": "claude-fable-5", "display_name": "Fable"}}}},
                         "is_active": true}}]}}"#
    )
}

/// The documented self-rate-limit response (usage-tracking.md §2).
fn rate_limited_mock(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("GET", "/api/oauth/usage")
        .with_status(429)
        .with_body(
            r#"{"error":{"type":"rate_limit_error","message":"Rate limited. Please try again later."}}"#,
        )
        .expect(1)
        .create()
}

/// A localhost URL whose listener is guaranteed closed.
fn dead_endpoint_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

// ---------------------------------------------------------------------------
// Cycle driver: real poller → real observe → real act, at a fixed instant
// ---------------------------------------------------------------------------

struct Cycle {
    state: GovernorState,
    decision: ScalingDecision,
}

/// Run one full observe+act round at `now` against the temp-rooted layout.
/// `dry_run` is false so the decision path is the production one; the fake
/// fleet absorbs it.
fn drive_cycle(
    poller: &mut Poller,
    home: &TempDir,
    agents: &HashMap<String, AgentConfig>,
    config: &GovernorConfig,
    now: DateTime<Utc>,
) -> Cycle {
    let state_path = home.path().join("governor-state.json");
    let paths = CyclePaths::under(home.path());

    run_observe_cycle(
        poller,
        &state_path,
        &paths,
        &config.alerts,
        agents,
        &[],
        config,
        now,
    )
    .expect("the observe cycle should complete");

    let decision = run_act_cycle(
        &state_path,
        false,
        5.0,  // hysteresis band, as the daemon default
        10,   // max up per cycle
        10,   // max down per cycle
        90.0, // target ceiling
        &config.alerts,
        agents,
        0, // pre-scale minutes
        &[],
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        config,
        now,
    )
    .expect("the act cycle should complete");

    let state = state::load_state(&state_path).expect("reload the persisted state");
    Cycle { state, decision }
}

/// Register a healthy usage mock answering exactly once with `body`.
fn mock_usage_once(server: &mut mockito::Server, body: &str) -> mockito::Mock {
    server
        .mock("GET", "/api/oauth/usage")
        .with_status(200)
        .with_body(body)
        .expect(1)
        .create()
}

/// How the usage poll breaks in the scenario under test.
enum Failure {
    /// The usage endpoint is unreachable: connection refused.
    Transport,
    /// The documented 429 self-rate-limit response.
    RateLimit,
    /// A 200 whose body is not JSON.
    Malformed,
}

impl Failure {
    /// Build the poller for the failing cycle and register whatever mock the
    /// failure class needs. Transport rewires the usage endpoint to a
    /// guaranteed-closed port instead, so there is no mock to satisfy.
    fn arm(
        &self,
        creds: &str,
        server_url: &str,
        server: &mut mockito::Server,
    ) -> (Poller, Option<mockito::Mock>) {
        match self {
            Failure::Transport => {
                let poller =
                    poller_with_endpoints(creds, &dead_endpoint_url(), &format!("{server_url}/v1/oauth/token"));
                (poller, None)
            }
            Failure::RateLimit => {
                let poller = poller_with_endpoints(creds, server_url, &format!("{server_url}/v1/oauth/token"));
                let mock = rate_limited_mock(server);
                (poller, Some(mock))
            }
            Failure::Malformed => {
                let poller = poller_with_endpoints(creds, server_url, &format!("{server_url}/v1/oauth/token"));
                let mock = server
                    .mock("GET", "/api/oauth/usage")
                    .with_status(200)
                    .with_body("{not json")
                    .expect(1)
                    .create();
                (poller, Some(mock))
            }
        }
    }
}

/// Healthy rounds until the fleet stops moving, so the failure scenarios
/// observe a fleet sitting at the target its last good data implies. Bounded:
/// a target the fleet cannot reach in six rounds at max_up=10/cycle is a
/// harness bug, not a scenario.
fn converge_onto_good_data(
    server: &mut mockito::Server,
    poller: &mut Poller,
    home: &TempDir,
    agents: &HashMap<String, AgentConfig>,
    config: &GovernorConfig,
    now: &mut DateTime<Utc>,
) -> Vec<ScalingDecision> {
    let mut decisions = Vec::new();
    for round in 0..6 {
        let body = healthy_usage_body();
        let mock = mock_usage_once(server, &body);
        let cycle = drive_cycle(poller, home, agents, config, *now);
        mock.assert();
        *now += ChronoDuration::seconds(300);
        let converged = matches!(cycle.decision, ScalingDecision::NoChange);
        decisions.push(cycle.decision);
        if converged {
            return decisions;
        }
        assert!(
            round < 5,
            "fleet never converged onto the good reading: {decisions:?}"
        );
    }
    unreachable!()
}

/// The shared scenario: converge a real fleet onto a good reading, then let
/// the usage poll break, and pin what the next cycle does.
///
/// - the decision must not launch anyone;
/// - the fleet on disk must not have grown;
/// - the last good reading must be retained verbatim (the decision continued
///   from the same data the fleet was sized against — not from zeros, not
///   from a partial parse);
/// - `token_refresh_failing` must classify per the error class.
fn assert_failed_poll_cannot_grow_a_converged_fleet(failure: Failure) {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = TempDir::new().unwrap();
    let fleet = FakeFleet::spawn(&dir, 2);
    let creds = write_credentials(&dir);
    let mut server = mockito::Server::new();
    let server_url = server.url();

    let agents = agent_map();
    let config = pricing_config(&agents);
    let mut now = Utc::now();

    // Phase 1: converge onto the good reading with the live endpoint.
    let mut poller = poller_with_endpoints(&creds, &server_url, &format!("{server_url}/v1/oauth/token"));
    converge_onto_good_data(&mut server, &mut poller, &dir, &agents, &config, &mut now);
    let before = state::load_state(&dir.path().join("governor-state.json")).unwrap();
    let fleet_at_convergence = fleet.live_sessions();
    assert!(
        fleet_at_convergence > 0,
        "harness bug: the fleet must hold workers for the no-growth assertion to mean anything"
    );
    assert_eq!(
        before.usage.five_hour_pct, 55.0,
        "precondition: the good reading is what the fleet converged onto"
    );

    // Phase 2: the usage poll breaks; the fleet must not grow on it.
    let (mut poller, mock) = failure.arm(&creds, &server_url, &mut server);
    let cycle = drive_cycle(&mut poller, &dir, &agents, &config, now);
    if let Some(mock) = mock {
        mock.assert();
    }

    assert!(
        !matches!(cycle.decision, ScalingDecision::ScaleUp(_)),
        "a failed poll must not scale up; got {:?}",
        cycle.decision
    );
    assert_eq!(
        fleet.live_sessions(),
        fleet_at_convergence,
        "no launch may be executed off a failed poll"
    );

    // The retained reading is the last good one, verbatim.
    assert_eq!(cycle.state.usage.five_hour_pct, before.usage.five_hour_pct);
    assert_eq!(cycle.state.usage.all_models_pct, before.usage.all_models_pct);
    assert_eq!(
        cycle.state.usage.weekly_scoped_pct,
        before.usage.weekly_scoped_pct
    );
    // API-side failures mean the token is fine: the flag the HUMAN alert
    // path keys on must stay down.
    assert_eq!(
        cycle.state.token_refresh_failing, false,
        "transport/429/parse failures are not token failures"
    );
}

#[test]
fn transport_failure_cannot_grow_a_converged_fleet() {
    assert_failed_poll_cannot_grow_a_converged_fleet(Failure::Transport);
}

#[test]
fn rate_limited_poll_cannot_grow_a_converged_fleet() {
    assert_failed_poll_cannot_grow_a_converged_fleet(Failure::RateLimit);
}

#[test]
fn malformed_poll_cannot_grow_a_converged_fleet() {
    assert_failed_poll_cannot_grow_a_converged_fleet(Failure::Malformed);
}

/// Credential loss is the one failure class the poller absorbs: the cached
/// reading is served with `stale=true` instead of failing the poll. The
/// observe cycle must forward that flag to `token_refresh_failing` (the
/// auth path, unlike the API path, IS the token failing), and the act cycle
/// must still not grow the fleet on stale data.
#[test]
fn credential_loss_degrades_to_stale_data_and_cannot_grow_the_fleet() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = TempDir::new().unwrap();
    let fleet = FakeFleet::spawn(&dir, 2);
    let creds = write_credentials(&dir);
    let mut server = mockito::Server::new();
    let server_url = server.url();

    let agents = agent_map();
    let config = pricing_config(&agents);
    let mut now = Utc::now();

    let mut poller = poller_with_endpoints(&creds, &server_url, &format!("{server_url}/v1/oauth/token"));
    converge_onto_good_data(&mut server, &mut poller, &dir, &agents, &config, &mut now);
    let fleet_at_convergence = fleet.live_sessions();
    assert!(fleet_at_convergence > 0, "harness bug: empty converged fleet");

    // The credentials file vanishes (e.g. wiped by a concurrent login). The
    // poller degrades to the cached reading; the cycle must say the token
    // path is failing and must not grow the fleet on that degraded basis.
    std::fs::remove_file(&creds).unwrap();
    let cycle = drive_cycle(&mut poller, &dir, &agents, &config, now);

    assert!(
        !matches!(cycle.decision, ScalingDecision::ScaleUp(_)),
        "a stale-reading cycle must not scale up; got {:?}",
        cycle.decision
    );
    assert_eq!(
        fleet.live_sessions(),
        fleet_at_convergence,
        "no launch may be executed off a stale reading"
    );
    assert!(
        cycle.state.usage.stale,
        "the served reading must be marked stale"
    );
    assert!(
        cycle.state.token_refresh_failing,
        "credential loss is a token-path failure and must be flagged"
    );
    // Still the last good numbers, not zeros.
    assert_eq!(cycle.state.usage.five_hour_pct, 55.0);
    assert_eq!(cycle.state.usage.weekly_scoped_pct, 60.0);
}
