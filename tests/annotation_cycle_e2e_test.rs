//! Runtime proof that a real governor cycle annotates the SQLite mirror.
//!
//! Every other annotation test calls `db::annotate_window_pct_deltas` or
//! `governor::annotation_skip_reason` directly, with hand-supplied arguments
//! that are mutually consistent by construction. Those tests cannot see the two
//! bugs that actually broke annotation in production, because both were in the
//! *arguments the governor passes*, not in the functions receiving them:
//!
//! - `bf-4igwc`: the worker-count pair compared the governor's tmux census
//!   against the collector's per-`(session, model)` count, so the stability
//!   guard tripped on essentially every cycle of a mixed-model fleet.
//! - `bf-3p0tb`: the span was taken from the collector's aggregation window
//!   rather than from the gap between the two API readings the delta was
//!   measured over, so the delta was attributed to the wrong interval.
//!
//! Both survived four rounds of green tests. This binary closes that gap: it
//! seeds a mirror, seeds a `prev_usage_snapshot`, drives one real
//! `run_governor_cycle`, and asserts the rows came back annotated with the
//! expected apportioned numbers.
//!
//! The failure mode being guarded against is a *silent skip* — the annotation
//! block logs a warning and moves on, and the cycle still returns `Ok(())`. So
//! nothing here asserts on "no error returned": the assertions are on annotated
//! values, and a captured log supplies the skip reason when they are missing.
//!
//! It lives in its own integration-test binary because it overwrites the
//! process-global `HOME` (to redirect `collector::default_db_path()` into a
//! temp dir) and owns the process-global `log` logger — neither is safe to
//! share with tests running concurrently in the same process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, DaemonConfig, GovernorConfig,
    PricingConfig, SprintConfig,
};
use claude_governor::db;
use claude_governor::governor::run_governor_cycle;
use claude_governor::poller::{UsageData, UsagePoller};
use claude_governor::schedule::Promotion;
use claude_governor::state::{self, PrevUsageSnapshot};
use rusqlite::Connection;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// The interval under test
// ---------------------------------------------------------------------------

/// Window percentages the previous cycle's poll returned.
const OLD_5H: f64 = 20.0;
const OLD_7D: f64 = 40.0;
const OLD_7DS: f64 = 30.0;

/// Window percentages this cycle's poll returns. Every window moved up, so the
/// reset guard has nothing to trip on.
const NEW_5H: f64 = 22.0;
const NEW_7D: f64 = 41.0;
const NEW_7DS: f64 = 33.0;

/// The deltas the cycle must apportion: 5h +2.0, 7d +1.0, 7ds +3.0.
const DELTA_5H: f64 = NEW_5H - OLD_5H;
const DELTA_7D: f64 = NEW_7D - OLD_7D;
const DELTA_7DS: f64 = NEW_7DS - OLD_7DS;

/// The fleet inside the delta span: three sessions, three *different* model
/// classes, spending $0.10 / $0.30 / $0.60 of a $1.00 total.
///
/// The mixed models are the point. A sonnet-only fleet has one `i` row per
/// session, so the collector's row count happens to equal the tmux session
/// count and the pre-`bf-4igwc` worker-count comparison accidentally agreed.
/// Production fleets are not sonnet-only, and that is where annotation was
/// silently skipped for months.
const FLEET: [(&str, &str, f64); 3] = [
    ("worker-a", "claude-sonnet-4-5", 0.10),
    ("worker-b", "claude-opus-4-1", 0.30),
    ("worker-c", "claude-haiku-4-5", 0.60),
];

/// Total spend of `FLEET`, the denominator every apportioned share is taken
/// against.
const FLEET_TOTAL_USD: f64 = 1.00;

/// Value written into the *previous* span's `p5h`/`p7d`/`p7ds` columns.
///
/// A sentinel rather than NULL: it proves the cycle left those rows alone,
/// where NULL would only prove it did not newly annotate them.
const ALREADY_ANNOTATED: f64 = 9.9;

/// Spend on the previous span's row. Large enough that if the cycle wrongly
/// claimed that row, the surviving rows' shares would visibly shrink.
const PRIOR_SPAN_USD: f64 = 4.0;

/// Floating-point tolerance for the apportioned values. The arithmetic is a
/// couple of multiplications on exactly-representable-ish decimals, so this is
/// slack, not a fudge factor.
const EPS: f64 = 1e-9;

// ---------------------------------------------------------------------------
// Log capture
// ---------------------------------------------------------------------------

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

fn init_logger() {
    log::set_logger(&TEST_LOGGER).expect("this binary owns the global logger");
    // The skip line is WARN; DEBUG picks up `annotate_window_pct_deltas`'s own
    // guard messages too, so a skip inside the db helper is just as legible as
    // one from `annotation_skip_reason`.
    log::set_max_level(log::LevelFilter::Debug);
}

/// Every captured record containing `pattern`.
fn logs_containing(pattern: &str) -> Vec<String> {
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, msg)| msg.contains(pattern))
        .map(|(level, msg)| format!("[{level}] {msg}"))
        .collect()
}

/// Panic with every reason the cycle gave for not annotating.
///
/// Called before the value assertions so the failure names the guard that
/// tripped instead of just reporting a NULL column.
fn fail_on_any_skip() {
    let mut skips = logs_containing("skipping window delta annotation");
    skips.extend(logs_containing("[annotate] skipping annotation"));
    skips.extend(logs_containing("failed to annotate window pct deltas"));
    assert!(
        skips.is_empty(),
        "the governor cycle skipped annotation instead of performing it:\n  {}",
        skips.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Credential-free poller
// ---------------------------------------------------------------------------

/// Returns one scripted reading, the `new_pct` end of the delta.
struct FakePoller {
    reading: UsageData,
    polls: usize,
}

impl UsagePoller for FakePoller {
    fn poll_usage(&mut self) -> anyhow::Result<UsageData> {
        self.polls += 1;
        Ok(self.reading.clone())
    }
}

fn usage_data(now: DateTime<Utc>) -> UsageData {
    UsageData {
        five_hour_utilization: NEW_5H,
        five_hour_resets_at: (now + ChronoDuration::hours(4)).to_rfc3339(),
        five_hour_hours_remaining: 4.0,
        seven_day_utilization: NEW_7D,
        seven_day_resets_at: (now + ChronoDuration::hours(120)).to_rfc3339(),
        seven_day_hours_remaining: 120.0,
        weekly_scoped_utilization: NEW_7DS,
        weekly_scoped_resets_at: (now + ChronoDuration::hours(120)).to_rfc3339(),
        weekly_scoped_hours_remaining: 120.0,
        // None on both ends, so the model-rotation reset (which clears the
        // snapshot this test depends on) cannot fire.
        weekly_scoped_model: None,
        limits: vec![],
        timestamp: now,
        // `stale` gates the whole annotation block. A stale reading here would
        // make this test pass vacuously, so it is pinned false.
        stale: false,
    }
}

// ---------------------------------------------------------------------------
// Mirror fixtures
// ---------------------------------------------------------------------------

/// One collector pass: `n` instance rows plus the fleet row summarising them.
///
/// `t0` is stamped `t1 - 5min` exactly as `run_collection_pass` does — nominal,
/// and reaching back past the start of a 300 s governor span. Annotation claims
/// rows by `t1` for precisely that reason, and this fixture would be dishonest
/// if it pretended otherwise.
fn seed_pass(
    conn: &Connection,
    t1: DateTime<Utc>,
    rows: &[(&str, &str, f64)],
    annotated: Option<f64>,
) {
    let t0 = (t1 - ChronoDuration::minutes(5)).to_rfc3339();
    let t1 = t1.to_rfc3339();

    for (sess, model, total_usd) in rows {
        let mut record = serde_json::json!({
            "r": "i", "ts": t1, "t0": t0, "t1": t1,
            "sess": sess, "sid": sess, "model": model,
            "pk": 1, "hr_et": 10, "dow": 2,
            "input-n": 0, "input-usd": 0.0,
            "output-n": 0, "output-usd": 0.0,
            "r-cache-n": 0, "r-cache-usd": 0.0,
            "w-cache-n": 0, "w-cache-usd": 0.0,
            "w-cache-1h-n": 0, "w-cache-1h-usd": 0.0,
            "total-usd": total_usd, "cache-eff": 0.0,
        });
        if let Some(v) = annotated {
            record["p5h"] = v.into();
            record["p7d"] = v.into();
            record["p7ds"] = v.into();
        }
        db::insert_instance(conn, &record).expect("seeding an i row should succeed");
    }

    let total_usd: f64 = rows.iter().map(|(_, _, usd)| *usd).sum();
    let mut fleet = serde_json::json!({
        "r": "f", "ts": t1, "t0": t0, "t1": t1,
        "pk": 1, "hr_et": 10, "dow": 2,
        // The collector sets `workers` to `instances.len()` — one entry per
        // (session, model) pair — and the governor reads exactly this field
        // back for both ends of the worker-count guard.
        "workers": rows.len(),
        "total-usd": total_usd, "p75-usd-hr": 5.0, "std-usd-hr": 1.0,
        "fleet-cache-eff": 0.5, "cache-eff-p25": 0.4,
    });
    if let Some(v) = annotated {
        fleet["p5h"] = v.into();
        fleet["p7d"] = v.into();
        fleet["p7ds"] = v.into();
        fleet["usd-per-pct-7ds"] = v.into();
    }
    db::insert_fleet(conn, &fleet).expect("seeding an f row should succeed");
}

/// The three annotated columns of the `i` row for `sess`, NULL included.
fn instance_annotation(conn: &Connection, sess: &str) -> (Option<f64>, Option<f64>, Option<f64>) {
    conn.query_row(
        "SELECT p5h, p7d, p7ds FROM i WHERE sess = ?",
        [sess],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap_or_else(|e| panic!("no i row for session {sess}: {e}"))
}

/// The annotated columns of the `f` row whose `t1` is `t1`.
fn fleet_annotation(
    conn: &Connection,
    t1: DateTime<Utc>,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    conn.query_row(
        "SELECT p5h, p7d, p7ds, usd_per_pct_7ds FROM f WHERE t1 = ?",
        [t1.to_rfc3339()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .unwrap_or_else(|e| panic!("no f row at t1={}: {e}", t1.to_rfc3339()))
}

/// Assert `actual` is non-NULL and equals `expected`.
///
/// The NULL case gets its own message because it is the signature of a silent
/// skip, and "expected 0.2, got None" reads as an arithmetic bug rather than as
/// "the annotation block never ran".
fn assert_annotated(label: &str, actual: Option<f64>, expected: f64) {
    let Some(actual) = actual else {
        panic!("{label} is still NULL — the cycle did not annotate this row (expected {expected})");
    };
    assert!(
        (actual - expected).abs() < EPS,
        "{label}: expected {expected}, got {actual}"
    );
}

// ---------------------------------------------------------------------------
// Cycle wiring
// ---------------------------------------------------------------------------

fn minimal_pricing_config() -> GovernorConfig {
    GovernorConfig {
        pricing: PricingConfig {
            models: HashMap::new(),
        },
        sprint: SprintConfig::default(),
        daemon: DaemonConfig::default(),
        alerts: AlertConfig::default(),
        composite_risk: CompositeRiskConfig::default(),
        cone_scaling: ConeScalingConfig::default(),
        agents: HashMap::new(),
        credentials_path: None,
    }
}

/// Drive one real cycle, everything but the poller at defaults.
///
/// `dry_run = true` keeps the cycle off the tmux scaling path. The annotation
/// block runs regardless: it sits before scaling and reads only the mirror and
/// the snapshot.
fn drive_cycle(poller: &mut FakePoller, state_path: &std::path::Path) -> anyhow::Result<()> {
    let agents: HashMap<String, AgentConfig> = HashMap::new();
    let promotions: Vec<Promotion> = Vec::new();

    run_governor_cycle(
        poller,
        state_path,
        true, // dry_run
        300,  // loop_interval
        2.0,  // hysteresis_band
        3,    // max_up_per_cycle
        2,    // max_down_per_cycle
        90.0, // target_ceiling
        &AlertConfig::default(),
        &agents,
        0, // pre_scale_minutes (disabled)
        &promotions,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &minimal_pricing_config(),
    )
}

/// Point `collector::default_db_path()` (and every other `~`-rooted path the
/// cycle touches) at a temp dir.
///
/// Also guarantees `~/.claude/projects` is absent, so `run_collection_pass`
/// finds no JSONL files and returns before writing anything — the seeded rows
/// below are the only rows in the mirror.
fn redirect_home(home: &TempDir) -> PathBuf {
    std::env::set_var("HOME", home.path());
    let db_path = claude_governor::collector::default_db_path();
    assert!(
        db_path.starts_with(home.path()),
        "HOME redirect did not take: db path is {}",
        db_path.display()
    );
    db_path
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// One real cycle over a mixed-model fleet must annotate the mirror.
///
/// Timeline, with `base` the instant the test starts and the cycle stamping its
/// own `now` a few milliseconds later:
///
/// ```text
///   base-610s ..... base-310s      base-330s .......... base-30s
///   [-- prior collector pass --]   [-- in-span collector pass --]
///                          |
///              base-300s   |                                     cycle now
///                  [========== the delta span ==================]
/// ```
///
/// The span is `[prev_snapshot.taken_at, now]` — the gap the +2.0/+1.0/+3.0
/// delta was measured over — and a pass belongs to the span containing its
/// `t1`. So the in-span pass is annotated and the prior pass is not, even
/// though the prior pass's `t1` falls inside the in-span pass's nominal
/// `[t0, t1]`. That is the distinction `bf-3p0tb` got wrong.
#[test]
fn a_real_cycle_annotates_a_mixed_model_fleet() {
    init_logger();

    let home = TempDir::new().expect("failed to create temp HOME");
    let db_path = redirect_home(&home);
    let state_path = home.path().join("governor-state.json");

    let base = Utc::now();
    let span_start = base - ChronoDuration::seconds(300);
    // Inside the span, and far enough from `now` that the cycle's own clock
    // cannot drift past it.
    let in_span_t1 = base - ChronoDuration::seconds(30);
    // Before the span opens: this pass belongs to the previous governor cycle,
    // which already annotated it.
    let prior_t1 = base - ChronoDuration::seconds(310);

    // --- Seed the mirror ---------------------------------------------------
    {
        let conn = db::open_db(&db_path).expect("failed to open the mirror");
        db::create_schema(&conn).expect("failed to create the mirror schema");
        seed_pass(
            &conn,
            prior_t1,
            &[("worker-prior", "claude-sonnet-4-5", PRIOR_SPAN_USD)],
            Some(ALREADY_ANNOTATED),
        );
        seed_pass(&conn, in_span_t1, &FLEET, None);
    }

    // --- Seed the snapshot the delta is measured from ----------------------
    {
        let mut seeded = state::GovernorState::new();
        seeded.burn_rate.prev_usage_snapshot = Some(PrevUsageSnapshot {
            taken_at: span_start,
            five_hour_pct: OLD_5H,
            seven_day_pct: OLD_7D,
            weekly_scoped_pct: OLD_7DS,
        });
        state::save_state(&seeded, &state_path).expect("failed to seed the state file");
    }

    // --- Drive the cycle ---------------------------------------------------
    let mut poller = FakePoller {
        reading: usage_data(base),
        polls: 0,
    };
    drive_cycle(&mut poller, &state_path).expect("the cycle should complete");
    assert_eq!(poller.polls, 1, "the cycle should have polled exactly once");

    // Name the guard before reporting the symptom.
    fail_on_any_skip();

    let conn = db::open_db(&db_path).expect("failed to reopen the mirror");

    // --- The in-span instance rows carry their share of each delta ---------
    for (sess, model, total_usd) in FLEET {
        let weight = total_usd / FLEET_TOTAL_USD;
        let (p5h, p7d, p7ds) = instance_annotation(&conn, sess);
        assert_annotated(&format!("i[{sess} {model}].p5h"), p5h, DELTA_5H * weight);
        assert_annotated(&format!("i[{sess} {model}].p7d"), p7d, DELTA_7D * weight);
        assert_annotated(&format!("i[{sess} {model}].p7ds"), p7ds, DELTA_7DS * weight);
    }

    // --- The in-span fleet row carries the whole delta ---------------------
    //
    // The span claims exactly one collector pass, so that row's share of the
    // span's fleet spend is 1.0.
    let (p5h, p7d, p7ds, usd_per_pct) = fleet_annotation(&conn, in_span_t1);
    assert_annotated("f[in-span].p5h", p5h, DELTA_5H);
    assert_annotated("f[in-span].p7d", p7d, DELTA_7D);
    assert_annotated("f[in-span].p7ds", p7ds, DELTA_7DS);
    assert_annotated(
        "f[in-span].usd_per_pct_7ds",
        usd_per_pct,
        FLEET_TOTAL_USD / DELTA_7DS,
    );

    // --- The previous span's rows are untouched ----------------------------
    //
    // `worker-prior`'s `t1` sits inside the in-span pass's nominal `[t0, t1]`
    // but outside the delta span, so keying annotation to the collector's
    // window instead of the poll span would rewrite these — and would dilute
    // every share above, since $4.00 of prior spend would join a $1.00
    // denominator.
    let (p5h, p7d, p7ds) = instance_annotation(&conn, "worker-prior");
    assert_annotated("i[worker-prior].p5h", p5h, ALREADY_ANNOTATED);
    assert_annotated("i[worker-prior].p7d", p7d, ALREADY_ANNOTATED);
    assert_annotated("i[worker-prior].p7ds", p7ds, ALREADY_ANNOTATED);

    let (p5h, p7d, p7ds, usd_per_pct) = fleet_annotation(&conn, prior_t1);
    assert_annotated("f[prior].p5h", p5h, ALREADY_ANNOTATED);
    assert_annotated("f[prior].p7d", p7d, ALREADY_ANNOTATED);
    assert_annotated("f[prior].p7ds", p7ds, ALREADY_ANNOTATED);
    assert_annotated("f[prior].usd_per_pct_7ds", usd_per_pct, ALREADY_ANNOTATED);
}
