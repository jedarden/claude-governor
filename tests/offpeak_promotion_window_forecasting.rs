//! Off-peak promotion-window forecasting regression (claudego-4ec0225e).
//!
//! The schedule module's own unit tests (`src/schedule.rs`) prove
//! `get_multiplier_at` and `effective_hours_remaining_from` behave per-window
//! in isolation. This binary proves the same discipline survives the real
//! observe cycle: the per-window numbers the governor actually publishes in
//! `state.schedule` — the burn-rate-side `promo_multiplier_*` and the
//! remaining-capacity `effective_hours_remaining_*` — move ONLY during the
//! applicable windows, on both axes the word "window" has:
//!
//! - **Subscription window** (`applies_to`): a promotion listing only
//!   `weekly_scoped` must leave `five_hour` and `seven_day` at 1.0 / raw
//!   wall-clock, even while `weekly_scoped` is boosted. One scenario inverts
//!   the listing — `five_hour` and `seven_day` boosted, `weekly_scoped` left
//!   out — so each window's multiplier is pinned to its OWN name, not merely
//!   to "not `weekly_scoped`": the cycle computes the three multipliers in
//!   three separate arms, and a window name swapped between two of them
//!   would otherwise stay green.
//! - **Time window**: off-peak hours inside the promotion's date range only —
//!   peak hours get nothing even mid-promotion, minutes before `start_date`
//!   get nothing, and the date bounds are Eastern calendar dates
//!   (`start_date` inclusive, `end_date` exclusive), not UTC ones.
//!
//! The burn-rate side (`promo_multiplier_*`) is the empirically-gated one:
//! `validate_promotion_from_db` must see a plausible off-peak/peak token
//! ratio in the collector mirror before the declared multiplier is trusted.
//! Every scenario here seeds a mirror whose ratio is exactly 2.0 (off-peak
//! rows carry 2x the tokens of peak rows at equal tokens-per-pct), so a
//! boosted window reading 1.0 means the schedule logic broke — not that
//! validation silently fell back.
//!
//! One asymmetry is pinned deliberately: remaining capacity
//! (`effective_hours_remaining_*`) is computed from the declared promotion
//! config and does NOT depend on empirical validation, while the burn-rate
//! multiplier does. The final test holds that contract open — if someone
//! later gates the capacity walk on validation too (or stops gating the
//! multiplier), this suite says so instead of letting the change drift in
//! silently.
//!
//! The later section extends the same discipline to the CONSUMERS of the
//! walk: the per-window `hours_remaining` that lands in
//! `state.capacity_forecast` (and through it the duty-cycle
//! `safe_worker_count` and the binding-window selection). Those scenarios
//! place the reset time itself on the boundaries — exactly on 08:00/14:00 ET,
//! exactly on a promotion's start/end midnight, or straddling a promotion
//! transition inside the span — and pin the exact effective hours, the
//! sizing decision derived from them, and the binding choice. With no
//! workers and no burn data every forecast margin is infinite, so the risk
//! scores tie and binding selection resolves to the last eligible window;
//! that tie is itself pinned (it is what `state.schedule`
//! `.effective_hours_remaining` display reads) so any change to eligibility,
//! tie-breaking, or promotion-in-risk coupling shows up here.
//!
//! The final section drives the same cycle under EST and across the DST
//! fall-back hour. Every scenario above runs in March (EDT, UTC-4); a
//! hardcoded EDT offset or a UTC-date activation would keep every number
//! above green while drifting exactly at the January start midnight (05:00Z,
//! not 04:00Z), the 13:00–19:00Z winter peak band, and the repeated 01:00
//! local hour of November's fall-back Sunday. The schedule unit tests pin
//! those instants in isolation; these prove they survive the observe cycle.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use claude_governor::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, DaemonConfig, GovernorConfig,
    PricingConfig, SprintConfig,
};
use claude_governor::db;
use claude_governor::governor::{run_observe_cycle, CyclePaths};
use claude_governor::poller::{UsageData, UsagePoller};
use claude_governor::schedule::Promotion;
use claude_governor::state;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// The promotion under test
// ---------------------------------------------------------------------------

/// The declared off-peak multiplier. Matches the March 2026 2x promotion the
/// research doc describes (`docs/research/off-hours-promotion.md`).
const MULTIPLIER: f64 = 2.0;

/// March 15–25, 2026, 2x off-peak, `weekly_scoped` ONLY.
///
/// Deliberately narrow `applies_to`: the excluded windows are how every test
/// here proves "only during the applicable windows" rather than "adjusted
/// everywhere".
fn promotion() -> Promotion {
    Promotion {
        name: "March 2026 2x off-peak".to_string(),
        start_date: "2026-03-15".to_string(),
        end_date: "2026-03-25".to_string(),
        peak_start_hour_et: 8,
        peak_end_hour_et: 14,
        offpeak_multiplier: MULTIPLIER,
        applies_to: vec!["weekly_scoped".to_string()],
    }
}

/// ET wall-clock → UTC, the inverse of the conversion the schedule module
/// applies internally. March 2026 is EDT (UTC-4); chrono-tz handles that, so
/// every timestamp below is written in Eastern terms and stays readable
/// against the peak/off-peak bands.
fn et(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    chrono_tz::America::New_York
        .with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("a real Eastern wall-clock time")
        .with_timezone(&Utc)
}

// ---------------------------------------------------------------------------
// Observe-cycle harness
// ---------------------------------------------------------------------------

/// Returns one scripted reading — the cycle's `now` plus per-window reset
/// offsets in hours. Offsets are parameters because the expected effective
/// hours are exact only when the whole walk sits inside one peak/off-peak
/// band; each test pins its own spans.
struct FakePoller {
    reading: UsageData,
}

impl UsagePoller for FakePoller {
    fn poll_usage(&mut self) -> anyhow::Result<UsageData> {
        Ok(self.reading.clone())
    }
}

fn reading(now: DateTime<Utc>, resets_hrs: (i64, i64, i64)) -> UsageData {
    UsageData {
        five_hour_utilization: 20.0,
        five_hour_resets_at: (now + ChronoDuration::hours(resets_hrs.0)).to_rfc3339(),
        five_hour_hours_remaining: resets_hrs.0 as f64,
        seven_day_utilization: 40.0,
        seven_day_resets_at: (now + ChronoDuration::hours(resets_hrs.1)).to_rfc3339(),
        seven_day_hours_remaining: resets_hrs.1 as f64,
        weekly_scoped_utilization: 30.0,
        weekly_scoped_resets_at: (now + ChronoDuration::hours(resets_hrs.2)).to_rfc3339(),
        weekly_scoped_hours_remaining: resets_hrs.2 as f64,
        weekly_scoped_model: None,
        limits: vec![],
        timestamp: now,
        // `stale: false` — a stale reading would gate the annotation block,
        // and although no snapshot is seeded here (so it would skip anyway),
        // keeping the pin explicit documents that nothing in these scenarios
        // may depend on collector writes.
        stale: false,
    }
}

/// Rows per peak/off-peak side. `compute_empirical_promo_ratio` needs 10 of
/// each before `validate_promotion_from_db` will validate, so 10 is the
/// smallest honest fixture.
const BATCH: usize = 10;

/// Tokens on a peak row; off-peak rows carry exactly double. With equal
/// `p7ds` on every row the observed ratio is exactly 2.0 — inside the 10%
/// validation tolerance of the declared multiplier.
const PEAK_TOKENS: u64 = 70_000;
const USD_PER_ROW: f64 = 1.0;

/// Seed the collector mirror with annotated rows whose off-peak/peak token
/// ratio is exactly `MULTIPLIER`, so the empirical gate validates the
/// declared multiplier instead of falling back to 1x.
///
/// `pk` is the collector's own classification of the interval, not a
/// re-derivation from wall-clock — which is what lets this fixture validate
/// regardless of which scenario timestamp reads it.
fn seed_validated_mirror(db_path: &Path) {
    let conn = db::open_db(db_path).expect("failed to open the mirror");
    db::create_schema(&conn).expect("failed to create the mirror schema");

    let t1 = Utc::now();
    let t0 = (t1 - ChronoDuration::minutes(5)).to_rfc3339();
    let t1_str = t1.to_rfc3339();

    for peak in [true, false] {
        let tokens = if peak { PEAK_TOKENS } else { 2 * PEAK_TOKENS };
        let label = if peak { "peak" } else { "offpeak" };
        for i in 0..BATCH {
            // Peak hours 8..14 ET, off-peak 14..24 — the bands `schedule` uses.
            let hr_et = if peak { 8 + (i % 6) } else { 14 + (i % 10) };
            let record = serde_json::json!({
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
                "p5h": 5.0, "p7d": 5.0, "p7ds": 5.0,
            });
            db::insert_instance(&conn, &record).expect("seeding an i row should succeed");
        }
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

/// Drive one real observe cycle at a fixed `now` against a temp-rooted
/// layout, with the promotion under test and a mirror pre-seeded to validate
/// at 2x. Returns the state the cycle persisted.
///
/// `seed_mirror = false` leaves the mirror empty, which is how the final test
/// exercises the conservative validation fallback.
fn drive_observe(now: DateTime<Utc>, resets_hrs: (i64, i64, i64), seed_mirror: bool) -> state::GovernorState {
    drive_observe_with(now, resets_hrs, seed_mirror, &[promotion()])
}

/// [`drive_observe`] with an explicit promotion schedule. The boundary tests
/// below drive the same instant against the real schedule and against an
/// empty one — the only way to show a number moved BECAUSE of the promotion
/// rather than because of the clock.
fn drive_observe_with(
    now: DateTime<Utc>,
    resets_hrs: (i64, i64, i64),
    seed_mirror: bool,
    promotions: &[Promotion],
) -> state::GovernorState {
    let home = TempDir::new().expect("temp home");
    let paths = CyclePaths::under(home.path());
    if seed_mirror {
        seed_validated_mirror(&paths.collector.db_path);
    }

    let state_path = home.path().join("governor-state.json");
    let mut poller = FakePoller {
        reading: reading(now, resets_hrs),
    };
    let agents: HashMap<String, AgentConfig> = HashMap::new();

    run_observe_cycle(
        &mut poller,
        &state_path,
        &paths,
        &AlertConfig::default(),
        &agents,
        promotions,
        &minimal_pricing_config(),
        now,
    )
    .expect("the observe cycle should complete");

    let loaded = state::load_state(&state_path).expect("reload the persisted state");
    home.close().expect("temp home cleanup");
    loaded
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "{what}: expected {expected}, got {actual}"
    );
}

// ---------------------------------------------------------------------------
// The matrix: off-peak × in-promo, per subscription window
// ---------------------------------------------------------------------------

/// Off-peak evening inside the promotion: `weekly_scoped` is boosted on both
/// axes; the two windows missing from `applies_to` stay untouched on both.
///
/// All reset spans sit inside the same off-peak block (Mon 20:00 ET + 2h), so
/// every effective-hours number below is hand-computable: raw hours × 1.0 or
/// × 2.0, with no peak minute mixed in.
#[test]
fn offpeak_during_promo_boosts_only_the_listed_window() {
    let now = et(2026, 3, 16, 20, 0); // Monday evening
    let st = drive_observe(now, (1, 2, 2), true);

    assert!(!st.schedule.is_peak_hour, "20:00 ET Monday is off-peak");
    assert!(st.schedule.is_promo_active, "March 16 is inside the promo dates");

    // Burn-rate side: validated 2x lands on weekly_scoped ONLY.
    assert!(
        st.burn_rate.promotion_validated,
        "seeded mirror must validate the declared 2x"
    );
    assert_close(
        st.burn_rate.offpeak_ratio_expected,
        2.0,
        "declared multiplier recorded on burn_rate",
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        2.0,
        "weekly_scoped burn multiplier (in applies_to)",
    );
    assert_close(
        st.schedule.promo_multiplier_five_hour,
        1.0,
        "five_hour burn multiplier (NOT in applies_to)",
    );
    assert_close(
        st.schedule.promo_multiplier_seven_day,
        1.0,
        "seven_day burn multiplier (NOT in applies_to)",
    );
    assert_close(
        st.schedule.promo_multiplier,
        2.0,
        "display multiplier is the max across windows",
    );

    // Remaining-capacity side: only the listed window's effective hours grow.
    assert_close(
        st.schedule.effective_hours_remaining_five_hour,
        1.0,
        "five_hour effective == raw 1h",
    );
    assert_close(
        st.schedule.effective_hours_remaining_seven_day,
        2.0,
        "seven_day effective == raw 2h",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        4.0,
        "weekly_scoped effective = 2h raw × 2x",
    );
    assert_close(
        st.schedule.raw_hours_remaining,
        2.0,
        "raw hours are wall-clock regardless of promotions",
    );
}

/// A promotion listing MORE than one subscription window boosts each listed
/// window under its own name — and the cycle computes those multipliers in
/// three separate arms, so this is the only scenario that tells them apart.
/// Every other test here lists only `weekly_scoped`: that pins each arm's
/// *rejection* of `"weekly_scoped"`, but a `"five_hour"`/`"seven_day"`
/// window-name swap between the two unboosted arms would keep all of them
/// green. Here the listing moves to the OTHER two windows, so both arms must
/// match their own name (and not each other's) while the unlisted window
/// stays raw — on the burn side, in the capacity walk, and in the sizing
/// each walk feeds: 70% ÷ (1.5%/hr × 2 effective hrs) = 23.33 → 23, and
/// 50% ÷ (1.5 × 4) = 8.33 → 8, each exactly half the unboosted cycle. A
/// five_hour-only listing then closes the matrix: it is the scenario a
/// five_hour/seven_day arm swap would fail.
#[test]
fn promotion_listing_several_windows_boosts_each_listed_window() {
    let now = et(2026, 3, 16, 20, 0); // Monday evening, all-off-peak spans
    let mut multi = promotion();
    multi.applies_to = vec!["five_hour".to_string(), "seven_day".to_string()];

    let boosted = drive_observe_with(now, (1, 2, 2), true, &[multi]);
    let flat = drive_observe_with(now, (1, 2, 2), false, &[]);

    assert!(!boosted.schedule.is_peak_hour);
    assert!(boosted.schedule.is_promo_active);
    assert!(
        boosted.burn_rate.promotion_validated,
        "the mirror still validates — a 1.0 below is an applies_to miss, not a fallback"
    );

    // Burn side: exactly the two listed windows move, each under its own name.
    assert_close(
        boosted.schedule.promo_multiplier_five_hour,
        2.0,
        "five_hour burn multiplier (listed)",
    );
    assert_close(
        boosted.schedule.promo_multiplier_seven_day,
        2.0,
        "seven_day burn multiplier (listed) — pins this arm against a \
         five_hour name swap",
    );
    assert_close(
        boosted.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "weekly_scoped burn multiplier (NOT listed)",
    );
    assert_close(
        boosted.schedule.promo_multiplier,
        2.0,
        "display multiplier is the max across windows",
    );

    // Capacity side: each listed window's walk doubles, the unlisted stays raw.
    assert_close(
        boosted.capacity_forecast.five_hour.hours_remaining,
        2.0,
        "1 raw hour × 2x",
    );
    assert_close(
        boosted.capacity_forecast.seven_day.hours_remaining,
        4.0,
        "2 raw hours × 2x",
    );
    assert_close(
        boosted.capacity_forecast.weekly_scoped.hours_remaining,
        2.0,
        "the unlisted window keeps its raw wall-clock walk",
    );

    // Sizing: the duty-cycle budget over the doubled effective hours.
    assert_eq!(
        boosted.capacity_forecast.five_hour.safe_worker_count,
        Some(23),
        "70% / (1.5%/hr × 2 effective hrs) = 23.33 → 23, exactly half the flat 46",
    );
    assert_eq!(
        boosted.capacity_forecast.seven_day.safe_worker_count,
        Some(8),
        "50% / (1.5%/hr × 4 effective hrs) = 8.33 → 8, exactly half the flat 16",
    );
    assert_eq!(
        boosted.capacity_forecast.weekly_scoped.safe_worker_count,
        Some(20),
        "the unlisted window's count matches the no-promotion cycle",
    );
    assert_eq!(flat.capacity_forecast.five_hour.safe_worker_count, Some(46));
    assert_eq!(flat.capacity_forecast.seven_day.safe_worker_count, Some(16));
    assert_eq!(flat.capacity_forecast.weekly_scoped.safe_worker_count, Some(20));

    // No burn here → margins tie → binding still resolves to the LAST eligible
    // window, which this listing leaves UNBOOSTED. The display effective-hours
    // field therefore reads the raw 2.0 walk — proving it follows the binding
    // window, not the largest boost (the doubled-effect test pins the mirrored
    // case, where the binding window IS the boosted one).
    assert_eq!(boosted.capacity_forecast.binding_window, "weekly_scoped");
    assert_close(
        boosted.schedule.effective_hours_remaining,
        2.0,
        "display reads the binding window's raw walk, not the boost",
    );

    // Completing the arm-to-name proof: this scenario cannot catch a
    // five_hour/seven_day NAME SWAP between the two boosted arms (both names
    // are listed, so either arm matches either). Listing only ONE of the pair
    // can: under a swap, the listed window's own arm reads 1.0 and the
    // unlisted one wrongly picks up the boost.
    let mut single = promotion();
    single.applies_to = vec!["five_hour".to_string()];
    let st = drive_observe_with(now, (1, 2, 2), true, &[single]);

    assert_close(
        st.schedule.promo_multiplier_five_hour,
        2.0,
        "the five_hour arm answers a five_hour-only listing",
    );
    assert_close(
        st.schedule.promo_multiplier_seven_day,
        1.0,
        "the seven_day arm must NOT answer a five_hour listing — a name swap \
         between the two arms reads 2.0 here",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        2.0,
        "the listed window's capacity walk doubles",
    );
    assert_close(
        st.capacity_forecast.seven_day.hours_remaining,
        2.0,
        "the unlisted window stays at its raw 2h walk — a name swap would \
         boost this to 4.0",
    );
}

/// Peak hours inside the promotion: nothing is boosted, on either axis, even
/// though the mirror validates and the promo is active. The reset spans stay
/// inside the 08:00–14:00 ET block so no off-peak minute enters any walk —
/// this is what makes the assertion "peak gets zero adjustment" rather than
/// "peak-adjusted-now gets adjustment later in the span".
#[test]
fn peak_hours_get_no_boost_even_with_active_promo() {
    let now = et(2026, 3, 16, 10, 0); // Monday 10:00 ET
    let st = drive_observe(now, (1, 2, 2), true);

    assert!(st.schedule.is_peak_hour, "10:00 ET Monday is peak");
    assert!(st.schedule.is_promo_active, "promo dates include March 16");
    assert!(
        st.burn_rate.promotion_validated,
        "the mirror still validates during peak — the 1.0 below is the \
         peak gate, not a validation fallback"
    );

    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "weekly_scoped burn multiplier during peak",
    );
    assert_close(
        st.schedule.promo_multiplier_five_hour,
        1.0,
        "five_hour burn multiplier during peak",
    );
    assert_close(
        st.schedule.promo_multiplier_seven_day,
        1.0,
        "seven_day burn multiplier during peak",
    );

    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        2.0,
        "weekly_scoped effective == raw 2h (peak minutes count 1x even mid-promo)",
    );
    assert_close(
        st.schedule.effective_hours_remaining_five_hour,
        1.0,
        "five_hour effective == raw 1h",
    );
}

// ---------------------------------------------------------------------------
// Date-range boundaries (Eastern calendar dates)
// ---------------------------------------------------------------------------

/// Off-peak minutes one week BEFORE `start_date`: no boost anywhere.
#[test]
fn offpeak_minutes_before_the_promo_start_get_no_boost() {
    let now = et(2026, 3, 9, 20, 0); // Monday evening, before the promo
    let st = drive_observe(now, (1, 2, 2), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(!st.schedule.is_promo_active, "March 9 precedes start_date");
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "weekly_scoped burn multiplier pre-promo",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        2.0,
        "weekly_scoped effective == raw 2h pre-promo",
    );
    assert_close(
        st.schedule.effective_hours_remaining_five_hour,
        1.0,
        "five_hour effective == raw 1h pre-promo",
    );
}

/// `start_date` is inclusive, and a weekend day has no peak block at all:
/// Sunday March 15 (the promo's first day) at 20:00 ET is boosted.
#[test]
fn promo_start_date_is_inclusive_and_weekend_stays_off_peak() {
    let now = et(2026, 3, 15, 20, 0); // Sunday evening, promo day one
    let st = drive_observe(now, (1, 2, 2), true);

    assert!(!st.schedule.is_peak_hour, "weekends are off-peak all day");
    assert!(st.schedule.is_promo_active, "start_date itself is active");
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        2.0,
        "weekly_scoped burn multiplier on the promo's first day",
    );
    assert_close(
        st.schedule.promo_multiplier_five_hour,
        1.0,
        "five_hour stays unboosted on the promo's first day",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        4.0,
        "weekly_scoped effective = 2h raw × 2x on the promo's first day",
    );
    assert_close(
        st.schedule.effective_hours_remaining_seven_day,
        2.0,
        "seven_day effective == raw 2h on the promo's first day",
    );
}

/// `end_date` is exclusive: Wednesday March 25 at 20:00 ET is still
/// off-peak, still an ET calendar date inside the span by wall-clock
/// intuition, but the promotion is already over.
#[test]
fn promo_end_date_is_exclusive() {
    let now = et(2026, 3, 25, 20, 0); // Wednesday evening, end_date day
    let st = drive_observe(now, (1, 2, 2), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(!st.schedule.is_promo_active, "end_date itself is not active");
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "weekly_scoped burn multiplier after the promo ends",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        2.0,
        "weekly_scoped effective == raw 2h after the promo ends",
    );
}

/// The date bounds are EASTERN calendar dates. `2026-03-15T01:00Z` is already
/// March 15 on the UTC calendar but still 21:00 March 14 in New York, so the
/// promotion has not started. If the activation ever drifted to comparing UTC
/// dates, this instant would flip to boosted three hours early.
#[test]
fn promo_activation_follows_the_eastern_date_not_utc() {
    // 2026-03-15T01:00Z == 2026-03-14T21:00 ET — off-peak, UTC date says
    // start_date, ET date says the day before.
    let now = Utc.with_ymd_and_hms(2026, 3, 15, 1, 0, 0).single().unwrap();
    let st = drive_observe(now, (1, 2, 2), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(
        !st.schedule.is_promo_active,
        "ET date is March 14 — the promo must not start on the UTC date"
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "no burn boost while the ET date still precedes start_date",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        2.0,
        "no capacity boost while the ET date still precedes start_date",
    );
}

/// Mirror image of the boundary above: `2026-03-25T02:00Z` is already March
/// 25 on the UTC calendar but still 22:00 March 24 in New York, so the
/// promotion is STILL ACTIVE — the last Eastern evening keeps its boost even
/// after the UTC calendar has rolled past `end_date`.
#[test]
fn promo_stays_active_through_the_last_eastern_evening() {
    // 2026-03-25T02:00Z == 2026-03-24T22:00 ET — off-peak Tuesday evening.
    let now = Utc.with_ymd_and_hms(2026, 3, 25, 2, 0, 0).single().unwrap();
    let st = drive_observe(now, (1, 2, 2), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(
        st.schedule.is_promo_active,
        "ET date is March 24 — the promo outlives the UTC end_date"
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        2.0,
        "the last Eastern evening keeps its burn boost",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        4.0,
        "the last Eastern evening keeps its capacity boost",
    );
    assert_close(
        st.schedule.effective_hours_remaining_five_hour,
        1.0,
        "five_hour stays raw even in the final boosted evening",
    );
}

// ---------------------------------------------------------------------------
// Validation gating: burn rate is gated, remaining capacity is not
// ---------------------------------------------------------------------------

/// An active promotion the empirical data cannot validate keeps the
/// burn-rate multiplier at the conservative 1x — while remaining capacity is
/// STILL computed from the declared multiplier. That asymmetry is the
/// current contract: capacity forecasting trusts config, burn attribution
/// demands evidence.
#[test]
fn unvalidated_promotion_keeps_burn_multiplier_at_1x_but_capacity_still_boosts() {
    let now = et(2026, 3, 16, 20, 0); // same instant as the boosted scenario
    let st = drive_observe(now, (1, 2, 2), false); // empty mirror → no data

    assert!(st.schedule.is_promo_active);
    assert!(
        !st.burn_rate.promotion_validated,
        "an empty mirror must not validate the declared multiplier"
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "unvalidated promotion falls back to 1x for burn attribution",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        4.0,
        "remaining capacity still uses the declared multiplier",
    );
}

// ---------------------------------------------------------------------------
// Reset & promotion boundaries through the forecast (claudego-cd7cb396)
//
// The tests above pin the schedule state at boundary instants. These place
// the RESET TIME itself on the boundary — exactly on 08:00/14:00 ET, exactly
// on a promotion's start/end midnight, or straddling a promotion transition
// inside the span — so the forecast walk has to attribute the boundary
// minute correctly, and follow the numbers into the decisions that consume
// them: `capacity_forecast.<window>.hours_remaining`, the duty-cycle
// `safe_worker_count` derived from it, and the binding-window selection.
//
// This harness has no workers and no burn history, so every
// `fleet_pct_per_hour` is 0, predicted exhaustion is infinite, and every
// `margin_hrs` is infinite: all three risk scores tie at -Inf and binding
// selection resolves to the last eligible window. That makes every number
// below exact and hand-computed against the defaults the cycle actually
// uses: ceiling 90 (DaemonConfig default), sizing rate 1.5%/worker/hr
// (baseline fallback at zero workers), utilizations 20/40/30 → remaining
// 70/50/60.
// ---------------------------------------------------------------------------

/// A reset span straddling the promotion's END midnight: Tue 22:00 ET + 4h
/// crosses 2026-03-25 00:00 ET, so the walk is 2h at 2x (Tuesday, promo still
/// on) + 2h at 1x (Wednesday, promo over) = 6.0 effective hours over 4.0
/// wall hours. The forecast must consume the mixed value — not the raw 4.0
/// and not the flat 2x 8.0 — even though the burn side still shows 2x at
/// `now`.
#[test]
fn forecast_crossing_the_promo_end_midnight_mixes_both_multipliers() {
    let now = et(2026, 3, 24, 22, 0); // Tuesday evening, the promo's last day
    let st = drive_observe(now, (1, 2, 4), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(
        st.schedule.is_promo_active,
        "22:00 ET Tuesday is still inside the promo"
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        2.0,
        "burn attribution is 2x at now",
    );

    assert_close(st.schedule.raw_hours_remaining, 4.0, "wall-clock hours to reset");
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        6.0,
        "2h at 2x + 2h at 1x across the promo end",
    );
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        6.0,
        "the forecast consumed the mixed walk",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        1.0,
        "five_hour walk is raw (not in applies_to) even across the boundary",
    );
    assert_close(
        st.capacity_forecast.seven_day.hours_remaining,
        2.0,
        "seven_day's reset lands exactly on the promo-end midnight; its raw \
         walk stops there at 1x",
    );
}

/// Mirror image: Sat 22:00 ET + 4h crosses the promotion's START midnight, so
/// the walk is 2h at 1x (Saturday, before start_date) + 2h at 2x (Sunday, the
/// promo's first day — no peak block on a weekend) = 6.0. At `now` the promo
/// has not started, so the instantaneous burn multiplier is still 1.0: the
/// deliberate disagreement between attribution (where the account is) and
/// forecast (where the window is going).
#[test]
fn forecast_crossing_the_promo_start_midnight_picks_up_the_boost() {
    let now = et(2026, 3, 14, 22, 0); // Saturday evening, before start_date
    let st = drive_observe(now, (1, 2, 4), true);

    assert!(!st.schedule.is_peak_hour, "Saturday night is off-peak");
    assert!(!st.schedule.is_promo_active, "Saturday precedes start_date");
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "burn attribution is still 1x at now",
    );

    assert_close(st.schedule.raw_hours_remaining, 4.0, "wall-clock hours to reset");
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        6.0,
        "2h at 1x + 2h at 2x across the promo start",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        1.0,
        "five_hour stays raw across the promo start",
    );
}

/// The reset lands exactly ON 14:00 ET. The peak window is half-open
/// ([08:00, 14:00)) and the walk is half-open ([now, reset)), so a
/// 13:00 → 14:00 span is ALL peak minutes: exactly 1.0 effective hour even
/// though the very next minute would be boosted 2x. A walk that stepped one
/// minute too far, or gave the boundary minute to the wrong band, reads 2.0.
#[test]
fn reset_exactly_on_peak_end_counts_only_peak_minutes() {
    let now = et(2026, 3, 16, 13, 0); // Monday 13:00 ET, inside peak
    let st = drive_observe(now, (1, 1, 1), true);

    assert!(st.schedule.is_peak_hour);
    assert!(st.schedule.is_promo_active);
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        1.0,
        "13:00 -> 14:00 is exactly one peak hour; the 14:00 minute must not leak in",
    );
    assert_close(
        st.schedule.effective_hours_remaining_weekly_scoped,
        1.0,
        "schedule and forecast agree on the boundary-exact walk",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        1.0,
        "same span, same 1.0 for a window the promo does not apply to",
    );
}

/// The mirror: a 07:00 → 08:00 span is ALL off-peak minutes inside the promo,
/// so the boosted window reads exactly 2.0 — the 08:00 peak minute is the
/// first one excluded. Reads 1.0 if the boundary minute flips bands.
#[test]
fn reset_exactly_on_peak_start_counts_only_offpeak_minutes() {
    let now = et(2026, 3, 16, 7, 0); // Monday 07:00 ET, pre-peak
    let st = drive_observe(now, (1, 1, 1), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(st.schedule.is_promo_active);
    assert_close(st.schedule.promo_multiplier_weekly_scoped, 2.0, "off-peak, promo on");
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        2.0,
        "07:00 -> 08:00 is one boosted hour; the 08:00 peak minute must not leak in",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        1.0,
        "same span stays raw for a window the promo does not apply to",
    );
}

/// The boosted window's reset lands exactly ON the promotion's end midnight:
/// Tue 23:00 ET + 1h stops at 2026-03-25 00:00 ET. Every walked minute is
/// still Tuesday at 2x, so the span is exactly 2.0 — the first 1x minute
/// would only arrive after the reset.
#[test]
fn reset_exactly_on_the_promo_end_midnight_keeps_the_boost() {
    let now = et(2026, 3, 24, 23, 0);
    let st = drive_observe(now, (1, 2, 1), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(st.schedule.is_promo_active);
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        2.0,
        "23:00 -> 00:00 is one boosted hour; expiry must not clip the walked minutes",
    );
}

/// And the span that STARTS on the end date: Wed 00:00 ET + 1h is entirely
/// post-promo off-peak, so the boosted window reads exactly 1.0. This is the
/// end-exclusive contract expressed in the walk rather than in a single
/// instant: 00:00 on end_date is the first inactive minute.
#[test]
fn span_starting_on_the_end_date_is_entirely_flat() {
    let now = et(2026, 3, 25, 0, 0);
    let st = drive_observe(now, (1, 2, 1), true);

    assert!(!st.schedule.is_peak_hour);
    assert!(!st.schedule.is_promo_active, "00:00 on end_date is already out");
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "off-peak but post-expiry",
    );
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        1.0,
        "an off-peak hour after expiry is one raw hour",
    );
}

/// The documented 2x capacity effect, followed into the sizing decision. The
/// same instant and readings are driven twice — once with the promotion, once
/// with an empty schedule. The spans are all off-peak, so the boosted
/// window's effective hours are exactly double (4.0 vs 2.0), and in the
/// whole-worker regime the duty-cycle budget math halves the sustainable
/// count: 60% remaining ÷ (1.5%/hr × 4 effective hrs) = 10 workers with the
/// promo, 60 ÷ (1.5 × 2) = 20 without. The windows the promotion does not
/// list (70%/1h → 46, 50%/2h → 16) must be identical across both cycles.
#[test]
fn doubled_effective_hours_halve_the_whole_worker_safe_count() {
    let now = et(2026, 3, 16, 20, 0); // Monday evening, all-off-peak spans

    let boosted = drive_observe(now, (1, 2, 2), true);
    let flat = drive_observe_with(now, (1, 2, 2), false, &[]);

    assert_close(
        boosted.capacity_forecast.weekly_scoped.hours_remaining,
        4.0,
        "2 wall hours × 2x",
    );
    assert_close(
        flat.capacity_forecast.weekly_scoped.hours_remaining,
        2.0,
        "raw wall hours with no promotion",
    );

    assert_eq!(
        boosted.capacity_forecast.weekly_scoped.safe_worker_count,
        Some(10),
        "60% / (1.5%/hr × 4 effective hrs) sustains 10 continuous workers",
    );
    assert_eq!(
        flat.capacity_forecast.weekly_scoped.safe_worker_count,
        Some(20),
        "the same budget over half the effective hours sustains exactly twice as many",
    );

    // Unboosted windows are untouched by the promotion on both cycles.
    assert_eq!(boosted.capacity_forecast.five_hour.safe_worker_count, Some(46));
    assert_eq!(flat.capacity_forecast.five_hour.safe_worker_count, Some(46));
    assert_eq!(boosted.capacity_forecast.seven_day.safe_worker_count, Some(16));
    assert_eq!(flat.capacity_forecast.seven_day.safe_worker_count, Some(16));

    // No fleet burn here → every margin is infinite → the risk scores tie and
    // binding resolves to the last eligible window on BOTH cycles: the
    // promotion alone must not perturb binding selection.
    assert_eq!(
        boosted.capacity_forecast.binding_window, "weekly_scoped",
        "tie-break pin: no-burn margins tie, the last eligible window binds",
    );
    assert_eq!(flat.capacity_forecast.binding_window, "weekly_scoped");
    // The display effective-hours field reads the BINDING window's walk, so
    // it doubles exactly when the binding window is the boosted one.
    assert_close(
        boosted.schedule.effective_hours_remaining,
        4.0,
        "display reads the binding window's boosted walk",
    );
    assert_close(
        flat.schedule.effective_hours_remaining,
        2.0,
        "display reads the binding window's raw walk",
    );

    // And the boundary must not manufacture urgency: with zero fleet burn,
    // exhaustion is never predicted inside any span, boosted or not.
    assert!(!boosted.capacity_forecast.weekly_scoped.cutoff_risk);
    assert!(!flat.capacity_forecast.weekly_scoped.cutoff_risk);
}

// ---------------------------------------------------------------------------
// Timezone & DST through the forecast
//
// The schedule module's unit tests pin the EST peak band
// (`peak_boundaries_pinned_in_utc_during_est`) and the DST-transition Sunday
// (`dst_transition_sundays_are_off_peak_all_day`) in isolation. These
// scenarios prove the same discipline survives the real observe cycle under
// the OTHER offset and across the fall-back hour itself — the two places a
// hardcoded EDT offset or a wall-clock-naive walk would drift while every
// March scenario above stayed green.
// ---------------------------------------------------------------------------

/// January 20–22, 2026 (Tue–Thu): the March promo's exact shape, but in EST
/// (UTC-5) — so the start midnight is 05:00Z and the peak band is
/// 13:00–19:00Z.
fn january_promotion() -> Promotion {
    Promotion {
        name: "January 2026 2x off-peak".to_string(),
        start_date: "2026-01-20".to_string(),
        end_date: "2026-01-22".to_string(),
        peak_start_hour_et: 8,
        peak_end_hour_et: 14,
        offpeak_multiplier: MULTIPLIER,
        applies_to: vec!["weekly_scoped".to_string()],
    }
}

/// November 1–3, 2026: the promotion's first day is the fall-back Sunday
/// (02:00 EDT → 01:00 EST at 06:00Z), its second the following Monday.
fn november_promotion() -> Promotion {
    Promotion {
        name: "November 2026 2x off-peak".to_string(),
        start_date: "2026-11-01".to_string(),
        end_date: "2026-11-03".to_string(),
        peak_start_hour_et: 8,
        peak_end_hour_et: 14,
        offpeak_multiplier: MULTIPLIER,
        applies_to: vec!["weekly_scoped".to_string()],
    }
}

/// EST activation and walks. `2026-01-20T04:59Z` is 23:59 Jan 19 in New York:
/// the UTC calendar already reads start_date but the ET date does not, so the
/// promotion must still be dormant — activation computed from UTC dates (or a
/// hardcoded EDT offset, which reads 00:59 Jan 20 at this instant) lights it
/// up early. One real hour later, 05:00Z is exactly Jan 20 00:00 ET: active
/// at last, and a +2h reset walks 00:00–02:00 ET, all off-peak, for exactly
/// 4.0 effective hours. The mixed walk then pins the winter peak band itself:
/// a 12:00 ET +4h reset spends 12:00–14:00 at 1x and 14:00–16:00 at 2x =
/// 6.0. With the band hardcoded at the EDT offsets (12:00–18:00Z) the same
/// UTC span walks 1.0 + 6.0 = 7.0 instead.
#[test]
fn est_promotion_activates_and_walks_on_the_eastern_clock() {
    // One minute before the EST start midnight: dormant on the ET date.
    let before = Utc.with_ymd_and_hms(2026, 1, 20, 4, 59, 0)
        .single()
        .unwrap();
    let st = drive_observe_with(before, (1, 2, 2), true, &[january_promotion()]);
    assert!(!st.schedule.is_peak_hour, "23:59 ET Monday is off-peak");
    assert!(
        !st.schedule.is_promo_active,
        "ET date is Jan 19 — 04:59Z must not activate a promo computed on \
         UTC dates or a hardcoded EDT offset"
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "no burn boost at 23:59 Jan 19 ET",
    );

    // 05:00Z is exactly Jan 20 00:00 ET — the promo's first minute.
    let start = et(2026, 1, 20, 0, 0);
    assert_eq!(
        start,
        Utc.with_ymd_and_hms(2026, 1, 20, 5, 0, 0).single().unwrap(),
        "Jan 20 00:00 ET is 05:00Z in EST, not the 04:00Z an EDT hardcode reads"
    );
    let st = drive_observe_with(start, (1, 2, 2), true, &[january_promotion()]);
    assert!(!st.schedule.is_peak_hour);
    assert!(st.schedule.is_promo_active, "00:00 ET Jan 20 is start_date");
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        2.0,
        "boosted at the EST midnight",
    );
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        4.0,
        "00:00–02:00 ET is two boosted hours",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        1.0,
        "five_hour stays raw across the EST start midnight",
    );

    // The winter peak band: 12:00 ET is 17:00Z in EST.
    let noon = et(2026, 1, 20, 12, 0);
    let st = drive_observe_with(noon, (1, 2, 4), true, &[january_promotion()]);
    assert!(
        st.schedule.is_peak_hour,
        "12:00 ET Jan 20 is peak — 13:00–19:00Z, not the 12:00–18:00Z EDT band"
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        1.0,
        "the peak gate holds under EST too",
    );
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        6.0,
        "12:00–14:00 peak at 1x + 14:00–16:00 off-peak at 2x = 6.0; an EDT \
         hardcode walks this same UTC span as 7.0",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        1.0,
        "12:00–13:00 ET is one peak hour",
    );
}

/// The fall-back hour through the walk. DST-transition Sundays are off-peak
/// all day by design, so a reset span crossing the repeated hour is ALL
/// boosted minutes: Nov 1 00:30 EDT + 3 wall hours = 6.0 effective — a walk
/// that double-counted or dropped the repeated local hour cannot land there.
/// And the repeated hour itself stays Sunday: 01:30 ET (EDT) + 1 real hour
/// reads 01:30 EST, still off-peak, exactly 2.0.
#[test]
fn dst_fall_back_sunday_walks_as_one_boosted_stretch() {
    // 04:30Z = 00:30 EDT Sunday. +3h crosses the 06:00Z fall-back.
    let now = et(2026, 11, 1, 0, 30);
    assert_eq!(
        now,
        Utc.with_ymd_and_hms(2026, 11, 1, 4, 30, 0).single().unwrap(),
        "00:30 ET on Nov 1 2026 is still EDT (04:30Z)"
    );
    let st = drive_observe_with(now, (1, 2, 3), true, &[november_promotion()]);
    assert!(
        !st.schedule.is_peak_hour,
        "the DST-transition Sunday is off-peak all day"
    );
    assert!(st.schedule.is_promo_active, "Nov 1 is start_date");
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        2.0,
        "Sunday off-peak, promo on",
    );
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        6.0,
        "3 wall hours across the repeated hour are all boosted minutes",
    );
    assert_close(
        st.capacity_forecast.five_hour.hours_remaining,
        1.0,
        "five_hour stays raw across the fall-back",
    );
    assert_close(
        st.capacity_forecast.seven_day.hours_remaining,
        2.0,
        "seven_day stays raw across the fall-back",
    );

    // The repeated local hour: 05:30Z is 01:30 EDT; one real hour later,
    // 06:30Z reads 01:30 EST — the same wall clock, one hour older.
    let now = Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0)
        .single()
        .unwrap();
    let st = drive_observe_with(now, (1, 2, 1), true, &[november_promotion()]);
    assert!(
        !st.schedule.is_peak_hour,
        "the repeated 01:00 hour is still Sunday, still off-peak"
    );
    assert_close(
        st.schedule.promo_multiplier_weekly_scoped,
        2.0,
        "the repeated hour keeps the boost",
    );
    assert_close(
        st.capacity_forecast.weekly_scoped.hours_remaining,
        2.0,
        "01:30 EDT → 01:30 EST is one boosted real hour",
    );
}
