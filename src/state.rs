//! Governor State Store
//!
//! Manages reading, writing, and atomic updates of governor-state.json.
//! The state file holds the governor's complete runtime snapshot: usage data,
//! capacity forecasts, burn rates, worker assignments, schedule, and alerts.
//!
//! Conventions:
//! - All fields use `#[serde(default)]` for backward compatibility — new fields
//!   added to the schema will deserialize as their default value when reading
//!   older state files.
//! - Writes are atomic (write to `.tmp`, rename) to prevent corruption.
//! - Previous state is preserved in `governor-state.prev.json` before each update.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Window name keys for consecutive-absent tracking
const WINDOW_FIVE_HOUR: &str = "five_hour";
const WINDOW_SEVEN_DAY: &str = "seven_day";
const WINDOW_WEEKLY_SCOPED: &str = "weekly_scoped";

/// Minimum consecutive absent polls before treating a window as structurally inactive.
///
/// This matches the threshold in governor.rs (MIN_CONSECUTIVE_ABSENT) and is duplicated
/// here to avoid circular dependencies between modules.
const MIN_CONSECUTIVE_ABSENT: u32 = 3;

/// Errors that can occur during state operations
#[derive(Debug, Error)]
pub enum StateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StateError>;

// ---------------------------------------------------------------------------
// Sub-structs
// ---------------------------------------------------------------------------

/// Current platform usage snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageState {
    /// Legacy field - kept for backward compatibility.
    /// New code should use weekly_scoped_pct instead.
    #[serde(default)]
    pub sonnet_pct: f64,
    pub all_models_pct: f64,
    pub five_hour_pct: f64,
    pub sonnet_resets_at: String,
    pub five_hour_resets_at: String,
    /// True when data was sourced from stale cache (token refresh failed)
    pub stale: bool,
    /// Resolved display name of the model the weekly_scoped window is scoped to
    /// (e.g. "Fable"), plumbed from the poller's `scoped_weekly()` accessor.
    /// `None` when no active model-scoped cap is present this period — metadata
    /// only, the binding key stays the generic "weekly_scoped". Null-tolerant:
    /// `Option<String>` already round-trips null ↔ None, so a missing/null field
    /// in an older state file deserializes as None (no panic), mirroring the
    /// null-tolerance applied to hard_limit_margin_hrs/cone_ratio/risk_score.
    #[serde(default)]
    pub weekly_scoped_model: Option<String>,
    /// Model-agnostic weekly_scoped utilization percentage.
    /// This is the correct field to use for the weekly_scoped window, regardless
    /// of which model (Fable, Opus, etc.) carries the scoped cap this period.
    /// The legacy sonnet_pct field is kept for backward compatibility only.
    #[serde(default)]
    pub weekly_scoped_pct: f64,
}

impl Default for UsageState {
    fn default() -> Self {
        Self {
            sonnet_pct: 0.0,
            all_models_pct: 0.0,
            five_hour_pct: 0.0,
            sonnet_resets_at: String::new(),
            five_hour_resets_at: String::new(),
            stale: false,
            weekly_scoped_model: None,
            weekly_scoped_pct: 0.0,
        }
    }
}

/// Human-readable label for the model-scoped weekly window.
///
/// Returns the resolved model display name (e.g. `"Fable"`) when one is known
/// for this period, otherwise the generic `"weekly_scoped"` key. Metadata only:
/// the binding key stays `"weekly_scoped"` and selection logic is untouched —
/// this is purely so logs/display label the third window with *which* model it
/// tracks instead of the stale hardcoded `"7d-sonnet"`/`"sonnet"`.
pub fn weekly_scoped_display_label(model: Option<&str>) -> &str {
    model.filter(|m| !m.is_empty()).unwrap_or("weekly_scoped")
}

/// Reset weekly_scoped EMA samples when the scoped model identity changes.
///
/// When Anthropic rotates which model carries the scoped weekly cap (e.g., Fable -> Opus),
/// the weekly_scoped slot should NOT reuse the previous model's EMA samples. This function
/// detects when the resolved model name differs from the persisted value and resets the
/// weekly_scoped burn rate state to cold (zero samples).
///
/// Returns true if a reset was performed (identity changed), false otherwise.
pub fn reset_weekly_scoped_on_model_change(
    prev_model: &Option<String>,
    new_model: &Option<String>,
    burn_rate_state: &mut BurnRateState,
) -> bool {
    match (prev_model.as_deref(), new_model.as_deref()) {
        (Some(old), Some(new)) if old != new => {
            log::info!(
                "[governor] weekly_scoped model identity changed: '{}' -> '{}', resetting EMA samples",
                old,
                new
            );
            // Reset weekly_scoped EMA samples to cold (zero)
            burn_rate_state.fleet_pct_hr_ema.weekly_scoped = 0.0;
            burn_rate_state.usd_per_pct_ema_weekly_scoped = 0.0;
            true
        }
        (Some(old), None) => {
            log::info!(
                "[governor] weekly_scoped model cleared (was '{}'), resetting EMA samples",
                old
            );
            burn_rate_state.fleet_pct_hr_ema.weekly_scoped = 0.0;
            burn_rate_state.usd_per_pct_ema_weekly_scoped = 0.0;
            true
        }
        (None, Some(new)) => {
            log::info!(
                "[governor] weekly_scoped model initialized as '{}', starting with cold EMA",
                new
            );
            // No reset needed - starting from cold (already zero)
            true
        }
        _ => false, // No change or both None
    }
}

/// Last fleet aggregate from the token collector
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetAggregate {
    pub t0: DateTime<Utc>,
    pub t1: DateTime<Utc>,
    pub sonnet_workers: u32,
    pub sonnet_usd_total: f64,
    pub sonnet_p75_usd_hr: f64,
    pub sonnet_std_usd_hr: f64,
    pub window_pct_deltas: WindowPctDeltas,
    /// Fleet-level cache efficiency (weighted average by total input tokens)
    pub fleet_cache_eff: f64,
    /// 25th percentile of per-instance cache efficiency
    pub cache_eff_p25: f64,
    /// CLI (subscription) tokens burned this interval
    pub cli_tokens: u64,
    /// CLI (subscription) USD cost this interval
    pub cli_cost: f64,
    /// SDK-CLI (credits) tokens burned this interval (informational only, not in quota windows)
    pub sdk_tokens: u64,
    /// SDK-CLI (credits) USD cost this interval (informational only, not in quota windows)
    pub sdk_cost: f64,
}

impl Default for FleetAggregate {
    fn default() -> Self {
        Self {
            t0: Utc::now(),
            t1: Utc::now(),
            sonnet_workers: 0,
            sonnet_usd_total: 0.0,
            sonnet_p75_usd_hr: 0.0,
            sonnet_std_usd_hr: 0.0,
            window_pct_deltas: WindowPctDeltas::default(),
            fleet_cache_eff: 0.0,
            cache_eff_p25: 0.0,
            cli_tokens: 0,
            cli_cost: 0.0,
            sdk_tokens: 0,
            sdk_cost: 0.0,
        }
    }
}

/// Per-window percentage deltas observed in the last fleet aggregate
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowPctDeltas {
    pub five_hour: f64,
    pub seven_day: f64,
    pub weekly_scoped: f64,
}

impl Default for WindowPctDeltas {
    fn default() -> Self {
        Self {
            five_hour: 0.0,
            seven_day: 0.0,
            weekly_scoped: 0.0,
        }
    }
}

/// Previous API usage snapshot for computing percentage deltas across governor cycles.
///
/// Persisted in state so that the governor can compute pct/hr from consecutive
/// API readings even across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrevUsageSnapshot {
    /// When this snapshot was taken (wall-clock time of the API poll)
    pub taken_at: DateTime<Utc>,
    pub five_hour_pct: f64,
    pub seven_day_pct: f64,
    pub weekly_scoped_pct: f64,
}

impl Default for PrevUsageSnapshot {
    fn default() -> Self {
        Self {
            taken_at: DateTime::<Utc>::default(),
            five_hour_pct: 0.0,
            seven_day_pct: 0.0,
            weekly_scoped_pct: 0.0,
        }
    }
}

/// Pending prediction for a window — used to score predictions when windows reset.
///
/// When a window starts (or at each cycle), we predict the final utilization
/// percentage that will be reached when the window resets. When a reset is
/// detected (utilization drops), we compare the predicted value to the actual
/// final value and score the prediction for calibration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingPrediction {
    /// When this prediction was made
    pub prediction_time: DateTime<Utc>,
    /// Predicted final utilization percentage when window resets
    pub predicted_final_pct: f64,
    /// Utilization percentage when the prediction was made
    pub starting_pct: f64,
}

impl Default for PendingPrediction {
    fn default() -> Self {
        Self {
            prediction_time: Utc::now(),
            predicted_final_pct: 0.0,
            starting_pct: 0.0,
        }
    }
}

/// Deserializes an f64 field, treating JSON null as f64::INFINITY.
/// serde_json serializes f64::INFINITY as null (JSON has no infinity literal),
/// so we need to round-trip null → infinity on deserialization.
fn deserialize_f64_null_as_infinity<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<f64, D::Error> {
    let opt: Option<f64> = Option::deserialize(d)?;
    Ok(opt.unwrap_or(f64::INFINITY))
}

/// Quality level of a window's burn rate estimate.
///
/// Distinguishes between calibrated forecasts (backed by sufficient samples) and
/// uncertain/cold-start forecasts (seeded from baseline or lacking data). Downstream
/// consumers (safe-mode logic, alert predicates) can branch on this field to apply
/// conservative heuristics when the forecast is not grounded in measurement.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EstimateQuality {
    /// Forecast is backed by >= MIN_SAMPLES_FOR_EMA samples or fresh per-instance rates.
    /// The burn rate is measured from real usage data — safe to use for scaling decisions.
    Calibrated,
    /// Forecast has no burn history yet (cold-start). Rate is seeded from a conservative
    /// baseline. The estimate may be wrong; safe-worker paths should use pessimistic bounds.
    ColdStart,
    /// Not enough samples to trust the EMA yet (< MIN_SAMPLES_FOR_EMA). Falls back to
    /// baseline rates with wider uncertainty bounds.
    InsufficientSamples,
}

impl Default for EstimateQuality {
    fn default() -> Self {
        // Default to Calibrated for backward compatibility: existing state files
        // that predate this field are assumed to represent mature, calibrated windows.
        Self::Calibrated
    }
}

/// Per-window capacity forecast
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowForecast {
    pub target_ceiling: f64,
    pub current_utilization: f64,
    pub remaining_pct: f64,
    pub hours_remaining: f64,
    pub fleet_pct_per_hour: f64,
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub predicted_exhaustion_hours: f64,
    pub cutoff_risk: bool,
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub margin_hrs: f64,
    pub binding: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_worker_count: Option<u32>,
    /// Conservative safe worker count using the p75 (fast-burn) per-worker rate.
    /// Lower than safe_worker_count when burn rate spread is non-zero.
    /// Used when cone_ratio is wide (uncertain predictions) to scale conservatively.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_worker_count_p75: Option<u32>,
    /// Confidence cone: pessimistic exhaustion hours (mean + 1σ burn rate → fewer hours)
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub exh_hrs_p25: f64,
    /// Confidence cone: central exhaustion hours (mean burn rate, same as predicted_exhaustion_hours)
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub exh_hrs_p50: f64,
    /// Confidence cone: optimistic exhaustion hours (mean − 1σ burn rate → more hours)
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub exh_hrs_p75: f64,
    /// Cone ratio = exh_hrs_p75 / exh_hrs_p25 (1.0 = no spread, higher = wider uncertainty)
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub cone_ratio: f64,
    /// Composite risk score (higher = riskier). Factors in margin, duration, and volatility.
    /// Used for binding window selection.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub risk_score: f64,
    /// Remaining headroom to the hard platform limit (100% - current_utilization).
    /// Unlike remaining_pct (which uses the target ceiling), this measures distance to the
    /// platform-enforced cutoff at 100%.
    #[serde(default)]
    pub hard_limit_remaining_pct: f64,
    /// Margin in hours against the hard platform limit (100%).
    /// positive = safe (won't hit 100% before reset), negative = will hit 100% before reset.
    /// Alert predicates use this instead of margin_hrs (which is against the target ceiling).
    /// A null in the state file means f64::INFINITY (no hard-limit constraint) — round-trip it
    /// back rather than failing the whole load and discarding all learned calibration.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_f64_null_as_infinity")]
    pub hard_limit_margin_hrs: f64,
    /// Quality indicator for the burn rate estimate backing this forecast.
    /// Calibrated = enough samples to trust the rate; ColdStart/InsufficientSamples = seeded
    /// from baseline, use conservative heuristics (p75 safe workers, wide uncertainty bounds).
    #[serde(default)]
    pub estimate_quality: EstimateQuality,
}

impl Default for WindowForecast {
    fn default() -> Self {
        Self {
            target_ceiling: 0.0,
            current_utilization: 0.0,
            remaining_pct: 0.0,
            hours_remaining: 0.0,
            fleet_pct_per_hour: 0.0,
            predicted_exhaustion_hours: 0.0,
            cutoff_risk: false,
            margin_hrs: 0.0,
            binding: false,
            safe_worker_count: None,
            safe_worker_count_p75: None,
            exh_hrs_p25: 0.0,
            exh_hrs_p50: 0.0,
            exh_hrs_p75: 0.0,
            cone_ratio: 0.0,
            risk_score: 0.0,
            hard_limit_remaining_pct: 0.0,
            hard_limit_margin_hrs: 0.0,
            estimate_quality: EstimateQuality::Calibrated,
        }
    }
}

/// Capacity forecast block (all three windows + derived metrics)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CapacityForecast {
    pub five_hour: WindowForecast,
    pub seven_day: WindowForecast,
    pub weekly_scoped: WindowForecast,
    pub binding_window: String,
    pub dollars_per_pct_7d_s: f64,
    pub estimated_remaining_dollars: f64,
}

impl Default for CapacityForecast {
    fn default() -> Self {
        Self {
            five_hour: WindowForecast::default(),
            seven_day: WindowForecast::default(),
            weekly_scoped: WindowForecast::default(),
            binding_window: String::new(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        }
    }
}

fn serde_default_one() -> f64 {
    1.0
}

/// Schedule block — peak hour and promotion state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleState {
    pub is_peak_hour: bool,
    pub is_promo_active: bool,
    /// Per-window promotion multipliers.
    /// Only windows listed in the promotion's `applies_to` get > 1.0.
    /// During peak hours all windows are 1.0.
    #[serde(default = "serde_default_one")]
    pub promo_multiplier_five_hour: f64,
    #[serde(default = "serde_default_one")]
    pub promo_multiplier_seven_day: f64,
    #[serde(default = "serde_default_one")]
    pub promo_multiplier_weekly_scoped: f64,
    /// Display multiplier: max across all windows (for backward-compatible display).
    #[serde(default = "serde_default_one")]
    pub promo_multiplier: f64,
    /// Per-window effective hours remaining (wall-clock hours × multiplier).
    pub effective_hours_remaining_five_hour: f64,
    pub effective_hours_remaining_seven_day: f64,
    pub effective_hours_remaining_weekly_scoped: f64,
    /// Effective hours for the binding window (for display).
    pub effective_hours_remaining: f64,
    pub raw_hours_remaining: f64,
}

impl Default for ScheduleState {
    fn default() -> Self {
        Self {
            is_peak_hour: false,
            is_promo_active: false,
            promo_multiplier_five_hour: 1.0,
            promo_multiplier_seven_day: 1.0,
            promo_multiplier_weekly_scoped: 1.0,
            promo_multiplier: 1.0,
            effective_hours_remaining_five_hour: 0.0,
            effective_hours_remaining_seven_day: 0.0,
            effective_hours_remaining_weekly_scoped: 0.0,
            effective_hours_remaining: 0.0,
            raw_hours_remaining: 0.0,
        }
    }
}

/// Per-worker scaling state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerState {
    pub current: u32,
    pub target: u32,
    pub min: u32,
    pub max: u32,
}

impl Default for WorkerState {
    fn default() -> Self {
        Self {
            current: 0,
            target: 0,
            min: 0,
            max: 0,
        }
    }
}

/// Per-model burn rate
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelBurnRate {
    pub pct_per_worker_per_hour: f64,
    pub dollars_per_worker_per_hour: f64,
    pub samples: u32,
}

impl Default for ModelBurnRate {
    fn default() -> Self {
        Self {
            pct_per_worker_per_hour: 0.0,
            dollars_per_worker_per_hour: 0.0,
            samples: 0,
        }
    }
}

/// Calibration state (prediction accuracy tracking)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CalibrationState {
    pub predictions_scored: u32,
    pub median_error_7ds: f64,
    pub auto_tuned_alpha: f64,
    pub auto_tuned_hysteresis: f64,
    pub last_tuned_at: Option<DateTime<Utc>>,
}

impl Default for CalibrationState {
    fn default() -> Self {
        Self {
            predictions_scored: 0,
            median_error_7ds: 0.0,
            auto_tuned_alpha: 0.0,
            auto_tuned_hysteresis: 0.0,
            last_tuned_at: None,
        }
    }
}

/// Burn rate state block
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BurnRateState {
    pub by_model: HashMap<String, ModelBurnRate>,
    pub tokens_per_pct_peak: u64,
    pub tokens_per_pct_offpeak: u64,
    pub offpeak_ratio_observed: f64,
    pub offpeak_ratio_expected: f64,
    pub promotion_validated: bool,
    /// Peak samples used in the most recent promotion validation
    pub promotion_peak_samples: usize,
    /// Off-peak samples used in the most recent promotion validation
    pub promotion_offpeak_samples: usize,
    pub last_sample_at: Option<DateTime<Utc>>,
    pub calibration: CalibrationState,

    /// EMA of fleet-level pct/hr for each window, derived from consecutive API reading deltas.
    ///
    /// Only updated when a positive delta is observed — zero-delta cycles (no measurable
    /// API change) leave the EMA unchanged so a single stale sample can't zero it out.
    #[serde(default)]
    pub fleet_pct_hr_ema: WindowPctDeltas,

    /// EMA of USD-per-pct ratio for each window (fleet total USD/hr ÷ fleet pct/hr).
    ///
    /// Used as a fallback: when fleet_pct_hr_ema is zero but dollar burn is non-zero,
    /// estimate pct/hr = fleet_usd_hr / usd_per_pct_ema.
    #[serde(default)]
    pub usd_per_pct_ema_five_hour: f64,
    #[serde(default)]
    pub usd_per_pct_ema_seven_day: f64,
    #[serde(default)]
    pub usd_per_pct_ema_weekly_scoped: f64,

    /// Number of positive-delta samples accumulated in fleet_pct_hr_ema.
    #[serde(default)]
    pub fleet_pct_ema_samples: u32,

    /// Previous API usage snapshot, used to compute cross-cycle pct deltas.
    #[serde(default)]
    pub prev_usage_snapshot: Option<PrevUsageSnapshot>,
}

impl Default for BurnRateState {
    fn default() -> Self {
        Self {
            by_model: HashMap::new(),
            tokens_per_pct_peak: 0,
            tokens_per_pct_offpeak: 0,
            offpeak_ratio_observed: 0.0,
            offpeak_ratio_expected: 0.0,
            promotion_validated: false,
            promotion_peak_samples: 0,
            promotion_offpeak_samples: 0,
            last_sample_at: None,
            calibration: CalibrationState::default(),
            fleet_pct_hr_ema: WindowPctDeltas::default(),
            usd_per_pct_ema_five_hour: 0.0,
            usd_per_pct_ema_seven_day: 0.0,
            usd_per_pct_ema_weekly_scoped: 0.0,
            fleet_pct_ema_samples: 0,
            prev_usage_snapshot: None,
        }
    }
}

/// Safe mode state — defensive fallback when predictions degrade
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafeModeState {
    pub active: bool,
    pub entered_at: Option<DateTime<Utc>>,
    pub trigger: Option<String>,
    pub median_error_at_entry: Option<f64>,
    pub predictions_since_entry: u32,
    /// Total predictions scored at the moment safe mode was entered.
    /// Used to compute predictions_since_entry each cycle.
    #[serde(default)]
    pub scored_at_entry: u32,
}

impl Default for SafeModeState {
    fn default() -> Self {
        Self {
            active: false,
            entered_at: None,
            trigger: None,
            median_error_at_entry: None,
            predictions_since_entry: 0,
            scored_at_entry: 0,
        }
    }
}

/// Baseline burn rates from configuration (fallback when collector is offline or EMA not ready)
///
/// These values are loaded from agent config's `baseline_burn_rate` settings and stored
/// in governor-state.json for persistence. They provide conservative fallback burn rates
/// when the token collector is offline or when insufficient EMA samples have been accumulated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineBurnRates {
    /// Baseline percentage burn per worker per hour (default: 1.5)
    pub pct_per_worker_per_hour: f64,

    /// Baseline dollar burn per worker per hour (default: 5.0)
    pub dollars_per_worker_per_hour: f64,
}

impl Default for BaselineBurnRates {
    fn default() -> Self {
        // Use config-derived defaults as the single source of truth
        Self {
            pct_per_worker_per_hour: crate::config::default_baseline_pct(),
            dollars_per_worker_per_hour: crate::config::default_baseline_dollars(),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level state
// ---------------------------------------------------------------------------

/// Alert cooldown state — per-type last fired timestamps
///
/// Used to deduplicate alerts and prevent spam.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertCooldown {
    /// Last fired timestamp for each alert type (keyed by alert type string)
    pub last_fired: HashMap<String, DateTime<Utc>>,
}

impl Default for AlertCooldown {
    fn default() -> Self {
        Self {
            last_fired: HashMap::new(),
        }
    }
}

impl AlertCooldown {
    /// Create a new empty cooldown tracker
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that an alert of the given type was just fired
    pub fn record_fired(&mut self, alert_type: &str, now: DateTime<Utc>) {
        self.last_fired.insert(alert_type.to_string(), now);
    }

    /// Get the last fired timestamp for an alert type
    pub fn get_last_fired(&self, alert_type: &str) -> Option<DateTime<Utc>> {
        self.last_fired.get(alert_type).copied()
    }

    /// Clear the cooldown for an alert type (when condition clears)
    pub fn clear(&mut self, alert_type: &str) {
        self.last_fired.remove(alert_type);
    }
}

/// Alert FP rate telemetry — rolling window tracking for false-positive regression detection.
///
/// Each alert type tracks the last N outcomes (true positive vs false positive).
/// The rolling window size is configurable (default 100). FP rate is computed as
/// false_positives / total in the window. This is written to governor-state.json
/// each cycle so `cgov status` and external dashboards can surface it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertFpTelemetry {
    /// Per-type rolling window of alert outcomes.
    /// Key is alert type string, value is a deque of bools (true = TP, false = FP).
    /// Only the last `window_size` entries are retained.
    pub outcomes: HashMap<String, Vec<bool>>,
    /// Rolling window size (default 100)
    pub window_size: usize,
    /// Total alerts recorded across all types
    pub total_recorded: u64,
    /// Total false positives across all types
    pub total_false_positives: u64,
}

impl Default for AlertFpTelemetry {
    fn default() -> Self {
        Self {
            outcomes: HashMap::new(),
            window_size: 100,
            total_recorded: 0,
            total_false_positives: 0,
        }
    }
}

impl AlertFpTelemetry {
    /// Record an alert outcome.
    pub fn record(&mut self, alert_type: &str, is_true_positive: bool) {
        let entries = self.outcomes.entry(alert_type.to_string()).or_default();
        entries.push(is_true_positive);
        if entries.len() > self.window_size {
            entries.remove(0);
        }
        self.total_recorded += 1;
        if !is_true_positive {
            self.total_false_positives += 1;
        }
    }

    /// Compute FP rate for a specific alert type over the rolling window.
    /// Returns None if no outcomes have been recorded.
    pub fn fp_rate(&self, alert_type: &str) -> Option<f64> {
        let entries = self.outcomes.get(alert_type)?;
        if entries.is_empty() {
            return None;
        }
        let fp_count = entries.iter().filter(|&&tp| !tp).count();
        Some(fp_count as f64 / entries.len() as f64)
    }

    /// Compute aggregate FP rate across all alert types over the rolling window.
    pub fn aggregate_fp_rate(&self) -> Option<f64> {
        if self.total_recorded == 0 {
            return None;
        }
        Some(self.total_false_positives as f64 / self.total_recorded as f64)
    }
}

/// Complete governor state
///
/// This struct matches the governor-state.json schema from the plan.
/// All fields have serde defaults for backward compatibility — new fields
/// added in later versions will deserialize as their default when reading
/// older state files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernorState {
    pub updated_at: DateTime<Utc>,
    pub usage: UsageState,
    pub last_fleet_aggregate: FleetAggregate,
    pub capacity_forecast: CapacityForecast,
    pub schedule: ScheduleState,
    pub workers: HashMap<String, WorkerState>,
    pub burn_rate: BurnRateState,
    pub alerts: Vec<serde_json::Value>,
    pub safe_mode: SafeModeState,
    /// Per-type alert cooldown timestamps for deduplication
    pub alert_cooldown: AlertCooldown,
    /// Whether OAuth token refresh is failing (set by poller)
    pub token_refresh_failing: bool,
    /// Number of consecutive collection intervals where fleet_cache_eff was below threshold.
    /// Reset to 0 when efficiency recovers. Used by LowCacheEfficiency alert.
    #[serde(default)]
    pub low_cache_eff_consecutive: u32,
    /// Alert FP rate telemetry for regression detection.
    /// Exposed in `cgov status` so dashboards can track alert quality over time.
    #[serde(default)]
    pub alert_fp_telemetry: AlertFpTelemetry,
    /// Pending predictions for each window, used to score predictions when windows reset.
    /// Key is window name ("five_hour", "seven_day", "weekly_scoped").
    #[serde(default)]
    pub pending_predictions: HashMap<String, PendingPrediction>,
    /// Previous API snapshot taken at the last poll() cycle.
    /// Used to compute window percentage deltas between consecutive cycles.
    #[serde(default)]
    pub previous_api_snapshot: Option<PrevUsageSnapshot>,
    /// Current API snapshot taken at the most recent poll() call.
    /// Updated after each successful poll completes.
    #[serde(default)]
    pub current_api_snapshot: Option<PrevUsageSnapshot>,
    /// 5-hour window percentage delta (current - previous).
    /// Computed from consecutive API readings across governor cycles.
    #[serde(default)]
    pub p5h_delta: Option<f64>,
    /// 7-day window percentage delta (current - previous).
    /// Computed from consecutive API readings across governor cycles.
    #[serde(default)]
    pub p7d_delta: Option<f64>,
    /// 7-day Sonnet window percentage delta (current - previous).
    /// Computed from consecutive API readings across governor cycles.
    #[serde(default)]
    pub p7ds_delta: Option<f64>,
    /// Per-agent baseline burn rates from config.
    /// Used as fallback when token collector is offline or EMA is not yet ready.
    /// Key is agent name (e.g., "needle-sonnet", "polish-opus").
    #[serde(default)]
    pub baseline_burn_rates: HashMap<String, BaselineBurnRates>,
    /// Per-window count of consecutive polls in which the window was absent
    /// (null) from the API response, or reported `is_active == false` for its
    /// limit entry. Once a window's count reaches
    /// [`crate::governor::INACTIVE_WINDOW_POLL_THRESHOLD`] it is treated as
    /// structurally inactive: excluded from binding-window candidacy so the
    /// governor stops pinning the worker count at the current value waiting
    /// for burn data that cannot arrive (observed live: the model-scoped
    /// weekly window null across every poll while the pooled windows had ample
    /// headroom). Reset to 0 the instant the window reappears. Only the
    /// dynamic weekly_scoped slot is observed to be absent in practice; this
    /// is tracked per-window so the same rule applies to any window if that
    /// ever changes.
    #[serde(default)]
    pub consecutive_absent_polls: HashMap<String, u32>,
}

impl Default for GovernorState {
    fn default() -> Self {
        Self {
            updated_at: Utc::now(),
            usage: UsageState::default(),
            last_fleet_aggregate: FleetAggregate::default(),
            capacity_forecast: CapacityForecast::default(),
            schedule: ScheduleState::default(),
            workers: HashMap::new(),
            burn_rate: BurnRateState::default(),
            alerts: Vec::new(),
            safe_mode: SafeModeState::default(),
            alert_cooldown: AlertCooldown::default(),
            token_refresh_failing: false,
            low_cache_eff_consecutive: 0,
            alert_fp_telemetry: AlertFpTelemetry::default(),
            pending_predictions: HashMap::new(),
            previous_api_snapshot: None,
            current_api_snapshot: None,
            p5h_delta: None,
            p7d_delta: None,
            p7ds_delta: None,
            baseline_burn_rates: HashMap::new(),
            consecutive_absent_polls: HashMap::new(),
        }
    }
}

impl GovernorState {
    /// Create a new empty state with the current timestamp
    pub fn new() -> Self {
        Self::default()
    }

    /// Update consecutive-absent counters for all three pooled windows.
    ///
    /// For each window (five_hour, seven_day, weekly_scoped), this method:
    /// - Increments the counter if the window is absent (null from API or empty resets_at)
    /// - Resets the counter to 0 if the window reports real data
    ///
    /// The counters persist across governor cycles and are used to detect
    /// structural absence (windows that are consistently missing from the API).
    ///
    /// # Arguments
    /// - `five_hour_present`: true if the 5-hour window reported real data
    /// - `seven_day_present`: true if the 7-day window reported real data
    /// - `weekly_scoped_present`: true if the weekly_scoped window reported real data
    ///
    /// # Window Presence Detection
    /// A window is considered "absent" when:
    /// - The API returned null for the window (`Option<UsageWindow>::None`), OR
    /// - The window's `resets_at` field is empty (indicates null/default fallback)
    ///
    /// This matches the `window_or_default()` semantics in poller.rs where a null
    /// window becomes (0.0, String::new(), 168.0).
    pub fn update_consecutive_absent_polls(
        &mut self,
        five_hour_present: bool,
        seven_day_present: bool,
        weekly_scoped_present: bool,
    ) {
        // Update each window's counter based on presence
        for (window_key, is_present) in [
            (WINDOW_FIVE_HOUR, five_hour_present),
            (WINDOW_SEVEN_DAY, seven_day_present),
            (WINDOW_WEEKLY_SCOPED, weekly_scoped_present),
        ] {
            let counter = self.consecutive_absent_polls.entry(window_key.to_string()).or_insert(0);

            if is_present {
                // Window is present: reset counter to 0
                *counter = 0;
            } else {
                // Window is absent: increment counter
                *counter += 1;
            }
        }
    }

    /// Check if a window has reached the consecutive-absent threshold.
    ///
    /// Returns true if the window's consecutive-absent counter is >= MIN_CONSECUTIVE_ABSENT,
    /// indicating the window should be treated as structurally inactive (excluded from
    /// binding-window candidacy).
    ///
    /// # Arguments
    /// - `window`: The window key ("five_hour", "seven_day", "weekly_scoped")
    pub fn is_window_consecutively_absent(&self, window: &str) -> bool {
        self.consecutive_absent_polls
            .get(window)
            .map(|&count| count >= MIN_CONSECUTIVE_ABSENT)
            .unwrap_or(false)
    }

    /// Get the consecutive-absent count for a specific window.
    ///
    /// Returns the number of consecutive polls where the window has been absent,
    /// or 0 if the window has never been absent.
    pub fn get_consecutive_absent_count(&self, window: &str) -> u32 {
        self.consecutive_absent_polls.get(window).copied().unwrap_or(0)
    }

    /// Populate baseline burn rates from agent configuration
    ///
    /// This method loads the baseline_burn_rate settings from the provided agent config map
    /// and stores them in the state. These values serve as fallback burn rates when the
    /// token collector is offline or when insufficient EMA samples have been accumulated.
    ///
    /// # Arguments
    /// - `agents_config`: A map of agent name to AgentConfig from governor.yaml
    ///
    /// # Example
    /// ```ignore
    /// let mut state = GovernorState::new();
    /// let config = GovernorConfig::load()?;
    /// state.load_baseline_burn_rates_from_config(&config.agents);
    /// ```
    pub fn load_baseline_burn_rates_from_config(
        &mut self,
        agents_config: &std::collections::HashMap<String, crate::config::AgentConfig>,
    ) {
        for (agent_name, agent_config) in agents_config {
            if let Some(baseline_config) = &agent_config.baseline_burn_rate {
                let baseline = BaselineBurnRates {
                    pct_per_worker_per_hour: baseline_config.pct_per_worker_per_hour,
                    dollars_per_worker_per_hour: baseline_config.dollars_per_worker_per_hour,
                };
                log::debug!(
                    "[state] loaded baseline_burn_rate for {}: pct={:.2}/hr, ${:.2}/hr",
                    agent_name,
                    baseline.pct_per_worker_per_hour,
                    baseline.dollars_per_worker_per_hour
                );
                self.baseline_burn_rates.insert(agent_name.clone(), baseline);
            }
            // If baseline_burn_rate is None, we don't insert anything
            // The caller can use BaselineBurnRates::default() as a fallback
        }
    }

    /// Get baseline burn rates for a specific agent
    ///
    /// Returns the configured baseline burn rates for the agent, or None if not configured.
    /// Callers can use `BaselineBurnRates::default()` as a fallback when None is returned.
    ///
    /// # Arguments
    /// - `agent_name`: The name of the agent (e.g., "needle-sonnet", "polish-opus")
    ///
    /// # Returns
    /// - `Some(BaselineBurnRates)` if the agent has a configured baseline
    /// - `None` if the agent is not in the state (cold-start or not configured)
    pub fn get_baseline_burn_rates(&self, agent_name: &str) -> Option<&BaselineBurnRates> {
        self.baseline_burn_rates.get(agent_name)
    }

    /// Update API snapshots after a poll.
    ///
    /// This method shifts the snapshot state: the current snapshot becomes the previous,
    /// and a new snapshot is set as current.
    ///
    /// # Arguments
    /// - `now`: The current timestamp when the snapshot was taken
    /// - `five_hour_pct`: The 5-hour window utilization percentage
    /// - `seven_day_pct`: The 7-day window utilization percentage
    /// - `weekly_scoped_pct`: The 7-day Sonnet window utilization percentage
    ///
    /// # Example
    /// ```no_run
    /// use chrono::Utc;
    /// # use claude_governor::state::GovernorState;
    /// let mut state = GovernorState::new();
    ///
    /// // After first poll, current is set, previous is None
    /// state.update_api_snapshot(Utc::now(), 10.0, 20.0, 15.0);
    /// assert!(state.previous_api_snapshot.is_none());
    /// assert!(state.current_api_snapshot.is_some());
    ///
    /// // After second poll, previous is set to old current, current is updated
    /// state.update_api_snapshot(Utc::now(), 12.0, 22.0, 18.0);
    /// assert!(state.previous_api_snapshot.is_some());
    /// assert!(state.current_api_snapshot.is_some());
    /// ```
    pub fn update_api_snapshot(
        &mut self,
        now: DateTime<Utc>,
        five_hour_pct: f64,
        seven_day_pct: f64,
        weekly_scoped_pct: f64,
    ) {
        // Shift: current becomes previous
        self.previous_api_snapshot = self.current_api_snapshot.take();

        // Set new current snapshot
        self.current_api_snapshot = Some(PrevUsageSnapshot {
            taken_at: now,
            five_hour_pct,
            seven_day_pct,
            weekly_scoped_pct,
        });
    }
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

/// Load governor state from a JSON file
///
/// Returns a default (empty) state if the file doesn't exist.
/// Returns a default state and logs a warning if the file is corrupt.
pub fn load_state(path: &Path) -> Result<GovernorState> {
    if !path.exists() {
        log::debug!(
            "[state] no state file at {}, starting fresh",
            path.display()
        );
        return Ok(GovernorState::new());
    }

    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);

    match serde_json::from_reader::<_, GovernorState>(reader) {
        Ok(state) => Ok(state),
        Err(e) => {
            log::warn!(
                "[state] corrupt state file at {}: {}, starting fresh",
                path.display(),
                e
            );
            Ok(GovernorState::new())
        }
    }
}

/// Load the previous state from the `.prev.json` file
///
/// Returns `None` if the file doesn't exist (first run or prev was deleted).
pub fn load_previous_state(path: &Path) -> Result<Option<GovernorState>> {
    let prev_path = previous_state_path(path);
    if !prev_path.exists() {
        return Ok(None);
    }

    let file = fs::File::open(&prev_path)?;
    let reader = BufReader::new(file);

    match serde_json::from_reader::<_, GovernorState>(reader) {
        Ok(state) => Ok(Some(state)),
        Err(e) => {
            log::warn!(
                "[state] corrupt previous state at {}: {}",
                prev_path.display(),
                e
            );
            Ok(None)
        }
    }
}

/// Atomically save governor state to a JSON file
///
/// Writes to a `.tmp` file first, then renames to the final path.
/// This ensures concurrent readers never see a partial write.
pub fn save_state(state: &GovernorState, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension("json.tmp");

    {
        let file = fs::File::create(&tmp_path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, state)?;
    }

    fs::rename(&tmp_path, path)?;

    Ok(())
}

/// Save current state as the previous state (before an update)
///
/// Writes to `governor-state.prev.json` (derived from the main path).
/// Uses the same atomic write pattern as `save_state`.
pub fn save_previous_state(state: &GovernorState, path: &Path) -> Result<()> {
    let prev_path = previous_state_path(path);

    if let Some(parent) = prev_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp_path = prev_path.with_extension("prev.json.tmp");

    {
        let file = fs::File::create(&tmp_path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, state)?;
    }

    fs::rename(&tmp_path, &prev_path)?;

    Ok(())
}

/// Derive the previous-state path from the main state path
///
/// `governor-state.json` -> `governor-state.prev.json`
fn previous_state_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "governor-state.json".to_string());

    let prev_name = match file_name.strip_suffix(".json") {
        Some(stem) => format!("{}.prev.json", stem),
        None => format!("{}.prev", file_name),
    };

    path.with_file_name(prev_name)
}

// ---------------------------------------------------------------------------
// Delta computation
// ---------------------------------------------------------------------------

/// Compute a time delta between the current and previous state
///
/// Returns the elapsed hours between `updated_at` timestamps.
/// Returns `None` if there is no previous state or if timestamps are equal.
pub fn elapsed_hours_since_previous(
    current: &GovernorState,
    previous: &GovernorState,
) -> Option<f64> {
    let elapsed = current
        .updated_at
        .signed_duration_since(previous.updated_at);
    let hours = elapsed.num_seconds() as f64 / 3600.0;
    if hours <= 0.0 {
        None
    } else {
        Some(hours)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a fully populated GovernorState for round-trip testing
    fn full_state() -> GovernorState {
        let mut by_model = HashMap::new();
        by_model.insert(
            "claude-sonnet-4-6".to_string(),
            ModelBurnRate {
                pct_per_worker_per_hour: 1.35,
                dollars_per_worker_per_hour: 5.54,
                samples: 12,
            },
        );
        by_model.insert(
            "claude-opus-4-6".to_string(),
            ModelBurnRate {
                pct_per_worker_per_hour: 3.80,
                dollars_per_worker_per_hour: 9.21,
                samples: 4,
            },
        );

        let mut workers = HashMap::new();
        workers.insert(
            "claude-anthropic-sonnet".to_string(),
            WorkerState {
                current: 2,
                target: 3,
                min: 1,
                max: 5,
            },
        );

        GovernorState {
            updated_at: "2026-03-18T14:30:00Z".parse().unwrap(),
            usage: UsageState {
                weekly_scoped_pct: 72.0,
                sonnet_pct: 72.0,
                all_models_pct: 81.0,
                five_hour_pct: 14.0,
                sonnet_resets_at: "2026-03-20T03:59:59Z".to_string(),
                five_hour_resets_at: "2026-03-18T15:59:59Z".to_string(),
                stale: false,
                weekly_scoped_model: Some("Fable".to_string()),
            },
            last_fleet_aggregate: FleetAggregate {
                t0: "2026-03-18T14:25:00Z".parse().unwrap(),
                t1: "2026-03-18T14:30:00Z".parse().unwrap(),
                sonnet_workers: 2,
                sonnet_usd_total: 0.3201,
                sonnet_p75_usd_hr: 2.147,
                sonnet_std_usd_hr: 0.312,
                window_pct_deltas: WindowPctDeltas {
                    five_hour: 0.66,
                    seven_day: 0.54,
                    weekly_scoped: 0.75,
                },
                fleet_cache_eff: 0.0,
                cache_eff_p25: 0.0,
                cli_tokens: 125000,
                cli_cost: 0.28,
                sdk_tokens: 45000,
                sdk_cost: 0.04,
            },
            capacity_forecast: CapacityForecast {
                five_hour: WindowForecast {
                    target_ceiling: 85.0,
                    current_utilization: 36.4,
                    remaining_pct: 48.6,
                    hours_remaining: 1.50,
                    fleet_pct_per_hour: 7.92,
                    predicted_exhaustion_hours: 6.14,
                    cutoff_risk: false,
                    margin_hrs: 4.64,
                    binding: false,
                    safe_worker_count: None,
                    ..Default::default()
                },
                seven_day: WindowForecast {
                    target_ceiling: 90.0,
                    current_utilization: 72.6,
                    remaining_pct: 17.4,
                    hours_remaining: 37.5,
                    fleet_pct_per_hour: 6.48,
                    predicted_exhaustion_hours: 2.69,
                    cutoff_risk: true,
                    margin_hrs: -34.81,
                    binding: false,
                    safe_worker_count: None,
                    ..Default::default()
                },
                weekly_scoped: WindowForecast {
                    target_ceiling: 90.0,
                    current_utilization: 63.5,
                    remaining_pct: 26.5,
                    hours_remaining: 37.5,
                    fleet_pct_per_hour: 9.0,
                    predicted_exhaustion_hours: 2.94,
                    cutoff_risk: true,
                    margin_hrs: -34.56,
                    binding: true,
                    safe_worker_count: Some(2),
                    ..Default::default()
                },
                binding_window: "weekly_scoped".to_string(),
                dollars_per_pct_7d_s: 1.648,
                estimated_remaining_dollars: 46.1,
            },
            schedule: ScheduleState {
                is_peak_hour: false,
                is_promo_active: true,
                promo_multiplier_five_hour: 2.0,
                promo_multiplier_seven_day: 1.0,
                promo_multiplier_weekly_scoped: 1.0,
                promo_multiplier: 2.0,
                effective_hours_remaining_five_hour: 84.5,
                effective_hours_remaining_seven_day: 37.5,
                effective_hours_remaining_weekly_scoped: 37.5,
                effective_hours_remaining: 84.5,
                raw_hours_remaining: 37.5,
            },
            workers,
            burn_rate: BurnRateState {
                by_model,
                tokens_per_pct_peak: 69780,
                tokens_per_pct_offpeak: 141350,
                offpeak_ratio_observed: 2.03,
                offpeak_ratio_expected: 2.0,
                promotion_validated: true,
                promotion_peak_samples: 0,
                promotion_offpeak_samples: 0,
                last_sample_at: Some("2026-03-18T14:15:00Z".parse().unwrap()),
                calibration: CalibrationState {
                    predictions_scored: 24,
                    median_error_7ds: -3.2,
                    auto_tuned_alpha: 0.22,
                    auto_tuned_hysteresis: 1.0,
                    last_tuned_at: Some("2026-03-20T04:00:00Z".parse().unwrap()),
                },
                ..Default::default()
            },
            alerts: vec![serde_json::json!({
                "type": "cutoff_risk",
                "window": "weekly_scoped",
                "message": "Binding window at risk of exceeding target"
            })],
            safe_mode: SafeModeState {
                active: true,
                entered_at: Some("2026-03-19T10:00:00Z".parse().unwrap()),
                trigger: Some("median_error".to_string()),
                median_error_at_entry: Some(14.2),
                predictions_since_entry: 1,
                scored_at_entry: 0,
            },
            alert_cooldown: AlertCooldown {
                last_fired: {
                    let mut m = HashMap::new();
                    m.insert(
                        "cutoff_imminent".to_string(),
                        "2026-03-18T14:00:00Z".parse().unwrap(),
                    );
                    m
                },
            },
            token_refresh_failing: false,
            low_cache_eff_consecutive: 0,
            alert_fp_telemetry: AlertFpTelemetry::default(),
            pending_predictions: HashMap::new(),
            previous_api_snapshot: None,
            current_api_snapshot: None,
            p5h_delta: None,
            p7d_delta: None,
            p7ds_delta: None,
            baseline_burn_rates: HashMap::new(),
            consecutive_absent_polls: HashMap::new(),
        }
    }

    // --- Round-trip serialize/deserialize ---

    #[test]
    fn round_trip_full_state() {
        let state = full_state();

        let json = serde_json::to_string(&state).unwrap();
        let loaded: GovernorState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.usage.sonnet_pct, 72.0);
        assert_eq!(loaded.usage.all_models_pct, 81.0);
        assert_eq!(
            loaded.usage.weekly_scoped_model.as_deref(),
            Some("Fable"),
            "weekly_scoped_model must round-trip as a populated Some"
        );
        assert_eq!(loaded.capacity_forecast.binding_window, "weekly_scoped");
        assert_eq!(loaded.burn_rate.tokens_per_pct_peak, 69780);
        assert_eq!(loaded.burn_rate.by_model["claude-sonnet-4-6"].samples, 12);
        assert_eq!(loaded.workers["claude-anthropic-sonnet"].current, 2);
        assert_eq!(loaded.alerts.len(), 1);
        assert_eq!(loaded.safe_mode.active, true);
        assert_eq!(loaded.safe_mode.trigger.as_deref(), Some("median_error"));
        assert_eq!(loaded.burn_rate.calibration.predictions_scored, 24);
        assert_eq!(
            loaded.capacity_forecast.weekly_scoped.safe_worker_count,
            Some(2)
        );
    }

    #[test]
    fn round_trip_preserves_all_timestamps() {
        let state = full_state();
        let json = serde_json::to_string(&state).unwrap();
        let loaded: GovernorState = serde_json::from_str(&json).unwrap();

        assert_eq!(
            loaded.updated_at,
            "2026-03-18T14:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            loaded.burn_rate.last_sample_at,
            Some("2026-03-18T14:15:00Z".parse().unwrap())
        );
        assert_eq!(
            loaded.safe_mode.entered_at,
            Some("2026-03-19T10:00:00Z".parse().unwrap())
        );
    }

    // --- Load from missing file -> default ---

    #[test]
    fn load_missing_file_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nonexistent-governor-state.json");

        let state = load_state(&path).unwrap();

        assert_eq!(state.usage.sonnet_pct, 0.0);
        assert!(state.workers.is_empty());
        assert!(state.burn_rate.by_model.is_empty());
        assert!(state.alerts.is_empty());
        assert!(!state.safe_mode.active);
    }

    #[test]
    fn load_empty_file_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("empty-state.json");
        fs::write(&path, "").unwrap();

        let state = load_state(&path).unwrap();

        assert_eq!(state.usage.sonnet_pct, 0.0);
        assert!(state.workers.is_empty());
    }

    #[test]
    fn load_corrupt_file_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("corrupt-state.json");
        fs::write(&path, "not valid json {{{").unwrap();

        let state = load_state(&path).unwrap();

        assert_eq!(state.usage.sonnet_pct, 0.0);
        assert!(state.workers.is_empty());
    }

    // --- Atomic write ---

    #[test]
    fn save_state_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("governor-state.json");

        let state = full_state();
        save_state(&state, &path).unwrap();

        assert!(path.exists());

        // Verify no .tmp file remains
        assert!(!path.with_extension("json.tmp").exists());

        // Verify we can load it back
        let loaded = load_state(&path).unwrap();
        assert_eq!(loaded.usage.sonnet_pct, 72.0);
    }

    #[test]
    fn atomic_write_no_partial_read() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("governor-state.json");

        // Write state
        let state = full_state();
        save_state(&state, &path).unwrap();

        // Read the raw bytes and verify it's valid JSON
        let bytes = fs::read(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        // Verify the structure is complete (not truncated)
        assert!(parsed.is_object());
        assert!(parsed.get("usage").is_some());
        assert!(parsed.get("capacity_forecast").is_some());
        assert!(parsed.get("burn_rate").is_some());
        assert!(parsed.get("safe_mode").is_some());
    }

    #[test]
    fn save_creates_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir
            .path()
            .join("nested")
            .join("dir")
            .join("governor-state.json");

        let state = full_state();
        save_state(&state, &path).unwrap();

        assert!(path.exists());
        let loaded = load_state(&path).unwrap();
        assert_eq!(loaded.usage.sonnet_pct, 72.0);
    }

    // --- Previous state ---

    #[test]
    fn save_and_load_previous_state() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("governor-state.json");

        let state = full_state();
        save_previous_state(&state, &path).unwrap();

        // Should create .prev.json
        let prev_path = temp_dir.path().join("governor-state.prev.json");
        assert!(prev_path.exists());

        // No .tmp file should remain
        assert!(!prev_path.with_extension("prev.json.tmp").exists());

        // Load it back
        let loaded = load_previous_state(&path).unwrap().unwrap();
        assert_eq!(loaded.usage.sonnet_pct, 72.0);
        assert_eq!(loaded.capacity_forecast.binding_window, "weekly_scoped");
    }

    #[test]
    fn load_previous_state_missing_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("governor-state.json");

        let result = load_previous_state(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn previous_state_preserved_across_updates() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("governor-state.json");

        // Write initial state
        let mut state1 = full_state();
        state1.usage.sonnet_pct = 50.0;
        save_state(&state1, &path).unwrap();

        // Update: save prev, then write new state
        let mut state2 = full_state();
        state2.usage.sonnet_pct = 75.0;

        // Save current as previous
        save_previous_state(&state1, &path).unwrap();
        // Write new state
        save_state(&state2, &path).unwrap();

        // Verify current state is the new one
        let current = load_state(&path).unwrap();
        assert_eq!(current.usage.sonnet_pct, 75.0);

        // Verify previous state is the old one
        let previous = load_previous_state(&path).unwrap().unwrap();
        assert_eq!(previous.usage.sonnet_pct, 50.0);
    }

    // --- Default values for optional fields ---

    #[test]
    fn default_state_has_sensible_zeros() {
        let state = GovernorState::default();

        assert_eq!(state.usage.sonnet_pct, 0.0);
        assert_eq!(state.last_fleet_aggregate.sonnet_workers, 0);
        assert_eq!(state.burn_rate.tokens_per_pct_peak, 0);
        assert!(state.burn_rate.by_model.is_empty());
        assert!(state.alerts.is_empty());
        assert!(!state.safe_mode.active);
        assert_eq!(state.schedule.promo_multiplier, 1.0);
        assert!(state.capacity_forecast.binding_window.is_empty());
        assert!(!state.capacity_forecast.five_hour.cutoff_risk);
        assert!(state
            .capacity_forecast
            .five_hour
            .safe_worker_count
            .is_none());
        assert_eq!(state.burn_rate.calibration.predictions_scored, 0);
    }

    #[test]
    fn deserializing_partial_json_fills_defaults() {
        // Simulate an older state file that only has a subset of fields
        let json = r#"{
            "updated_at": "2026-03-18T14:30:00Z",
            "usage": {
                "sonnet_pct": 72.0
            },
            "alerts": []
        }"#;

        let state: GovernorState = serde_json::from_str(json).unwrap();

        // Provided field
        assert_eq!(state.usage.sonnet_pct, 72.0);

        // Missing fields get defaults
        assert_eq!(state.usage.all_models_pct, 0.0);
        assert_eq!(state.usage.five_hour_pct, 0.0);
        assert!(state.workers.is_empty());
        assert!(state.burn_rate.by_model.is_empty());
        assert!(!state.safe_mode.active);
        assert_eq!(state.schedule.promo_multiplier, 1.0);
        assert_eq!(state.burn_rate.calibration.predictions_scored, 0);
    }

    // --- Delta computation ---

    #[test]
    fn elapsed_hours_since_previous_computes_correctly() {
        let current = GovernorState {
            updated_at: "2026-03-18T14:30:00Z".parse().unwrap(),
            ..GovernorState::default()
        };
        let previous = GovernorState {
            updated_at: "2026-03-18T14:00:00Z".parse().unwrap(),
            ..GovernorState::default()
        };

        let hours = elapsed_hours_since_previous(&current, &previous).unwrap();
        assert!((hours - 0.5).abs() < 1e-9);
    }

    #[test]
    fn elapsed_hours_returns_none_for_same_timestamp() {
        let ts = "2026-03-18T14:30:00Z".parse().unwrap();
        let current = GovernorState {
            updated_at: ts,
            ..GovernorState::default()
        };
        let previous = GovernorState {
            updated_at: ts,
            ..GovernorState::default()
        };

        assert!(elapsed_hours_since_previous(&current, &previous).is_none());
    }

    #[test]
    fn elapsed_hours_returns_none_when_current_before_previous() {
        let current = GovernorState {
            updated_at: "2026-03-18T14:00:00Z".parse().unwrap(),
            ..GovernorState::default()
        };
        let previous = GovernorState {
            updated_at: "2026-03-18T14:30:00Z".parse().unwrap(),
            ..GovernorState::default()
        };

        assert!(elapsed_hours_since_previous(&current, &previous).is_none());
    }

    // --- previous_state_path helper ---

    #[test]
    fn previous_state_path_derives_correctly() {
        let path = Path::new("/home/user/.needle/state/governor-state.json");
        let prev = previous_state_path(path);

        assert_eq!(
            prev,
            Path::new("/home/user/.needle/state/governor-state.prev.json")
        );
    }

    #[test]
    fn previous_state_path_works_with_non_json_extension() {
        let path = Path::new("/tmp/state-file");
        let prev = previous_state_path(path);

        assert_eq!(prev, Path::new("/tmp/state-file.prev"));
    }

    // --- Baseline burn rates ---

    #[test]
    fn load_baseline_burn_rates_from_config_populates_state() {
        use crate::config::{AgentConfig, BaselineBurnRateConfig};

        let mut state = GovernorState::new();
        let mut agents_config = std::collections::HashMap::new();

        // Add two agents with different baseline configurations
        agents_config.insert(
            "needle-sonnet".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent sonnet".to_string(),
                session_pattern: "sonnet-*".to_string(),
                heartbeat_dir: "/tmp".to_string(),
                min_workers: 0,
                max_workers: 8,
                subscription: true,
                baseline_burn_rate: Some(BaselineBurnRateConfig {
                    pct_per_worker_per_hour: 1.8,
                    dollars_per_worker_per_hour: 6.5,
                }),
            },
        );

        agents_config.insert(
            "polish-opus".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent opus".to_string(),
                session_pattern: "opus-*".to_string(),
                heartbeat_dir: "/tmp".to_string(),
                min_workers: 0,
                max_workers: 4,
                subscription: true,
                baseline_burn_rate: Some(BaselineBurnRateConfig {
                    pct_per_worker_per_hour: 2.5,
                    dollars_per_worker_per_hour: 10.0,
                }),
            },
        );

        // Agent without baseline_burn_rate configured
        agents_config.insert(
            "needle-default".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent default".to_string(),
                session_pattern: "default-*".to_string(),
                heartbeat_dir: "/tmp".to_string(),
                min_workers: 0,
                max_workers: 8,
                subscription: false,
                baseline_burn_rate: None,
            },
        );

        state.load_baseline_burn_rates_from_config(&agents_config);

        // Should have entries for the two agents with configured baselines
        assert_eq!(state.baseline_burn_rates.len(), 2);

        // Check needle-sonnet baseline
        let sonnet_baseline = state.get_baseline_burn_rates("needle-sonnet");
        assert!(sonnet_baseline.is_some());
        assert!((sonnet_baseline.unwrap().pct_per_worker_per_hour - 1.8).abs() < 1e-9);
        assert!((sonnet_baseline.unwrap().dollars_per_worker_per_hour - 6.5).abs() < 1e-9);

        // Check polish-opus baseline
        let opus_baseline = state.get_baseline_burn_rates("polish-opus");
        assert!(opus_baseline.is_some());
        assert!((opus_baseline.unwrap().pct_per_worker_per_hour - 2.5).abs() < 1e-9);
        assert!((opus_baseline.unwrap().dollars_per_worker_per_hour - 10.0).abs() < 1e-9);

        // Agent without baseline should return None
        let default_baseline = state.get_baseline_burn_rates("needle-default");
        assert!(default_baseline.is_none());

        // Unknown agent should return None
        let unknown_baseline = state.get_baseline_burn_rates("unknown-agent");
        assert!(unknown_baseline.is_none());
    }

    #[test]
    fn get_baseline_burn_rates_returns_none_for_unknown_agent() {
        let state = GovernorState::new();
        assert!(state.get_baseline_burn_rates("unknown").is_none());
    }

    #[test]
    fn baseline_burn_rates_roundtrip_serialization() {
        use crate::config::{AgentConfig, BaselineBurnRateConfig};

        let mut state = GovernorState::new();
        let mut agents_config = std::collections::HashMap::new();

        agents_config.insert(
            "test-agent".to_string(),
            AgentConfig {
                launch_cmd: "test".to_string(),
                session_pattern: "test-*".to_string(),
                heartbeat_dir: "/tmp".to_string(),
                min_workers: 0,
                max_workers: 8,
                subscription: true,
                baseline_burn_rate: Some(BaselineBurnRateConfig {
                    pct_per_worker_per_hour: 3.0,
                    dollars_per_worker_per_hour: 12.0,
                }),
            },
        );

        state.load_baseline_burn_rates_from_config(&agents_config);

        // Serialize and deserialize
        let json = serde_json::to_string(&state).unwrap();
        let loaded: GovernorState = serde_json::from_str(&json).unwrap();

        // Verify baseline_burn_rates survived the roundtrip
        assert_eq!(loaded.baseline_burn_rates.len(), 1);
        let baseline = loaded.get_baseline_burn_rates("test-agent");
        assert!(baseline.is_some());
        assert!((baseline.unwrap().pct_per_worker_per_hour - 3.0).abs() < 1e-9);
    }

    // --- safe_worker_count serialization ---

    #[test]
    fn safe_worker_count_none_skipped_in_json() {
        let forecast = WindowForecast {
            safe_worker_count: None,
            ..WindowForecast::default()
        };

        let json = serde_json::to_value(&forecast).unwrap();
        assert!(!json.as_object().unwrap().contains_key("safe_worker_count"));
    }

    #[test]
    fn safe_worker_count_some_included_in_json() {
        let forecast = WindowForecast {
            safe_worker_count: Some(5),
            ..WindowForecast::default()
        };

        let json = serde_json::to_value(&forecast).unwrap();
        assert_eq!(
            json.as_object().unwrap().get("safe_worker_count").unwrap(),
            5
        );
    }

    // --- JSON output matches plan schema field names ---

    #[test]
    fn json_field_names_match_plan() {
        let state = full_state();
        let json = serde_json::to_value(&state).unwrap();
        let obj = json.as_object().unwrap();

        // Top-level keys
        assert!(obj.contains_key("updated_at"));
        assert!(obj.contains_key("usage"));
        assert!(obj.contains_key("last_fleet_aggregate"));
        assert!(obj.contains_key("capacity_forecast"));
        assert!(obj.contains_key("schedule"));
        assert!(obj.contains_key("workers"));
        assert!(obj.contains_key("burn_rate"));
        assert!(obj.contains_key("alerts"));
        assert!(obj.contains_key("safe_mode"));

        // Usage keys
        let usage = obj["usage"].as_object().unwrap();
        assert!(usage.contains_key("sonnet_pct"));
        assert!(usage.contains_key("all_models_pct"));
        assert!(usage.contains_key("five_hour_pct"));
        assert!(usage.contains_key("sonnet_resets_at"));
        assert!(usage.contains_key("five_hour_resets_at"));

        // Burn rate keys
        let br = obj["burn_rate"].as_object().unwrap();
        assert!(br.contains_key("by_model"));
        assert!(br.contains_key("tokens_per_pct_peak"));
        assert!(br.contains_key("tokens_per_pct_offpeak"));
        assert!(br.contains_key("offpeak_ratio_observed"));
        assert!(br.contains_key("offpeak_ratio_expected"));
        assert!(br.contains_key("promotion_validated"));
        assert!(br.contains_key("last_sample_at"));
        assert!(br.contains_key("calibration"));

        // Safe mode keys
        let sm = obj["safe_mode"].as_object().unwrap();
        assert!(sm.contains_key("active"));
        assert!(sm.contains_key("entered_at"));
        assert!(sm.contains_key("trigger"));
        assert!(sm.contains_key("median_error_at_entry"));
        assert!(sm.contains_key("predictions_since_entry"));
    }

    // --- Concurrent read safety (atomic write) ---

    #[test]
    fn concurrent_read_never_sees_partial_write() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("governor-state.json");

        // Write initial state
        let state_v1 = GovernorState {
            updated_at: "2026-03-18T14:00:00Z".parse().unwrap(),
            ..GovernorState::default()
        };
        save_state(&state_v1, &path).unwrap();

        // Write many updates rapidly
        for i in 0..100 {
            let state = GovernorState {
                updated_at: Utc::now(),
                usage: UsageState {
                    sonnet_pct: i as f64,
                    ..UsageState::default()
                },
                ..GovernorState::default()
            };
            save_state(&state, &path).unwrap();
        }

        // Final read must be valid JSON with a complete structure
        let loaded = load_state(&path).unwrap();
        assert!(loaded.usage.sonnet_pct >= 0.0);
        assert!(loaded.usage.sonnet_pct <= 99.0);

        // Verify it's well-formed
        let bytes = fs::read(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(parsed.is_object());
        assert!(parsed.get("usage").is_some());
        assert!(parsed.get("capacity_forecast").is_some());
    }

    // --- Snapshot state tracking ---

    #[test]
    fn update_api_snapshot_first_poll_sets_current_only() {
        let mut state = GovernorState::new();
        let now = Utc::now();

        // First poll: only current should be set, previous should remain None
        state.update_api_snapshot(now, 10.0, 20.0, 15.0);

        assert!(state.previous_api_snapshot.is_none(),
                "On first poll, previous_api_snapshot should be None");
        assert!(state.current_api_snapshot.is_some(),
                "On first poll, current_api_snapshot should be Some");

        let curr = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(curr.five_hour_pct, 10.0);
        assert_eq!(curr.seven_day_pct, 20.0);
        assert_eq!(curr.weekly_scoped_pct, 15.0);
        assert_eq!(curr.taken_at, now);
    }

    #[test]
    fn first_poll_transition_no_panic_with_none_previous() {
        // Test the first poll transition scenario:
        // - previous_api_snapshot starts as None
        // - current_api_snapshot becomes Some after first poll
        // - No panic should occur when accessing/processing None previous snapshot
        let mut state = GovernorState::new();

        // Verify initial state: both snapshots should be None
        assert!(state.previous_api_snapshot.is_none(),
                "Initial state: previous_api_snapshot should be None");
        assert!(state.current_api_snapshot.is_none(),
                "Initial state: current_api_snapshot should be None");

        // Simulate first successful poll with realistic utilization values
        let now = Utc::now();
        let five_hour_pct = 45.2;
        let seven_day_pct = 67.8;
        let weekly_scoped_pct = 72.3;

        // This should not panic even though previous_api_snapshot is None
        state.update_api_snapshot(now, five_hour_pct, seven_day_pct, weekly_scoped_pct);

        // Verify the transition: None -> Some for current_api_snapshot
        assert!(state.previous_api_snapshot.is_none(),
                "After first poll: previous_api_snapshot should still be None");
        assert!(state.current_api_snapshot.is_some(),
                "After first poll: current_api_snapshot should be Some");

        // Verify all snapshot fields are stored correctly
        let snapshot = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(snapshot.five_hour_pct, five_hour_pct,
                   "Five-hour utilization should match input");
        assert_eq!(snapshot.seven_day_pct, seven_day_pct,
                   "Seven-day utilization should match input");
        assert_eq!(snapshot.weekly_scoped_pct, weekly_scoped_pct,
                   "Seven-day sonnet utilization should match input");
        assert_eq!(snapshot.taken_at, now,
                   "Timestamp should match poll time");
    }

    #[test]
    fn first_poll_handles_zero_utilization() {
        // Edge case: first poll with zero utilization
        let mut state = GovernorState::new();
        let now = Utc::now();

        // Should handle zero values gracefully
        state.update_api_snapshot(now, 0.0, 0.0, 0.0);

        assert!(state.previous_api_snapshot.is_none());
        assert!(state.current_api_snapshot.is_some());

        let snapshot = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(snapshot.five_hour_pct, 0.0);
        assert_eq!(snapshot.seven_day_pct, 0.0);
        assert_eq!(snapshot.weekly_scoped_pct, 0.0);
    }

    #[test]
    fn first_poll_handles_high_utilization() {
        // Edge case: first poll near capacity limits
        let mut state = GovernorState::new();
        let now = Utc::now();

        // Should handle high utilization values (near 100%)
        state.update_api_snapshot(now, 95.7, 98.2, 99.1);

        assert!(state.previous_api_snapshot.is_none());
        assert!(state.current_api_snapshot.is_some());

        let snapshot = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(snapshot.five_hour_pct, 95.7);
        assert_eq!(snapshot.seven_day_pct, 98.2);
        assert_eq!(snapshot.weekly_scoped_pct, 99.1);
    }

    #[test]
    fn update_api_snapshot_second_poll_shifts_snapshots() {
        let mut state = GovernorState::new();
        let now1 = Utc::now();
        let now2 = now1 + chrono::Duration::seconds(60);

        // First poll
        state.update_api_snapshot(now1, 10.0, 20.0, 15.0);

        // Second poll: should shift current to previous, then set new current
        state.update_api_snapshot(now2, 12.5, 22.0, 18.0);

        assert!(state.previous_api_snapshot.is_some(),
                "On second poll, previous_api_snapshot should be Some");
        assert!(state.current_api_snapshot.is_some(),
                "On second poll, current_api_snapshot should be Some");

        // Verify previous holds the first poll's data
        let prev = state.previous_api_snapshot.as_ref().unwrap();
        assert_eq!(prev.five_hour_pct, 10.0, "previous should hold first poll data");
        assert_eq!(prev.seven_day_pct, 20.0);
        assert_eq!(prev.weekly_scoped_pct, 15.0);
        assert_eq!(prev.taken_at, now1);

        // Verify current holds the second poll's data
        let curr = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(curr.five_hour_pct, 12.5, "current should hold second poll data");
        assert_eq!(curr.seven_day_pct, 22.0);
        assert_eq!(curr.weekly_scoped_pct, 18.0);
        assert_eq!(curr.taken_at, now2);
    }

    #[test]
    fn update_api_snapshot_consecutive_polls_maintains_chain() {
        let mut state = GovernorState::new();

        // Simulate multiple polls
        let mut prev_values = Vec::new();
        for i in 0..5 {
            let now = Utc::now() + chrono::Duration::seconds(i as i64 * 60);
            let five_hr = 10.0 + i as f64 * 2.5;  // 10.0, 12.5, 15.0, 17.5, 20.0
            let seven_day = 20.0 + i as f64 * 2.0;  // 20.0, 22.0, 24.0, 26.0, 28.0
            let weekly_scoped = 15.0 + i as f64 * 3.0;  // 15.0, 18.0, 21.0, 24.0, 27.0

            state.update_api_snapshot(now, five_hr, seven_day, weekly_scoped);
            prev_values.push((five_hr, seven_day, weekly_scoped));

            // After the first poll, verify the chain
            if i > 0 {
                assert!(state.previous_api_snapshot.is_some());
                assert!(state.current_api_snapshot.is_some());

                let prev = state.previous_api_snapshot.as_ref().unwrap();
                let curr = state.current_api_snapshot.as_ref().unwrap();

                // Previous should hold the previous iteration's values
                let (p5h, p7d, p7ds) = prev_values[i - 1];
                assert_eq!(prev.five_hour_pct, p5h);
                assert_eq!(prev.seven_day_pct, p7d);
                assert_eq!(prev.weekly_scoped_pct, p7ds);

                // Current should hold the current iteration's values
                let (c5h, c7d, c7ds) = prev_values[i];
                assert_eq!(curr.five_hour_pct, c5h);
                assert_eq!(curr.seven_day_pct, c7d);
                assert_eq!(curr.weekly_scoped_pct, c7ds);
            }
        }
    }

    #[test]
    fn update_api_snapshot_handles_negative_deltas() {
        let mut state = GovernorState::new();
        let now1 = Utc::now();
        let now2 = now1 + chrono::Duration::seconds(60);

        // First poll with high utilization
        state.update_api_snapshot(now1, 80.0, 90.0, 85.0);

        // Second poll with low utilization (simulating window reset)
        state.update_api_snapshot(now2, 5.0, 15.0, 8.0);

        assert!(state.previous_api_snapshot.is_some());
        assert!(state.current_api_snapshot.is_some());

        // Verify the negative delta is correctly captured
        let prev = state.previous_api_snapshot.as_ref().unwrap();
        let curr = state.current_api_snapshot.as_ref().unwrap();

        assert_eq!(prev.five_hour_pct, 80.0);
        assert_eq!(curr.five_hour_pct, 5.0);  // Window reset: 80.0 -> 5.0

        assert_eq!(prev.seven_day_pct, 90.0);
        assert_eq!(curr.seven_day_pct, 15.0);  // Window reset: 90.0 -> 15.0

        assert_eq!(prev.weekly_scoped_pct, 85.0);
        assert_eq!(curr.weekly_scoped_pct, 8.0);  // Window reset: 85.0 -> 8.0
    }

    // --- Consecutive-absent poll tracking ---

    #[test]
    fn consecutive_absent_initializes_to_zero() {
        let state = GovernorState::new();

        // All counters should start at 0 (or be absent from the map)
        assert_eq!(state.get_consecutive_absent_count(WINDOW_FIVE_HOUR), 0);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_SEVEN_DAY), 0);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 0);
    }

    #[test]
    fn consecutive_absent_increments_on_absent_window() {
        let mut state = GovernorState::new();

        // Simulate three consecutive absent polls for weekly_scoped
        state.update_consecutive_absent_polls(true, true, false);  // weekly_scoped absent
        state.update_consecutive_absent_polls(true, true, false);  // weekly_scoped absent
        state.update_consecutive_absent_polls(true, true, false);  // weekly_scoped absent

        // weekly_scoped counter should be 3
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 3);

        // Other windows should remain at 0 (they were present)
        assert_eq!(state.get_consecutive_absent_count(WINDOW_FIVE_HOUR), 0);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_SEVEN_DAY), 0);
    }

    #[test]
    fn consecutive_absent_resets_on_present_window() {
        let mut state = GovernorState::new();

        // Simulate 3 consecutive absent polls for weekly_scoped
        state.update_consecutive_absent_polls(true, true, false);
        state.update_consecutive_absent_polls(true, true, false);
        state.update_consecutive_absent_polls(true, true, false);

        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 3);

        // Now simulate a present poll (window reappears)
        state.update_consecutive_absent_polls(true, true, true);  // weekly_scoped present

        // Counter should reset to 0
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 0);
    }

    #[test]
    fn consecutive_absent_multiple_windows_independent() {
        let mut state = GovernorState::new();

        // five_hour absent, seven_day present, weekly_scoped absent
        state.update_consecutive_absent_polls(false, true, false);
        state.update_consecutive_absent_polls(false, true, false);
        state.update_consecutive_absent_polls(false, true, false);

        assert_eq!(state.get_consecutive_absent_count(WINDOW_FIVE_HOUR), 3);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_SEVEN_DAY), 0);  // was present
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 3);

        // five_hour reappears, weekly_scoped stays absent
        state.update_consecutive_absent_polls(true, true, false);

        assert_eq!(state.get_consecutive_absent_count(WINDOW_FIVE_HOUR), 0);  // reset
        assert_eq!(state.get_consecutive_absent_count(WINDOW_SEVEN_DAY), 0);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 4);  // incremented
    }

    #[test]
    fn is_window_consecutively_absent_threshold() {
        let mut state = GovernorState::new();

        // Below threshold (2 < 3)
        state.update_consecutive_absent_polls(true, true, false);
        state.update_consecutive_absent_polls(true, true, false);
        assert!(!state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));

        // At threshold (3 == 3)
        state.update_consecutive_absent_polls(true, true, false);
        assert!(state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));

        // Above threshold (4 > 3)
        state.update_consecutive_absent_polls(true, true, false);
        assert!(state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));
    }

    #[test]
    fn is_window_consecutively_absent_unknown_window() {
        let state = GovernorState::new();

        // Unknown window should return false (not consecutively absent)
        assert!(!state.is_window_consecutively_absent("unknown_window"));
    }

    #[test]
    fn consecutive_absent_roundtrips_serialization() {
        let mut state = GovernorState::new();

        // Set some counters
        state.update_consecutive_absent_polls(false, true, false);
        state.update_consecutive_absent_polls(false, true, false);
        state.update_consecutive_absent_polls(false, true, false);

        // Serialize and deserialize
        let json = serde_json::to_string(&state).unwrap();
        let loaded: GovernorState = serde_json::from_str(&json).unwrap();

        // Verify counters survived the roundtrip
        assert_eq!(loaded.get_consecutive_absent_count(WINDOW_FIVE_HOUR), 3);
        assert_eq!(loaded.get_consecutive_absent_count(WINDOW_SEVEN_DAY), 0);
        assert_eq!(loaded.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 3);
    }

    #[test]
    fn consecutive_absent_persists_across_cycles() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("governor-state.json");

        // First cycle: start with 2 consecutive absents
        let mut state1 = GovernorState::new();
        state1.update_consecutive_absent_polls(true, true, false);
        state1.update_consecutive_absent_polls(true, true, false);
        save_state(&state1, &path).unwrap();

        // Second cycle: load and continue
        let mut state2 = load_state(&path).unwrap();
        assert_eq!(state2.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 2);

        // Add one more absent poll (should reach threshold)
        state2.update_consecutive_absent_polls(true, true, false);
        assert_eq!(state2.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 3);
        assert!(state2.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));

        // Save and load again
        save_state(&state2, &path).unwrap();
        let state3 = load_state(&path).unwrap();

        // Threshold status should persist
        assert!(state3.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));
        assert_eq!(state3.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 3);
    }

    #[test]
    fn consecutive_absent_default_null_tolerant() {
        // Simulate loading an older state file that doesn't have consecutive_absent_polls
        let old_json = r#"{
            "updated_at": "2026-03-18T14:30:00Z",
            "usage": {
                "sonnet_pct": 72.0,
                "all_models_pct": 81.0,
                "five_hour_pct": 14.0
            },
            "alerts": []
        }"#;

        let state: GovernorState = serde_json::from_str(old_json).unwrap();

        // Should deserialize successfully with default empty map
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 0);
        assert!(!state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));
    }

    #[test]
    fn consecutive_absent_realistic_scenario() {
        let mut state = GovernorState::new();

        // Scenario: weekly_scoped window is null for 3 polls, then reappears
        // This is the observed live failure mode mentioned in the docs

        // Poll 1: weekly_scoped absent (null from API)
        state.update_consecutive_absent_polls(true, true, false);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 1);
        assert!(!state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));

        // Poll 2: weekly_scoped still absent
        state.update_consecutive_absent_polls(true, true, false);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 2);
        assert!(!state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));

        // Poll 3: weekly_scoped still absent (now at threshold)
        state.update_consecutive_absent_polls(true, true, false);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 3);
        assert!(state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));

        // Poll 4: weekly_scoped reappears (API now returns data)
        state.update_consecutive_absent_polls(true, true, true);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 0);
        assert!(!state.is_window_consecutively_absent(WINDOW_WEEKLY_SCOPED));

        // Poll 5: weekly_scoped present again (counter stays at 0)
        state.update_consecutive_absent_polls(true, true, true);
        assert_eq!(state.get_consecutive_absent_count(WINDOW_WEEKLY_SCOPED), 0);
    }
}

#[cfg(test)]
mod null_roundtrip_test {
    use super::*;

    #[test]
    fn test_window_forecast_null_roundtrip() {
        let json = r#"{"target_ceiling":90.0,"current_utilization":12.0,"remaining_pct":78.0,"hours_remaining":7.2,"fleet_pct_per_hour":0.0,"predicted_exhaustion_hours":null,"cutoff_risk":false,"margin_hrs":null,"binding":true}"#;
        let wf: WindowForecast =
            serde_json::from_str(json).expect("should deserialize null as infinity");
        assert!(wf.predicted_exhaustion_hours.is_infinite());
        assert!(wf.margin_hrs.is_infinite() || wf.margin_hrs.is_sign_negative());
    }

    #[test]
    fn test_window_forecast_roundtrip_through_serialize() {
        // Create a forecast with infinity values (as produced when burn rate is 0)
        let wf = WindowForecast {
            fleet_pct_per_hour: 0.0,
            predicted_exhaustion_hours: f64::INFINITY,
            margin_hrs: f64::NEG_INFINITY,
            ..WindowForecast::default()
        };
        // Serialize (infinity → null)
        let json = serde_json::to_string(&wf).unwrap();
        assert!(json.contains("null"));
        // Deserialize back (null → infinity)
        let wf2: WindowForecast = serde_json::from_str(&json).unwrap();
        assert!(wf2.predicted_exhaustion_hours.is_infinite());
    }

    #[test]
    fn test_usage_state_weekly_scoped_model_null_roundtrip() {
        // Option<String> is inherently null-tolerant: null deserializes as None
        // without panicking, mirroring the custom null-as-infinity pattern used
        // for hard_limit_margin_hrs/cone_ratio/risk_score (which need a custom
        // deserializer only because they are f64, not Option).
        let null_json = r#"{"sonnet_pct": 72.0, "weekly_scoped_model": null}"#;
        let u: UsageState =
            serde_json::from_str(null_json).expect("null must deserialize as None");
        assert!(u.weekly_scoped_model.is_none());

        // Absent field (older state file) -> None via struct-level #[serde(default)].
        let absent_json = r#"{"sonnet_pct": 72.0}"#;
        let u2: UsageState =
            serde_json::from_str(absent_json).expect("absent field must default to None");
        assert!(u2.weekly_scoped_model.is_none());

        // Some round-trips through serialize/deserialize.
        let populated = UsageState {
            weekly_scoped_model: Some("Fable".to_string()),
            ..UsageState::default()
        };
        let s = serde_json::to_string(&populated).unwrap();
        assert!(s.contains("\"weekly_scoped_model\":\"Fable\""));
        let reloaded: UsageState = serde_json::from_str(&s).unwrap();
        assert_eq!(reloaded.weekly_scoped_model.as_deref(), Some("Fable"));
    }

    #[test]
    fn test_weekly_scoped_display_label() {
        // Resolved model name surfaces verbatim — this is the label every
        // human-facing surface (logs, summary, dashboard, decision reasons) uses
        // for the third window instead of the stale "7d-sonnet"/"sonnet".
        assert_eq!(weekly_scoped_display_label(Some("Fable")), "Fable");
        assert_eq!(weekly_scoped_display_label(Some("Opus")), "Opus");

        // None (no active model-scoped cap this period) -> generic fallback key.
        assert_eq!(weekly_scoped_display_label(None), "weekly_scoped");

        // Empty string (poller populated nothing meaningful) -> fallback, not "".
        assert_eq!(weekly_scoped_display_label(Some("")), "weekly_scoped");
    }
}
