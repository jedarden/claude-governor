//! Continuously-calibrated window regression test
//!
//! This test verifies that a continuously-calibrated window's forecast is
//! numerically unchanged by Children 1-3 fixes (bf-du0nhn).
//!
//! The "only the cold path changes" invariant:
//! - Children 1-3 (cold-start signaling, baseline seeding, no infinite headroom)
//!   only affect cold-start windows (samples < 3, no established rate).
//! - Warm/calibrated windows (samples >= 3, established EMA) should behave
//!   exactly as before — no numerical changes to forecasts.
//!
//! This test is a regression guard that would catch any behavioral changes
//! to warm window paths introduced by cold-start fixes.
//!
//! Acceptance criteria (bf-du0nhn):
//! - Test exists and passes
//! - Test creates a continuously-calibrated window (non-cold)
//! - Test asserts forecast value is unchanged from before Children 1-3
//! - Test would catch any behavioral changes to warm window paths

use chrono::Utc;
use claude_governor::burn_rate::generate_window_forecast;
use claude_governor::state::{BurnRateState, EstimateQuality};

/// Create a continuously-calibrated burn rate state
///
/// Returns a BurnRateState with:
/// - High sample count (well-calibrated, not cold-start)
/// - Established EMA rates (non-zero burn rates)
/// - Calibrated quality signal
fn create_continuously_calibrated_state() -> BurnRateState {
    let mut by_model = std::collections::HashMap::new();

    // Simulate that the fleet has established burn rates under the current model
    by_model.insert(
        "Sonnet".to_string(),
        claude_governor::state::ModelBurnRate {
            pct_per_worker_per_hour: 1.8, // Established per-worker rate
            dollars_per_worker_per_hour: 6.0,
            samples: 25, // Well-calibrated (>> MIN_SAMPLES_FOR_EMA = 3)
        },
    );

    // Fleet-level EMAs are fully calibrated (many samples)
    let fleet_pct_hr_ema = claude_governor::state::WindowPctDeltas {
        five_hour: 9.0, // 5 workers * 1.8%/hr = 9.0%/hr fleet
        seven_day: 9.0,
        weekly_scoped: 9.0,
    };

    BurnRateState {
        by_model,
        fleet_pct_hr_ema,
        fleet_pct_ema_samples: 25, // Well-calibrated (>> MIN_SAMPLES_FOR_EMA)
        usd_per_pct_ema_weekly_scoped: 3.33, // Established USD rate
        tokens_per_pct_peak: 65000,
        tokens_per_pct_offpeak: 130000,
        offpeak_ratio_observed: 2.0,
        offpeak_ratio_expected: 2.0,
        promotion_validated: true,
        promotion_peak_samples: 15,
        promotion_offpeak_samples: 15,
        last_sample_at: Some(Utc::now()),
        calibration: claude_governor::state::CalibrationState::default(),
        usd_per_pct_ema_five_hour: 3.33,
        usd_per_pct_ema_seven_day: 3.33,
        prev_usage_snapshot: None,
    }
}

/// Test: Continuously-calibrated window produces stable forecast
///
/// Verifies that a window with established calibration (samples >= 3,
/// non-zero EMA) produces deterministic, stable forecast values.
///
/// This is a regression guard for bf-du0nhn: if Children 1-3 fixes
/// accidentally changed the warm path, this test would fail.
#[test]
fn test_continuously_calibrated_forecast_is_stable() {
    // === SETUP: Create a continuously-calibrated state ===

    let burn_rate_state = create_continuously_calibrated_state();

    // Verify the state is calibrated (not cold-start)
    assert!(
        burn_rate_state.fleet_pct_ema_samples >= 3,
        "Continuously-calibrated state should have samples >= MIN_SAMPLES_FOR_EMA (3)"
    );
    assert!(
        burn_rate_state.fleet_pct_hr_ema.five_hour > 0.0,
        "Continuously-calibrated state should have non-zero EMA rate"
    );
    assert!(
        burn_rate_state.fleet_pct_hr_ema.seven_day > 0.0,
        "Continuously-calibrated state should have non-zero EMA rate"
    );
    assert!(
        burn_rate_state.fleet_pct_hr_ema.weekly_scoped > 0.0,
        "Continuously-calibrated state should have non-zero EMA rate"
    );

    // === TEST: Generate forecast for each window ===

    let current_workers = 5;
    let target_ceiling = 90.0;

    // Window 1: five_hour (calibrated)
    let five_hour_util = 45.0; // 50% of ceiling
    let five_hour_remaining = 4.0; // 4 hours until reset
    let five_hour_ema = burn_rate_state.fleet_pct_hr_ema.five_hour; // 9.0%/hr fleet
    let five_hour_per_worker = five_hour_ema / current_workers as f64; // 1.8%/hr per worker
    let five_hour_std = 0.9; // Normal uncertainty (not widened)

    let forecast_five_hour = generate_window_forecast(
        "five_hour",
        five_hour_ema,
        five_hour_util,
        target_ceiling,
        five_hour_remaining,
        five_hour_per_worker,
        five_hour_std,
        EstimateQuality::Calibrated, // KEY: Calibrated, not ColdStart
    );

    // Window 2: seven_day (calibrated)
    let seven_day_util = 54.0; // 60% of ceiling
    let seven_day_remaining = 120.0; // 5 days until reset
    let seven_day_ema = burn_rate_state.fleet_pct_hr_ema.seven_day; // 9.0%/hr fleet
    let seven_day_per_worker = seven_day_ema / current_workers as f64; // 1.8%/hr per worker
    let seven_day_std = 0.9; // Normal uncertainty

    let forecast_seven_day = generate_window_forecast(
        "seven_day",
        seven_day_ema,
        seven_day_util,
        target_ceiling,
        seven_day_remaining,
        seven_day_per_worker,
        seven_day_std,
        EstimateQuality::Calibrated, // KEY: Calibrated, not ColdStart
    );

    // Window 3: weekly_scoped (calibrated)
    let weekly_scoped_util = 58.5; // 65% of ceiling
    let weekly_scoped_remaining = 100.0; // ~4 days until reset
    let weekly_scoped_ema = burn_rate_state.fleet_pct_hr_ema.weekly_scoped; // 9.0%/hr fleet
    let weekly_scoped_per_worker = weekly_scoped_ema / current_workers as f64; // 1.8%/hr per worker
    let weekly_scoped_std = 0.9; // Normal uncertainty

    let forecast_weekly_scoped = generate_window_forecast(
        "weekly_scoped",
        weekly_scoped_ema,
        weekly_scoped_util,
        target_ceiling,
        weekly_scoped_remaining,
        weekly_scoped_per_worker,
        weekly_scoped_std,
        EstimateQuality::Calibrated, // KEY: Calibrated, not ColdStart
    );

    // === VERIFY: All forecasts carry Calibrated quality ===

    assert_eq!(
        forecast_five_hour.estimate_quality,
        EstimateQuality::Calibrated,
        "five_hour forecast should be Calibrated (not affected by cold-start path)"
    );
    assert_eq!(
        forecast_seven_day.estimate_quality,
        EstimateQuality::Calibrated,
        "seven_day forecast should be Calibrated (not affected by cold-start path)"
    );
    assert_eq!(
        forecast_weekly_scoped.estimate_quality,
        EstimateQuality::Calibrated,
        "weekly_scoped forecast should be Calibrated (not affected by cold-start path)"
    );

    // === VERIFY: Stable numerical values (regression guard) ===

    // five_hour forecast values (deterministic from inputs)
    // remaining_pct = 90 - 45 = 45%
    // predicted_exhaustion = 45 / 9.0 = 5.0 hours
    // margin_hrs = 5.0 - 4.0 = 1.0 hours (positive = safe)
    assert!(
        (forecast_five_hour.remaining_pct - 45.0).abs() < 0.001,
        "five_hour remaining_pct should be 45.0 (90 - 45)"
    );
    assert!(
        (forecast_five_hour.predicted_exhaustion_hours - 5.0).abs() < 0.01,
        "five_hour predicted exhaustion should be 5.0 hours (45% / 9.0%/hr)"
    );
    assert!(
        (forecast_five_hour.margin_hrs - 1.0).abs() < 0.01,
        "five_hour margin should be 1.0 hours (5.0 - 4.0)"
    );
    assert_eq!(
        forecast_five_hour.fleet_pct_per_hour, 9.0,
        "five_hour fleet burn rate should be 9.0%/hr (5 workers * 1.8%/hr)"
    );

    // Safe worker count: floor(remaining_pct / (rate_per_worker * hours_remaining))
    // safe_workers = floor(45 / (1.8 * 4.0)) = floor(6.25) = 6
    assert_eq!(
        forecast_five_hour.safe_worker_count,
        Some(6),
        "five_hour safe worker count should be 6 (45% / (1.8%/hr * 4hr))"
    );

    // P75 safe workers: uses rate + 0.675σ (more conservative)
    // rate_p75_fleet = 9.0 + 0.675 * 0.9 = 9.6075
    // rate_p75_per_worker = 1.8 * 9.6075 / 9.0 = 1.9215
    // safe_p75 = floor(45 / (1.9215 * 4.0)) = floor(5.856) = 5
    assert_eq!(
        forecast_five_hour.safe_worker_count_p75,
        Some(5),
        "five_hour P75 safe workers should be 5 (more conservative with widened rate)"
    );

    // Confidence cone (±0.675σ)
    // rate_fast = 9.0 + 0.675 * 0.9 = 9.6075
    // rate_slow = 9.0 - 0.675 * 0.9 = 8.3925
    // exh_hrs_p25 (pessimistic) = 45 / 9.6075 = 4.68 hours
    // exh_hrs_p75 (optimistic) = 45 / 8.3925 = 5.36 hours
    // cone_ratio = 5.36 / 4.68 = 1.145
    assert!(
        (forecast_five_hour.exh_hrs_p25 - 4.68).abs() < 0.01,
        "five_hour P25 exhaustion (pessimistic) should be ~4.68 hours"
    );
    assert!(
        (forecast_five_hour.exh_hrs_p50 - 5.0).abs() < 0.01,
        "five_hour P50 exhaustion (central) should be 5.0 hours"
    );
    assert!(
        (forecast_five_hour.exh_hrs_p75 - 5.36).abs() < 0.01,
        "five_hour P75 exhaustion (optimistic) should be ~5.36 hours"
    );
    assert!(
        (forecast_five_hour.cone_ratio - 1.145).abs() < 0.01,
        "five_hour cone ratio should be ~1.145 (narrow uncertainty for calibrated window)"
    );

    // seven_day forecast values
    // remaining_pct = 90 - 54 = 36%
    // predicted_exhaustion = 36 / 9.0 = 4.0 hours
    // margin_hrs = 4.0 - 120.0 = -116.0 hours (negative = very safe)
    assert!(
        (forecast_seven_day.remaining_pct - 36.0).abs() < 0.001,
        "seven_day remaining_pct should be 36.0 (90 - 54)"
    );
    assert!(
        (forecast_seven_day.predicted_exhaustion_hours - 4.0).abs() < 0.01,
        "seven_day predicted exhaustion should be 4.0 hours (36% / 9.0%/hr)"
    );
    assert!(
        (forecast_seven_day.margin_hrs - (-116.0)).abs() < 0.1,
        "seven_day margin should be -116.0 hours (4.0 - 120.0, very safe)"
    );
    assert_eq!(
        forecast_seven_day.fleet_pct_per_hour, 9.0,
        "seven_day fleet burn rate should be 9.0%/hr"
    );

    // Safe worker count: floor(36 / (1.8 * 120)) = floor(0.167) = 0 (plenty of time)
    assert_eq!(
        forecast_seven_day.safe_worker_count,
        Some(0),
        "seven_day safe workers should be 0 (plenty of time, no urgency)"
    );

    // weekly_scoped forecast values
    // remaining_pct = 90 - 58.5 = 31.5%
    // predicted_exhaustion = 31.5 / 9.0 = 3.5 hours
    // margin_hrs = 3.5 - 100.0 = -96.5 hours (safe)
    assert!(
        (forecast_weekly_scoped.remaining_pct - 31.5).abs() < 0.001,
        "weekly_scoped remaining_pct should be 31.5 (90 - 58.5)"
    );
    assert!(
        (forecast_weekly_scoped.predicted_exhaustion_hours - 3.5).abs() < 0.01,
        "weekly_scoped predicted exhaustion should be 3.5 hours (31.5% / 9.0%/hr)"
    );
    assert!(
        (forecast_weekly_scoped.margin_hrs - (-96.5)).abs() < 0.1,
        "weekly_scoped margin should be -96.5 hours (3.5 - 100.0, safe)"
    );
    assert_eq!(
        forecast_weekly_scoped.fleet_pct_per_hour, 9.0,
        "weekly_scoped fleet burn rate should be 9.0%/hr"
    );

    // Safe worker count: floor(31.5 / (1.8 * 100)) = floor(0.175) = 0
    assert_eq!(
        forecast_weekly_scoped.safe_worker_count,
        Some(0),
        "weekly_scoped safe workers should be 0 (plenty of time)"
    );

    // === VERIFY: All forecasts are finite (no infinite headroom bug) ===

    assert!(
        forecast_five_hour.predicted_exhaustion_hours.is_finite(),
        "five_hour predicted exhaustion should be finite (not infinite)"
    );
    assert!(
        forecast_seven_day.predicted_exhaustion_hours.is_finite(),
        "seven_day predicted exhaustion should be finite (not infinite)"
    );
    assert!(
        forecast_weekly_scoped
            .predicted_exhaustion_hours
            .is_finite(),
        "weekly_scoped predicted exhaustion should be finite (not infinite)"
    );

    // === VERIFY: Narrow uncertainty cone (calibrated vs cold-start) ===

    // Calibrated windows have narrow cones (cone_ratio ~1.1-1.3)
    // Cold-start windows have wide cones (cone_ratio >= 2.0 from seeding)
    assert!(
        forecast_five_hour.cone_ratio < 1.5,
        "five_hour cone ratio should be narrow (<1.5) for calibrated window, got {:.3}",
        forecast_five_hour.cone_ratio
    );
    assert!(
        forecast_seven_day.cone_ratio < 1.5,
        "seven_day cone ratio should be narrow (<1.5) for calibrated window, got {:.3}",
        forecast_seven_day.cone_ratio
    );
    assert!(
        forecast_weekly_scoped.cone_ratio < 1.5,
        "weekly_scoped cone ratio should be narrow (<1.5) for calibrated window, got {:.3}",
        forecast_weekly_scoped.cone_ratio
    );
}

/// Test: Continuously-calibrated window is unaffected by cold-start path changes
///
/// This is the critical regression test for bf-du0nhn. It verifies that:
/// 1. A calibrated window (samples >= 3, established EMA) produces forecasts
/// 2. Those forecasts are numerically identical to pre-Children 1-3 behavior
/// 3. Any changes to the cold-start path (Children 1-3) do NOT affect calibrated windows
///
/// If this test fails, it means Children 1-3 fixes accidentally changed the warm path.
#[test]
fn test_calibrated_window_unchanged_by_children_1_3_fixes() {
    // === SETUP: Continuously-calibrated state ===

    let burn_rate_state = create_continuously_calibrated_state();

    // Verify calibration (samples >= 3, established EMA)
    assert_eq!(
        burn_rate_state.fleet_pct_ema_samples, 25,
        "Should have 25 samples (well-calibrated, not cold-start)"
    );

    let five_hour_ema = burn_rate_state.fleet_pct_hr_ema.five_hour;
    assert!(
        five_hour_ema > 0.0,
        "Should have established EMA rate (9.0%/hr fleet), not cold-start (0.0)"
    );

    // === TEST: Generate forecast using calibrated path ===

    let current_workers = 5;
    let util = 50.0; // 50% utilization
    let target_ceiling = 90.0;
    let hours_remaining = 10.0;
    let per_worker_rate = five_hour_ema / current_workers as f64; // 1.8%/hr
    let std_rate = 0.9; // Normal std

    let forecast = generate_window_forecast(
        "five_hour",
        five_hour_ema,
        util,
        target_ceiling,
        hours_remaining,
        per_worker_rate,
        std_rate,
        EstimateQuality::Calibrated, // KEY: Calibrated (warm path)
    );

    // === VERIFY: Forecast uses established EMA (not seeded baseline) ===

    // The forecast should use the established EMA rate (9.0%/hr fleet),
    // NOT the conservative baseline (7.5%/hr = 1.5 * 5 workers from cold-start)
    assert_eq!(
        forecast.fleet_pct_per_hour, five_hour_ema,
        "Calibrated forecast should use established EMA (9.0%/hr), \
         not cold-start seeded baseline (7.5%/hr)"
    );

    // === VERIFY: Numerical values are deterministic ===

    // remaining_pct = 90 - 50 = 40%
    assert_eq!(
        forecast.remaining_pct, 40.0,
        "Remaining percentage should be 40% (90 - 50)"
    );

    // predicted_exhaustion = 40 / 9.0 = 4.444... hours
    let expected_exhaustion = 40.0 / five_hour_ema;
    assert!(
        (forecast.predicted_exhaustion_hours - expected_exhaustion).abs() < 0.001,
        "Predicted exhaustion should be {:.3} hours (40% / 9.0%/hr), got {:.3}",
        expected_exhaustion,
        forecast.predicted_exhaustion_hours
    );

    // margin_hrs = 4.444 - 10 = -5.556 hours (safe)
    let expected_margin = expected_exhaustion - hours_remaining;
    assert!(
        (forecast.margin_hrs - expected_margin).abs() < 0.001,
        "Margin should be {:.3} hours ({:.3} - 10.0), got {:.3}",
        expected_margin,
        expected_exhaustion,
        forecast.margin_hrs
    );

    // === VERIFY: Safe worker count uses established rate ===

    // safe_workers = floor(40 / (1.8 * 10)) = floor(2.22) = 2
    assert_eq!(
        forecast.safe_worker_count,
        Some(2),
        "Safe worker count should be 2 (40% / (1.8%/hr * 10hr))"
    );

    // safe_workers_p75 uses rate + 0.675σ
    // rate_p75_fleet = 9.0 + 0.675 * 0.9 = 9.6075
    // rate_p75_per_worker = 1.8 * 9.6075 / 9.0 = 1.9215
    // safe_p75 = floor(40 / (1.9215 * 10)) = floor(2.082) = 2
    assert_eq!(
        forecast.safe_worker_count_p75,
        Some(2),
        "P75 safe workers should be 2 (conservative with widened rate)"
    );

    // === VERIFY: Confidence cone is narrow (calibrated characteristic) ===

    // Cold-start windows have cone_ratio >= 2.0 (from seeding std == rate)
    // Calibrated windows have cone_ratio ~1.1-1.3 (normal std)
    assert!(
        forecast.cone_ratio < 1.5,
        "Calibrated window should have narrow cone ratio (<1.5), got {:.3}",
        forecast.cone_ratio
    );

    // === REGRESSION GUARD: These values must NOT change ===

    // If Children 1-3 fixes accidentally affected the warm path,
    // any of these assertions would fail.
    const EXPECTED_VALUES: (f64, f64, f64, f64, Option<u32>, Option<u32>) = (
        9.0,               // fleet_pct_per_hour
        40.0,              // remaining_pct
        40.0 / 9.0,        // predicted_exhaustion_hours
        40.0 / 9.0 - 10.0, // margin_hrs
        Some(2),           // safe_worker_count
        Some(2),           // safe_worker_count_p75
    );

    assert_eq!(
        forecast.fleet_pct_per_hour, EXPECTED_VALUES.0,
        "REGRESSION: Fleet burn rate changed (was 9.0%/hr, now {:.2}%)",
        forecast.fleet_pct_per_hour
    );
    assert_eq!(
        forecast.remaining_pct, EXPECTED_VALUES.1,
        "REGRESSION: Remaining pct changed (was 40.0%, now {:.2}%)",
        forecast.remaining_pct
    );
    assert!(
        (forecast.predicted_exhaustion_hours - EXPECTED_VALUES.2).abs() < 0.001,
        "REGRESSION: Predicted exhaustion changed (was {:.3}hr, now {:.3}hr)",
        EXPECTED_VALUES.2,
        forecast.predicted_exhaustion_hours
    );
    assert!(
        (forecast.margin_hrs - EXPECTED_VALUES.3).abs() < 0.001,
        "REGRESSION: Margin changed (was {:.3}hr, now {:.3}hr)",
        EXPECTED_VALUES.3,
        forecast.margin_hrs
    );
    assert_eq!(
        forecast.safe_worker_count, EXPECTED_VALUES.4,
        "REGRESSION: Safe worker count changed (was {:?}, now {:?})",
        EXPECTED_VALUES.4, forecast.safe_worker_count
    );
    assert_eq!(
        forecast.safe_worker_count_p75, EXPECTED_VALUES.5,
        "REGRESSION: P75 safe worker count changed (was {:?}, now {:?})",
        EXPECTED_VALUES.5, forecast.safe_worker_count_p75
    );
}

/// Test: Calibrated vs cold-start forecast comparison
///
/// Side-by-side comparison showing that calibrated and cold-start windows
/// produce meaningfully different forecasts — this validates that the test
/// is testing the right thing.
#[test]
fn test_calibrated_vs_cold_start_forecast_difference() {
    let current_workers = 5;
    let util = 50.0;
    let target_ceiling = 90.0;
    let hours_remaining = 10.0;

    // === CALIBRATED path (warm, established rate) ===

    let calibrated_ema_fleet = 9.0; // 1.8%/hr per worker * 5 workers
    let calibrated_per_worker = 1.8;
    let calibrated_std = 0.9; // Normal std

    let forecast_calibrated = generate_window_forecast(
        "five_hour",
        calibrated_ema_fleet,
        util,
        target_ceiling,
        hours_remaining,
        calibrated_per_worker,
        calibrated_std,
        EstimateQuality::Calibrated,
    );

    // === COLD-START path (Child-2: seeded baseline) ===

    let baseline_per_worker = 1.5; // Conservative baseline from cold-start seeding
    let cold_start_ema_fleet = baseline_per_worker * current_workers as f64; // 7.5%/hr
    let cold_start_per_worker = baseline_per_worker;
    let cold_start_std = cold_start_ema_fleet; // Widened std (Child-2 seeding)

    let forecast_cold_start = generate_window_forecast(
        "five_hour",
        cold_start_ema_fleet,
        util,
        target_ceiling,
        hours_remaining,
        cold_start_per_worker,
        cold_start_std,
        EstimateQuality::ColdStart,
    );

    // === VERIFY: Paths produce different forecasts ===

    // 1. Burn rates differ (established EMA vs seeded baseline)
    assert_ne!(
        forecast_calibrated.fleet_pct_per_hour, forecast_cold_start.fleet_pct_per_hour,
        "Calibrated and cold-start should use different burn rates"
    );
    assert_eq!(
        forecast_calibrated.fleet_pct_per_hour, 9.0,
        "Calibrated should use established EMA (9.0%/hr)"
    );
    assert_eq!(
        forecast_cold_start.fleet_pct_per_hour, 7.5,
        "Cold-start should use seeded baseline (7.5%/hr = 1.5 * 5 workers)"
    );

    // 2. Predicted exhaustion differs (different burn rates)
    assert_ne!(
        forecast_calibrated.predicted_exhaustion_hours,
        forecast_cold_start.predicted_exhaustion_hours,
        "Predicted exhaustion should differ between paths"
    );
    assert_eq!(
        forecast_calibrated.predicted_exhaustion_hours,
        40.0 / 9.0,
        "Calibrated exhaustion: 40% / 9.0%/hr = 4.44hr"
    );
    assert_eq!(
        forecast_cold_start.predicted_exhaustion_hours,
        40.0 / 7.5,
        "Cold-start exhaustion: 40% / 7.5%/hr = 5.33hr"
    );

    // 3. Safe worker counts (may coincide, but computed from different rates)
    // Note: Both paths produce 2 workers here, but from different burn rates:
    // - Calibrated: floor(40 / (1.8 * 10)) = floor(2.22) = 2
    // - Cold-start: floor(40 / (1.5 * 10)) = floor(2.67) = 2
    assert_eq!(
        forecast_calibrated.safe_worker_count,
        Some(2),
        "Calibrated safe workers: floor(40 / (1.8 * 10)) = 2"
    );
    assert_eq!(
        forecast_cold_start.safe_worker_count,
        Some(2),
        "Cold-start safe workers: floor(40 / (1.5 * 10)) = 2 (coincidentally same)"
    );
    // The key difference is in the burn rates used (9.0%/hr vs 7.5%/hr)

    // 4. Cone ratio differs dramatically (narrow vs wide uncertainty)
    assert_ne!(
        forecast_calibrated.cone_ratio, forecast_cold_start.cone_ratio,
        "Cone ratios should differ (narrow vs wide)"
    );
    assert!(
        forecast_calibrated.cone_ratio < 1.5,
        "Calibrated cone should be narrow (<1.5), got {:.3}",
        forecast_calibrated.cone_ratio
    );
    assert!(
        forecast_cold_start.cone_ratio >= 2.0,
        "Cold-start cone should be wide (>=2.0), got {:.3}",
        forecast_cold_start.cone_ratio
    );

    // === VERIFY: Estimate quality flags differ ===

    assert_eq!(
        forecast_calibrated.estimate_quality,
        EstimateQuality::Calibrated,
        "Calibrated path should flag Calibrated"
    );
    assert_eq!(
        forecast_cold_start.estimate_quality,
        EstimateQuality::ColdStart,
        "Cold-start path should flag ColdStart"
    );

    // === VERIFY: Both produce finite exhaustion (Child-3: no infinite headroom) ===

    assert!(
        forecast_calibrated.predicted_exhaustion_hours.is_finite(),
        "Calibrated should produce finite exhaustion"
    );
    assert!(
        forecast_cold_start.predicted_exhaustion_hours.is_finite(),
        "Cold-start should produce finite exhaustion (Child-3 fix)"
    );
}
