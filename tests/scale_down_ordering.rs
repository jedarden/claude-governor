//! Scale-down ordered by worst verified-closure yield per dollar
//! (claudego-f80857a2).
//!
//! When a window forecasts exhaustion the governor sheds workers. The shed
//! order used to be cost-only — the most expensive pool per worker-hour lost
//! workers first, idle workers ahead of busy ones inside a pool. With the
//! NEEDLE attempt ledger's per-adapter economics available, the pool whose
//! measured spend bought the least verified output sheds first:
//!
//! - two idle workers on adapters of different verified-closure yield — the
//!   worse yield per dollar loses its worker, even when it is the cheap pool;
//! - equal yields (and the ledger-blind entry point) keep today's
//!   cost-per-hour order;
//! - a pool the ledger has no evidence for, or one that verified nothing,
//!   sheds ahead of pools with verified output — but a busy worker is never
//!   selected while an idle candidate exists, whatever the pool-level order
//!   said.
//!
//! Fixtures call `distribute_workers_by_ledger_yield` (the production shed
//! order) and `select_workers_to_stop` (the within-pool idle-first executor)
//! directly: no tmux, no launch stubs, no environment swaps.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use chrono::{Duration, Utc};

use claude_governor::config::{AgentConfig, AlertConfig, GovernorConfig, PricingConfig};
use claude_governor::governor::distribute_workers_by_ledger_yield;
use claude_governor::ledger_yield::AdapterYield;
use claude_governor::state::{self, ModelBurnRate};
use claude_governor::worker::{select_workers_to_stop, Heartbeat};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Build an `AgentConfig` from JSON rather than a struct literal, matching
/// `tests/governor_scaling_fixes.rs`: serde tolerates optional fields the
/// struct may gain later.
fn agent_config(adapter: &str, heartbeat_dir: &Path) -> AgentConfig {
    serde_json::from_value(serde_json::json!({
        "launch_cmd": format!("cgov-launch --agent {adapter} --cwd /tmp/fleet"),
        "session_pattern": "cgovtest-*",
        "heartbeat_dir": heartbeat_dir.to_string_lossy(),
        "min_workers": 0,
        "max_workers": 4,
        "subscription": false,
    }))
    .expect("agent config fixture should deserialize")
}

/// Two idle workers, one per pool: `budget` runs a cheap adapter
/// ($4/worker-hour), `premium` an expensive one ($40/worker-hour). Costs come
/// from burn-rate data keyed by the `--agent` adapter name — which is also
/// the ledger's adapter key — so the cost and yield axes are pinned
/// independently: cost says shed `premium` first, always.
fn two_pool_fleet() -> (HashMap<String, AgentConfig>, HashMap<String, ModelBurnRate>) {
    let hb = Path::new("/tmp/cgovtest-hb").to_path_buf();
    let mut agents = HashMap::new();
    agents.insert("budget".to_string(), agent_config("adapter-bad", &hb));
    agents.insert("premium".to_string(), agent_config("adapter-best", &hb));
    let burn = HashMap::from([
        (
            "adapter-bad".to_string(),
            ModelBurnRate {
                pct_per_worker_per_hour: 0.0,
                dollars_per_worker_per_hour: 4.0,
                samples: 24,
            },
        ),
        (
            "adapter-best".to_string(),
            ModelBurnRate {
                pct_per_worker_per_hour: 0.0,
                dollars_per_worker_per_hour: 40.0,
                samples: 24,
            },
        ),
    ]);
    (agents, burn)
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

fn adapter_yield(adapter: &str, attempts: u64, verified: u64, cost_usd: f64) -> AdapterYield {
    AdapterYield {
        adapter: adapter.to_string(),
        attempts,
        verified,
        verified_yield: (attempts > 0).then(|| verified as f64 / attempts as f64),
        cost_usd,
        cost_per_verified_usd: (verified > 0).then(|| cost_usd / verified as f64),
    }
}

/// Shed one of the fixture fleet's two workers: both pools run one idle
/// worker and the fleet total drops 2 → 1.
fn shed_one(
    agents: &HashMap<String, AgentConfig>,
    burn: &HashMap<String, ModelBurnRate>,
    ledger: Option<&BTreeMap<String, AdapterYield>>,
) -> HashMap<String, u32> {
    let current = HashMap::from([("budget".to_string(), 1u32), ("premium".to_string(), 1u32)]);
    distribute_workers_by_ledger_yield(
        agents,
        &current,
        1,
        burn,
        &governor_config(),
        false,
        &state::CapacityForecast::default(),
        ledger,
    )
}

// ---------------------------------------------------------------------------
// The ledger order dominates the cost order when it has evidence
// ---------------------------------------------------------------------------

#[test]
fn scale_down_stops_the_worst_yield_per_dollar_adapter_first() {
    let (agents, burn) = two_pool_fleet();
    let mut ledger = BTreeMap::new();
    // The cheap adapter burned $50 for 2 verified closures ($25 each); the
    // expensive one is efficient at $1 per verified closure.
    ledger.insert(
        "adapter-bad".to_string(),
        adapter_yield("adapter-bad", 10, 2, 50.0),
    );
    ledger.insert(
        "adapter-best".to_string(),
        adapter_yield("adapter-best", 45, 40, 40.0),
    );

    let result = shed_one(&agents, &burn, Some(&ledger));

    assert_eq!(
        result.get("budget"),
        Some(&0),
        "the cheap pool's terrible yield per dollar must lose its idle worker \
         first — the pre-ledger cost order would have shed premium ($40/hr) \
         and kept budget ($4/hr)"
    );
    assert_eq!(
        result.get("premium"),
        Some(&1),
        "the efficient pool keeps its worker"
    );
    let total: u32 = result.values().sum();
    assert_eq!(total, 1, "exactly one worker is shed");
}

// ---------------------------------------------------------------------------
// Ties keep today's cost order
// ---------------------------------------------------------------------------

#[test]
fn equal_yields_keep_todays_cost_order() {
    let (agents, burn) = two_pool_fleet();
    // Both adapters verified at $2 per closure — the ledger cannot
    // distinguish the pools, so today's cost-per-hour order decides and the
    // expensive pool sheds first.
    let mut ledger = BTreeMap::new();
    ledger.insert(
        "adapter-bad".to_string(),
        adapter_yield("adapter-bad", 20, 10, 20.0),
    );
    ledger.insert(
        "adapter-best".to_string(),
        adapter_yield("adapter-best", 20, 10, 20.0),
    );

    let result = shed_one(&agents, &burn, Some(&ledger));
    assert_eq!(
        result.get("premium"),
        Some(&0),
        "equal yields tie into the cost-per-hour order: the expensive pool sheds first"
    );
    assert_eq!(result.get("budget"), Some(&1));

    // The ledger-blind entry point — `None` economics, which is also what a
    // failed ledger read degrades to — is today's behaviour exactly.
    let blind = shed_one(&agents, &burn, None);
    assert_eq!(blind, result, "no ledger evidence at all: cost order");
}

// ---------------------------------------------------------------------------
// The ledger's blind spots shed before pools with verified output
// ---------------------------------------------------------------------------

#[test]
fn pool_without_ledger_evidence_sheds_before_pools_with_verified_output() {
    let (agents, burn) = two_pool_fleet();
    // `adapter-bad` has no in-window ledger rows: shedding it destroys no
    // measured verified output, so it goes before the pool whose value the
    // ledger can actually point at — even though that pool is the expensive
    // one the cost order would shed first.
    let mut ledger = BTreeMap::new();
    ledger.insert(
        "adapter-best".to_string(),
        adapter_yield("adapter-best", 45, 40, 40.0),
    );

    let result = shed_one(&agents, &burn, Some(&ledger));
    assert_eq!(
        result.get("budget"),
        Some(&0),
        "an unmeasured pool sheds before a pool with verified output"
    );
    assert_eq!(result.get("premium"), Some(&1));
}

#[test]
fn zero_verified_pool_sheds_first_even_when_cheap() {
    let (agents, burn) = two_pool_fleet();
    // The cheap adapter recorded attempts but nothing verified — the worst
    // possible yield per dollar is no yield per dollar — while the expensive
    // one verified 40. Proven waste sheds before proven value, whatever the
    // hourly cost says.
    let mut ledger = BTreeMap::new();
    ledger.insert(
        "adapter-bad".to_string(),
        adapter_yield("adapter-bad", 10, 0, 0.0),
    );
    ledger.insert(
        "adapter-best".to_string(),
        adapter_yield("adapter-best", 45, 40, 40.0),
    );

    let result = shed_one(&agents, &burn, Some(&ledger));
    assert_eq!(
        result.get("budget"),
        Some(&0),
        "nothing verified sheds before anything that verified"
    );
    assert_eq!(result.get("premium"), Some(&1));
}

// ---------------------------------------------------------------------------
// A busy worker is never selected while an idle candidate exists
// ---------------------------------------------------------------------------

#[test]
fn no_busy_worker_is_selected_while_idle_candidates_exist() {
    let now = Utc::now();
    let heartbeat = |session: &str, age: Duration, is_idle: bool| Heartbeat {
        session: session.to_string(),
        timestamp: now - age,
        is_idle,
        current_task: (!is_idle).then(|| "claudego-f80857a2".to_string()),
        model: "glm-5.3-flash".to_string(),
    };
    let heartbeats = HashMap::from([
        (
            "cgovtest-idle-a".to_string(),
            heartbeat("cgovtest-idle-a", Duration::hours(2), true),
        ),
        (
            "cgovtest-busy-b".to_string(),
            heartbeat("cgovtest-busy-b", Duration::minutes(1), false),
        ),
    ]);
    let live: HashSet<String> = heartbeats.keys().cloned().collect();

    let picked = select_workers_to_stop(1, heartbeats.clone(), &live);
    assert_eq!(
        picked,
        vec!["cgovtest-idle-a".to_string()],
        "the idle worker is the shutdown candidate; the busy worker is not \
         selected, whatever the pool-level shed order said"
    );

    // A deeper cut still exhausts the idle candidates before touching the
    // busy one.
    let picked = select_workers_to_stop(2, heartbeats, &live);
    assert_eq!(
        picked.last(),
        Some(&"cgovtest-busy-b".to_string()),
        "the busy worker is the last candidate, never the first"
    );
}
