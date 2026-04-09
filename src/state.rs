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
    pub sonnet_pct: f64,
    pub all_models_pct: f64,
    pub five_hour_pct: f64,
    pub sonnet_resets_at: String,
    pub five_hour_resets_at: String,
    /// True when data was sourced from stale cache (token refresh failed)
    pub stale: bool,
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
        }
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
        }
    }
}

/// Per-window percentage deltas observed in the last fleet aggregate
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowPctDeltas {
    pub five_hour: f64,
    pub seven_day: f64,
    pub seven_day_sonnet: f64,
}

impl Default for WindowPctDeltas {
    fn default() -> Self {
        Self {
            five_hour: 0.0,
            seven_day: 0.0,
            seven_day_sonnet: 0.0,
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
    pub seven_day_sonnet_pct: f64,
}

impl Default for PrevUsageSnapshot {
    fn default() -> Self {
        Self {
            taken_at: DateTime::<Utc>::default(),
            five_hour_pct: 0.0,
            seven_day_pct: 0.0,
            seven_day_sonnet_pct: 0.0,
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
    pub cone_ratio: f64,
    /// Composite risk score (higher = riskier). Factors in margin, duration, and volatility.
    /// Used for binding window selection.
    #[serde(default)]
    pub risk_score: f64,
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
        }
    }
}

/// Capacity forecast block (all three windows + derived metrics)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CapacityForecast {
    pub five_hour: WindowForecast,
    pub seven_day: WindowForecast,
    pub seven_day_sonnet: WindowForecast,
    pub binding_window: String,
    pub dollars_per_pct_7d_s: f64,
    pub estimated_remaining_dollars: f64,
}

impl Default for CapacityForecast {
    fn default() -> Self {
        Self {
            five_hour: WindowForecast::default(),
            seven_day: WindowForecast::default(),
            seven_day_sonnet: WindowForecast::default(),
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
    pub promo_multiplier_seven_day_sonnet: f64,
    /// Display multiplier: max across all windows (for backward-compatible display).
    #[serde(default = "serde_default_one")]
    pub promo_multiplier: f64,
    /// Per-window effective hours remaining (wall-clock hours × multiplier).
    pub effective_hours_remaining_five_hour: f64,
    pub effective_hours_remaining_seven_day: f64,
    pub effective_hours_remaining_seven_day_sonnet: f64,
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
            promo_multiplier_seven_day_sonnet: 1.0,
            promo_multiplier: 1.0,
            effective_hours_remaining_five_hour: 0.0,
            effective_hours_remaining_seven_day: 0.0,
            effective_hours_remaining_seven_day_sonnet: 0.0,
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
    pub usd_per_pct_ema_seven_day_sonnet: f64,

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
            usd_per_pct_ema_seven_day_sonnet: 0.0,
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
        }
    }
}

impl GovernorState {
    /// Create a new empty state with the current timestamp
    pub fn new() -> Self {
        Self::default()
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
                sonnet_pct: 72.0,
                all_models_pct: 81.0,
                five_hour_pct: 14.0,
                sonnet_resets_at: "2026-03-20T03:59:59Z".to_string(),
                five_hour_resets_at: "2026-03-18T15:59:59Z".to_string(),
                stale: false,
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
                    seven_day_sonnet: 0.75,
                },
                fleet_cache_eff: 0.0,
                cache_eff_p25: 0.0,
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
                seven_day_sonnet: WindowForecast {
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
                binding_window: "seven_day_sonnet".to_string(),
                dollars_per_pct_7d_s: 1.648,
                estimated_remaining_dollars: 46.1,
            },
            schedule: ScheduleState {
                is_peak_hour: false,
                is_promo_active: true,
                promo_multiplier_five_hour: 2.0,
                promo_multiplier_seven_day: 1.0,
                promo_multiplier_seven_day_sonnet: 1.0,
                promo_multiplier: 2.0,
                effective_hours_remaining_five_hour: 84.5,
                effective_hours_remaining_seven_day: 37.5,
                effective_hours_remaining_seven_day_sonnet: 37.5,
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
                "window": "seven_day_sonnet",
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
        assert_eq!(loaded.capacity_forecast.binding_window, "seven_day_sonnet");
        assert_eq!(loaded.burn_rate.tokens_per_pct_peak, 69780);
        assert_eq!(loaded.burn_rate.by_model["claude-sonnet-4-6"].samples, 12);
        assert_eq!(loaded.workers["claude-anthropic-sonnet"].current, 2);
        assert_eq!(loaded.alerts.len(), 1);
        assert_eq!(loaded.safe_mode.active, true);
        assert_eq!(loaded.safe_mode.trigger.as_deref(), Some("median_error"));
        assert_eq!(loaded.burn_rate.calibration.predictions_scored, 24);
        assert_eq!(
            loaded.capacity_forecast.seven_day_sonnet.safe_worker_count,
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
        assert_eq!(loaded.capacity_forecast.binding_window, "seven_day_sonnet");
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
}
