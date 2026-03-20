//! Per-instance burn rate computation
//!
//! Computes dollar_per_hour and pct_per_hour from token collector interval records.
//! Each window (5h, 7d, 7d-sonnet) is computed independently with guard conditions
//! to reject unreliable data.

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
}
