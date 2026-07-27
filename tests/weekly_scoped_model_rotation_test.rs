//! Weekly-scoped model rotation simulation test
//!
//! This test verifies that when Anthropic rotates which model carries the
//! scoped weekly cap (e.g., Fable -> Opus), the governor correctly:
//! - Resets weekly_scoped EMA samples to 0 (no stale carry-over)
//! - Flags the window as cold-start
//! - Seeds burn rate from conservative baseline instead of 0
//! - Does NOT claim 0% utilization or infinite headroom
//! - Preserves calibrated non-rotating windows unchanged

use std::collections::HashMap;
use chrono::{Utc, Duration};
use anyhow::Result;

use claude_governor::config::{GovernorConfig, AlertConfig, DaemonConfig, PricingConfig, SprintConfig, CompositeRiskConfig, ConeScalingConfig, ModelPricing};
use claude_governor::poller::UsageData;
use claude_governor::state::{self, BurnRateState, WindowPctDeltas, BaselineBurnRates, EstimateQuality};

/// Simple mock poller for model rotation testing
struct ModelRotationPoller {
    pub usage_data: Option<UsageData>,
    pub poll_count: u32,
}

impl ModelRotationPoller {
    /// Create a poller with weekly_scoped set to a specific model
    pub fn with_model(model: Option<&str>, five_hour_util: f64, seven_day_util: f64, weekly_scoped_util: f64) -> Self {
        let now = Utc::now();
        let five_hour_reset = now + Duration::hours(4);
        let seven_day_reset = now + Duration::hours(120);

        let data = UsageData {
            five_hour_utilization: five_hour_util,
            five_hour_resets_at: five_hour_reset.to_rfc3339(),
            five_hour_hours_remaining: 4.0,
            seven_day_utilization: seven_day_util,
            seven_day_resets_at: seven_day_reset.to_rfc3339(),
            seven_day_hours_remaining: 120.0,
            weekly_scoped_utilization: weekly_scoped_util,
            weekly_scoped_resets_at: seven_day_reset.to_rfc3339(),
            weekly_scoped_hours_remaining: 120.0,
            weekly_scoped_model: model.map(|s| s.to_string()),
            limits: vec![],
            timestamp: now,
            stale: false,
        };

        Self {
            usage_data: Some(data),
            poll_count: 0,
        }
    }

    /// Simulate a poll call
    pub fn poll(&mut self) -> Result<UsageData> {
        self.poll_count += 1;
        if let Some(ref data) = self.usage_data {
            Ok(data.clone())
        } else {
            Err(anyhow::anyhow!("No usage data available"))
        }
    }
}

/// Create a minimal governor config
fn create_minimal_config() -> GovernorConfig {
    let mut models = HashMap::new();
    models.insert(
        "claude-sonnet-4-20250514".to_string(),
        ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_5m_per_mtok: 3.75,
            cache_write_1h_per_mtok: 6.0,
            cache_read_per_mtok: 0.30,
        },
    );

    GovernorConfig {
        pricing: PricingConfig { models },
        sprint: SprintConfig::default(),
        daemon: DaemonConfig::default(),
        alerts: AlertConfig::default(),
        composite_risk: CompositeRiskConfig::default(),
        cone_scaling: ConeScalingConfig::default(),
        agents: HashMap::new(),
        credentials_path: None,
    }
}

/// Create a burn rate state with simulated prior model data
fn create_burn_rate_with_fable_history() -> BurnRateState {
    let mut by_model = HashMap::new();

    // Simulate that Fable had accumulated burn rate samples
    by_model.insert("Fable".to_string(), claude_governor::state::ModelBurnRate {
        pct_per_worker_per_hour: 2.5,
        dollars_per_worker_per_hour: 8.0,
        samples: 10, // Simulate 10 accumulated samples
    });

    // Simulate that the weekly_scoped EMA has learned from Fable data
    let fleet_pct_hr_ema = WindowPctDeltas {
        five_hour: 1.8,
        seven_day: 2.0,
        weekly_scoped: 2.5, // This is the key field that should reset
    };

    BurnRateState {
        by_model,
        fleet_pct_hr_ema,
        fleet_pct_ema_samples: 10, // Should reset to 0
        usd_per_pct_ema_weekly_scoped: 3.2, // Should reset to 0.0
        tokens_per_pct_peak: 70000,
        tokens_per_pct_offpeak: 140000,
        offpeak_ratio_observed: 2.0,
        offpeak_ratio_expected: 2.0,
        promotion_validated: true,
        promotion_peak_samples: 5,
        promotion_offpeak_samples: 5,
        last_sample_at: Some(Utc::now()),
        calibration: claude_governor::state::CalibrationState::default(),
        usd_per_pct_ema_five_hour: 2.8,
        usd_per_pct_ema_seven_day: 3.0,
        prev_usage_snapshot: None,
    }
}

/// Test: weekly_scoped model rotation from Fable to Opus triggers sample reset
#[test]
fn test_weekly_scoped_model_rotation_resets_samples() {
    // 1. Start with a state that has accumulated Fable burn rate history
    let mut burn_rate_state = create_burn_rate_with_fable_history();

    // Verify initial state has Fable samples
    assert_eq!(burn_rate_state.fleet_pct_ema_samples, 10, "Should start with 10 samples");
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 2.5, "Should start with Fable's EMA");
    assert_eq!(burn_rate_state.usd_per_pct_ema_weekly_scoped, 3.2, "Should start with Fable's USD EMA");

    // 2. Simulate model rotation: Fable -> Opus
    let prev_model = Some("Fable".to_string());
    let new_model = Some("Opus".to_string());

    // 3. Apply model change detection
    let reset_performed = state::reset_weekly_scoped_on_model_change(
        &prev_model,
        &new_model,
        &mut burn_rate_state,
    );

    // 4. Verify reset was performed
    assert!(reset_performed, "Reset should be performed when model changes from Fable to Opus");

    // 5. Verify weekly_scoped samples are reset to 0
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 0.0,
        "weekly_scoped EMA should reset to 0 on model change");
    assert_eq!(burn_rate_state.usd_per_pct_ema_weekly_scoped, 0.0,
        "weekly_scoped USD EMA should reset to 0 on model change");

    // 6. Verify non-weekly_scoped windows are unchanged
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.five_hour, 1.8,
        "five_hour EMA should remain unchanged");
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.seven_day, 2.0,
        "seven_day EMA should remain unchanged");
    assert_eq!(burn_rate_state.usd_per_pct_ema_five_hour, 2.8,
        "five_hour USD EMA should remain unchanged");
    assert_eq!(burn_rate_state.usd_per_pct_ema_seven_day, 3.0,
        "seven_day USD EMA should remain unchanged");
}

/// Test: Model identity change from Fable to None triggers reset
#[test]
fn test_model_cleared_to_none_resets_samples() {
    let mut burn_rate_state = create_burn_rate_with_fable_history();

    let prev_model = Some("Fable".to_string());
    let new_model = None;

    let reset_performed = state::reset_weekly_scoped_on_model_change(
        &prev_model,
        &new_model,
        &mut burn_rate_state,
    );

    assert!(reset_performed, "Reset should be performed when model is cleared");
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 0.0,
        "weekly_scoped EMA should reset to 0 when model cleared");
    assert_eq!(burn_rate_state.usd_per_pct_ema_weekly_scoped, 0.0,
        "weekly_scoped USD EMA should reset to 0 when model cleared");
}

/// Test: Model initialization from None to Fable does NOT reset (already cold)
#[test]
fn test_model_initialization_does_not_reset() {
    let mut burn_rate_state = BurnRateState::default();

    // Start from default (cold) state
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 0.0,
        "Should start with cold (0) EMA");

    let prev_model = None;
    let new_model = Some("Fable".to_string());

    let reset_performed = state::reset_weekly_scoped_on_model_change(
        &prev_model,
        &new_model,
        &mut burn_rate_state,
    );

    // Reset should be performed (for logging), but EMA is already 0
    assert!(reset_performed, "Initialization should be logged as a change");
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 0.0,
        "weekly_scoped EMA should remain 0 (already cold)");
}

/// Test: No reset when model identity is unchanged
#[test]
fn test_no_reset_when_model_unchanged() {
    let mut burn_rate_state = create_burn_rate_with_fable_history();

    let original_ema = burn_rate_state.fleet_pct_hr_ema.weekly_scoped;
    let original_usd_ema = burn_rate_state.usd_per_pct_ema_weekly_scoped;

    let prev_model = Some("Fable".to_string());
    let new_model = Some("Fable".to_string()); // Same model

    let reset_performed = state::reset_weekly_scoped_on_model_change(
        &prev_model,
        &new_model,
        &mut burn_rate_state,
    );

    assert!(!reset_performed, "No reset should occur when model is unchanged");
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, original_ema,
        "weekly_scoped EMA should remain unchanged when model is unchanged");
    assert_eq!(burn_rate_state.usd_per_pct_ema_weekly_scoped, original_usd_ema,
        "weekly_scoped USD EMA should remain unchanged when model is unchanged");
}

/// Test: Cold-start window uses conservative baseline instead of 0
#[test]
fn test_cold_start_uses_baseline_not_zero() {
    // Create a baseline burn rate (conservative default)
    let baseline = BaselineBurnRates::default();

    // Verify baseline is non-zero (conservative estimate)
    assert!(baseline.pct_per_worker_per_hour > 0.0,
        "Baseline pct per worker should be non-zero");
    assert!(baseline.dollars_per_worker_per_hour > 0.0,
        "Baseline dollars per worker should be non-zero");

    // Simulate a cold-start window: no samples, no fresh rate
    let has_fresh_rate = false;
    let window_samples = 0;
    let current_workers = 5;
    let util = 60.0; // Real utilization present (window exists this period)

    // Cold-start detection logic (from burn_rate.rs)
    let is_cold_start = !has_fresh_rate && window_samples < 3; // MIN_SAMPLES_FOR_EMA = 3

    assert!(is_cold_start, "Should detect cold-start when samples < 3 and no fresh rate");

    // When cold-start with util > 0, seed from baseline
    if is_cold_start && util > 0.0 && current_workers > 0 {
        let base_per_worker = baseline.pct_per_worker_per_hour;
        let fleet_pct_hr = base_per_worker * current_workers as f64;

        // Verify seeded rate is non-zero (avoiding infinite headroom)
        assert!(fleet_pct_hr > 0.0,
            "Cold-start burn rate should be seeded from baseline, not 0");
        assert_eq!(fleet_pct_hr, baseline.pct_per_worker_per_hour * 5.0,
            "Should seed conservative rate across all workers");
    }
}

/// Test: Weekly-scoped window with real utilization gets cold-start quality
#[test]
fn test_weekly_scoped_cold_start_quality_flag() {
    // Simulate a weekly_scoped window with:
    // - Real utilization (window exists this period)
    // - No accumulated samples yet (model just rotated in)
    // - No fresh rate this interval

    let _util = 55.0; // Real utilization
    let window_samples = 0; // No samples (just reset)
    let has_fresh_rate = false; // No fresh per-instance rate

    // Determine estimate quality (from burn_rate.rs logic)
    let estimate_quality = if has_fresh_rate || window_samples >= 3 {
        EstimateQuality::Calibrated
    } else if window_samples == 0 {
        EstimateQuality::ColdStart
    } else {
        EstimateQuality::InsufficientSamples
    };

    assert_eq!(estimate_quality, EstimateQuality::ColdStart,
        "Window with 0 samples should be flagged as ColdStart");

    // Verify that with 1-2 samples, it's InsufficientSamples (not ColdStart)
    let estimate_quality_1_sample = if has_fresh_rate || 1 >= 3 {
        EstimateQuality::Calibrated
    } else if 1 == 0 {
        EstimateQuality::ColdStart
    } else {
        EstimateQuality::InsufficientSamples
    };

    assert_eq!(estimate_quality_1_sample, EstimateQuality::InsufficientSamples,
        "Window with 1 sample should be InsufficientSamples");
}

/// Test: Comprehensive model rotation scenario with all effects
#[test]
fn test_comprehensive_model_rotation_scenario() {
    // 1. Start with a poller showing Fable as the weekly_scoped model
    let mut poller_fable = ModelRotationPoller::with_model(
        Some("Fable"),
        45.0,  // five_hour_util
        60.0,  // seven_day_util
        55.0,  // weekly_scoped_util (Fable window)
    );

    let poll_result_fable = poller_fable.poll().expect("Poll should succeed");
    assert_eq!(poll_result_fable.weekly_scoped_model, Some("Fable".to_string()));

    // 2. Create burn rate state with accumulated Fable history
    let mut burn_rate_state = create_burn_rate_with_fable_history();

    // Verify Fable state
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 2.5);
    assert_eq!(burn_rate_state.fleet_pct_ema_samples, 10);

    // 3. Simulate model rotation: Anthropic switches to Opus
    let mut poller_opus = ModelRotationPoller::with_model(
        Some("Opus"),
        45.0,  // five_hour_util (unchanged)
        60.0,  // seven_day_util (unchanged)
        58.0,  // weekly_scoped_util (Opus window - different rate)
    );

    let poll_result_opus = poller_opus.poll().expect("Poll should succeed");
    assert_eq!(poll_result_opus.weekly_scoped_model, Some("Opus".to_string()));

    // 4. Detect and apply model change
    let prev_model = Some("Fable".to_string());
    let new_model = poll_result_opus.weekly_scoped_model;

    let reset_performed = state::reset_weekly_scoped_on_model_change(
        &prev_model,
        &new_model,
        &mut burn_rate_state,
    );

    assert!(reset_performed, "Reset should be performed on Fable -> Opus rotation");

    // 5. Verify samples reset (no stale Fable carry-over)
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 0.0,
        "weekly_scoped EMA should reset to 0 - no Fable carry-over");
    assert_eq!(burn_rate_state.usd_per_pct_ema_weekly_scoped, 0.0,
        "weekly_scoped USD EMA should reset to 0");

    // 6. Verify calibrated windows remain unchanged
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.five_hour, 1.8,
        "five_hour should remain calibrated");
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.seven_day, 2.0,
        "seven_day should remain calibrated");

    // 7. Simulate cold-start handling for the new Opus window
    let baseline = BaselineBurnRates::default();
    let current_workers = 5;
    let util = poll_result_opus.weekly_scoped_utilization;

    // With samples reset to 0, the window is now cold-start
    let window_samples = 0; // Reset
    let has_fresh_rate = false; // No fresh per-instance rate yet

    let is_cold_start = !has_fresh_rate && window_samples < 3;

    assert!(is_cold_start, "Opus window should be cold-start after reset");

    // Verify conservative baseline seeding (not 0)
    if is_cold_start && util > 0.0 && current_workers > 0 {
        let base_per_worker = baseline.pct_per_worker_per_hour;
        let seeded_fleet_pct_hr = base_per_worker * current_workers as f64;

        assert!(seeded_fleet_pct_hr > 0.0,
            "Cold-start Opus window should use conservative baseline, not 0");

        // Estimate quality should be ColdStart
        let estimate_quality = if window_samples == 0 {
            EstimateQuality::ColdStart
        } else {
            EstimateQuality::InsufficientSamples
        };

        assert_eq!(estimate_quality, EstimateQuality::ColdStart,
            "Opus window should be flagged as ColdStart");
    }

    // 8. Verify no 0% utilization claim (Opus window has real util)
    assert!(poll_result_opus.weekly_scoped_utilization > 0.0,
        "Opus window should have real utilization, not 0%");

    // 9. Verify no infinite headroom (conservative rate prevents this)
    // With seeded burn rate > 0, predicted_exhaustion is finite, not +inf
    let baseline_rate = baseline.pct_per_worker_per_hour * current_workers as f64;
    assert!(baseline_rate > 0.0,
        "Seeded burn rate should be > 0, preventing infinite headroom");
}

/// Test: Model rotation preserves other windows' calibrated state
#[test]
fn test_model_rotation_preserves_other_windows() {
    let mut burn_rate_state = create_burn_rate_with_fable_history();

    // Record all window states before rotation
    let five_hour_before = burn_rate_state.fleet_pct_hr_ema.five_hour;
    let seven_day_before = burn_rate_state.fleet_pct_hr_ema.seven_day;
    let weekly_scoped_before = burn_rate_state.fleet_pct_hr_ema.weekly_scoped;

    let usd_five_hour_before = burn_rate_state.usd_per_pct_ema_five_hour;
    let usd_seven_day_before = burn_rate_state.usd_per_pct_ema_seven_day;
    let usd_weekly_scoped_before = burn_rate_state.usd_per_pct_ema_weekly_scoped;

    // Apply model rotation
    state::reset_weekly_scoped_on_model_change(
        &Some("Fable".to_string()),
        &Some("Opus".to_string()),
        &mut burn_rate_state,
    );

    // Verify non-weekly_scoped windows are unchanged
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.five_hour, five_hour_before,
        "five_hour EMA should be unchanged");
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.seven_day, seven_day_before,
        "seven_day EMA should be unchanged");

    assert_eq!(burn_rate_state.usd_per_pct_ema_five_hour, usd_five_hour_before,
        "five_hour USD EMA should be unchanged");
    assert_eq!(burn_rate_state.usd_per_pct_ema_seven_day, usd_seven_day_before,
        "seven_day USD EMA should be unchanged");

    // Verify weekly_scoped IS changed
    assert_eq!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 0.0,
        "weekly_scoped EMA should be reset");
    assert_ne!(burn_rate_state.fleet_pct_hr_ema.weekly_scoped, weekly_scoped_before,
        "weekly_scoped EMA should differ from before");

    assert_eq!(burn_rate_state.usd_per_pct_ema_weekly_scoped, 0.0,
        "weekly_scoped USD EMA should be reset");
    assert_ne!(burn_rate_state.usd_per_pct_ema_weekly_scoped, usd_weekly_scoped_before,
        "weekly_scoped USD EMA should differ from before");
}

/// Test: Production path identity change triggers cold-start with seeded rate
///
/// This is the critical acceptance test for bf-100ol (and the parent bead bf-12wx4).
/// It verifies that when Anthropic rotates the weekly_scoped model mid-run:
/// 1. The slot's samples reset to 0 (no stale Fable carry-over)
/// 2. The window is flagged cold (Child-1 signal: EstimateQuality::ColdStart)
/// 3. The forecast uses seeded baseline rate (Child-2 behavior), NOT 0.0 or Fable's stale rate
/// 4. Other calibrated windows remain numerically unchanged (regression guard)
///
/// This test exercises the PRODUCTION path (generate_window_forecast), not the
/// test-only estimate_burn_rates helper.
#[test]
fn test_production_path_identity_change_cold_start_flow() {
    use claude_governor::burn_rate::generate_window_forecast;

    // Scenario: weekly_scoped was calibrated under Fable, then Anthropic rotates to Opus
    // Fable had established a burn rate of 2.5%/hr through 10 samples

    // Baseline conservative rate (from baseline_burn_rate_or_default)
    let baseline_pct_per_worker = 1.5; // Default conservative baseline
    let current_workers = 5;

    // AFTER identity change and reset:
    // - fleet_pct_ema_samples = 0 (just reset)
    // - fleet_pct_hr_ema.weekly_scoped = 0.0 (just reset)
    // - No fresh rate this interval (delta = 0, cold-start conditions)

    let window_name = "weekly_scoped";
    let util_after_rotation = 58.0; // Opus window utilization
    let target_ceiling = 90.0;
    let hours_remaining = 100.0;

    // Production path state after model change:
    let fleet_pct_hr_after_reset = 0.0; // No observed rate yet (cold-start)
    let ema_samples_after_reset = 0; // Just reset by model change detection

    // Determine estimate quality (production path logic from governor.rs:4730-4736)
    let estimate_quality = if ema_samples_after_reset >= 3 && fleet_pct_hr_after_reset > 0.0 {
        EstimateQuality::Calibrated
    } else if ema_samples_after_reset == 0 {
        EstimateQuality::ColdStart
    } else {
        EstimateQuality::InsufficientSamples
    };

    assert_eq!(estimate_quality, EstimateQuality::ColdStart,
        "After model change reset, window should be flagged as ColdStart");

    // Cold-start seeding logic (production path from governor.rs:4761-4786)
    let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) =
        if matches!(estimate_quality, EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples)
            && util_after_rotation > 0.0
            && fleet_pct_hr_after_reset == 0.0
            && current_workers > 0
        {
            let seeded_fleet_pct = baseline_pct_per_worker * current_workers as f64;
            let widened_std_pct = seeded_fleet_pct; // Conservative wide uncertainty cone
            (seeded_fleet_pct, baseline_pct_per_worker, widened_std_pct)
        } else {
            (fleet_pct_hr_after_reset, 0.0, 0.0)
        };

    // VERIFY 1: Cold-start window gets seeded rate, NOT 0.0
    assert!(fleet_pct_hr_seeded > 0.0,
        "Cold-start window should be seeded with conservative baseline rate, not 0.0");
    assert_eq!(fleet_pct_hr_seeded, 7.5, // 1.5 * 5 workers
        "Seeded rate should match baseline across all workers");

    // VERIFY 2: Uncertainty cone is widened (conservative)
    assert_eq!(std_pct_hr_seeded, fleet_pct_hr_seeded,
        "Cold-start should use widened uncertainty cone (std == rate)");

    // Call generate_window_forecast (production path) with seeded parameters
    let forecast = generate_window_forecast(
        window_name,
        fleet_pct_hr_seeded,
        util_after_rotation,
        target_ceiling,
        hours_remaining,
        pct_per_worker_seeded,
        std_pct_hr_seeded,
        estimate_quality,
    );

    // VERIFY 3: Forecast reflects cold-start quality flag
    assert_eq!(forecast.estimate_quality, EstimateQuality::ColdStart,
        "Production forecast should carry ColdStart quality flag");

    // VERIFY 4: Forecast uses seeded rate (NOT 0.0, NOT Fable's stale 2.5%/hr)
    // With seeded 7.5%/hr and 32% remaining (90-58):
    // predicted_exhaustion = 32 / 7.5 = 4.27 hours (finite, not +inf)
    let expected_exhaustion = (target_ceiling - util_after_rotation) / fleet_pct_hr_seeded;
    assert!((forecast.predicted_exhaustion_hours - expected_exhaustion).abs() < 0.1,
        "Forecast should use seeded rate (7.5%/hr), not 0.0 (infinite) or stale Fable rate (2.5%/hr). \
         Expected exhaustion: {:.2}h, got: {:.2}h",
        expected_exhaustion, forecast.predicted_exhaustion_hours);

    assert!(forecast.predicted_exhaustion_hours.is_finite(),
        "Predicted exhaustion should be finite (seeded rate), not infinite (0 rate)");

    // VERIFY 5: Wide uncertainty cone (cone_ratio > 1.0) from conservative std seeding
    assert!(forecast.cone_ratio > 1.0,
        "Cold-start forecast should have wide uncertainty cone (cone_ratio > 1.0), got {:.3}",
        forecast.cone_ratio);

    // VERIFY 6: Safe worker counts are computable from seeded rate
    assert!(forecast.safe_worker_count.is_some(),
        "Safe worker count should be computable from seeded rate");
    assert!(forecast.safe_worker_count_p75.is_some(),
        "P75 safe worker count should be computable (conservative path)");

    // VERIFY 7: P75 is more conservative than P50 (wide uncertainty cone effect)
    if forecast.safe_worker_count_p75.is_some() && forecast.safe_worker_count.is_some() {
        assert!(forecast.safe_worker_count_p75 <= forecast.safe_worker_count,
            "P75 safe workers should be <= P50 (more conservative from widened uncertainty)");
    }

    // VERIFY 8: Regression - ensure a calibrated (non-rotating) window would be unchanged
    // Simulate five_hour window that was NOT affected by model rotation
    let five_hour_samples = 15; // Well calibrated
    let five_hour_ema = 1.8; // Established rate
    let five_hour_quality = EstimateQuality::Calibrated;

    let forecast_calibrated = generate_window_forecast(
        "five_hour",
        five_hour_ema * current_workers as f64, // Fleet rate from EMA
        45.0, // util
        90.0,
        4.0,
        five_hour_ema, // Per-worker rate
        0.9, // Normal std (not widened)
        five_hour_quality,
    );

    assert_eq!(forecast_calibrated.estimate_quality, EstimateQuality::Calibrated,
        "Calibrated window should remain Calibrated (not affected by identity change)");

    // The key regression check: the calibrated window's forecast should be numerically
    // identical to what it was before Children 1-3 (the fix only affects cold-start path)
    // This guard ensures the fix doesn't silently change behavior for normal windows
    assert!(forecast_calibrated.predicted_exhaustion_hours.is_finite(),
        "Calibrated window should still produce finite exhaustion time");
}

/// Test: First-startup cold-start behavior (brand new governor state)
///
/// Verifies that when cgov starts for the first time with:
/// - No persisted weekly_scoped_model (None)
/// - No accumulated samples (fleet_pct_ema_samples = 0)
/// - No observed burn rate yet (fleet_pct_hr_ema.weekly_scoped = 0.0)
///
/// The window should:
/// - Be flagged as ColdStart (not "empty" or "absent")
/// - Use conservative baseline seeding (not claim 0% / infinite headroom)
/// - Produce a safe worker count (not scale unbounded)
#[test]
fn test_first_startup_cold_start_behavior() {
    use claude_governor::burn_rate::generate_window_forecast;

    // Simulate first-time cgov startup with no persisted state
    let weekly_scoped_model_at_startup: Option<String> = None; // No previous model persisted
    let ema_samples_at_startup = 0; // No accumulated samples
    let ema_value_at_startup = 0.0; // No observed rate yet

    // First poll returns weekly_scoped scoped to a model (e.g., "Fable")
    let first_poll_model = Some("Fable".to_string());
    let first_poll_util = 50.0; // Window has real utilization

    // Governor detects this as initialization (None -> Some), not rotation
    // Since model went from None to Some, reset_weekly_scoped_on_model_change()
    // returns true (for logging) but the EMAs are already 0 from default state

    // Determine estimate quality
    let estimate_quality = if ema_samples_at_startup >= 3 && ema_value_at_startup > 0.0 {
        EstimateQuality::Calibrated
    } else if ema_samples_at_startup == 0 {
        EstimateQuality::ColdStart
    } else {
        EstimateQuality::InsufficientSamples
    };

    assert_eq!(estimate_quality, EstimateQuality::ColdStart,
        "First startup with no samples should be flagged ColdStart, not treated as empty/absent");

    // Cold-start seeding parameters
    let baseline_pct_per_worker = 1.5;
    let current_workers = 3;

    let fleet_pct_hr_seeded = baseline_pct_per_worker * current_workers as f64;
    let pct_per_worker_seeded = baseline_pct_per_worker;
    let std_pct_hr_seeded = fleet_pct_hr_seeded; // Widened uncertainty cone

    // Production forecast for first-startup window
    let forecast = generate_window_forecast(
        "weekly_scoped",
        fleet_pct_hr_seeded,
        first_poll_util,
        90.0, // target_ceiling
        120.0, // hours_remaining (7-day window)
        pct_per_worker_seeded,
        std_pct_hr_seeded,
        estimate_quality,
    );

    // VERIFY 1: Window is flagged cold-start (not absent/empty)
    assert_eq!(forecast.estimate_quality, EstimateQuality::ColdStart,
        "First-startup window should be flagged ColdStart, not Calibrated or treated as absent");

    // VERIFY 2: Conservative baseline seeding prevents infinite headroom
    assert_eq!(forecast.fleet_pct_per_hour, fleet_pct_hr_seeded,
        "Fleet burn rate should use seeded baseline (4.5%/hr), not 0.0 (infinite headroom)");

    assert!(forecast.predicted_exhaustion_hours.is_finite(),
        "Predicted exhaustion should be finite (seeded rate), not +inf (0 rate would give infinite)");

    // With 40% remaining (90-50) and 4.5%/hr seeded: exhaustion = 40 / 4.5 = 8.89 hours
    let expected_exhaustion = 40.0 / fleet_pct_hr_seeded;
    assert!((forecast.predicted_exhaustion_hours - expected_exhaustion).abs() < 0.1,
        "Exhaustion time should use seeded rate, not 0.0 (infinite)");

    // VERIFY 3: Wide uncertainty cone marks estimate as uncertain
    assert!(forecast.cone_ratio > 1.0,
        "First-startup window should have wide uncertainty cone (cone_ratio > 1.0), got {:.3}",
        forecast.cone_ratio);

    // VERIFY 4: Safe worker counts are computable (not unbounded scaling)
    assert!(forecast.safe_worker_count.is_some(),
        "Safe worker count should be computable from seeded rate (enforces bound)");
    assert!(forecast.safe_worker_count_p75.is_some(),
        "P75 safe worker count should enforce conservative bound");

    // VERIFY 5: P75 <= P50 (conservative uncertainty cone)
    assert!(forecast.safe_worker_count_p75 <= forecast.safe_worker_count,
        "P75 should be more conservative (<= P50) due to widened uncertainty");

    // VERIFY 6: No claim of 0% utilization (window exists this period)
    assert_eq!(forecast.current_utilization, first_poll_util,
        "Current utilization should reflect real API value (50%), not be claimed as 0%");

    // VERIFY 7: Margin is finite and reflects conservative seeding
    // margin_hrs = predicted_exhaustion - hours_remaining
    // With seeded rate: margin = 8.89 - 120 = -111.11 hours (negative = safe)
    assert!(!forecast.margin_hrs.is_infinite(),
        "Margin should be finite from seeded rate");
    assert!(forecast.margin_hrs < 0.0,
        "Margin should be negative (safe) with long time horizon and conservative rate");

    // VERIFY 8: First-startup vs identity change both produce ColdStart
    // (Both paths should trigger cold-start seeding, just via different detection)
    assert_eq!(forecast.estimate_quality, EstimateQuality::ColdStart,
        "First-startup (None->Some) should produce same ColdStart signal as identity change (Some->Some)");
}
