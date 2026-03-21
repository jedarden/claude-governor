//! Per-instance burn rate computation and promotion validation
//!
//! Computes dollar_per_hour and pct_per_hour from token collector interval records.
//! Each window (5h, 7d, 7d-sonnet) is computed independently with guard conditions
//! to reject unreliable data.
//!
//! Also validates declared promotion multipliers against observed consumption data
//! by comparing median tokens-per-pct between peak and off-peak intervals.

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

/// Compute per-instance per-window burn rates from an interval record
///
/// Returns a burn rate entry for each window that passes all guard conditions:
/// - Elapsed time >= 2 minutes
/// - Window pct_delta is not null
/// - Window pct_delta is not zero when tokens > 0 (API rounding artifact)
/// - No window reset detected (utilization drop > 1pp)
pub fn compute_instance_burn(
    record: &InstanceRecord,
    elapsed_hours: f64,
) -> Vec<InstanceBurnRate> {
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
const MIN_VALIDATION_SAMPLES: usize = 5;

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
    if len % 2 == 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a WindowUtilization for a named window
    fn win(
        name: &str,
        pct_delta: Option<f64>,
        current: f64,
        previous: f64,
    ) -> WindowUtilization {
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
                win("five_hour", Some(2.0), 38.0, 40.0),   // reset: drop of 2pp
                win("seven_day", Some(3.0), 65.0, 62.0),    // normal: rise of 3pp
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
                win("five_hour", None, 41.0, 40.0),             // null pct_delta -> skip
                win("seven_day", Some(0.0), 63.0, 63.0),        // zero pct + tokens -> skip
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
        assert!(result.validated, "expected validated, got reason: {:?}", result.reason);
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
        assert!(result.reason.unwrap().contains("median peak tokens_per_pct is zero"));
    }
}
