//! Runtime proof that empirical promotion validation succeeds on the live path.
//!
//! `bf-3vbt4` proved a real governor cycle annotates the mirror. `bf-5y6qi`
//! proved that annotated rows are readable by `compute_empirical_promo_ratio`
//! and by the `instance_compare` / `promo_check` views. Both stopped short of
//! the join between them: every promotion-validation test to date annotated its
//! fixture by calling `db::annotate_window_pct_deltas` directly, so none of them
//! could tell whether the rows a *real cycle* leaves behind are the rows the
//! validator can actually consume.
//!
//! That join is the parent bead's (`bf-42ovy`) whole purpose. Before the
//! call-site fixes, `validate_promotion_from_db` returned `no data found in
//! token-history DB` forever — `compute_empirical_promo_ratio` selects
//! `WHERE p7ds IS NOT NULL AND p7ds > 0`, and nothing ever wrote `p7ds`. The
//! governor swallowed that failure into `effective_multiplier`'s conservative
//! 1.0 fallback, so a declared 2x promotion was silently discarded for the
//! whole of every promotion window, with nothing in the state file to
//! distinguish "validated at 1.0" from "never had any data".
//!
//! So this binary drives two real `run_governor_cycle` calls over seeded
//! collector data and asserts on what the *governor itself* wrote:
//!
//! 1. after the cycles, the mirror holds `i` rows with non-NULL, positive
//!    `p7ds` — 10 peak and 10 off-peak;
//! 2. `compute_empirical_promo_ratio` returns `Some` against that data;
//! 3. `validate_promotion_from_db` no longer returns `no data found`, and the
//!    cycle's own state file shows the promotion validated off real samples
//!    rather than falling back to 1.0;
//! 4. `instance_compare` and `promo_check` return non-NULL `usd_per_pct_7ds`;
//! 5. the API-delta EMA that scaling runs on is bit-identical whether or not
//!    the mirror carries annotations, and an unannotated mirror still produces
//!    exactly the old conservative fallback.
//!
//! The first cycle deliberately annotates only peak rows, so the test also
//! captures the *old* failure mode in passing: with peak samples but no
//! off-peak samples, `compute_empirical_promo_ratio` returns `None` and the
//! validator reports `no data found` — the exact string the parent bead exists
//! to eliminate. Cycle two supplies the off-peak side and the string goes away.
//!
//! Every test here roots its cycle's `~`-rooted paths (collector mirror,
//! accuracy log) in its own `TempDir` via `CyclePaths::under`, so the binary
//! no longer overwrites the process-global `HOME` and its tests no longer
//! need to serialise on a lock.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use claude_governor::burn_rate::{
    compute_empirical_promo_ratio, effective_multiplier, validate_promotion_from_db,
};
use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, DaemonConfig, GovernorConfig,
    PricingConfig, SprintConfig,
};
use claude_governor::db;
use claude_governor::governor::{run_governor_cycle, CyclePaths};
use claude_governor::poller::{UsageData, UsagePoller};
use claude_governor::schedule::Promotion;
use claude_governor::state::{self, GovernorState, PrevUsageSnapshot};
use rusqlite::Connection;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// The two intervals under test
// ---------------------------------------------------------------------------

/// Window percentages before the first cycle's span.
const PCT_0: (f64, f64, f64) = (20.0, 40.0, 30.0);
/// After the first cycle's span — every window up 3.0, so no reset guard trips.
const PCT_1: (f64, f64, f64) = (23.0, 43.0, 33.0);
/// After the second cycle's span — up another 3.0.
const PCT_2: (f64, f64, f64) = (26.0, 46.0, 36.0);

/// The 7d-scoped delta each span carries. Equal across both spans on purpose:
/// the observed ratio is `(offpeak_tokens / p7ds_offpeak) / (peak_tokens /
/// p7ds_peak)`, so equal deltas make it depend only on the token counts, and
/// the expected 2.0 below is arithmetic rather than a fitted constant.
const DELTA_7DS: f64 = PCT_1.2 - PCT_0.2;

/// Rows per batch. `compute_empirical_promo_ratio` requires 10 of each side
/// before it reports `sufficient_data`, and `validate_promotion_from_db`
/// refuses to validate below that, so 10 is the smallest honest fixture.
const BATCH: usize = 10;

/// Spend per instance row. Equal across every row so each row's apportioned
/// share of its span's delta is `DELTA_7DS / BATCH`, identical on both sides.
const USD_PER_ROW: f64 = 1.0;

/// Tokens on a peak row, and double that on an off-peak row — the promotion
/// under test claims off-peak percentage buys 2x the tokens.
const PEAK_TOKENS: u64 = 70_000;
const OFFPEAK_TOKENS: u64 = 2 * PEAK_TOKENS;

/// The multiplier the promotion declares, and the ratio the seeded data
/// implies. They agree, so a validator reading real data validates; a validator
/// reading nothing falls back to 1.0. That gap is what makes point 3 visible.
const DECLARED_MULTIPLIER: f64 = 2.0;

/// `validate_promotion_from_db`'s tolerance is 10% of the declared multiplier;
/// this is tighter, because the fixture's arithmetic is exact and any drift
/// means the apportionment changed rather than that the data got noisy.
const RATIO_EPS: f64 = 1e-6;

/// The sentinel this whole bead exists to eliminate.
const NO_DATA_REASON: &str = "no data found in token-history DB";

// ---------------------------------------------------------------------------
// Credential-free poller
// ---------------------------------------------------------------------------

/// Returns one scripted reading — the far end of the span under test.
struct FakePoller {
    reading: UsageData,
}

impl UsagePoller for FakePoller {
    fn poll_usage(&mut self) -> anyhow::Result<UsageData> {
        Ok(self.reading.clone())
    }
}

fn usage_data(pct: (f64, f64, f64), now: DateTime<Utc>) -> UsageData {
    UsageData {
        five_hour_utilization: pct.0,
        five_hour_resets_at: (now + ChronoDuration::hours(4)).to_rfc3339(),
        five_hour_hours_remaining: 4.0,
        seven_day_utilization: pct.1,
        seven_day_resets_at: (now + ChronoDuration::hours(120)).to_rfc3339(),
        seven_day_hours_remaining: 120.0,
        weekly_scoped_utilization: pct.2,
        weekly_scoped_resets_at: (now + ChronoDuration::hours(120)).to_rfc3339(),
        weekly_scoped_hours_remaining: 120.0,
        // None on both ends, so the model-rotation reset (which clears the
        // snapshot the annotation span is measured from) cannot fire.
        weekly_scoped_model: None,
        limits: vec![],
        timestamp: now,
        // `stale` gates the whole annotation block; a stale reading would make
        // every assertion below pass vacuously, so it is pinned false.
        stale: false,
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One collector pass: `BATCH` instance rows plus the fleet row summarising
/// them, all stamped `t1`.
///
/// `peak` sets the `pk` column the validator partitions on, and picks a
/// plausible `hr_et` band to go with it. `pk` is the collector's own
/// classification of the interval, not a re-derivation from wall-clock, which
/// is why this fixture can seed both sides regardless of when the test runs.
///
/// `annotated` pre-fills `p5h`/`p7d`/`p7ds`; `None` leaves them NULL, which is
/// what the collector writes and therefore the state a cycle must move off.
fn seed_pass(
    conn: &Connection,
    t1: DateTime<Utc>,
    label: &str,
    peak: bool,
    tokens: u64,
    annotated: Option<f64>,
) {
    let t0 = (t1 - ChronoDuration::minutes(5)).to_rfc3339();
    let t1_str = t1.to_rfc3339();

    for i in 0..BATCH {
        // Peak hours 8..14 ET, off-peak 14..24 — the bands `schedule` uses.
        let hr_et = if peak { 8 + (i % 6) } else { 14 + (i % 10) };
        let mut record = serde_json::json!({
            "r": "i", "ts": t1_str, "t0": t0, "t1": t1_str,
            "sess": format!("{label}-{i}"), "sid": format!("{label}-{i}"),
            "model": "claude-sonnet-4-5",
            "pk": if peak { 1 } else { 0 }, "hr_et": hr_et, "dow": 2,
            "input-n": tokens, "input-usd": USD_PER_ROW,
            "output-n": 0, "output-usd": 0.0,
            "r-cache-n": 0, "r-cache-usd": 0.0,
            "w-cache-n": 0, "w-cache-usd": 0.0,
            "w-cache-1h-n": 0, "w-cache-1h-usd": 0.0,
            "total-usd": USD_PER_ROW, "cache-eff": 0.0,
        });
        if let Some(v) = annotated {
            record["p5h"] = v.into();
            record["p7d"] = v.into();
            record["p7ds"] = v.into();
        }
        db::insert_instance(conn, &record).expect("seeding an i row should succeed");
    }

    let mut fleet = serde_json::json!({
        "r": "f", "ts": t1_str, "t0": t0, "t1": t1_str,
        "pk": if peak { 1 } else { 0 }, "hr_et": 10, "dow": 2,
        // The collector sets `workers` to `instances.len()`, and the governor
        // reads exactly this field back for both ends of the worker guard.
        "workers": BATCH,
        "total-usd": USD_PER_ROW * BATCH as f64,
        "p75-usd-hr": 5.0, "std-usd-hr": 1.0,
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

fn open_seeded_mirror(db_path: &Path) -> Connection {
    let conn = db::open_db(db_path).expect("failed to open the mirror");
    db::create_schema(&conn).expect("failed to create the mirror schema");
    conn
}

/// Write the state the next cycle starts from: a previous API reading taken
/// `span_secs` ago.
///
/// Back-dating rather than sleeping is the point. A cycle's annotation span is
/// `[prev_snapshot.taken_at, now]`, and the elapsed guard rejects anything
/// under 2 minutes — a test cannot wait that out twice. Back-dating produces
/// exactly the snapshot the daemon's own clock would have left behind five
/// minutes earlier, and every other input to the cycle stays real.
fn seed_snapshot(state_path: &Path, pct: (f64, f64, f64), taken_at: DateTime<Utc>) {
    let mut seeded = state::load_state(state_path).expect("failed to load state");
    seeded.burn_rate.prev_usage_snapshot = Some(PrevUsageSnapshot {
        taken_at,
        five_hour_pct: pct.0,
        seven_day_pct: pct.1,
        weekly_scoped_pct: pct.2,
    });
    state::save_state(&seeded, state_path).expect("failed to seed the state file");
}

/// A promotion that is active whenever this test runs.
///
/// The bounds are UTC dates ±2 days wide; `is_promo_active_at` compares against
/// the *Eastern* date, which is never more than a day from the UTC one, so the
/// window contains it no matter the hour the suite runs at.
fn active_promotion(now: DateTime<Utc>) -> Promotion {
    Promotion {
        name: "live-path-test".to_string(),
        start_date: (now - ChronoDuration::days(2))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string(),
        end_date: (now + ChronoDuration::days(2))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string(),
        peak_start_hour_et: 8,
        peak_end_hour_et: 14,
        offpeak_multiplier: DECLARED_MULTIPLIER,
        applies_to: vec![
            "five_hour".to_string(),
            "seven_day".to_string(),
            "weekly_scoped".to_string(),
        ],
    }
}

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

/// Drive one real cycle, everything but the poller and the promotions at
/// defaults.
///
/// `dry_run = true` keeps the cycle off the tmux scaling path. Neither the
/// annotation block nor the promotion validation is affected by it: both read
/// the mirror and the state file and write back to them. `paths` names the
/// temp-rooted collector/calibration layout the cycle runs against — the same
/// `CyclePaths::under(home)` the caller seeded the mirror through.
fn drive_cycle(
    pct: (f64, f64, f64),
    state_path: &Path,
    paths: &CyclePaths,
    promotions: &[Promotion],
) {
    let mut poller = FakePoller {
        reading: usage_data(pct, Utc::now()),
    };
    let agents: HashMap<String, AgentConfig> = HashMap::new();

    run_governor_cycle(
        &mut poller,
        state_path,
        paths,
        true, // dry_run
        300,  // loop_interval
        2.0,  // hysteresis_band
        3,    // max_up_per_cycle
        2,    // max_down_per_cycle
        90.0, // target_ceiling
        &AlertConfig::default(),
        &agents,
        0, // pre_scale_minutes (disabled)
        promotions,
        &CompositeRiskConfig::default(),
        &ConeScalingConfig::default(),
        &minimal_pricing_config(),
    )
    .expect("the cycle should complete");
}

// ---------------------------------------------------------------------------
// Mirror queries
// ---------------------------------------------------------------------------

/// Count `i` rows on one side of the peak split that carry usable annotation —
/// exactly the predicate `compute_empirical_promo_ratio` selects on.
fn annotated_rows(conn: &Connection, peak: bool) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM i WHERE pk = ? AND p7ds IS NOT NULL AND p7ds > 0",
        [i64::from(peak)],
        |row| row.get(0),
    )
    .expect("counting annotated rows should succeed")
}

/// Rows the given view exposes with a non-NULL `usd_per_pct_7ds`.
///
/// Both views compute that column as `total_usd / p7ds` guarded by
/// `p7ds IS NOT NULL AND p7ds > 0`, so it is NULL for the entire history until
/// something annotates — which is the second half of what the parent bead
/// unblocks.
fn view_rows_with_usd_per_pct(conn: &Connection, view: &str) -> i64 {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {view} WHERE usd_per_pct_7ds IS NOT NULL"),
        [],
        |row| row.get(0),
    )
    .unwrap_or_else(|e| panic!("querying {view} should succeed: {e}"))
}

// ---------------------------------------------------------------------------
// 1-4: the live path, end to end
// ---------------------------------------------------------------------------

/// Two real cycles over seeded collector data unblock empirical validation.
///
/// Timeline, with `base` the instant the test starts:
///
/// ```text
///   base-600s                                        base
///       [============ cycle 1's span ==================]
///                  ^ peak batch (t1 = base-450s)
///                        base-300s                  base
///                            [==== cycle 2's span =====]
///                                        ^ off-peak batch (t1 = base-100s)
/// ```
///
/// The batches are seeded one cycle apart precisely so the spans partition
/// them: the peak batch's `t1` falls before cycle 2's span opens, so cycle 2
/// annotates only the off-peak batch and leaves the peak batch's values alone.
/// Each side therefore carries its own span's delta, apportioned across its own
/// `BATCH` rows, which is what makes the observed ratio interpretable.
#[test]
fn two_real_cycles_unblock_empirical_promotion_validation() {
    let home = TempDir::new().expect("failed to create temp HOME");
    let cycle_paths = CyclePaths::under(home.path());
    let db_path = cycle_paths.collector.db_path.clone();
    let state_path = home.path().join("governor-state.json");

    let base = Utc::now();
    let promotions = vec![active_promotion(base)];

    // --- Cycle 1: the peak side ------------------------------------------
    {
        let conn = open_seeded_mirror(&db_path);
        seed_pass(
            &conn,
            base - ChronoDuration::seconds(450),
            "peak",
            true,
            PEAK_TOKENS,
            None,
        );
    }
    seed_snapshot(&state_path, PCT_0, base - ChronoDuration::seconds(600));
    drive_cycle(PCT_1, &state_path, &cycle_paths, &promotions);

    // With peak rows annotated but no off-peak rows, the validator still has
    // nothing to compare against — `compute_empirical_promo_ratio` needs both
    // sides non-empty. This is the pre-fix production symptom, reproduced here
    // on purpose so the assertion after cycle 2 is a real transition and not a
    // fixture that was already green.
    let after_first = validate_promotion_from_db(&db_path, DECLARED_MULTIPLIER);
    assert_eq!(
        after_first.reason.as_deref(),
        Some(NO_DATA_REASON),
        "one-sided data should still report the no-data sentinel"
    );

    // --- Cycle 2: the off-peak side ---------------------------------------
    //
    // Seeded after cycle 1 has run, so these rows were never inside cycle 1's
    // span and the peak batch is already outside cycle 2's.
    {
        let conn = db::open_db(&db_path).expect("failed to reopen the mirror");
        seed_pass(
            &conn,
            base - ChronoDuration::seconds(100),
            "offpeak",
            false,
            OFFPEAK_TOKENS,
            None,
        );
    }
    seed_snapshot(&state_path, PCT_1, base - ChronoDuration::seconds(300));
    drive_cycle(PCT_2, &state_path, &cycle_paths, &promotions);

    let conn = db::open_db(&db_path).expect("failed to reopen the mirror");

    // --- 1. The cycles left annotated rows behind -------------------------
    //
    // NULL here is the signature of a silent skip: the annotation block logs a
    // warning and the cycle still returns `Ok(())`, so only the rows can say
    // whether it ran.
    assert_eq!(
        annotated_rows(&conn, true),
        BATCH as i64,
        "the peak batch should carry non-NULL positive p7ds after cycle 1"
    );
    assert_eq!(
        annotated_rows(&conn, false),
        BATCH as i64,
        "the off-peak batch should carry non-NULL positive p7ds after cycle 2"
    );

    // Each row got its span's delta apportioned across an equally-funded
    // batch, so both sides land on the same share.
    let expected_p7ds = DELTA_7DS / BATCH as f64;
    let p7ds_range: (f64, f64) = conn
        .query_row("SELECT MIN(p7ds), MAX(p7ds) FROM i", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("reading the annotated range should succeed");
    assert!(
        (p7ds_range.0 - expected_p7ds).abs() < RATIO_EPS
            && (p7ds_range.1 - expected_p7ds).abs() < RATIO_EPS,
        "every row should carry p7ds {expected_p7ds}, got range {p7ds_range:?}"
    );

    // --- 2. compute_empirical_promo_ratio reads them ----------------------
    let ratio = compute_empirical_promo_ratio(&db_path)
        .expect("compute_empirical_promo_ratio should return Some against cycle-annotated data");
    assert_eq!(ratio.peak_samples, BATCH, "peak samples");
    assert_eq!(ratio.offpeak_samples, BATCH, "off-peak samples");
    assert!(ratio.sufficient_data, "10 of each side is sufficient");
    // Equal p7ds on both sides, so the ratio is purely the token ratio: 2x.
    assert!(
        (ratio.observed_ratio - DECLARED_MULTIPLIER).abs() < RATIO_EPS,
        "expected observed ratio {DECLARED_MULTIPLIER}, got {}",
        ratio.observed_ratio
    );

    // --- 3. The no-data failure is gone, and 1.0 is not silently used -----
    let validation = validate_promotion_from_db(&db_path, DECLARED_MULTIPLIER);
    assert_ne!(
        validation.reason.as_deref(),
        Some(NO_DATA_REASON),
        "the sentinel this bead exists to eliminate is still being returned"
    );
    assert!(
        validation.validated,
        "a 2.0 observed ratio against a declared 2.0 should validate, got reason {:?}",
        validation.reason
    );
    assert!(
        (effective_multiplier(&validation) - DECLARED_MULTIPLIER).abs() < RATIO_EPS,
        "the declared multiplier should survive validation, not collapse to the 1.0 fallback"
    );

    // The same conclusion, reached by the governor rather than by this test:
    // cycle 2 ran the validation itself and persisted the outcome.
    let persisted = state::load_state(&state_path).expect("failed to read the cycle's state");
    assert!(
        persisted.burn_rate.promotion_validated,
        "the cycle should have recorded the promotion as validated"
    );
    assert_eq!(
        persisted.burn_rate.promotion_peak_samples, BATCH,
        "the cycle's own validation should have seen the peak batch"
    );
    assert_eq!(
        persisted.burn_rate.promotion_offpeak_samples, BATCH,
        "the cycle's own validation should have seen the off-peak batch"
    );
    assert!(
        (persisted.burn_rate.offpeak_ratio_observed - DECLARED_MULTIPLIER).abs() < RATIO_EPS,
        "the cycle recorded observed ratio {}, expected {DECLARED_MULTIPLIER}",
        persisted.burn_rate.offpeak_ratio_observed
    );

    // --- 4. The analytics views come alive --------------------------------
    assert_eq!(
        view_rows_with_usd_per_pct(&conn, "instance_compare"),
        2 * BATCH as i64,
        "instance_compare should expose usd_per_pct_7ds for every annotated row"
    );
    assert!(
        view_rows_with_usd_per_pct(&conn, "promo_check") > 0,
        "promo_check should expose usd_per_pct_7ds for the annotated groups"
    );
    // The value, not just its non-NULL-ness: $1.00 of spend against a
    // `DELTA_7DS / BATCH` share of a percent.
    let usd_per_pct: f64 = conn
        .query_row(
            "SELECT usd_per_pct_7ds FROM instance_compare WHERE sess = 'peak-0'",
            [],
            |row| row.get(0),
        )
        .expect("instance_compare should have a row for peak-0");
    assert!(
        (usd_per_pct - USD_PER_ROW / expected_p7ds).abs() < RATIO_EPS,
        "expected usd_per_pct_7ds {}, got {usd_per_pct}",
        USD_PER_ROW / expected_p7ds
    );
}

// ---------------------------------------------------------------------------
// 5: nothing changes when annotation data is absent
// ---------------------------------------------------------------------------

/// Run one cycle against two mirrors that differ only in whether their rows are
/// annotated, and return each run's persisted state.
///
/// Both runs get the same seeded rows, the same snapshot, the same reading and
/// the same promotion. The only difference is the `annotated` argument to
/// `seed_pass`, so any divergence in the result is attributable to annotation
/// data and nothing else.
fn cycle_with_and_without_annotation() -> (GovernorState, GovernorState) {
    let mut states = Vec::new();

    for annotated in [None, Some(DELTA_7DS / BATCH as f64)] {
        let home = TempDir::new().expect("failed to create temp HOME");
        let cycle_paths = CyclePaths::under(home.path());
        let db_path = cycle_paths.collector.db_path.clone();
        let state_path = home.path().join("governor-state.json");
        let base = Utc::now();

        {
            let conn = open_seeded_mirror(&db_path);
            seed_pass(
                &conn,
                base - ChronoDuration::seconds(100),
                "peak",
                true,
                PEAK_TOKENS,
                annotated,
            );
            seed_pass(
                &conn,
                base - ChronoDuration::seconds(100),
                "offpeak",
                false,
                OFFPEAK_TOKENS,
                annotated,
            );
        }
        seed_snapshot(&state_path, PCT_0, base - ChronoDuration::seconds(300));
        drive_cycle(PCT_1, &state_path, &cycle_paths, &[active_promotion(base)]);

        states.push(state::load_state(&state_path).expect("failed to read the cycle's state"));
    }

    let annotated = states.pop().expect("two runs");
    let bare = states.pop().expect("two runs");
    (bare, annotated)
}

/// Relative tolerance for comparing time-normalized EMAs across the two runs
/// of `cycle_with_and_without_annotation`.
///
/// Each run seeds its previous snapshot at `base - 300s` and the cycle polls
/// at the real current instant, so every pct/hr (and usd/pct) EMA divides by
/// a span that includes the wall-clock time between that run's `base` and its
/// poll — fsync and scheduling jitter of a second or two on a loaded host.
/// That jitter moves both runs equally (~0.3% per second on the 300s span)
/// and is not what this test is about; a divergence beyond 1% would be.
const SPAN_JITTER_TOLERANCE: f64 = 0.01;

/// Assert two time-normalized EMA readings agree within wall-clock span
/// jitter (see [`SPAN_JITTER_TOLERANCE`]).
fn assert_ema_close(label: &str, bare: f64, annotated: f64) {
    assert!(
        ((bare - annotated) / annotated).abs() < SPAN_JITTER_TOLERANCE,
        "{label}: bare {bare} vs annotated {annotated} diverged beyond span jitter"
    );
}

/// The burn-rate EMA that scaling reads is derived from consecutive API
/// readings alone, and annotation must not perturb it.
///
/// The annotation block sits *after* the EMA update in the cycle and shares no
/// inputs with it, so this is a structural property — but it is a property a
/// future change could quietly break by sourcing pct/hr from the annotated
/// columns, which look like a more direct measurement than they are. (They are
/// the same API deltas, apportioned; feeding them back would close a loop.)
#[test]
fn the_api_delta_ema_is_unaffected_by_annotation_data() {
    let (bare, annotated) = cycle_with_and_without_annotation();

    // `WindowPctDeltas` is not `PartialEq`, so compare it field by field —
    // spelling the windows out also names which one drifted on failure.
    assert_ema_close(
        "five_hour pct/hr EMA",
        bare.burn_rate.fleet_pct_hr_ema.five_hour,
        annotated.burn_rate.fleet_pct_hr_ema.five_hour,
    );
    assert_ema_close(
        "seven_day pct/hr EMA",
        bare.burn_rate.fleet_pct_hr_ema.seven_day,
        annotated.burn_rate.fleet_pct_hr_ema.seven_day,
    );
    assert_ema_close(
        "weekly_scoped pct/hr EMA",
        bare.burn_rate.fleet_pct_hr_ema.weekly_scoped,
        annotated.burn_rate.fleet_pct_hr_ema.weekly_scoped,
    );
    assert_ema_close(
        "five_hour usd/pct EMA",
        bare.burn_rate.usd_per_pct_ema_five_hour,
        annotated.burn_rate.usd_per_pct_ema_five_hour,
    );
    assert_ema_close(
        "seven_day usd/pct EMA",
        bare.burn_rate.usd_per_pct_ema_seven_day,
        annotated.burn_rate.usd_per_pct_ema_seven_day,
    );
    assert_ema_close(
        "weekly_scoped usd/pct EMA",
        bare.burn_rate.usd_per_pct_ema_weekly_scoped,
        annotated.burn_rate.usd_per_pct_ema_weekly_scoped,
    );
    assert_eq!(
        bare.burn_rate.fleet_pct_ema_samples, annotated.burn_rate.fleet_pct_ema_samples,
        "the EMA sample count should not depend on annotation data"
    );
    // The EMA is only meaningful if it actually moved; equal-but-zero would
    // satisfy every assertion above without exercising anything.
    assert!(
        bare.burn_rate.fleet_pct_ema_samples > 0,
        "the cycle should have fed the EMA a sample"
    );
    // And the jitter allowance must not be masking a real signal: the seeded
    // span is 300s, so both EMAs should be far from zero.
    assert!(
        bare.burn_rate.fleet_pct_hr_ema.five_hour > 0.0,
        "the bare run's pct/hr EMA should carry the seeded delta"
    );
}

/// An unannotated mirror still produces exactly the old conservative fallback.
///
/// This is the other half of point 5: unblocking validation must not change
/// what happens when there is nothing to validate against. A fleet that has not
/// yet accumulated annotated history — a fresh install, or the first cycles
/// after a window reset — has to keep landing on 1.0 rather than on some
/// half-populated ratio.
#[test]
fn an_unannotated_mirror_still_falls_back_to_one_x() {
    let home = TempDir::new().expect("failed to create temp HOME");
    let cycle_paths = CyclePaths::under(home.path());
    let db_path = cycle_paths.collector.db_path.clone();
    let state_path = home.path().join("governor-state.json");
    let base = Utc::now();

    // A mirror with rows but no annotation: the state every install starts in,
    // and the state production was stuck in for the life of the bug.
    {
        let conn = open_seeded_mirror(&db_path);
        seed_pass(
            &conn,
            base - ChronoDuration::seconds(100),
            "peak",
            true,
            PEAK_TOKENS,
            None,
        );
    }
    // No snapshot at all, so the annotation block has no span to work over and
    // the mirror stays unannotated through the cycle.
    drive_cycle(PCT_1, &state_path, &cycle_paths, &[active_promotion(base)]);

    assert_eq!(
        annotated_rows(
            &db::open_db(&db_path).expect("failed to reopen the mirror"),
            true
        ),
        0,
        "without a previous snapshot there is no span, so nothing should be annotated"
    );

    let validation = validate_promotion_from_db(&db_path, DECLARED_MULTIPLIER);
    assert!(!validation.validated);
    assert_eq!(
        validation.reason.as_deref(),
        Some(NO_DATA_REASON),
        "an unannotated mirror should still report the no-data reason"
    );
    assert!(
        (effective_multiplier(&validation) - 1.0).abs() < RATIO_EPS,
        "the conservative fallback should still be 1.0 when there is no data"
    );

    let persisted = state::load_state(&state_path).expect("failed to read the cycle's state");
    assert!(
        !persisted.burn_rate.promotion_validated,
        "the cycle should not claim validation it could not perform"
    );
    assert_eq!(persisted.burn_rate.promotion_peak_samples, 0);
    assert_eq!(persisted.burn_rate.promotion_offpeak_samples, 0);
}

// ---------------------------------------------------------------------------
// JSONL stays authoritative-but-null
// ---------------------------------------------------------------------------

/// Annotation lives in the SQLite mirror only; the JSONL log is not rewritten.
///
/// The plan makes JSONL the authoritative append-only record and the DB a
/// mirror, so the annotated columns are a mirror-side enrichment. Rewriting
/// JSONL lines in place would break that contract — and would mean the governor
/// mutating a log the collector owns.
///
/// The check is byte-level on a log seeded with the same records the mirror
/// holds, driven through the same cycle that annotates the mirror.
#[test]
fn annotation_does_not_rewrite_the_jsonl_log() {
    let home = TempDir::new().expect("failed to create temp HOME");
    let cycle_paths = CyclePaths::under(home.path());
    let db_path = cycle_paths.collector.db_path.clone();
    let state_path = home.path().join("governor-state.json");
    let base = Utc::now();
    let t1 = base - ChronoDuration::seconds(100);

    {
        let conn = open_seeded_mirror(&db_path);
        seed_pass(&conn, t1, "peak", true, PEAK_TOKENS, None);
    }

    // The same records the mirror holds, in the collector's own file, with the
    // annotated fields explicitly null.
    let history_path = cycle_paths.collector.history_path.clone();
    let jsonl: String = (0..BATCH)
        .map(|i| {
            format!(
                "{}\n",
                serde_json::json!({
                    "r": "i", "ts": t1.to_rfc3339(), "t1": t1.to_rfc3339(),
                    "sess": format!("peak-{i}"), "model": "claude-sonnet-4-5",
                    "total-usd": USD_PER_ROW,
                    "p5h": serde_json::Value::Null,
                    "p7d": serde_json::Value::Null,
                    "p7ds": serde_json::Value::Null,
                })
            )
        })
        .collect();
    std::fs::create_dir_all(history_path.parent().expect("history path has a parent"))
        .expect("failed to create the state dir");
    std::fs::write(&history_path, &jsonl).expect("failed to seed the JSONL log");

    seed_snapshot(&state_path, PCT_0, base - ChronoDuration::seconds(300));
    drive_cycle(PCT_1, &state_path, &cycle_paths, &[active_promotion(base)]);

    // The cycle annotated the mirror...
    assert_eq!(
        annotated_rows(
            &db::open_db(&db_path).expect("failed to reopen the mirror"),
            true
        ),
        BATCH as i64,
        "the cycle should have annotated the mirror"
    );
    // ...and left the log exactly as the collector wrote it.
    let after = std::fs::read_to_string(&history_path).expect("failed to reread the JSONL log");
    assert_eq!(
        after, jsonl,
        "the JSONL log must not be rewritten by annotation"
    );
}
