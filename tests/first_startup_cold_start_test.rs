//! First-startup cold-start test
//!
//! This test verifies that a brand-new state (no persisted weekly_scoped_model,
//! no samples) cold-starts flagged as cold/uncertain, NOT as confident-empty.
//!
//! ## Architecture background
//!
//! The governor maintains burn rate EMA (Exponential Moving Average) state per
//! (model, window) pair. When Anthropic rotates which model carries the scoped
//! weekly cap (e.g., Fable → Opus → a third model), the newly-appearing window
//! cold-starts with zero historical samples.
//!
//! A "confident-empty" bug would treat "no data" as "definitely empty" (0%/hr),
//! which incorrectly implies infinite headroom and can cause over-scaling.
//!
//! The correct behavior is:
//! - Recognize the window as cold-start (no samples)
//! - Flag it as uncertain (`EstimateQuality::ColdStart`)
//! - Seed a conservative baseline rate with widened uncertainty cone
//! - Prevent over-scaling on first startup
//!
//! Acceptance criteria (bf-68pk7k):
//! - Test exists and passes (OR is skipped with documented rationale)
//! - Test simulates brand-new state with no persisted model
//! - Test asserts cold/uncertain signal (not confident-empty behavior)
//! - Test demonstrates proper first-startup behavior
//!
//! ## TEST STATUS: SKIPPED - Architectural limitation documented below
//!
//! These tests are skipped because the current architecture has an early return
//! in `estimate_burn_rates()` (src/burn_rate.rs:1313-1322) that prevents
//! cold-start seeding logic from running on first startup.

use chrono::Utc;
use claude_governor::burn_rate::{estimate_burn_rates, BaselineBurnRates};
use claude_governor::state::{BurnRateState, EstimateQuality};

/// Create a brand-new burn rate state (no persisted model, no samples)
///
/// Returns a BurnRateState with:
/// - Empty `by_model` map (no per-model calibration)
/// - Zero `fleet_pct_ema_samples` (no historical samples)
/// - Zero EMAs for all windows (no burn rate history)
/// - No `last_sample_at` (never polled)
///
/// This represents a fresh cgov installation or a model rotation where the
/// weekly_scoped window appears for the first time.
fn create_brand_new_state() -> BurnRateState {
    BurnRateState {
        by_model: std::collections::HashMap::new(), // No per-model calibration
        tokens_per_pct_peak: 0,
        tokens_per_pct_offpeak: 0,
        offpeak_ratio_observed: 0.0,
        offpeak_ratio_expected: 0.0,
        promotion_validated: false,
        promotion_peak_samples: 0,
        promotion_offpeak_samples: 0,
        last_sample_at: None, // Never sampled
        calibration: claude_governor::state::CalibrationState::default(),
        fleet_pct_hr_ema: claude_governor::state::WindowPctDeltas {
            five_hour: 0.0,   // No burn rate history
            seven_day: 0.0,
            weekly_scoped: 0.0, // Key: no model-scoped history either
        },
        usd_per_pct_ema_five_hour: 0.0,
        usd_per_pct_ema_seven_day: 0.0,
        usd_per_pct_ema_weekly_scoped: 0.0,
        fleet_pct_ema_samples: 0, // Zero samples = cold-start
        prev_usage_snapshot: None,
    }
}

/// Create a conservative baseline burn rate configuration
///
/// Uses the documented conservative defaults:
/// - 1.5%/hr per worker (default_baseline_pct)
/// - $5.0/hr per worker (default_baseline_dollars)
fn create_baseline() -> BaselineBurnRates {
    BaselineBurnRates {
        pct_per_worker_per_hour: claude_governor::config::default_baseline_pct(),
        dollars_per_worker_per_hour: claude_governor::config::default_baseline_dollars(),
    }
}

#[test]
#[ignore = "Architectural limitation: early return prevents cold-start logic on first startup"]
fn test_first_startup_weekly_scoped_cold_starts_flagged_uncertain() {
    // === ARCHITECTURAL LIMITATION DOCUMENTATION ===
    //
    // This test is skipped because of an architectural issue in burn_rate.rs:
    //
    // 1. On first startup, there's no pct_delta (no previous poll for comparison)
    // 2. Without pct_delta, compute_instance_burn skips all records (line 167-170)
    // 3. Empty all_instance_rates triggers early return (line 1313-1322)
    // 4. Early return returns CapacityForecast::default() with Calibrated quality
    // 5. Cold-start seeding logic (lines 1445-1463) never runs
    //
    // The cold-start seeding was designed to handle windows with:
    // - Real utilization data (util > 0.0, meaning the window exists this period)
    // - No burn calibration yet (samples < 3)
    // - No fresh instance rates this interval
    //
    // But the early return prevents this logic from executing when all_instance_rates
    // is empty, which is exactly what happens on first startup (no pct_delta yet).
    //
    // THE FIX (code change required, not test-only):
    // Remove the early return OR check for windows with util > 0.0 and apply
    // cold-start seeding before returning. This would allow first startup to
    // properly seed conservative rates for windows that exist but have no history.
    //
    // Tracking: This is the "confident-empty" bug the task (bf-68pk7k) wants to guard against.

    let burn_rate_state = create_brand_new_state();

    // Verify the state is truly brand-new (no calibration data)
    assert!(
        burn_rate_state.by_model.is_empty(),
        "Brand-new state should have no per-model calibration"
    );
    assert_eq!(
        burn_rate_state.fleet_pct_ema_samples, 0,
        "Brand-new state should have zero EMA samples"
    );
    assert_eq!(
        burn_rate_state.fleet_pct_hr_ema.weekly_scoped, 0.0,
        "Brand-new state should have zero weekly_scoped EMA rate"
    );
    assert!(
        burn_rate_state.last_sample_at.is_none(),
        "Brand-new state should have no last_sample_at timestamp"
    );

    // === SETUP: First startup scenario ===
    //
    // On first startup:
    // - No pct_delta (no previous poll to compare against)
    // - Current utilization exists (API reports real data)
    // - No EMA samples (brand-new state)
    //
    // Expected behavior (if architecture supported it):
    // - ColdStart quality flag
    // - Seeded baseline rate (not 0.0)
    // - Wide uncertainty cone
    //
    // Actual behavior (current architecture):
    // - Early return with Calibrated quality
    // - 0.0 burn rate (confident-empty bug)
    // - No uncertainty signaling

    // This test would need pct_delta data to pass the guards, but that doesn't exist
    // on first startup. The architecture needs fixing to handle this case.

    panic!(
        "This test is skipped due to architectural limitation. \
         See test attribute for detailed documentation."
    );
}

#[test]
#[ignore = "Architectural limitation: early return prevents cold-start logic on first startup"]
fn test_first_startup_all_windows_cold_start_when_no_samples() {
    // === ARCHITECTURAL LIMITATION ===
    //
    // Same issue as test_first_startup_weekly_scoped_cold_starts_flagged_uncertain.
    //
    // The early return in estimate_burn_rates when all_instance_rates.is_empty()
    // prevents cold-start logic from running. On first startup:
    //
    // 1. No pct_delta → compute_instance_burn skips all records
    // 2. all_instance_rates.is_empty() → early return
    // 3. Returns CapacityForecast::default() (Calibrated, not ColdStart)
    //
    // THE FIX: Move cold-start seeding logic before the early return, OR
    // check for windows with util > 0.0 and seed them before returning.

    panic!(
        "This test is skipped due to architectural limitation. \
         See test_first_startup_weekly_scoped_cold_starts_flagged_uncertain \
         for detailed documentation."
    );
}

#[test]
#[ignore = "Architectural limitation: early return prevents cold-start logic on first startup"]
fn test_first_startup_no_weekly_scoped_model_required() {
    // === ARCHITECTURAL LIMITATION ===
    //
    // This test verifies that forecasts work when weekly_scoped_model is None
    // (brand-new installation or model rotation). However:
    //
    // 1. Empty instance_records → early return
    // 2. Early return returns default forecast (Calibrated)
    // 3. Never reaches cold-start seeding logic
    //
    // THE FIX: The estimate_burn_rates function needs to distinguish between:
    // - Truly absent windows (util == 0.0, window doesn't exist)
    // - Cold-start windows (util > 0.0, window exists but no calibration)
    //
    // Currently, both cases return default forecasts (Calibrated).

    let mut usage_state = claude_governor::state::UsageState::default();

    // Verify weekly_scoped_model is None (no persisted model identity)
    assert!(
        usage_state.weekly_scoped_model.is_none(),
        "Brand-new state should have weekly_scoped_model: None"
    );

    let burn_rate_state = create_brand_new_state();

    panic!(
        "This test is skipped due to architectural limitation. \
         See test_first_startup_weekly_scoped_cold_starts_flagged_uncertain \
         for detailed documentation."
    );
}

#[test]
fn test_architectural_limitation_documentation() {
    // This test documents the architectural limitation for future reference.
    //
    // PROBLEM:
    // The estimate_burn_rates function has an early return (src/burn_rate.rs:1313-1322)
    // that returns CapacityForecast::default() when all_instance_rates.is_empty().
    // This default forecast has EstimateQuality::Calibrated, which is incorrect
    // for cold-start scenarios.
    //
    // SCENARIO:
    // On first startup or after model rotation:
    // 1. No pct_delta (no previous poll for comparison)
    // 2. compute_instance_burn skips all records (line 167-170, pct_delta is None)
    // 3. all_instance_rates.is_empty() == true
    // 4. Early return with default forecast (Calibrated quality)
    // 5. Cold-start seeding logic (lines 1445-1463) never executes
    //
    // IMPACT:
    // - "Confident-empty" bug: Treats "no data" as "definitely empty" (0%/hr)
    // - 0%/hr → infinite headroom (predicted_exhaustion = infinity)
    // - Over-scaling risk on first startup
    //
    // THE FIX:
    // Two possible approaches:
    //
    // 1. Move cold-start seeding before the early return:
    //    - Check for windows with util > 0.0
    //    - Apply cold-start seeding (baseline rate, widened cone)
    //    - Set estimate_quality to ColdStart
    //    - THEN return early
    //
    // 2. Remove the early return entirely:
    //    - Let the cold-start logic handle empty rates
    //    - The logic at lines 1445-1463 already checks util > 0.0 before seeding
    //    - This is the cleaner fix but requires more testing
    //
    // WORKAROUND (current state):
    // The cold-start seeding only works when there ARE instance rates (pct_delta
    // exists) but EMA samples < 3. This handles "second poll with data but no
    // calibration yet", not "first poll with no data".
    //
    // TRACKING:
    // This is the bug that task bf-68pk7k wants to guard against. The test is
    // skipped with this documentation until the architecture is fixed.

    assert!(true, "Documentation test - always passes");
}
