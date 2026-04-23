//! Adaptive Burn Rate Estimator
//!
//! Empirically calibrates per-model burn rates (%/hr, tokens/hr, $/hr) using
//! observed consumption data from the token collector.
//!
//! ## Capabilities
//!
//! - Per-instance dollar/pct burn computation with guard conditions
//! - Fleet-level per-worker stats (mean, p75, stddev) across active sessions
//! - Per-(model, window) EMA with configurable alpha (default 0.2)
//! - Capacity forecast per window: fleet_pct_per_hour, predicted_exhaustion_hours,
//!   will_exhaust_before_reset, safe_worker_count
//! - Binding window identification (soonest to exhaust)
//! - Separate peak/off-peak tokens_per_pct for promotion validation
//! - Baseline fallback until 3 valid samples per window
//!
//! ## Data Flow
//!
//! 1. Read instance (i) records from token-history.db for the most recent interval
//! 2. Compute per-instance burn rates with guard conditions
//! 3. Aggregate to fleet-level stats
//! 4. Update per-(model, window) EMA estimates
//! 5. Generate capacity forecast for each window
//! 6. Identify binding window and safe worker count
//! 7. Update GovernorState with burn_rate, last_fleet_aggregate, capacity_forecast

use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Per-window utilization data for burn rate computation
#[derive(Debug, Clone, PartialEq)]
pub struct WindowUtilization {
    /// Window name: "five_hour", "seven_day", "seven_day_sonnet"
    pub window: String,

    /// Pct change this interval (None = not yet annotated by governor)
    pub pct_delta: Option<f64>,

    /// Current utilization snapshot %
    pub current_utilization: f64,

    /// Previous interval utilization snapshot %
    pub previous_utilization: f64,
}

impl WindowUtilization {
    /// Create a WindowUtilization from percentage delta and current/previous values
    pub fn from_pct_delta(
        window: &str,
        pct_delta: Option<f64>,
        current: f64,
        previous: f64,
    ) -> Self {
        Self {
            window: window.to_string(),
            pct_delta,
            current_utilization: current,
            previous_utilization: previous,
        }
    }
}

/// Convert a database instance record to a burn_rate InstanceRecord
///
/// Takes the flat db record with window percentage deltas and constructs
/// the nested WindowUtilization structure used by the burn rate estimator.
impl From<crate::db::DbInstanceRecord> for InstanceRecord {
    fn from(db_rec: crate::db::DbInstanceRecord) -> Self {
        let windows = vec![
            WindowUtilization::from_pct_delta(
                "five_hour",
                db_rec.p5h,
                db_rec.current_p5h,
                db_rec.prev_p5h,
            ),
            WindowUtilization::from_pct_delta(
                "seven_day",
                db_rec.p7d,
                db_rec.current_p7d,
                db_rec.prev_p7d,
            ),
            WindowUtilization::from_pct_delta(
                "seven_day_sonnet",
                db_rec.p7ds,
                db_rec.current_p7ds,
                db_rec.prev_p7ds,
            ),
        ];

        InstanceRecord {
            session: db_rec.session,
            model: db_rec.model,
            total_usd: db_rec.total_usd,
            total_tokens: db_rec.total_tokens,
            windows,
        }
    }
}

/// Instance interval record (mirrors the `i` table in token-history.db)
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceRecord {
    /// Session identifier
    pub session: String,

    /// Model identifier
    pub model: String,

    /// Total USD cost for this interval
    pub total_usd: f64,

    /// Total tokens consumed this interval
    pub total_tokens: u64,

    /// Per-window utilization data
    pub windows: Vec<WindowUtilization>,
}

/// Computed burn rate for one instance in one window
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceBurnRate {
    /// Session identifier
    pub session: String,

    /// Model identifier
    pub model: String,

    /// Window name
    pub window: String,

    /// Dollar burn rate (USD per hour)
    pub dollar_per_hour: f64,

    /// Percentage burn rate (pct points per hour)
    pub pct_per_hour: f64,

    /// Elapsed hours for this interval
    pub elapsed_hours: f64,
}

/// Minimum elapsed time to compute burn rates (2 minutes)
const MIN_ELAPSED_MINUTES: f64 = 2.0;

/// Utilization drop threshold for window reset detection (1 percentage point)
const WINDOW_RESET_THRESHOLD: f64 = 1.0;

/// Minimum samples required before EMA is considered reliable
#[allow(dead_code)]
const MIN_SAMPLES_FOR_EMA: u32 = 3;

/// Compute per-instance per-window burn rates from an interval record
///
/// Returns a burn rate entry for each window that passes all guard conditions:
/// - Elapsed time >= 2 minutes
/// - Window pct_delta is not null
/// - Window pct_delta is not zero when tokens > 0 (API rounding artifact)
/// - No window reset detected (utilization drop > 1pp)
pub fn compute_instance_burn(record: &InstanceRecord, elapsed_hours: f64) -> Vec<InstanceBurnRate> {
    let mut results = Vec::new();

    // Guard: skip if elapsed < 2 minutes
    if elapsed_hours < MIN_ELAPSED_MINUTES / 60.0 {
        return results;
    }

    for win in &record.windows {
        // Guard: skip if pct_delta is null (not yet annotated)
        let pct_delta = match win.pct_delta {
            Some(d) => d,
            None => continue,
        };

        // Guard: skip if pct_delta is 0 but tokens > 0 (API rounding artifact)
        if pct_delta == 0.0 && record.total_tokens > 0 {
            continue;
        }

        // Window reset detection: if current_utilization < previous_utilization - 1.0
        if win.current_utilization < win.previous_utilization - WINDOW_RESET_THRESHOLD {
            continue;
        }

        let pct_per_hour = pct_delta / elapsed_hours;
        let dollar_per_hour = record.total_usd / elapsed_hours;

        results.push(InstanceBurnRate {
            session: record.session.clone(),
            model: record.model.clone(),
            window: win.window.clone(),
            dollar_per_hour,
            pct_per_hour,
            elapsed_hours,
        });
    }

    results
}

// ---------------------------------------------------------------------------
// Promotion validation
// ---------------------------------------------------------------------------

/// Minimum samples required in each category (peak/off-peak) for validation
pub const MIN_VALIDATION_SAMPLES: usize = 5;

/// Tolerance for ratio validation (10%)
const VALIDATION_TOLERANCE: f64 = 0.10;

/// Threshold below which ratio indicates promotion may not be applying
const PROMO_NOT_APPLYING_THRESHOLD: f64 = 1.2;

/// Threshold above which ratio indicates anomaly
const ANOMALY_THRESHOLD: f64 = 2.5;

/// A single consumption sample for promotion validation
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionSample {
    /// Tokens consumed per percentage point of utilization
    pub tokens_per_pct: f64,

    /// Whether this sample was during peak hours
    pub is_peak: bool,

    /// Number of workers running when this sample was taken
    pub worker_count: u32,

    /// When this sample was taken
    pub timestamp: DateTime<Utc>,
}

/// Result of promotion validation
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionValidationResult {
    /// Whether the promotion multiplier is validated
    pub validated: bool,

    /// Observed ratio (median off-peak / median peak)
    pub observed_ratio: f64,

    /// Declared multiplier from promotions.json
    pub declared_multiplier: f64,

    /// Median tokens_per_pct for peak samples
    pub median_peak: f64,

    /// Median tokens_per_pct for off-peak samples
    pub median_offpeak: f64,

    /// Number of peak samples used
    pub peak_samples: usize,

    /// Number of off-peak samples used
    pub offpeak_samples: usize,

    /// Reason for validation failure (None if validated)
    pub reason: Option<String>,
}

/// Compute the median of a f64 slice
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let len = sorted.len();
    if len.is_multiple_of(2) {
        (sorted[len / 2 - 1] + sorted[len / 2]) / 2.0
    } else {
        sorted[len / 2]
    }
}

/// Validate a declared promotion multiplier against measured consumption data
///
/// Groups samples by worker count, selects the group with the most samples,
/// and computes the observed ratio of median off-peak tokens_per_pct to
/// median peak tokens_per_pct.
///
/// Validation rules (from plan):
/// - Within 10% of declared multiplier: **validated**, use declared multiplier.
/// - Observed ratio < 1.2: promotion may not be applying — fall back to 1x.
/// - Observed ratio > 2.5: anomaly — use observed ratio instead of declared.
/// - Otherwise: not validated, reason provided.
pub fn validate_promotion(
    samples: &[PromotionSample],
    declared_multiplier: f64,
) -> PromotionValidationResult {
    // No promotion to validate
    if declared_multiplier <= 1.0 {
        return PromotionValidationResult {
            validated: true,
            observed_ratio: 1.0,
            declared_multiplier,
            median_peak: 0.0,
            median_offpeak: 0.0,
            peak_samples: 0,
            offpeak_samples: 0,
            reason: None,
        };
    }

    // Group samples by worker count
    let mut by_worker: HashMap<u32, Vec<&PromotionSample>> = HashMap::new();
    for sample in samples {
        by_worker
            .entry(sample.worker_count)
            .or_default()
            .push(sample);
    }

    // Select the worker count with the most samples
    let best_count = match by_worker
        .iter()
        .max_by_key(|(_, v)| v.len())
        .map(|(&c, _)| c)
    {
        Some(c) => c,
        None => {
            return PromotionValidationResult {
                validated: false,
                observed_ratio: 0.0,
                declared_multiplier,
                median_peak: 0.0,
                median_offpeak: 0.0,
                peak_samples: 0,
                offpeak_samples: 0,
                reason: Some("no samples".to_string()),
            }
        }
    };

    let group = &by_worker[&best_count];

    let peak_values: Vec<f64> = group
        .iter()
        .filter(|s| s.is_peak)
        .map(|s| s.tokens_per_pct)
        .collect();

    let offpeak_values: Vec<f64> = group
        .iter()
        .filter(|s| !s.is_peak)
        .map(|s| s.tokens_per_pct)
        .collect();

    // Check minimum sample counts
    if peak_values.len() < MIN_VALIDATION_SAMPLES || offpeak_values.len() < MIN_VALIDATION_SAMPLES {
        return PromotionValidationResult {
            validated: false,
            observed_ratio: 0.0,
            declared_multiplier,
            median_peak: 0.0,
            median_offpeak: 0.0,
            peak_samples: peak_values.len(),
            offpeak_samples: offpeak_values.len(),
            reason: Some(format!(
                "insufficient samples: {} peak, {} off-peak (need {} each)",
                peak_values.len(),
                offpeak_values.len(),
                MIN_VALIDATION_SAMPLES
            )),
        };
    }

    let median_peak = median(&peak_values);
    let median_offpeak = median(&offpeak_values);

    // Guard against zero median peak (would cause division by zero)
    if median_peak <= 0.0 {
        return PromotionValidationResult {
            validated: false,
            observed_ratio: 0.0,
            declared_multiplier,
            median_peak,
            median_offpeak,
            peak_samples: peak_values.len(),
            offpeak_samples: offpeak_values.len(),
            reason: Some("median peak tokens_per_pct is zero".to_string()),
        };
    }

    let observed_ratio = median_offpeak / median_peak;

    let lower_bound = declared_multiplier * (1.0 - VALIDATION_TOLERANCE);
    let upper_bound = declared_multiplier * (1.0 + VALIDATION_TOLERANCE);

    if observed_ratio >= lower_bound && observed_ratio <= upper_bound {
        // Within 10% of declared: validated
        PromotionValidationResult {
            validated: true,
            observed_ratio,
            declared_multiplier,
            median_peak,
            median_offpeak,
            peak_samples: peak_values.len(),
            offpeak_samples: offpeak_values.len(),
            reason: None,
        }
    } else if observed_ratio < PROMO_NOT_APPLYING_THRESHOLD {
        // Ratio too low: promotion may not be applying
        PromotionValidationResult {
            validated: false,
            observed_ratio,
            declared_multiplier,
            median_peak,
            median_offpeak,
            peak_samples: peak_values.len(),
            offpeak_samples: offpeak_values.len(),
            reason: Some(format!(
                "observed ratio {:.2} < {:.2}: promotion may not be applying",
                observed_ratio, PROMO_NOT_APPLYING_THRESHOLD
            )),
        }
    } else if observed_ratio > ANOMALY_THRESHOLD {
        // Ratio too high: anomaly
        PromotionValidationResult {
            validated: false,
            observed_ratio,
            declared_multiplier,
            median_peak,
            median_offpeak,
            peak_samples: peak_values.len(),
            offpeak_samples: offpeak_values.len(),
            reason: Some(format!(
                "observed ratio {:.2} > {:.2}: anomaly, use observed ratio",
                observed_ratio, ANOMALY_THRESHOLD
            )),
        }
    } else {
        // Outside tolerance but not in anomaly/not-applying range
        PromotionValidationResult {
            validated: false,
            observed_ratio,
            declared_multiplier,
            median_peak,
            median_offpeak,
            peak_samples: peak_values.len(),
            offpeak_samples: offpeak_values.len(),
            reason: Some(format!(
                "observed ratio {:.2} outside tolerance [{:.2}, {:.2}]",
                observed_ratio, lower_bound, upper_bound
            )),
        }
    }
}

/// Determine the effective multiplier to use based on validation state
///
/// - If validated: use the declared multiplier
/// - If not validated with anomaly (> 2.5): use observed ratio
/// - Otherwise: fall back to 1.0 (conservative)
pub fn effective_multiplier(result: &PromotionValidationResult) -> f64 {
    if result.validated {
        return result.declared_multiplier;
    }

    // Anomaly: use observed ratio
    if result.observed_ratio > ANOMALY_THRESHOLD {
        return result.observed_ratio;
    }

    // Not validated: conservative fallback to 1x
    1.0
}

/// Result of empirical promo ratio computation from token-history DB
#[derive(Debug, Clone, PartialEq)]
pub struct EmpiricalPromoRatio {
    /// Observed ratio (median off-peak / median peak)
    pub observed_ratio: f64,

    /// Median tokens_per_pct for peak samples
    pub median_peak: f64,

    /// Median tokens_per_pct for off-peak samples
    pub median_offpeak: f64,

    /// Number of peak samples used
    pub peak_samples: usize,

    /// Number of off-peak samples used
    pub offpeak_samples: usize,

    /// Whether there's sufficient data for validation (>= 10 samples each)
    pub sufficient_data: bool,
}

/// Compute empirical promo ratio from token-history DB
///
/// Reads the last N instance records from the database, groups them by
/// peak/off-peak periods, and computes the median tokens-per-percent for
/// each period type. Returns the observed ratio.
///
/// Returns None if the database cannot be read or if no samples are found.
pub fn compute_empirical_promo_ratio(db_path: &std::path::Path) -> Option<EmpiricalPromoRatio> {
    use crate::db::open_db;

    let conn = open_db(db_path).ok()?;

    // Query the last 500 instance records with p7ds data
    let mut stmt = conn
        .prepare(
            "SELECT pk, p7ds, input_n + output_n + r_cache_n + w_cache_n + w_cache_1h_n AS total_tokens
             FROM i
             WHERE p7ds IS NOT NULL AND p7ds > 0
             ORDER BY t1 DESC
             LIMIT 500",
        )
        .ok()?;

    let mut peak_tokens_per_pct: Vec<f64> = Vec::new();
    let mut offpeak_tokens_per_pct: Vec<f64> = Vec::new();

    let rows = stmt
        .query_map([], |row| {
            let pk: i64 = row.get(0)?;
            let p7ds: f64 = row.get(1)?;
            let total_tokens: i64 = row.get(2)?;
            Ok((pk != 0, p7ds, total_tokens as f64))
        })
        .ok()?;

    for row in rows {
        let (is_peak, p7ds, total_tokens) = row.ok()?;
        let tokens_per_pct = total_tokens / p7ds;

        if is_peak {
            peak_tokens_per_pct.push(tokens_per_pct);
        } else {
            offpeak_tokens_per_pct.push(tokens_per_pct);
        }
    }

    if peak_tokens_per_pct.is_empty() || offpeak_tokens_per_pct.is_empty() {
        return None;
    }

    let median_peak = median(&peak_tokens_per_pct);
    let median_offpeak = median(&offpeak_tokens_per_pct);

    let observed_ratio = if median_peak > 0.0 {
        median_offpeak / median_peak
    } else {
        0.0
    };

    let peak_samples = peak_tokens_per_pct.len();
    let offpeak_samples = offpeak_tokens_per_pct.len();
    let sufficient_data = peak_samples >= 10 && offpeak_samples >= 10;

    Some(EmpiricalPromoRatio {
        observed_ratio,
        median_peak,
        median_offpeak,
        peak_samples,
        offpeak_samples,
        sufficient_data,
    })
}

/// Validate promotion using empirical data from token-history DB
///
/// This is a convenience function that combines `compute_empirical_promo_ratio`
/// with validation logic. Returns a PromotionValidationResult.
pub fn validate_promotion_from_db(
    db_path: &std::path::Path,
    declared_multiplier: f64,
) -> PromotionValidationResult {
    let empirical = match compute_empirical_promo_ratio(db_path) {
        Some(e) => e,
        None => {
            return PromotionValidationResult {
                validated: false,
                observed_ratio: 0.0,
                declared_multiplier,
                median_peak: 0.0,
                median_offpeak: 0.0,
                peak_samples: 0,
                offpeak_samples: 0,
                reason: Some("no data found in token-history DB".to_string()),
            }
        }
    };

    // Check minimum sample counts (10 each for empirical validation)
    if empirical.peak_samples < 10 || empirical.offpeak_samples < 10 {
        return PromotionValidationResult {
            validated: false,
            observed_ratio: empirical.observed_ratio,
            declared_multiplier,
            median_peak: empirical.median_peak,
            median_offpeak: empirical.median_offpeak,
            peak_samples: empirical.peak_samples,
            offpeak_samples: empirical.offpeak_samples,
            reason: Some(format!(
                "insufficient samples: {} peak, {} off-peak (need 10 each)",
                empirical.peak_samples, empirical.offpeak_samples
            )),
        };
    }

    // Guard against zero median peak
    if empirical.median_peak <= 0.0 {
        return PromotionValidationResult {
            validated: false,
            observed_ratio: empirical.observed_ratio,
            declared_multiplier,
            median_peak: empirical.median_peak,
            median_offpeak: empirical.median_offpeak,
            peak_samples: empirical.peak_samples,
            offpeak_samples: empirical.offpeak_samples,
            reason: Some("median peak tokens_per_pct is zero".to_string()),
        };
    }

    let lower_bound = declared_multiplier * (1.0 - VALIDATION_TOLERANCE);
    let upper_bound = declared_multiplier * (1.0 + VALIDATION_TOLERANCE);

    if empirical.observed_ratio >= lower_bound && empirical.observed_ratio <= upper_bound {
        // Within 10% of declared: validated
        PromotionValidationResult {
            validated: true,
            observed_ratio: empirical.observed_ratio,
            declared_multiplier,
            median_peak: empirical.median_peak,
            median_offpeak: empirical.median_offpeak,
            peak_samples: empirical.peak_samples,
            offpeak_samples: empirical.offpeak_samples,
            reason: None,
        }
    } else if empirical.observed_ratio < PROMO_NOT_APPLYING_THRESHOLD {
        // Ratio too low: promotion may not be applying
        PromotionValidationResult {
            validated: false,
            observed_ratio: empirical.observed_ratio,
            declared_multiplier,
            median_peak: empirical.median_peak,
            median_offpeak: empirical.median_offpeak,
            peak_samples: empirical.peak_samples,
            offpeak_samples: empirical.offpeak_samples,
            reason: Some(format!(
                "observed ratio {:.2} < {:.2}: promotion may not be applying",
                empirical.observed_ratio, PROMO_NOT_APPLYING_THRESHOLD
            )),
        }
    } else if empirical.observed_ratio > ANOMALY_THRESHOLD {
        // Ratio too high: anomaly
        PromotionValidationResult {
            validated: false,
            observed_ratio: empirical.observed_ratio,
            declared_multiplier,
            median_peak: empirical.median_peak,
            median_offpeak: empirical.median_offpeak,
            peak_samples: empirical.peak_samples,
            offpeak_samples: empirical.offpeak_samples,
            reason: Some(format!(
                "observed ratio {:.2} > {:.2}: anomaly, use observed ratio",
                empirical.observed_ratio, ANOMALY_THRESHOLD
            )),
        }
    } else {
        // Outside tolerance but not in anomaly/not-applying range
        PromotionValidationResult {
            validated: false,
            observed_ratio: empirical.observed_ratio,
            declared_multiplier,
            median_peak: empirical.median_peak,
            median_offpeak: empirical.median_offpeak,
            peak_samples: empirical.peak_samples,
            offpeak_samples: empirical.offpeak_samples,
            reason: Some(format!(
                "observed ratio {:.2} outside tolerance [{:.2}, {:.2}]",
                empirical.observed_ratio, lower_bound, upper_bound
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-Window Composite Risk Optimization
// ---------------------------------------------------------------------------

/// Compute the cost/urgency of a single window.
///
/// The cost function measures how "urgent" a window is based on its margin
/// relative to time remaining. Windows that are closer to exhaustion (lower
/// margin) have higher cost.
///
/// Formula: `cost = margin_hrs / hours_remaining`
///
/// - Positive margin (safe): cost > 0, lower is safer
/// - Zero margin (at limit): cost = 0
/// - Negative margin (will exhaust): cost < 0, more negative = more urgent
///
/// Returns `None` if hours_remaining is 0 (cannot compute cost).
pub fn window_cost(margin_hrs: f64, hours_remaining: f64) -> Option<f64> {
    if hours_remaining <= 0.0 {
        return None;
    }
    Some(margin_hrs / hours_remaining)
}

/// Compute the composite risk score across all windows.
///
/// The composite risk is a weighted average of all window costs, with the
/// binding window receiving extra weight to ensure it remains the primary
/// constraint.
///
/// Formula: `composite_risk = sum(cost_i * weight_i) / sum(weight_i)`
///
/// Where:
/// - `cost_i` = window cost for window i
/// - `weight_i` = `binding_weight` if window i is the binding window, else 1.0
///
/// Returns `None` if no windows have valid costs.
pub fn composite_risk(
    forecasts: &[crate::state::WindowForecast],
    binding_idx: usize,
    binding_weight: f64,
) -> Option<f64> {
    if forecasts.is_empty() || binding_idx >= forecasts.len() {
        return None;
    }

    let mut weighted_sum = 0.0;
    let mut total_weight = 0.0;
    let mut has_valid = false;

    for (i, forecast) in forecasts.iter().enumerate() {
        let cost = window_cost(forecast.margin_hrs, forecast.hours_remaining)?;
        let weight = if i == binding_idx {
            binding_weight
        } else {
            1.0
        };
        weighted_sum += cost * weight;
        total_weight += weight;
        has_valid = true;
    }

    if !has_valid || total_weight == 0.0 {
        return None;
    }

    Some(weighted_sum / total_weight)
}

/// Compute the optimal worker count using composite risk optimization.
///
/// When composite risk optimization is enabled, this function allows scaling
/// higher than the binding window's safe_worker_count by considering the
/// capacity available in other windows.
///
/// The key insight: when the binding window is near reset, its constraint
/// is temporary. Non-binding windows (e.g., 7-day) may have ample capacity
/// that can absorb additional workers for the binding window's remaining time.
///
/// Algorithm:
/// 1. Find binding window's safe_worker_count (baseline)
/// 2. For each non-binding window with cost above threshold:
///    - Compute how many workers it can support for the binding window's
///      remaining time (not its own hours_remaining, which would be too
///      conservative for long windows like 7-day)
///    - Take the maximum across all such windows
/// 3. Return the maximum if it exceeds the binding baseline, else None
///
/// Returns `None` if no improvement over binding window is possible.
pub fn compute_composite_safe_workers(
    forecasts: &[crate::state::WindowForecast],
    binding_idx: usize,
    _binding_weight: f64,
    cost_threshold: f64,
    current_workers: u32,
) -> Option<u32> {
    if forecasts.is_empty() || binding_idx >= forecasts.len() || current_workers == 0 {
        return None;
    }

    let binding = &forecasts[binding_idx];
    let binding_hours = binding.hours_remaining;

    if binding_hours <= 0.0 {
        return None;
    }

    let binding_safe = binding.safe_worker_count.unwrap_or(0);
    let mut max_safe = binding_safe;

    for (i, forecast) in forecasts.iter().enumerate() {
        if i == binding_idx {
            continue;
        }

        // Only consider non-binding windows with cost above threshold
        if !window_cost(forecast.margin_hrs, forecast.hours_remaining)
            .is_some_and(|c| c > cost_threshold)
        {
            continue;
        }

        // Compute per-worker burn rate for this window from fleet aggregate
        let pct_per_worker = if forecast.fleet_pct_per_hour > 0.0 {
            forecast.fleet_pct_per_hour / current_workers as f64
        } else {
            continue;
        };

        // Safe workers = how many workers can run for binding_hours
        // without exhausting this non-binding window.
        // Using binding_hours (not the window's own hours_remaining) because
        // we only need to survive until the binding window resets.
        let safe = (forecast.remaining_pct / (pct_per_worker * binding_hours)).floor() as u32;
        max_safe = max_safe.max(safe);
    }

    // Only return composite result if it improves over binding
    if max_safe > binding_safe {
        Some(max_safe)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Adaptive Burn Rate Estimator
// ---------------------------------------------------------------------------

/// EMA smoothing factor (alpha = 0.2 per plan)
const EMA_ALPHA: f64 = 0.2;

/// Known window names
const WINDOWS: &[&str] = &["five_hour", "seven_day", "seven_day_sonnet"];

/// Fleet-level per-worker statistics for one window
#[derive(Debug, Clone)]
pub struct FleetWorkerStats {
    /// Number of active workers in this sample
    pub worker_count: u32,

    /// Mean pct_per_hour across workers
    pub mean_pct_hr: f64,

    /// 75th-percentile pct_per_hour (nearest-rank)
    pub p75_pct_hr: f64,

    /// Population standard deviation of pct_per_hour
    pub std_pct_hr: f64,

    /// Mean dollar_per_hour across workers
    pub mean_usd_hr: f64,
}

/// Per-(model, window) EMA state
#[derive(Debug, Clone)]
pub struct ModelWindowEma {
    /// Current EMA value for pct_per_worker_per_hour
    pub ema_pct: f64,

    /// Current EMA value for dollars_per_worker_per_hour
    pub ema_usd: f64,

    /// Number of valid samples accumulated
    pub samples: u32,
}

impl Default for ModelWindowEma {
    fn default() -> Self {
        Self {
            ema_pct: 0.0,
            ema_usd: 0.0,
            samples: 0,
        }
    }
}

/// Baseline burn rates from configuration (fallback before EMA is ready)
#[derive(Debug, Clone)]
pub struct BaselineBurnRates {
    /// Default pct per worker per hour when no EMA is available
    pub pct_per_worker_per_hour: f64,

    /// Default dollars per worker per hour when no EMA is available
    pub dollars_per_worker_per_hour: f64,
}

impl Default for BaselineBurnRates {
    fn default() -> Self {
        // Conservative defaults: ~1.5%/hr per worker, ~$5/hr per worker
        Self {
            pct_per_worker_per_hour: 1.5,
            dollars_per_worker_per_hour: 5.0,
        }
    }
}

/// Result of the adaptive burn rate estimation pass
#[derive(Debug, Clone)]
pub struct BurnRateEstimate {
    /// Per-model EMA state keyed by (model, window)
    pub ema_state: HashMap<(String, String), ModelWindowEma>,

    /// Fleet worker stats per window
    pub fleet_stats: HashMap<String, FleetWorkerStats>,

    /// Whether any window had valid data this cycle
    pub had_valid_data: bool,
}

/// Compute fleet-level per-worker statistics for a single window
///
/// Takes per-instance burn rates for one window and computes aggregate stats.
fn compute_fleet_stats(
    _window: &str,
    instance_rates: &[&InstanceBurnRate],
    total_workers: u32,
) -> FleetWorkerStats {
    if instance_rates.is_empty() || total_workers == 0 {
        return FleetWorkerStats {
            worker_count: total_workers,
            mean_pct_hr: 0.0,
            p75_pct_hr: 0.0,
            std_pct_hr: 0.0,
            mean_usd_hr: 0.0,
        };
    }

    let n = instance_rates.len();
    let sum_pct: f64 = instance_rates.iter().map(|r| r.pct_per_hour).sum();
    let sum_usd: f64 = instance_rates.iter().map(|r| r.dollar_per_hour).sum();
    let mean_pct = sum_pct / n as f64;
    let mean_usd = sum_usd / n as f64;

    // Population standard deviation for pct_per_hour
    let variance: f64 = instance_rates
        .iter()
        .map(|r| (r.pct_per_hour - mean_pct).powi(2))
        .sum::<f64>()
        / n as f64;
    let std_pct = variance.sqrt();

    // Nearest-rank p75
    let mut sorted_pct: Vec<f64> = instance_rates.iter().map(|r| r.pct_per_hour).collect();
    sorted_pct.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p75_idx = ((n as f64 * 0.75).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1);
    let p75_pct = sorted_pct[p75_idx];

    FleetWorkerStats {
        worker_count: total_workers,
        mean_pct_hr: mean_pct,
        p75_pct_hr: p75_pct,
        std_pct_hr: std_pct,
        mean_usd_hr: mean_usd,
    }
}

/// Update EMA state with a new sample
///
/// EMA formula: `new_ema = alpha * new_value + (1 - alpha) * old_ema`
/// On the first sample (samples == 0), the EMA is initialized directly.
fn update_ema(ema: &mut ModelWindowEma, pct_hr: f64, usd_hr: f64) {
    if ema.samples == 0 {
        ema.ema_pct = pct_hr;
        ema.ema_usd = usd_hr;
    } else {
        ema.ema_pct = EMA_ALPHA * pct_hr + (1.0 - EMA_ALPHA) * ema.ema_pct;
        ema.ema_usd = EMA_ALPHA * usd_hr + (1.0 - EMA_ALPHA) * ema.ema_usd;
    }
    ema.samples += 1;
}

/// Get the effective burn rate for a (model, window) pair
///
/// Returns the EMA value if enough samples have been accumulated,
/// otherwise falls back to the baseline.
fn effective_burn_rate(ema: &ModelWindowEma, baseline: &BaselineBurnRates) -> (f64, f64) {
    if ema.samples >= MIN_SAMPLES_FOR_EMA {
        (ema.ema_pct, ema.ema_usd)
    } else {
        (
            baseline.pct_per_worker_per_hour,
            baseline.dollars_per_worker_per_hour,
        )
    }
}

// ---------------------------------------------------------------------------
// Per-Window Risk Score Computation
// ---------------------------------------------------------------------------

/// Duration weight for each window type.
///
/// Shorter windows are higher risk because they reset more frequently,
/// meaning we have less time to recover from exhaustion.
fn duration_weight(window: &str) -> f64 {
    match window {
        "five_hour" => 3.0,        // 5h window: highest urgency (resets every 5 hours)
        "seven_day_sonnet" => 1.5, // 7d sonnet: medium urgency
        "seven_day" => 1.0,        // 7d: lowest urgency (resets every 7 days)
        _ => 1.0,
    }
}

/// Compute the composite risk score for a single window.
///
/// The risk score combines three factors:
/// 1. **Margin urgency**: How close are we to exhaustion? (1.0 - margin_pct)
/// 2. **Duration weight**: Shorter windows are higher risk (5h > 7d-sonnet > 7d)
/// 3. **Volatility factor**: Wider confidence cone = higher uncertainty = higher risk
///
/// Formula:
/// ```text
/// risk_score = (1.0 - margin_pct) * duration_weight * volatility_factor
/// ```
///
/// Where:
/// - `margin_pct` = margin_hrs / hours_remaining (fraction of time remaining)
/// - `duration_weight` = 3.0 for 5h, 1.5 for 7d-sonnet, 1.0 for 7d
/// - `volatility_factor` = cone_ratio (wider cone = higher risk)
///
/// A higher risk_score means the window is more urgent and should be prioritized
/// as the binding window for scaling decisions.
///
/// Returns `None` if hours_remaining is 0 (cannot compute margin percentage).
pub fn compute_risk_score(
    window: &str,
    margin_hrs: f64,
    hours_remaining: f64,
    cone_ratio: f64,
) -> Option<f64> {
    if hours_remaining <= 0.0 {
        return None;
    }

    // Margin as a percentage of time remaining
    // Positive margin: safe (margin_pct > 0)
    // Zero margin: at limit (margin_pct = 0)
    // Negative margin: will exhaust (margin_pct < 0)
    let margin_pct = margin_hrs / hours_remaining;

    // (1.0 - margin_pct) gives us urgency:
    // - margin_pct = 1.0 (100% headroom) → urgency = 0.0 (no risk)
    // - margin_pct = 0.0 (at limit) → urgency = 1.0 (high risk)
    // - margin_pct < 0.0 (will exhaust) → urgency > 1.0 (very high risk)
    let urgency = 1.0 - margin_pct;

    // Duration weight: shorter windows = higher urgency
    let weight = duration_weight(window);

    // Volatility factor: wider cone = higher uncertainty = higher risk
    // cone_ratio = exh_hrs_p75 / exh_hrs_p25 (1.0 = no spread, higher = wider)
    let volatility = cone_ratio.max(1.0);

    Some(urgency * weight * volatility)
}

/// Generate a capacity forecast for a single window
///
/// Computes fleet_pct_per_hour, predicted_exhaustion_hours,
/// will_exhaust_before_reset, safe_worker_count, the confidence cone
/// (exh_hrs_p25/p50/p75, cone_ratio) using per-worker burn rate stddev,
/// and the composite risk_score for binding window selection.
///
/// The cone is derived from the Normal distribution assumption:
///   rate_p25 = fleet_pct_hr - 0.675 * std_pct_hr  (slow burn → more hours → p75 of hours)
///   rate_p75 = fleet_pct_hr + 0.675 * std_pct_hr  (fast burn → fewer hours → p25 of hours)
///
/// When std_pct_hr is zero (no spread data), p25/p50/p75 all equal the p50 estimate.
pub fn generate_window_forecast(
    window: &str,
    fleet_pct_hr: f64,
    current_utilization: f64,
    target_ceiling: f64,
    hours_remaining: f64,
    mean_rate_per_worker: f64,
    std_pct_hr: f64,
) -> crate::state::WindowForecast {
    let remaining_pct = (target_ceiling - current_utilization).max(0.0);

    let predicted_exhaustion_hours = if fleet_pct_hr > 0.0 {
        remaining_pct / fleet_pct_hr
    } else {
        f64::INFINITY
    };

    let cutoff_risk = predicted_exhaustion_hours < hours_remaining;
    // margin_hrs: positive = safe (exhaustion after reset), negative = risky (exhaustion before reset)
    let margin_hrs = predicted_exhaustion_hours - hours_remaining;

    // p50 safe workers: uses the mean per-worker burn rate.
    let safe_worker_count = if mean_rate_per_worker > 0.0 && hours_remaining > 0.0 {
        let safe = (remaining_pct / (mean_rate_per_worker * hours_remaining)).floor() as u64;
        Some(safe.min(u32::MAX as u64) as u32)
    } else {
        None
    };

    const Z_0_675: f64 = 0.675;
    const MIN_RATE: f64 = 1e-9;

    // p75 safe workers: uses the p75 (fast-burn) per-worker rate — more conservative.
    // Derived by scaling the mean rate by the ratio of the fleet's p75 burn rate to p50.
    // When std_pct_hr == 0, p75 rate == p50 rate and safe_worker_count_p75 == safe_worker_count.
    let safe_worker_count_p75 =
        if mean_rate_per_worker > 0.0 && hours_remaining > 0.0 && fleet_pct_hr > 0.0 {
            let rate_p75_fleet = (fleet_pct_hr + Z_0_675 * std_pct_hr).max(MIN_RATE);
            let rate_p75_per_worker = mean_rate_per_worker * rate_p75_fleet / fleet_pct_hr;
            let safe = (remaining_pct / (rate_p75_per_worker * hours_remaining)).floor() as u64;
            Some(safe.min(u32::MAX as u64) as u32)
        } else {
            safe_worker_count
        };

    // Confidence cone using ±0.675σ (25th/75th percentile of a Normal distribution).
    // High burn rate → fewer remaining hours, so:
    //   exh_hrs_p25 (pessimistic) uses rate at +0.675σ (fast burn)
    //   exh_hrs_p75 (optimistic)  uses rate at -0.675σ (slow burn)

    let (exh_hrs_p25, exh_hrs_p50, exh_hrs_p75, cone_ratio) = if fleet_pct_hr > 0.0 {
        let rate_fast = (fleet_pct_hr + Z_0_675 * std_pct_hr).max(MIN_RATE);
        let rate_slow = (fleet_pct_hr - Z_0_675 * std_pct_hr).max(MIN_RATE);

        let p25 = remaining_pct / rate_fast;
        let p50 = predicted_exhaustion_hours;
        let p75 = remaining_pct / rate_slow;

        let ratio = if p25 > 0.0 { p75 / p25 } else { 1.0 };
        (p25, p50, p75, ratio)
    } else {
        (
            predicted_exhaustion_hours,
            predicted_exhaustion_hours,
            predicted_exhaustion_hours,
            1.0,
        )
    };

    // Compute composite risk score for binding window selection
    let risk_score =
        compute_risk_score(window, margin_hrs, hours_remaining, cone_ratio).unwrap_or(0.0);

    crate::state::WindowForecast {
        target_ceiling,
        current_utilization,
        remaining_pct,
        hours_remaining,
        fleet_pct_per_hour: fleet_pct_hr,
        predicted_exhaustion_hours,
        cutoff_risk,
        margin_hrs,
        binding: false,
        safe_worker_count,
        safe_worker_count_p75,
        exh_hrs_p25,
        exh_hrs_p50,
        exh_hrs_p75,
        cone_ratio,
        risk_score,
        hard_limit_remaining_pct: (100.0 - current_utilization).max(0.0),
        hard_limit_margin_hrs: if fleet_pct_hr > 0.0 {
            (100.0 - current_utilization).max(0.0) / fleet_pct_hr - hours_remaining
        } else {
            f64::INFINITY
        },
    }
}

/// The main adaptive burn rate estimation function
///
/// Reads instance records, computes per-instance burn rates, aggregates to
/// fleet stats, updates EMA state, and generates capacity forecasts.
///
/// Returns updated EMA state and fleet stats for persisting in GovernorState.
#[allow(clippy::too_many_arguments)]
pub fn estimate_burn_rates(
    instance_records: &[InstanceRecord],
    elapsed_hours: f64,
    current_workers: u32,
    prev_workers: u32,
    ema_state: &mut HashMap<(String, String), ModelWindowEma>,
    _baseline: &BaselineBurnRates,
    current_utilization: &HashMap<String, f64>, // window -> current %
    target_ceiling: f64,
    hours_remaining: &HashMap<String, f64>, // window -> hours until reset
) -> (BurnRateEstimate, crate::state::CapacityForecast) {
    // Guard: skip if worker count changed this interval
    if prev_workers != 0 && prev_workers != current_workers {
        log::debug!(
            "[burn_rate] worker count changed ({} -> {}), skipping EMA update",
            prev_workers,
            current_workers
        );
        return (
            BurnRateEstimate {
                ema_state: ema_state.clone(),
                fleet_stats: HashMap::new(),
                had_valid_data: false,
            },
            crate::state::CapacityForecast::default(),
        );
    }

    // Compute per-instance burn rates
    let mut all_instance_rates: Vec<InstanceBurnRate> = Vec::new();
    for record in instance_records {
        let rates = compute_instance_burn(record, elapsed_hours);
        all_instance_rates.extend(rates);
    }

    if all_instance_rates.is_empty() {
        return (
            BurnRateEstimate {
                ema_state: ema_state.clone(),
                fleet_stats: HashMap::new(),
                had_valid_data: false,
            },
            crate::state::CapacityForecast::default(),
        );
    }

    // Group instance rates by window
    let mut rates_by_window: HashMap<String, Vec<&InstanceBurnRate>> = HashMap::new();
    for rate in &all_instance_rates {
        rates_by_window
            .entry(rate.window.clone())
            .or_default()
            .push(rate);
    }

    // Compute fleet stats per window
    let mut fleet_stats: HashMap<String, FleetWorkerStats> = HashMap::new();
    for window in WINDOWS {
        let rates = rates_by_window
            .get(*window)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        fleet_stats.insert(
            window.to_string(),
            compute_fleet_stats(window, rates, current_workers),
        );
    }

    // Update EMA state per (model, window)
    for rate in &all_instance_rates {
        let key = (rate.model.clone(), rate.window.clone());
        let pct_per_worker = if current_workers > 0 {
            rate.pct_per_hour / current_workers as f64
        } else {
            0.0
        };
        let usd_per_worker = if current_workers > 0 {
            rate.dollar_per_hour / current_workers as f64
        } else {
            0.0
        };

        let ema = ema_state.entry(key).or_default();
        update_ema(ema, pct_per_worker, usd_per_worker);
    }

    // Generate capacity forecasts
    let mut forecasts = HashMap::new();
    for window in WINDOWS {
        let stats = fleet_stats
            .get(*window)
            .cloned()
            .unwrap_or_else(|| FleetWorkerStats {
                worker_count: current_workers,
                mean_pct_hr: 0.0,
                p75_pct_hr: 0.0,
                std_pct_hr: 0.0,
                mean_usd_hr: 0.0,
            });

        // Use fleet-level mean pct/hr as the fleet burn rate
        let fleet_pct_hr = stats.mean_pct_hr;

        // Get p75 per-worker rate for safe worker computation
        let p75_per_worker = if current_workers > 0 {
            stats.p75_pct_hr / current_workers as f64
        } else {
            0.0
        };

        let util = current_utilization.get(*window).copied().unwrap_or(0.0);
        let hrs_left = hours_remaining.get(*window).copied().unwrap_or(0.0);

        forecasts.insert(
            window.to_string(),
            generate_window_forecast(
                window,
                fleet_pct_hr,
                util,
                target_ceiling,
                hrs_left,
                p75_per_worker,
                stats.std_pct_hr,
            ),
        );
    }

    // Identify binding window (highest risk_score)
    // The risk_score combines margin urgency, duration weight, and volatility (cone_ratio).
    // Higher risk_score = more urgent window that should drive scaling decisions.
    let binding_window = WINDOWS
        .iter()
        .max_by(|&a, &b| {
            let fa = forecasts.get(*a).map(|f| f.risk_score).unwrap_or(0.0);
            let fb = forecasts.get(*b).map(|f| f.risk_score).unwrap_or(0.0);
            fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|w| w.to_string())
        .unwrap_or_default();

    // Set binding flag on the binding window
    for (win, forecast) in forecasts.iter_mut() {
        forecast.binding = *win == binding_window;
    }

    // Build CapacityForecast state
    let capacity_forecast = crate::state::CapacityForecast {
        five_hour: forecasts.get("five_hour").cloned().unwrap_or_default(),
        seven_day: forecasts.get("seven_day").cloned().unwrap_or_default(),
        seven_day_sonnet: forecasts
            .get("seven_day_sonnet")
            .cloned()
            .unwrap_or_default(),
        binding_window: binding_window.clone(),
        dollars_per_pct_7d_s: 0.0, // Computed externally from fleet aggregate
        estimated_remaining_dollars: 0.0, // Computed externally
    };

    // Log per-window capacity forecast
    for window in WINDOWS {
        let f = forecasts.get(*window).cloned().unwrap_or_default();
        let binding = if f.binding { " BINDING" } else { "" };
        let cutoff = if f.cutoff_risk { " CUTOFF_RISK" } else { "" };
        let safe_str = f
            .safe_worker_count
            .map(|w| format!(" at {} workers", w))
            .unwrap_or_default();

        log::info!(
            "[burn_rate] {}: {:.1}% remaining, resets in {:.1}h{}{} — exhausts in {:.1}h{}",
            window,
            f.remaining_pct,
            f.hours_remaining,
            binding,
            cutoff,
            f.predicted_exhaustion_hours,
            safe_str,
        );
    }

    if !binding_window.is_empty() {
        let binding_forecast = forecasts.get(&binding_window);
        match binding_forecast.and_then(|f| f.safe_worker_count) {
            None => log::info!(
                "[burn_rate] → binding window {}: insufficient burn rate data, using max_workers as ceiling",
                binding_window,
            ),
            Some(safe_w) => log::info!(
                "[burn_rate] → target: {} workers (safe_worker_count from binding window {})",
                safe_w,
                binding_window,
            ),
        }
    }

    (
        BurnRateEstimate {
            ema_state: ema_state.clone(),
            fleet_stats,
            had_valid_data: true,
        },
        capacity_forecast,
    )
}

/// Build the burn_rate state block for GovernorState from EMA data
///
/// Converts internal EMA state to the state schema's `BurnRateState`.
pub fn build_burn_rate_state(
    ema_state: &HashMap<(String, String), ModelWindowEma>,
    tokens_per_pct_peak: u64,
    tokens_per_pct_offpeak: u64,
    offpeak_ratio_observed: f64,
    offpeak_ratio_expected: f64,
    promotion_validated: bool,
    promotion_peak_samples: usize,
    promotion_offpeak_samples: usize,
    last_sample_at: Option<DateTime<Utc>>,
    calibration: crate::state::CalibrationState,
) -> crate::state::BurnRateState {
    let mut by_model: HashMap<String, crate::state::ModelBurnRate> = HashMap::new();

    // Aggregate per-model: use the max samples across windows, and average rates
    let mut model_windows: HashMap<String, Vec<&ModelWindowEma>> = HashMap::new();
    for ((model, _window), ema) in ema_state {
        model_windows.entry(model.clone()).or_default().push(ema);
    }

    for (model, emas) in &model_windows {
        let max_samples = emas.iter().map(|e| e.samples).max().unwrap_or(0);
        let avg_pct = if !emas.is_empty() {
            emas.iter().map(|e| e.ema_pct).sum::<f64>() / emas.len() as f64
        } else {
            0.0
        };
        let avg_usd = if !emas.is_empty() {
            emas.iter().map(|e| e.ema_usd).sum::<f64>() / emas.len() as f64
        } else {
            0.0
        };

        by_model.insert(
            model.clone(),
            crate::state::ModelBurnRate {
                pct_per_worker_per_hour: avg_pct,
                dollars_per_worker_per_hour: avg_usd,
                samples: max_samples,
            },
        );
    }

    crate::state::BurnRateState {
        by_model,
        tokens_per_pct_peak,
        tokens_per_pct_offpeak,
        offpeak_ratio_observed,
        offpeak_ratio_expected,
        promotion_validated,
        promotion_peak_samples,
        promotion_offpeak_samples,
        last_sample_at,
        calibration,
        // EMA fields are not computed here; they are managed by the governor loop
        fleet_pct_hr_ema: crate::state::WindowPctDeltas::default(),
        usd_per_pct_ema_five_hour: 0.0,
        usd_per_pct_ema_seven_day: 0.0,
        usd_per_pct_ema_seven_day_sonnet: 0.0,
        fleet_pct_ema_samples: 0,
        prev_usage_snapshot: None,
    }
}

/// Log per-window capacity forecast (for governor loop integration)
pub fn log_capacity_forecast(forecast: &crate::state::CapacityForecast) {
    let windows = [
        ("5h", &forecast.five_hour),
        ("7d", &forecast.seven_day),
        ("7d-sonnet", &forecast.seven_day_sonnet),
    ];

    for (label, f) in &windows {
        let binding = if f.binding { " BINDING" } else { "" };
        let cutoff = if f.cutoff_risk { " CUTOFF_RISK" } else { "" };
        let safe_str = f
            .safe_worker_count
            .map(|w| format!(" at {} workers", w))
            .unwrap_or_default();

        log::info!(
            "[governor] {}: {:.1}% remaining, resets in {:.1}h{}{} — exhausts in {:.1}h{}",
            label,
            f.remaining_pct,
            f.hours_remaining,
            binding,
            cutoff,
            f.predicted_exhaustion_hours,
            safe_str,
        );
    }

    if !forecast.binding_window.is_empty() {
        let binding_forecast = match forecast.binding_window.as_str() {
            "five_hour" => &forecast.five_hour,
            "seven_day" => &forecast.seven_day,
            _ => &forecast.seven_day_sonnet,
        };
        match binding_forecast.safe_worker_count {
            None => log::info!(
                "[governor] → binding window {}: insufficient burn rate data, will use max_workers as ceiling",
                forecast.binding_window,
            ),
            Some(w) => log::info!(
                "[governor] → safe_worker_count: {} workers from binding window {}",
                w,
                forecast.binding_window,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a WindowUtilization for a named window
    fn win(name: &str, pct_delta: Option<f64>, current: f64, previous: f64) -> WindowUtilization {
        WindowUtilization {
            window: name.to_string(),
            pct_delta,
            current_utilization: current,
            previous_utilization: previous,
        }
    }

    /// Helper: build a basic InstanceRecord with one window
    fn basic_record(pct_delta: Option<f64>) -> InstanceRecord {
        InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.50,
            total_tokens: 500_000,
            windows: vec![win("five_hour", pct_delta, 42.0, 40.0)],
        }
    }

    // --- Core computation ---

    #[test]
    fn compute_burn_rate_from_known_record() {
        let record = basic_record(Some(2.0));
        let elapsed = 0.5; // 30 minutes

        let rates = compute_instance_burn(&record, elapsed);

        assert_eq!(rates.len(), 1);
        let r = &rates[0];
        assert_eq!(r.session, "sess-abc");
        assert_eq!(r.model, "claude-sonnet-4-20250514");
        assert_eq!(r.window, "five_hour");
        assert_eq!(r.elapsed_hours, 0.5);

        // pct_per_hour = 2.0 / 0.5 = 4.0
        assert!(
            (r.pct_per_hour - 4.0).abs() < 1e-9,
            "expected pct_per_hour=4.0, got {}",
            r.pct_per_hour
        );

        // dollar_per_hour = 1.50 / 0.5 = 3.0
        assert!(
            (r.dollar_per_hour - 3.0).abs() < 1e-9,
            "expected dollar_per_hour=3.0, got {}",
            r.dollar_per_hour
        );
    }

    // --- Guard: skip interval < 2 minutes ---

    #[test]
    fn guard_skip_short_interval() {
        let record = basic_record(Some(2.0));
        // 1 minute = 1/60 hours, which is < 2/60
        let rates = compute_instance_burn(&record, 1.0 / 60.0);
        assert!(rates.is_empty());
    }

    #[test]
    fn guard_exact_two_minutes_passes() {
        let record = basic_record(Some(2.0));
        let rates = compute_instance_burn(&record, 2.0 / 60.0);
        assert_eq!(rates.len(), 1);
    }

    #[test]
    fn guard_under_two_minutes_rejects() {
        let record = basic_record(Some(2.0));
        // 1.999 minutes
        let rates = compute_instance_burn(&record, 1.999 / 60.0);
        assert!(rates.is_empty());
    }

    // --- Guard: skip null pct_delta ---

    #[test]
    fn guard_skip_null_pct_delta() {
        let record = basic_record(None);
        let rates = compute_instance_burn(&record, 1.0);
        assert!(rates.is_empty());
    }

    #[test]
    fn guard_mixed_null_and_valid_windows() {
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.50,
            total_tokens: 500_000,
            windows: vec![
                win("five_hour", None, 42.0, 40.0),
                win("seven_day", Some(3.0), 65.0, 62.0),
                win("seven_day_sonnet", None, 70.0, 68.0),
            ],
        };

        let rates = compute_instance_burn(&record, 1.0);
        assert_eq!(rates.len(), 1);
        assert_eq!(rates[0].window, "seven_day");
    }

    // --- Guard: skip zero pct_delta with non-zero tokens ---

    #[test]
    fn guard_skip_zero_pct_delta_with_tokens() {
        let record = basic_record(Some(0.0));
        assert!(record.total_tokens > 0);

        let rates = compute_instance_burn(&record, 1.0);
        assert!(rates.is_empty());
    }

    #[test]
    fn guard_zero_pct_delta_allowed_with_zero_tokens() {
        let mut record = basic_record(Some(0.0));
        record.total_tokens = 0;

        // Zero pct_delta with zero tokens is not a rounding artifact — allow it
        let rates = compute_instance_burn(&record, 1.0);
        assert_eq!(rates.len(), 1);
        assert!(
            rates[0].pct_per_hour == 0.0,
            "expected 0 pct_per_hour, got {}",
            rates[0].pct_per_hour
        );
    }

    // --- Window reset detection ---

    #[test]
    fn window_reset_discards_affected_window() {
        // current=38, previous=40 -> drop of 2pp > 1pp threshold -> reset detected
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.50,
            total_tokens: 500_000,
            windows: vec![win("five_hour", Some(2.0), 38.0, 40.0)],
        };

        let rates = compute_instance_burn(&record, 1.0);
        assert!(rates.is_empty());
    }

    #[test]
    fn window_reset_discards_only_affected_window() {
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.50,
            total_tokens: 500_000,
            windows: vec![
                win("five_hour", Some(2.0), 38.0, 40.0), // reset: drop of 2pp
                win("seven_day", Some(3.0), 65.0, 62.0), // normal: rise of 3pp
            ],
        };

        let rates = compute_instance_burn(&record, 1.0);
        assert_eq!(rates.len(), 1);
        assert_eq!(rates[0].window, "seven_day");
    }

    #[test]
    fn window_reset_boundary_1pp_drop_is_ok() {
        // current=39.0, previous=40.0 -> drop of exactly 1.0pp, which is NOT > 1.0
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.50,
            total_tokens: 500_000,
            windows: vec![win("five_hour", Some(1.0), 39.0, 40.0)],
        };

        let rates = compute_instance_burn(&record, 1.0);
        assert_eq!(rates.len(), 1);
    }

    #[test]
    fn window_reset_slight_increase_is_ok() {
        // current=40.5, previous=40.0 -> no reset
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.50,
            total_tokens: 500_000,
            windows: vec![win("five_hour", Some(0.5), 40.5, 40.0)],
        };

        let rates = compute_instance_burn(&record, 1.0);
        assert_eq!(rates.len(), 1);
    }

    // --- Multi-window burn ---

    #[test]
    fn multi_window_each_computed_independently() {
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 2.40,
            total_tokens: 800_000,
            windows: vec![
                win("five_hour", Some(1.0), 41.0, 40.0),
                win("seven_day", Some(3.0), 63.0, 60.0),
                win("seven_day_sonnet", Some(5.0), 75.0, 70.0),
            ],
        };

        let elapsed = 2.0; // 2 hours
        let rates = compute_instance_burn(&record, elapsed);

        assert_eq!(rates.len(), 3);

        // All should share the same dollar_per_hour
        let expected_dollar_hr = 2.40 / 2.0; // 1.20
        for r in &rates {
            assert!(
                (r.dollar_per_hour - expected_dollar_hr).abs() < 1e-9,
                "window {}: expected dollar_per_hour={}, got {}",
                r.window,
                expected_dollar_hr,
                r.dollar_per_hour
            );
        }

        // Each window should have its own pct_per_hour
        let by_window: std::collections::HashMap<&str, &InstanceBurnRate> =
            rates.iter().map(|r| (r.window.as_str(), r)).collect();

        assert!(
            (by_window["five_hour"].pct_per_hour - 0.5).abs() < 1e-9,
            "five_hour: expected 0.5, got {}",
            by_window["five_hour"].pct_per_hour
        );
        assert!(
            (by_window["seven_day"].pct_per_hour - 1.5).abs() < 1e-9,
            "seven_day: expected 1.5, got {}",
            by_window["seven_day"].pct_per_hour
        );
        assert!(
            (by_window["seven_day_sonnet"].pct_per_hour - 2.5).abs() < 1e-9,
            "seven_day_sonnet: expected 2.5, got {}",
            by_window["seven_day_sonnet"].pct_per_hour
        );
    }

    #[test]
    fn multi_window_partial_guards() {
        // One window null, one zero pct_delta with tokens, one valid
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.0,
            total_tokens: 100_000,
            windows: vec![
                win("five_hour", None, 41.0, 40.0),      // null pct_delta -> skip
                win("seven_day", Some(0.0), 63.0, 63.0), // zero pct + tokens -> skip
                win("seven_day_sonnet", Some(2.0), 72.0, 70.0), // valid
            ],
        };

        let rates = compute_instance_burn(&record, 1.0);
        assert_eq!(rates.len(), 1);
        assert_eq!(rates[0].window, "seven_day_sonnet");
    }

    #[test]
    fn empty_windows_returns_empty() {
        let record = InstanceRecord {
            session: "sess-abc".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            total_usd: 1.50,
            total_tokens: 500_000,
            windows: vec![],
        };

        let rates = compute_instance_burn(&record, 1.0);
        assert!(rates.is_empty());
    }

    // -----------------------------------------------------------------------
    // Promotion validation tests
    // -----------------------------------------------------------------------

    /// Helper: create a promotion sample
    fn promo_sample(tokens_per_pct: f64, is_peak: bool, worker_count: u32) -> PromotionSample {
        PromotionSample {
            tokens_per_pct,
            is_peak,
            worker_count,
            timestamp: Utc::now(),
        }
    }

    /// Helper: generate peak samples (tokens_per_pct = 70_000)
    fn peak_samples(n: usize, worker_count: u32) -> Vec<PromotionSample> {
        (0..n)
            .map(|_| promo_sample(70_000.0, true, worker_count))
            .collect()
    }

    /// Helper: generate off-peak samples (tokens_per_pct = 140_000, i.e., 2x peak)
    fn offpeak_samples(n: usize, worker_count: u32) -> Vec<PromotionSample> {
        (0..n)
            .map(|_| promo_sample(140_000.0, false, worker_count))
            .collect()
    }

    // --- No promotion (multiplier <= 1.0) ---

    #[test]
    fn validation_no_promo_is_always_valid() {
        let result = validate_promotion(&[], 1.0);
        assert!(result.validated);
        assert!((result.observed_ratio - 1.0).abs() < 1e-9);
        assert!(result.reason.is_none());
    }

    // --- Empty samples ---

    #[test]
    fn validation_empty_samples_fails() {
        let result = validate_promotion(&[], 2.0);
        assert!(!result.validated);
        assert_eq!(result.reason.as_deref(), Some("no samples"));
    }

    // --- Insufficient samples ---

    #[test]
    fn validation_too_few_samples_fails() {
        let mut samples = peak_samples(3, 2);
        samples.extend(offpeak_samples(3, 2));

        let result = validate_promotion(&samples, 2.0);
        assert!(!result.validated);
        assert_eq!(result.peak_samples, 3);
        assert_eq!(result.offpeak_samples, 3);
        assert!(result.reason.unwrap().contains("insufficient samples"));
    }

    #[test]
    fn validation_exactly_5_each_succeeds() {
        let mut samples = peak_samples(5, 2);
        samples.extend(offpeak_samples(5, 2));

        let result = validate_promotion(&samples, 2.0);
        assert!(result.validated);
        assert!((result.observed_ratio - 2.0).abs() < 1e-9);
    }

    // --- Validated: within 10% of declared 2.0 ---

    #[test]
    fn validation_exact_ratio_is_validated() {
        // peak=70k, offpeak=140k -> ratio=2.0
        let mut samples = peak_samples(5, 2);
        samples.extend(offpeak_samples(5, 2));

        let result = validate_promotion(&samples, 2.0);
        assert!(result.validated);
        assert!((result.observed_ratio - 2.0).abs() < 1e-9);
        assert!((result.median_peak - 70_000.0).abs() < 1e-9);
        assert!((result.median_offpeak - 140_000.0).abs() < 1e-9);
        assert!(result.reason.is_none());
    }

    #[test]
    fn validation_ratio_within_10_percent_is_validated() {
        // 1.82x is within 10% of 2.0 (range [1.8, 2.2])
        let mut samples = peak_samples(5, 2);
        // offpeak = 70000 * 1.82 = 127400
        for _ in 0..5 {
            samples.push(promo_sample(127_400.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(result.validated);
        assert!((result.observed_ratio - 1.82).abs() < 0.01);
    }

    #[test]
    fn validation_ratio_at_upper_bound_is_validated() {
        // 2.2x is exactly at +10% of 2.0
        let mut samples = peak_samples(5, 2);
        // offpeak = 70000 * 2.2 = 154000
        for _ in 0..5 {
            samples.push(promo_sample(154_000.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(result.validated);
        assert!((result.observed_ratio - 2.2).abs() < 0.01);
    }

    #[test]
    fn validation_ratio_at_lower_bound_is_validated() {
        // 1.8x is exactly at -10% of 2.0
        let mut samples = peak_samples(5, 2);
        // offpeak = 70000 * 1.8 = 126000
        for _ in 0..5 {
            samples.push(promo_sample(126_000.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(result.validated);
        assert!((result.observed_ratio - 1.8).abs() < 0.01);
    }

    #[test]
    fn validation_ratio_just_outside_upper_bound_fails() {
        // 2.21x is just outside +10% of 2.0 (2.2)
        let mut samples = peak_samples(5, 2);
        // offpeak = 70000 * 2.21 = 154700
        for _ in 0..5 {
            samples.push(promo_sample(154_700.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(!result.validated);
        assert!(result.reason.unwrap().contains("outside tolerance"));
    }

    // --- Promotion not applying (ratio < 1.2) ---

    #[test]
    fn validation_ratio_below_1_2_means_not_applying() {
        // ratio=1.1 -> promotion not applying
        let mut samples = peak_samples(5, 2);
        // offpeak = 70000 * 1.1 = 77000
        for _ in 0..5 {
            samples.push(promo_sample(77_000.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(!result.validated);
        assert!(result.observed_ratio < 1.2);
        assert!(result.reason.unwrap().contains("may not be applying"));
    }

    #[test]
    fn validation_ratio_1_0_means_no_promo_effect() {
        // ratio=1.0 -> no promotion effect at all
        let mut samples = peak_samples(5, 2);
        for _ in 0..5 {
            samples.push(promo_sample(70_000.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(!result.validated);
        assert!((result.observed_ratio - 1.0).abs() < 1e-9);
    }

    // --- Anomaly (ratio > 2.5) ---

    #[test]
    fn validation_ratio_above_2_5_is_anomaly() {
        // ratio=3.0 -> anomaly
        let mut samples = peak_samples(5, 2);
        // offpeak = 70000 * 3.0 = 210000
        for _ in 0..5 {
            samples.push(promo_sample(210_000.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(!result.validated);
        assert!(result.observed_ratio > 2.5);
        assert!(result.reason.unwrap().contains("anomaly"));
    }

    #[test]
    fn effective_multiplier_uses_observed_for_anomaly() {
        let mut samples = peak_samples(5, 2);
        for _ in 0..5 {
            samples.push(promo_sample(210_000.0, false, 2));
        }

        let result = validate_promotion(&samples, 2.0);
        assert!(!result.validated);
        // effective_multiplier should use observed ratio for anomaly
        let mult = effective_multiplier(&result);
        assert!((mult - 3.0).abs() < 0.01);
    }

    // --- effective_multiplier ---

    #[test]
    fn effective_multiplier_validated_returns_declared() {
        let result = PromotionValidationResult {
            validated: true,
            observed_ratio: 2.0,
            declared_multiplier: 2.0,
            median_peak: 70_000.0,
            median_offpeak: 140_000.0,
            peak_samples: 5,
            offpeak_samples: 5,
            reason: None,
        };

        assert!((effective_multiplier(&result) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn effective_multiplier_not_validated_returns_1x() {
        let result = PromotionValidationResult {
            validated: false,
            observed_ratio: 1.1,
            declared_multiplier: 2.0,
            median_peak: 70_000.0,
            median_offpeak: 77_000.0,
            peak_samples: 5,
            offpeak_samples: 5,
            reason: Some("promotion may not be applying".to_string()),
        };

        assert!((effective_multiplier(&result) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn effective_multiplier_insufficient_samples_returns_1x() {
        let result = validate_promotion(&[], 2.0);
        assert!((effective_multiplier(&result) - 1.0).abs() < 1e-9);
    }

    // --- Grouping by worker count ---

    #[test]
    fn validation_uses_largest_worker_count_group() {
        // Group with worker_count=2: 5 peak + 5 offpeak
        let mut samples = peak_samples(5, 2);
        samples.extend(offpeak_samples(5, 2));

        // Group with worker_count=3: 3 peak + 3 offpeak (insufficient)
        samples.extend(peak_samples(3, 3));
        samples.extend(offpeak_samples(3, 3));

        let result = validate_promotion(&samples, 2.0);
        assert!(result.validated);
        // Should use worker_count=2 group (10 samples vs 6)
        assert!((result.observed_ratio - 2.0).abs() < 1e-9);
    }

    #[test]
    fn validation_ignores_mixed_worker_counts() {
        // worker_count=2: 5 peak + 5 offpeak (valid)
        // worker_count=3: 5 peak + 5 offpeak (valid, but different scale)
        let mut samples = peak_samples(5, 2);
        samples.extend(offpeak_samples(5, 2));
        // Different worker count with different consumption rate
        samples.extend(peak_samples(5, 3));
        for _ in 0..5 {
            samples.push(promo_sample(100_000.0, false, 3)); // ~1.43x ratio for wc=3
        }

        let result = validate_promotion(&samples, 2.0);
        // Both groups have 10 samples; should pick one and validate against it
        // The important thing is it doesn't mix them
        assert!(result.peak_samples >= 5);
        assert!(result.offpeak_samples >= 5);
    }

    // --- Median computation ---

    #[test]
    fn median_odd_count_returns_middle() {
        assert!((median(&[1.0, 3.0, 5.0]) - 3.0).abs() < 1e-9);
        assert!((median(&[10.0, 20.0, 30.0, 40.0, 50.0]) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn median_even_count_returns_average() {
        assert!((median(&[1.0, 2.0, 3.0, 4.0]) - 2.5).abs() < 1e-9);
        assert!((median(&[10.0, 20.0]) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn median_empty_returns_zero() {
        assert!((median(&[]) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn median_single_element() {
        assert!((median(&[42.0]) - 42.0).abs() < 1e-9);
    }

    #[test]
    fn median_unsorted_input() {
        assert!((median(&[5.0, 1.0, 3.0, 2.0, 4.0]) - 3.0).abs() < 1e-9);
    }

    // --- Validation with varied samples (noise) ---

    #[test]
    fn validation_with_noisy_samples_still_validates() {
        // Peak samples with noise around 70k
        let peak: Vec<PromotionSample> = vec![65_000, 68_000, 70_000, 72_000, 75_000]
            .into_iter()
            .map(|v| promo_sample(v as f64, true, 2))
            .collect();

        // Off-peak samples with noise around 140k (2x peak)
        let offpeak: Vec<PromotionSample> = vec![130_000, 135_000, 140_000, 145_000, 150_000]
            .into_iter()
            .map(|v| promo_sample(v as f64, false, 2))
            .collect();

        let mut samples = peak;
        samples.extend(offpeak);

        let result = validate_promotion(&samples, 2.0);
        assert!(
            result.validated,
            "expected validated, got reason: {:?}",
            result.reason
        );
        // median peak = 70_000, median offpeak = 140_000 -> ratio = 2.0
        assert!((result.median_peak - 70_000.0).abs() < 1e-9);
        assert!((result.median_offpeak - 140_000.0).abs() < 1e-9);
        assert!((result.observed_ratio - 2.0).abs() < 0.01);
    }

    // --- Zero median peak guard ---

    #[test]
    fn validation_zero_median_peak_fails() {
        let mut samples = Vec::new();
        // Peak samples all zero
        for _ in 0..5 {
            samples.push(promo_sample(0.0, true, 2));
        }
        samples.extend(offpeak_samples(5, 2));

        let result = validate_promotion(&samples, 2.0);
        assert!(!result.validated);
        assert!(result
            .reason
            .unwrap()
            .contains("median peak tokens_per_pct is zero"));
    }

    // -----------------------------------------------------------------------
    // Adaptive Burn Rate Estimator tests
    // -----------------------------------------------------------------------

    /// Helper: create an instance record with two valid windows
    fn multi_window_record(
        session: &str,
        model: &str,
        total_usd: f64,
        total_tokens: u64,
        five_hour_delta: Option<f64>,
        seven_day_delta: Option<f64>,
        seven_ds_delta: Option<f64>,
        five_current: f64,
        five_prev: f64,
        seven_current: f64,
        seven_prev: f64,
        seven_ds_current: f64,
        seven_ds_prev: f64,
    ) -> InstanceRecord {
        InstanceRecord {
            session: session.to_string(),
            model: model.to_string(),
            total_usd,
            total_tokens,
            windows: vec![
                win("five_hour", five_hour_delta, five_current, five_prev),
                win("seven_day", seven_day_delta, seven_current, seven_prev),
                win(
                    "seven_day_sonnet",
                    seven_ds_delta,
                    seven_ds_current,
                    seven_ds_prev,
                ),
            ],
        }
    }

    #[test]
    fn estimate_with_two_workers_computes_fleet_stats() {
        let baseline = BaselineBurnRates::default();
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        let instances = vec![
            multi_window_record(
                "w1",
                "claude-sonnet-4-20250514",
                2.0, // $2/hr
                500_000,
                Some(1.0),
                Some(0.5),
                Some(0.75),
                41.0,
                40.0, // 5h: delta=1
                60.5,
                60.0, // 7d: delta=0.5
                70.75,
                70.0, // 7ds: delta=0.75
            ),
            multi_window_record(
                "w2",
                "claude-sonnet-4-20250514",
                3.0, // $3/hr
                750_000,
                Some(2.0),
                Some(1.0),
                Some(1.5),
                42.0,
                40.0, // 5h: delta=2
                61.0,
                60.0, // 7d: delta=1
                71.5,
                70.0, // 7ds: delta=1.5
            ),
        ];

        let elapsed = 1.0; // 1 hour
        let current_workers = 2;
        let prev_workers = 2;
        let mut utilization = HashMap::new();
        utilization.insert("five_hour".to_string(), 42.0);
        utilization.insert("seven_day".to_string(), 61.0);
        utilization.insert("seven_day_sonnet".to_string(), 71.5);
        let mut hrs_left = HashMap::new();
        hrs_left.insert("five_hour".to_string(), 3.0);
        hrs_left.insert("seven_day".to_string(), 37.5);
        hrs_left.insert("seven_day_sonnet".to_string(), 37.5);

        let (estimate, _forecast) = estimate_burn_rates(
            &instances,
            elapsed,
            current_workers,
            prev_workers,
            &mut ema_state,
            &baseline,
            &utilization,
            90.0,
            &hrs_left,
        );

        assert!(estimate.had_valid_data);

        // Fleet stats for five_hour: 2 instances with pct_per_hour 1.0 and 2.0
        let five_stats = estimate.fleet_stats.get("five_hour").unwrap();
        assert!((five_stats.mean_pct_hr - 1.5).abs() < 1e-9); // (1+2)/2
        assert_eq!(five_stats.worker_count, 2);
        assert_eq!(five_stats.p75_pct_hr, 2.0); // p75 of [1, 2]

        // EMA should be updated for (model, window) pairs
        assert!(estimate.ema_state.contains_key(&(
            "claude-sonnet-4-20250514".to_string(),
            "five_hour".to_string()
        )));
    }

    #[test]
    fn estimate_with_changed_workers_skips() {
        let baseline = BaselineBurnRates::default();
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        let instances = vec![multi_window_record(
            "w1",
            "claude-sonnet-4-20250514",
            2.0,
            500_000,
            Some(1.0),
            Some(0.5),
            Some(0.75),
            41.0,
            40.0,
            60.5,
            60.0,
            70.75,
            70.0,
        )];

        let (estimate, _forecast) = estimate_burn_rates(
            &instances,
            1.0,
            2, // current
            3, // previous (changed!)
            &mut ema_state,
            &baseline,
            &HashMap::new(),
            90.0,
            &HashMap::new(),
        );

        assert!(!estimate.had_valid_data);
        assert!(ema_state.is_empty()); // EMA not updated
    }

    #[test]
    fn estimate_with_no_valid_data_returns_empty() {
        let baseline = BaselineBurnRates::default();
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        // All windows have null pct_delta
        let instances = vec![multi_window_record(
            "w1",
            "claude-sonnet-4-20250514",
            2.0,
            500_000,
            None,
            None,
            None, // all null
            40.0,
            40.0,
            60.0,
            60.0,
            70.0,
            70.0,
        )];

        let (estimate, forecast) = estimate_burn_rates(
            &instances,
            1.0,
            1,
            1,
            &mut ema_state,
            &baseline,
            &HashMap::new(),
            90.0,
            &HashMap::new(),
        );

        assert!(!estimate.had_valid_data);
        assert!(forecast.binding_window.is_empty());
    }

    #[test]
    fn binding_window_is_most_constrained() {
        let baseline = BaselineBurnRates::default();
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        let instances = vec![multi_window_record(
            "w1",
            "claude-sonnet-4-20250514",
            2.0,
            500_000,
            Some(5.0),
            Some(0.5),
            Some(2.0),
            45.0,
            40.0,
            60.5,
            60.0,
            72.0,
            70.0,
        )];

        let mut utilization = HashMap::new();
        utilization.insert("five_hour".to_string(), 45.0);
        utilization.insert("seven_day".to_string(), 60.5);
        utilization.insert("seven_day_sonnet".to_string(), 72.0);
        let mut hrs_left = HashMap::new();
        hrs_left.insert("five_hour".to_string(), 100.0); // plenty of time
        hrs_left.insert("seven_day".to_string(), 3.0); // very constrained
        hrs_left.insert("seven_day_sonnet".to_string(), 37.5);

        let (_estimate, forecast) = estimate_burn_rates(
            &instances,
            1.0,
            1,
            1,
            &mut ema_state,
            &baseline,
            &utilization,
            90.0,
            &hrs_left,
        );

        // five_hour: fleet_pct_hr=5.0, remain=45, exh=9, margin=9-100=-91 (most negative, highest risk)
        // seven_day: fleet_pct_hr=0.5, remain=29.5, exh=59, margin=59-3=+56 (positive=safe, lowest risk)
        // seven_day_sonnet: fleet_pct_hr=2.0, remain=18, exh=9, margin=9-37.5=-28.5
        assert_eq!(forecast.binding_window, "five_hour");
        assert!(forecast.five_hour.binding);
        assert!(!forecast.seven_day.binding);
        assert!(!forecast.seven_day_sonnet.binding);
    }

    #[test]
    fn ema_updates_over_multiple_cycles() {
        let baseline = BaselineBurnRates::default();
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        let instances = vec![multi_window_record(
            "w1",
            "claude-sonnet-4-20250514",
            2.0,
            500_000,
            Some(2.0),
            Some(1.0),
            Some(1.5),
            42.0,
            40.0,
            61.0,
            60.0,
            71.5,
            70.0,
        )];

        let mut utilization = HashMap::new();
        utilization.insert("five_hour".to_string(), 42.0);
        utilization.insert("seven_day".to_string(), 61.0);
        utilization.insert("seven_day_sonnet".to_string(), 71.5);
        let mut hrs_left = HashMap::new();
        hrs_left.insert("five_hour".to_string(), 3.0);
        hrs_left.insert("seven_day".to_string(), 37.5);
        hrs_left.insert("seven_day_sonnet".to_string(), 37.5);

        // Cycle 1: first sample, EMA initializes directly
        estimate_burn_rates(
            &instances,
            1.0,
            1,
            1,
            &mut ema_state,
            &baseline,
            &utilization,
            90.0,
            &hrs_left,
        );

        let ema = ema_state
            .get(&(
                "claude-sonnet-4-20250514".to_string(),
                "five_hour".to_string(),
            ))
            .unwrap();
        assert_eq!(ema.samples, 1);
        // First sample: ema = value directly = pct_per_worker = 2.0/1 = 2.0
        assert!((ema.ema_pct - 2.0).abs() < 1e-9);

        // Cycle 2: EMA updates with alpha=0.2
        let instances2 = vec![multi_window_record(
            "w1",
            "claude-sonnet-4-20250514",
            4.0, // doubled cost
            500_000,
            Some(4.0),
            Some(2.0),
            Some(3.0),
            44.0,
            40.0,
            62.0,
            60.0,
            73.0,
            70.0,
        )];

        estimate_burn_rates(
            &instances2,
            1.0,
            1,
            1,
            &mut ema_state,
            &baseline,
            &utilization,
            90.0,
            &hrs_left,
        );

        let ema2 = ema_state
            .get(&(
                "claude-sonnet-4-20250514".to_string(),
                "five_hour".to_string(),
            ))
            .unwrap();
        assert_eq!(ema2.samples, 2);
        // EMA = 0.2 * 4.0 + 0.8 * 2.0 = 0.8 + 1.6 = 2.4
        assert!((ema2.ema_pct - 2.4).abs() < 1e-9);
    }

    #[test]
    fn baseline_used_until_min_samples() {
        let baseline = BaselineBurnRates {
            pct_per_worker_per_hour: 99.0,
            dollars_per_worker_per_hour: 50.0,
        };
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        // Add 2 samples (below MIN_SAMPLES_FOR_EMA = 3)
        for i in 0..2 {
            let instances = vec![multi_window_record(
                "w1",
                "claude-sonnet-4-20250514",
                2.0,
                500_000,
                Some(1.0),
                Some(0.5),
                Some(0.75),
                41.0 + i as f64,
                40.0,
                60.5,
                60.0,
                70.75,
                70.0,
            )];

            let mut utilization = HashMap::new();
            utilization.insert("five_hour".to_string(), 41.0 + i as f64);
            utilization.insert("seven_day".to_string(), 60.5);
            utilization.insert("seven_day_sonnet".to_string(), 70.75);
            let mut hrs_left = HashMap::new();
            hrs_left.insert("five_hour".to_string(), 3.0);
            hrs_left.insert("seven_day".to_string(), 37.5);
            hrs_left.insert("seven_day_sonnet".to_string(), 37.5);

            estimate_burn_rates(
                &instances,
                1.0,
                1,
                1,
                &mut ema_state,
                &baseline,
                &utilization,
                90.0,
                &hrs_left,
            );
        }

        let ema = ema_state
            .get(&(
                "claude-sonnet-4-20250514".to_string(),
                "five_hour".to_string(),
            ))
            .unwrap();
        assert_eq!(ema.samples, 2);

        // Should use baseline because samples < 3
        let (pct, usd) = effective_burn_rate(ema, &baseline);
        assert!((pct - 99.0).abs() < 1e-9);
        assert!((usd - 50.0).abs() < 1e-9);
    }

    #[test]
    fn ema_used_after_min_samples() {
        let baseline = BaselineBurnRates {
            pct_per_worker_per_hour: 99.0,
            dollars_per_worker_per_hour: 50.0,
        };
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        // Add 3 samples (reaches MIN_SAMPLES_FOR_EMA)
        for i in 0..3 {
            let instances = vec![multi_window_record(
                "w1",
                "claude-sonnet-4-20250514",
                2.0,
                500_000,
                Some(1.0),
                Some(0.5),
                Some(0.75),
                41.0 + i as f64,
                40.0,
                60.5,
                60.0,
                70.75,
                70.0,
            )];

            let mut utilization = HashMap::new();
            utilization.insert("five_hour".to_string(), 41.0 + i as f64);
            utilization.insert("seven_day".to_string(), 60.5);
            utilization.insert("seven_day_sonnet".to_string(), 70.75);
            let mut hrs_left = HashMap::new();
            hrs_left.insert("five_hour".to_string(), 3.0);
            hrs_left.insert("seven_day".to_string(), 37.5);
            hrs_left.insert("seven_day_sonnet".to_string(), 37.5);

            estimate_burn_rates(
                &instances,
                1.0,
                1,
                1,
                &mut ema_state,
                &baseline,
                &utilization,
                90.0,
                &hrs_left,
            );
        }

        let ema = ema_state
            .get(&(
                "claude-sonnet-4-20250514".to_string(),
                "five_hour".to_string(),
            ))
            .unwrap();
        assert_eq!(ema.samples, 3);

        // Should use EMA because samples >= 3
        let (pct, usd) = effective_burn_rate(ema, &baseline);
        // EMA value, not baseline
        assert!(pct < 99.0, "Should use EMA not baseline, got {}", pct);
        assert!(usd < 50.0, "Should use EMA not baseline, got {}", usd);
    }

    #[test]
    fn each_window_independent_ema_sampling() {
        let baseline = BaselineBurnRates::default();
        let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();

        // Only five_hour has valid data; seven_day and seven_day_sonnet are null
        let instances = vec![multi_window_record(
            "w1",
            "claude-sonnet-4-20250514",
            2.0,
            500_000,
            Some(2.0),
            None,
            None, // only 5h valid
            42.0,
            40.0,
            60.0,
            60.0,
            70.0,
            70.0,
        )];

        let mut utilization = HashMap::new();
        utilization.insert("five_hour".to_string(), 42.0);
        let mut hrs_left = HashMap::new();
        hrs_left.insert("five_hour".to_string(), 3.0);

        estimate_burn_rates(
            &instances,
            1.0,
            1,
            1,
            &mut ema_state,
            &baseline,
            &utilization,
            90.0,
            &hrs_left,
        );

        let five_ema = ema_state.get(&(
            "claude-sonnet-4-20250514".to_string(),
            "five_hour".to_string(),
        ));
        let seven_ema = ema_state.get(&(
            "claude-sonnet-4-20250514".to_string(),
            "seven_day".to_string(),
        ));

        assert!(five_ema.is_some());
        assert_eq!(five_ema.unwrap().samples, 1);
        assert!(seven_ema.is_none());
    }

    #[test]
    fn compute_fleet_stats_empty() {
        let stats = compute_fleet_stats("five_hour", &[], 2);
        assert_eq!(stats.worker_count, 2);
        assert!((stats.mean_pct_hr - 0.0).abs() < 1e-9);
        assert!((stats.p75_pct_hr - 0.0).abs() < 1e-9);
    }

    #[test]
    fn compute_fleet_stats_single_instance() {
        let rate = InstanceBurnRate {
            session: "s1".to_string(),
            model: "m1".to_string(),
            window: "five_hour".to_string(),
            dollar_per_hour: 5.0,
            pct_per_hour: 2.0,
            elapsed_hours: 1.0,
        };
        let stats = compute_fleet_stats("five_hour", &[&rate], 1);
        assert!((stats.mean_pct_hr - 2.0).abs() < 1e-9);
        assert!((stats.p75_pct_hr - 2.0).abs() < 1e-9);
        assert!((stats.mean_usd_hr - 5.0).abs() < 1e-9);
        assert!((stats.std_pct_hr - 0.0).abs() < 1e-9);
    }

    #[test]
    fn update_ema_first_sample_initializes() {
        let mut ema = ModelWindowEma::default();
        update_ema(&mut ema, 3.0, 10.0);
        assert_eq!(ema.samples, 1);
        assert!((ema.ema_pct - 3.0).abs() < 1e-9);
        assert!((ema.ema_usd - 10.0).abs() < 1e-9);
    }

    #[test]
    fn update_ema_subsequent_smooths() {
        let mut ema = ModelWindowEma {
            ema_pct: 2.0,
            ema_usd: 5.0,
            samples: 1,
        };
        update_ema(&mut ema, 4.0, 10.0);
        assert_eq!(ema.samples, 2);
        // EMA = 0.2 * 4.0 + 0.8 * 2.0 = 0.8 + 1.6 = 2.4
        assert!((ema.ema_pct - 2.4).abs() < 1e-9);
        // EMA = 0.2 * 10.0 + 0.8 * 5.0 = 2.0 + 4.0 = 6.0
        assert!((ema.ema_usd - 6.0).abs() < 1e-9);
    }

    #[test]
    fn generate_window_forecast_basic() {
        let f = generate_window_forecast(
            "seven_day_sonnet",
            2.0,  // fleet_pct_hr
            72.0, // current utilization
            90.0, // target ceiling
            37.5, // hours remaining
            1.0,  // p75 per worker
            0.0,  // std_pct_hr (no spread)
        );

        assert!((f.remaining_pct - 18.0).abs() < 1e-9);
        assert!((f.hours_remaining - 37.5).abs() < 1e-9);
        assert!((f.fleet_pct_per_hour - 2.0).abs() < 1e-9);
        assert!((f.predicted_exhaustion_hours - 9.0).abs() < 1e-9);
        assert!(f.cutoff_risk); // 9h < 37.5h → exhausts before reset
        assert!((f.margin_hrs + 28.5).abs() < 1e-9); // 9 - 37.5 = -28.5 (negative = risky)
        assert_eq!(f.safe_worker_count, Some(0)); // floor(18 / (1.0 * 37.5)) = 0
                                                  // With zero stddev, all cone values equal the p50
        assert!((f.exh_hrs_p25 - 9.0).abs() < 1e-9);
        assert!((f.exh_hrs_p50 - 9.0).abs() < 1e-9);
        assert!((f.exh_hrs_p75 - 9.0).abs() < 1e-9);
        assert!((f.cone_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn generate_window_forecast_zero_burn() {
        let f = generate_window_forecast(
            "five_hour",
            0.0, // zero burn rate
            36.0,
            90.0,
            3.0,
            0.0,
            0.0, // std_pct_hr
        );

        assert!(f.predicted_exhaustion_hours.is_infinite());
        assert!(!f.cutoff_risk);
        assert_eq!(f.safe_worker_count, None);
    }

    #[test]
    fn build_burn_rate_state_from_ema() {
        let mut ema_state = HashMap::new();
        ema_state.insert(
            ("sonnet".to_string(), "five_hour".to_string()),
            ModelWindowEma {
                ema_pct: 1.5,
                ema_usd: 5.0,
                samples: 10,
            },
        );
        ema_state.insert(
            ("sonnet".to_string(), "seven_day".to_string()),
            ModelWindowEma {
                ema_pct: 0.8,
                ema_usd: 3.0,
                samples: 8,
            },
        );

        let state = build_burn_rate_state(
            &ema_state,
            69780,
            141350,
            2.03,
            2.0,
            true,
            10,
            12,
            Some(Utc::now()),
            crate::state::CalibrationState::default(),
        );

        let model = state.by_model.get("sonnet").unwrap();
        assert_eq!(model.samples, 10); // max across windows
        assert!((model.pct_per_worker_per_hour - 1.15).abs() < 0.01); // avg of 1.5 and 0.8
        assert!((model.dollars_per_worker_per_hour - 4.0).abs() < 0.01); // avg of 5.0 and 3.0
        assert_eq!(state.tokens_per_pct_peak, 69780);
        assert_eq!(state.tokens_per_pct_offpeak, 141350);
        assert!(state.promotion_validated);
    }

    #[test]
    fn log_capacity_forecast_does_not_panic() {
        let forecast = crate::state::CapacityForecast {
            five_hour: crate::state::WindowForecast {
                target_ceiling: 90.0,
                current_utilization: 36.0,
                remaining_pct: 54.0,
                hours_remaining: 1.5,
                fleet_pct_per_hour: 7.92,
                predicted_exhaustion_hours: 6.82,
                cutoff_risk: false,
                margin_hrs: 5.32, // 6.82 - 1.5 = 5.32 (positive = safe, exhaustion after reset)
                binding: false,
                safe_worker_count: None,
                ..Default::default()
            },
            seven_day: crate::state::WindowForecast {
                target_ceiling: 90.0,
                current_utilization: 72.6,
                remaining_pct: 17.4,
                hours_remaining: 37.5,
                fleet_pct_per_hour: 6.48,
                predicted_exhaustion_hours: 2.69,
                cutoff_risk: true,
                margin_hrs: -34.81, // 2.69 - 37.5 = -34.81 (negative = risky, exhaustion before reset)
                binding: false,
                safe_worker_count: None,
                ..Default::default()
            },
            seven_day_sonnet: crate::state::WindowForecast {
                target_ceiling: 90.0,
                current_utilization: 63.5,
                remaining_pct: 26.5,
                hours_remaining: 37.5,
                fleet_pct_per_hour: 9.0,
                predicted_exhaustion_hours: 2.94,
                cutoff_risk: true,
                margin_hrs: -34.56, // 2.94 - 37.5 = -34.56 (negative = risky, exhaustion before reset)
                binding: true,
                safe_worker_count: Some(2),
                ..Default::default()
            },
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 1.648,
            estimated_remaining_dollars: 46.1,
        };

        // Should not panic
        log_capacity_forecast(&forecast);
    }

    // -----------------------------------------------------------------------
    // Cross-Window Composite Risk Optimization tests
    // -----------------------------------------------------------------------

    /// Helper: build a WindowForecast with common fields defaulted.
    fn wf(
        remaining_pct: f64,
        hours_remaining: f64,
        fleet_pct_hr: f64,
        margin_hrs: f64,
        safe: Option<u32>,
    ) -> crate::state::WindowForecast {
        crate::state::WindowForecast {
            target_ceiling: 90.0,
            current_utilization: 90.0 - remaining_pct,
            remaining_pct,
            hours_remaining,
            fleet_pct_per_hour: fleet_pct_hr,
            predicted_exhaustion_hours: 0.0,
            cutoff_risk: false,
            margin_hrs,
            binding: false,
            safe_worker_count: safe,
            ..Default::default()
        }
    }

    #[test]
    fn window_cost_positive_margin() {
        // Safe window: 5 hrs margin, 10 hrs remaining → cost = 0.5
        assert_eq!(window_cost(5.0, 10.0), Some(0.5));
    }

    #[test]
    fn window_cost_zero_margin() {
        // At limit: 0 margin → cost = 0
        assert_eq!(window_cost(0.0, 10.0), Some(0.0));
    }

    #[test]
    fn window_cost_negative_margin() {
        // Will exhaust: -2 hrs margin, 10 hrs remaining → cost = -0.2
        assert_eq!(window_cost(-2.0, 10.0), Some(-0.2));
    }

    #[test]
    fn window_cost_zero_hours_remaining() {
        // Cannot compute: 0 hours remaining
        assert_eq!(window_cost(5.0, 0.0), None);
    }

    #[test]
    fn window_cost_negative_hours_remaining() {
        // Cannot compute: negative hours remaining
        assert_eq!(window_cost(5.0, -1.0), None);
    }

    #[test]
    fn composite_risk_basic() {
        // Three windows: binding (idx 0) has negative cost, others positive
        let forecasts = vec![
            wf(5.0, 10.0, 1.0, -2.0, Some(3)),    // cost = -0.2
            wf(60.0, 150.0, 1.0, 130.0, Some(0)), // cost = 0.867
            wf(50.0, 150.0, 1.0, 120.0, Some(0)), // cost = 0.8
        ];
        // binding_weight=2: (-0.2*2 + 0.867 + 0.8) / (2+1+1) = (−0.4+1.667)/4 = 0.317
        let risk = composite_risk(&forecasts, 0, 2.0).unwrap();
        assert!(risk > 0.3 && risk < 0.35);
    }

    #[test]
    fn composite_risk_empty_forecasts() {
        assert_eq!(composite_risk(&[], 0, 2.0), None);
    }

    #[test]
    fn composite_risk_invalid_binding_idx() {
        let forecasts = vec![wf(5.0, 10.0, 1.0, 2.0, None)];
        assert_eq!(composite_risk(&forecasts, 5, 2.0), None);
    }

    #[test]
    fn composite_risk_zero_hours_in_one_window() {
        // If one window has 0 hours_remaining, window_cost returns None,
        // which causes composite_risk to return None (uses ? operator)
        let forecasts = vec![
            wf(5.0, 10.0, 1.0, 2.0, None),
            wf(60.0, 0.0, 1.0, 130.0, None), // zero hours → None cost
            wf(50.0, 150.0, 1.0, 120.0, None),
        ];
        assert_eq!(composite_risk(&forecasts, 0, 2.0), None);
    }

    #[test]
    fn compute_composite_safe_workers_near_reset_allows_more() {
        // Scenario: 5h window near reset with 7d window ample
        //
        // 5h (binding, idx 0): util=85%, remaining=5%, hours_remaining=0.5
        //   fleet_pct_hr=3.0 (3 workers * 1%/hr each)
        //   safe_worker_count = floor(5 / (1.0 * 0.5)) = 10
        //   margin = 0.5 - (5/3) = -1.167
        //
        // 7d (idx 1): util=30%, remaining=60%, hours_remaining=150
        //   fleet_pct_hr=3.0
        //   safe_worker_count = floor(60 / (1.0 * 150)) = 0 (too conservative)
        //   margin = 150 - (60/3) = 130
        //
        // 7d_sonnet (idx 2): similar to 7d
        //   remaining=55%, hours_remaining=150, fleet_pct_hr=3.0
        //   margin = 150 - (55/3) = 131.67
        //
        // With composite: 7d safe over binding_hours(0.5):
        //   safe = floor(60 / (1.0 * 0.5)) = 120 >> 10
        let forecasts = vec![
            wf(5.0, 0.5, 3.0, -1.167, Some(10)),   // 5h binding
            wf(60.0, 150.0, 3.0, 130.0, Some(0)),  // 7d
            wf(55.0, 150.0, 3.0, 131.67, Some(0)), // 7d_sonnet
        ];

        let result = compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 3);
        assert!(
            result.is_some(),
            "composite should activate for near-reset scenario"
        );
        let composite_safe = result.unwrap();
        assert!(
            composite_safe > 10,
            "composite safe ({}) should exceed binding safe (10)",
            composite_safe
        );
        // 7d: floor(60 / (1.0 * 0.5)) = 120
        // 7d_sonnet: floor(55 / (1.0 * 0.5)) = 110
        // max = 120
        assert_eq!(composite_safe, 120);
    }

    #[test]
    fn compute_composite_safe_workers_all_windows_stressed_returns_none() {
        // All windows near exhaustion → no improvement possible
        let forecasts = vec![
            wf(2.0, 1.0, 3.0, -1.0, Some(2)), // binding, cost = -1.0
            wf(3.0, 5.0, 3.0, -4.0, Some(0)), // cost = -0.8 > 0? No: -0.8 < 0
            wf(1.0, 2.0, 3.0, -0.5, Some(0)), // cost = -0.25 > 0? No
        ];

        // With cost_threshold=0.0, no non-binding window has cost > 0
        let result = compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 3);
        assert!(result.is_none());
    }

    #[test]
    fn compute_composite_safe_workers_non_binding_below_threshold_skipped() {
        // 7d has slightly negative cost → skipped with threshold 0.0
        let forecasts = vec![
            wf(5.0, 0.5, 3.0, -1.167, Some(10)),  // 5h binding
            wf(60.0, 150.0, 3.0, -10.0, Some(0)), // 7d: cost = -10/150 ≈ -0.067 < 0
        ];

        let result = compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 3);
        assert!(
            result.is_none(),
            "7d below threshold should not activate composite"
        );
    }

    #[test]
    fn compute_composite_safe_workers_negative_threshold_allows_stressed() {
        // Same as above but with negative threshold → 7d cost > -0.5
        let forecasts = vec![
            wf(5.0, 0.5, 3.0, -1.167, Some(10)),  // 5h binding
            wf(60.0, 150.0, 3.0, -10.0, Some(0)), // 7d: cost ≈ -0.067 > -0.5
        ];

        let result = compute_composite_safe_workers(&forecasts, 0, 2.0, -0.5, 3);
        assert!(
            result.is_some(),
            "negative threshold should allow slightly stressed windows"
        );
        assert!(result.unwrap() > 10);
    }

    #[test]
    fn compute_composite_safe_workers_no_improvement_returns_none() {
        // Non-binding windows have ample cost but can't support more workers
        // (very small remaining_pct relative to burn rate)
        let forecasts = vec![
            wf(5.0, 0.5, 3.0, -1.167, Some(10)), // 5h binding, safe=10
            wf(0.1, 150.0, 3.0, 130.0, Some(0)), // 7d: safe over 0.5h = floor(0.1/(1.0*0.5)) = 0
        ];

        let result = compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 3);
        assert!(result.is_none(), "no improvement over binding → None");
    }

    #[test]
    fn compute_composite_safe_workers_empty_forecasts() {
        assert_eq!(compute_composite_safe_workers(&[], 0, 2.0, 0.0, 3), None);
    }

    #[test]
    fn compute_composite_safe_workers_invalid_binding_idx() {
        let forecasts = vec![wf(5.0, 10.0, 1.0, 2.0, None)];
        assert_eq!(
            compute_composite_safe_workers(&forecasts, 5, 2.0, 0.0, 3),
            None
        );
    }

    #[test]
    fn compute_composite_safe_workers_zero_current_workers() {
        let forecasts = vec![
            wf(5.0, 0.5, 3.0, -1.167, Some(10)),
            wf(60.0, 150.0, 3.0, 130.0, Some(0)),
        ];
        assert_eq!(
            compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 0),
            None
        );
    }

    #[test]
    fn compute_composite_safe_workers_zero_binding_hours() {
        let forecasts = vec![
            wf(5.0, 0.0, 3.0, -1.167, Some(10)), // binding hours = 0
            wf(60.0, 150.0, 3.0, 130.0, Some(0)),
        ];
        assert_eq!(
            compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 3),
            None
        );
    }

    #[test]
    fn compute_composite_safe_workers_non_binding_zero_fleet_pct() {
        // Non-binding window has no burn data → can't compute per-worker rate
        let forecasts = vec![
            wf(5.0, 0.5, 3.0, -1.167, Some(10)),
            wf(60.0, 150.0, 0.0, 130.0, Some(0)), // fleet_pct_hr = 0
        ];
        let result = compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 3);
        // No non-binding window contributes → no improvement
        assert!(result.is_none());
    }

    #[test]
    fn compute_composite_safe_workers_binding_safe_is_none_uses_zero() {
        // Binding window has no safe_worker_count → baseline is 0
        // Non-binding has positive cost → composite can still improve
        let forecasts = vec![
            wf(5.0, 0.5, 3.0, -1.167, None),   // 5h binding, no safe count
            wf(60.0, 150.0, 3.0, 130.0, None), // 7d, safe over 0.5h = 120
        ];

        let result = compute_composite_safe_workers(&forecasts, 0, 2.0, 0.0, 3);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), 120);
    }

    // -----------------------------------------------------------------------
    // Per-Window Risk Score tests (composite risk edge cases)
    // -----------------------------------------------------------------------

    #[test]
    fn compute_risk_score_five_hour_higher_risk_than_seven_day_same_margin() {
        // Same margin_pct (20%) but 5h window has 3x duration weight → higher risk
        let margin_pct = 0.2; // 20% margin
        let hours_remaining_5h = 5.0;
        let hours_remaining_7d = 168.0; // 7 days
        let cone_ratio = 1.0; // no volatility

        // Scale margin_hrs to achieve the same margin_pct for each window
        let margin_hrs_5h = margin_pct * hours_remaining_5h; // 1.0
        let margin_hrs_7d = margin_pct * hours_remaining_7d; // 33.6

        let risk_5h =
            compute_risk_score("five_hour", margin_hrs_5h, hours_remaining_5h, cone_ratio).unwrap();
        let risk_7d =
            compute_risk_score("seven_day", margin_hrs_7d, hours_remaining_7d, cone_ratio).unwrap();

        // Same urgency (1.0 - 0.2 = 0.8) but 5h has 3x the duration weight
        // risk_5h = 0.8 * 3.0 * 1.0 = 2.4
        // risk_7d = 0.8 * 1.0 * 1.0 = 0.8
        // ratio = 3.0
        assert!(risk_5h > risk_7d);
        assert!((risk_5h / risk_7d - 3.0).abs() < 0.01);
    }

    #[test]
    fn compute_risk_score_negative_margin_increases_risk() {
        // Negative margin (will exhaust) → urgency > 1.0 → higher risk
        let margin_hrs = -2.0; // will exhaust 2 hours before reset
        let hours_remaining = 10.0;
        let cone_ratio = 1.0;

        let risk =
            compute_risk_score("seven_day", margin_hrs, hours_remaining, cone_ratio).unwrap();

        // margin_pct = -2/10 = -0.2, urgency = 1.0 - (-0.2) = 1.2
        // risk_score = 1.2 * 1.0 * 1.0 = 1.2
        assert!((risk - 1.2).abs() < 0.01);
    }

    #[test]
    fn compute_risk_score_wide_cone_increases_risk() {
        // Wide cone (high uncertainty) amplifies risk
        let margin_hrs = 2.0;
        let hours_remaining = 10.0;
        let cone_ratio_narrow = 1.0; // no spread
        let cone_ratio_wide = 2.5; // wide spread

        let risk_narrow =
            compute_risk_score("seven_day", margin_hrs, hours_remaining, cone_ratio_narrow)
                .unwrap();
        let risk_wide =
            compute_risk_score("seven_day", margin_hrs, hours_remaining, cone_ratio_wide).unwrap();

        // Wide cone should have 2.5x the risk
        assert!((risk_wide / risk_narrow - 2.5).abs() < 0.1);
    }

    #[test]
    fn compute_risk_score_zero_hours_returns_none() {
        // Cannot compute risk when hours_remaining is 0
        let result = compute_risk_score("five_hour", 1.0, 0.0, 1.0);
        assert!(result.is_none());
    }

    #[test]
    fn compute_risk_score_all_factors_combine() {
        // Test full formula: (1.0 - margin_pct) * duration_weight * volatility
        let margin_hrs = -1.0; // will exhaust
        let hours_remaining = 5.0; // 5h window
        let cone_ratio = 2.0; // moderate volatility

        let risk =
            compute_risk_score("five_hour", margin_hrs, hours_remaining, cone_ratio).unwrap();

        // margin_pct = -1/5 = -0.2
        // urgency = 1.0 - (-0.2) = 1.2
        // duration_weight = 3.0 (five_hour)
        // volatility = 2.0
        // risk = 1.2 * 3.0 * 2.0 = 7.2
        assert!((risk - 7.2).abs() < 0.01);
    }

    #[test]
    fn compute_risk_score_positive_margin_decreases_risk() {
        // Positive margin (safe) → urgency < 1.0 → lower risk
        let margin_hrs = 5.0; // 5 hours of headroom
        let hours_remaining = 10.0; // 50% margin
        let cone_ratio = 1.0;

        let risk =
            compute_risk_score("seven_day", margin_hrs, hours_remaining, cone_ratio).unwrap();

        // margin_pct = 5/10 = 0.5
        // urgency = 1.0 - 0.5 = 0.5
        // risk = 0.5 * 1.0 * 1.0 = 0.5
        assert!((risk - 0.5).abs() < 0.01);
    }

    #[test]
    fn compute_risk_score_duration_weights_correct() {
        // Verify duration weights: 5h > 7d-sonnet > 7d
        let margin_hrs = 1.0;
        let hours_remaining = 10.0;
        let cone_ratio = 1.0;

        let risk_5h =
            compute_risk_score("five_hour", margin_hrs, hours_remaining, cone_ratio).unwrap();
        let risk_7ds =
            compute_risk_score("seven_day_sonnet", margin_hrs, hours_remaining, cone_ratio)
                .unwrap();
        let risk_7d =
            compute_risk_score("seven_day", margin_hrs, hours_remaining, cone_ratio).unwrap();

        // All have same margin_pct and volatility, so ratios should equal duration weight ratios
        assert!((risk_5h / risk_7d - 3.0).abs() < 0.1); // 5h weight is 3x 7d
        assert!((risk_5h / risk_7ds - 2.0).abs() < 0.1); // 5h weight is 2x 7d-sonnet
        assert!((risk_7ds / risk_7d - 1.5).abs() < 0.1); // 7d-sonnet weight is 1.5x 7d
    }

    #[test]
    fn compute_risk_score_edge_case_exactly_at_limit() {
        // Zero margin (at limit) → urgency = 1.0
        let margin_hrs = 0.0;
        let hours_remaining = 10.0;
        let cone_ratio = 1.0;

        let risk =
            compute_risk_score("seven_day", margin_hrs, hours_remaining, cone_ratio).unwrap();

        // margin_pct = 0, urgency = 1.0, risk = 1.0 * 1.0 * 1.0 = 1.0
        assert!((risk - 1.0).abs() < 0.01);
    }

    #[test]
    fn compute_risk_score_volatility_min_is_1_0() {
        // Even with cone_ratio < 1.0 (invalid), volatility factor is clamped to 1.0
        let margin_hrs = 1.0;
        let hours_remaining = 10.0;
        let cone_ratio = 0.5; // Below 1.0

        let risk =
            compute_risk_score("seven_day", margin_hrs, hours_remaining, cone_ratio).unwrap();

        // volatility = max(0.5, 1.0) = 1.0
        // margin_pct = 0.1, urgency = 0.9
        // risk = 0.9 * 1.0 * 1.0 = 0.9
        assert!((risk - 0.9).abs() < 0.01);
    }
}
