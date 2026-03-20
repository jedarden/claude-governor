//! Peak/off-peak schedule calculator
//!
//! Handles:
//! - Peak hour detection (8AM-2PM ET weekdays)
//! - Promotion loading and multiplier calculation
//! - Effective hours remaining accounting for off-peak multipliers

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Peak hours: 08:00-14:00 ET (half-open: 08:00 inclusive, 14:00 exclusive)
const PEAK_START_HOUR_ET: u32 = 8;
const PEAK_END_HOUR_ET: u32 = 14;

/// Promotion definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Promotion {
    /// Human-readable name
    pub name: String,

    /// Start date (inclusive) in YYYY-MM-DD format
    pub start_date: String,

    /// End date (exclusive) in YYYY-MM-DD format
    pub end_date: String,

    /// Peak start hour in ET (default: 8)
    #[serde(default = "default_peak_start")]
    pub peak_start_hour_et: u32,

    /// Peak end hour in ET (default: 14)
    #[serde(default = "default_peak_end")]
    pub peak_end_hour_et: u32,

    /// Off-peak multiplier (e.g., 2.0 for 2x off-peak)
    pub offpeak_multiplier: f64,

    /// Which windows this promotion applies to
    pub applies_to: Vec<String>,
}

fn default_peak_start() -> u32 {
    PEAK_START_HOUR_ET
}

fn default_peak_end() -> u32 {
    PEAK_END_HOUR_ET
}

/// Load promotions from a JSON file
pub fn load_promotions(path: &Path) -> Vec<Promotion> {
    if !path.exists() {
        log::debug!("[schedule] no promotions file at {}, returning empty", path.display());
        return Vec::new();
    }

    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[schedule] failed to read promotions file: {}", e);
            return Vec::new();
        }
    };

    match serde_json::from_str(&contents) {
        Ok(promos) => promos,
        Err(e) => {
            log::warn!("[schedule] failed to parse promotions file: {}", e);
            Vec::new()
        }
    }
}

/// Check if a timestamp falls on a weekend (Saturday or Sunday) in ET
pub fn is_weekend(t: DateTime<Utc>) -> bool {
    let et = to_eastern(t);
    let weekday = et.weekday();
    weekday == chrono::Weekday::Sat || weekday == chrono::Weekday::Sun
}

/// Convert UTC to Eastern Time (handles DST automatically via chrono-tz)
fn to_eastern(t: DateTime<Utc>) -> DateTime<chrono_tz::Tz> {
    t.with_timezone(&chrono_tz::America::New_York)
}

/// Check if a timestamp is during peak hours
///
/// Peak = 08:00-14:00 ET weekdays (half-open: 08:00 is peak, 14:00 is off-peak)
/// Weekends are always off-peak.
pub fn is_peak_at(t: DateTime<Utc>) -> bool {
    if is_weekend(t) {
        return false;
    }

    let et = to_eastern(t);
    let hour = et.hour();

    // Half-open interval: [08:00, 14:00)
    hour >= PEAK_START_HOUR_ET && hour < PEAK_END_HOUR_ET
}

/// Check if now is during peak hours
pub fn is_peak_now() -> bool {
    is_peak_at(Utc::now())
}

/// Get the active promotion multiplier at a specific time
///
/// Returns the off-peak multiplier if a promotion is active and the time is off-peak.
/// Returns 1.0 if no promotion is active or if the time is during peak hours.
pub fn get_multiplier_at(t: DateTime<Utc>, promotions: &[Promotion]) -> f64 {
    // If peak hours, always 1.0
    if is_peak_at(t) {
        return 1.0;
    }

    // Check for active promotion
    for promo in promotions {
        if is_promo_active_at(t, promo) {
            return promo.offpeak_multiplier;
        }
    }

    1.0
}

/// Get the current multiplier
pub fn current_multiplier(promotions: &[Promotion]) -> f64 {
    get_multiplier_at(Utc::now(), promotions)
}

/// Check if a promotion is active at a specific time
fn is_promo_active_at(t: DateTime<Utc>, promo: &Promotion) -> bool {
    // Parse dates as ET dates (start of day in ET)
    let et = to_eastern(t);
    let et_date = et.date_naive();

    let start_date: chrono::NaiveDate = match promo.start_date.parse() {
        Ok(d) => d,
        Err(_) => {
            log::warn!("[schedule] invalid start_date format: {}", promo.start_date);
            return false;
        }
    };

    let end_date: chrono::NaiveDate = match promo.end_date.parse() {
        Ok(d) => d,
        Err(_) => {
            log::warn!("[schedule] invalid end_date format: {}", promo.end_date);
            return false;
        }
    };

    // Active if start_date <= current_date < end_date
    et_date >= start_date && et_date < end_date
}

/// Calculate effective hours remaining accounting for off-peak multipliers
///
/// Walks forward from now to reset_time in 1-minute steps, applying the
/// multiplier at each step. This accounts for the fact that off-peak hours
/// during promotions provide "2x" value.
///
/// Example: 40 hours remaining with 30 hours off-peak during 2x promo
/// = 30 * 2 + 10 * 1 = 70 effective hours
pub fn effective_hours_remaining(
    reset_time: DateTime<Utc>,
    promotions: &[Promotion],
) -> f64 {
    effective_hours_remaining_from(Utc::now(), reset_time, promotions)
}

/// Calculate effective hours remaining from a specific start time
pub fn effective_hours_remaining_from(
    start_time: DateTime<Utc>,
    reset_time: DateTime<Utc>,
    promotions: &[Promotion],
) -> f64 {
    if reset_time <= start_time {
        return 0.0;
    }

    let mut effective_hours = 0.0;
    let mut current = start_time;

    // Walk in 1-minute steps
    while current < reset_time {
        let step_end = std::cmp::min(current + chrono::Duration::minutes(1), reset_time);
        let step_hours = (step_end - current).num_seconds() as f64 / 3600.0;

        let multiplier = get_multiplier_at(current, promotions);
        effective_hours += step_hours * multiplier;

        current = step_end;
    }

    effective_hours
}

/// Find the next promotion transition time between now and a deadline
///
/// Returns (transition_time, multiplier_before, multiplier_after) if a transition exists,
/// None if the multiplier stays constant.
pub fn find_next_transition(
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    promotions: &[Promotion],
) -> Option<(DateTime<Utc>, f64, f64)> {
    if end_time <= start_time {
        return None;
    }

    let mut current = start_time;
    let initial_mult = get_multiplier_at(current, promotions);

    // Walk in 1-minute steps to find first change
    while current < end_time {
        let next_time = current + chrono::Duration::minutes(1);
        let next_mult = get_multiplier_at(next_time, promotions);

        if (next_mult - initial_mult).abs() > 1e-9 {
            return Some((next_time, initial_mult, next_mult));
        }

        current = next_time;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    // Test promotion: March 15-25, 2026 with 2x off-peak
    fn test_promo() -> Promotion {
        Promotion {
            name: "March 2026 Promo".to_string(),
            start_date: "2026-03-15".to_string(),
            end_date: "2026-03-25".to_string(),
            peak_start_hour_et: 8,
            peak_end_hour_et: 14,
            offpeak_multiplier: 2.0,
            applies_to: vec!["seven_day_sonnet".to_string()],
        }
    }

    // Helper: create UTC time from ET components
    fn et_to_utc(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        // March 2026 is EDT (DST starts March 8, 2026)
        // EDT = UTC-4
        chrono_tz::America::New_York
            .with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn weekday_morning_before_peak_is_off_peak() {
        // Monday March 16, 2026 at 7:59 AM ET -> off-peak
        let t = et_to_utc(2026, 3, 16, 7, 59);
        assert!(!is_peak_at(t));
    }

    #[test]
    fn weekday_at_8am_is_peak() {
        // Monday March 16, 2026 at 8:00 AM ET -> peak
        let t = et_to_utc(2026, 3, 16, 8, 0);
        assert!(is_peak_at(t));
    }

    #[test]
    fn weekday_at_2pm_is_off_peak() {
        // Monday March 16, 2026 at 2:00 PM ET -> off-peak (half-open interval)
        let t = et_to_utc(2026, 3, 16, 14, 0);
        assert!(!is_peak_at(t));
    }

    #[test]
    fn weekday_at_1pm_is_peak() {
        // Monday March 16, 2026 at 1:59 PM ET -> peak
        let t = et_to_utc(2026, 3, 16, 13, 59);
        assert!(is_peak_at(t));
    }

    #[test]
    fn weekend_is_always_off_peak() {
        // Saturday March 21, 2026 at 10:00 AM ET -> off-peak
        let t = et_to_utc(2026, 3, 21, 10, 0);
        assert!(is_weekend(t));
        assert!(!is_peak_at(t));

        // Sunday March 22, 2026 at 11:00 AM ET -> off-peak
        let t = et_to_utc(2026, 3, 22, 11, 0);
        assert!(is_weekend(t));
        assert!(!is_peak_at(t));
    }

    #[test]
    fn multiplier_during_peak_is_1() {
        let promos = vec![test_promo()];
        // Monday March 16, 2026 at 10:00 AM ET (peak, during promo)
        let t = et_to_utc(2026, 3, 16, 10, 0);
        assert!((get_multiplier_at(t, &promos) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_off_peak_during_promo_is_2x() {
        let promos = vec![test_promo()];
        // Monday March 16, 2026 at 6:00 AM ET (off-peak, during promo)
        let t = et_to_utc(2026, 3, 16, 6, 0);
        assert!((get_multiplier_at(t, &promos) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_after_promo_ends_is_1() {
        let promos = vec![test_promo()];
        // March 26, 2026 at 6:00 AM ET (after promo ended on 25th)
        let t = et_to_utc(2026, 3, 26, 6, 0);
        assert!((get_multiplier_at(t, &promos) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_before_promo_starts_is_1() {
        let promos = vec![test_promo()];
        // March 14, 2026 at 6:00 AM ET (before promo starts on 15th)
        let t = et_to_utc(2026, 3, 14, 6, 0);
        assert!((get_multiplier_at(t, &promos) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn effective_hours_with_no_promo_equals_raw() {
        let start = et_to_utc(2026, 3, 10, 0, 0);
        let reset = start + Duration::hours(40);
        let promos: Vec<Promotion> = vec![]; // No promo active

        let effective = effective_hours_remaining_from(start, reset, &promos);
        assert!((effective - 40.0).abs() < 0.1);
    }

    #[test]
    fn effective_hours_with_offpeak_promo_is_greater() {
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 6:00 AM ET (off-peak)
        // End: Wednesday March 18, 2026 at 6:00 AM ET (48 hours later)
        let start = et_to_utc(2026, 3, 16, 6, 0);
        let reset = start + Duration::hours(48);

        let effective = effective_hours_remaining_from(start, reset, &promos);

        // 48 hours with 2x off-peak should be > 48
        assert!(effective > 48.0, "expected > 48, got {}", effective);

        // Approximately: 48 hours = 6h peak/day * 2 days * 1x + 18h off-peak/day * 2 days * 2x
        // = 12 + 72 = 84 effective hours
        // But exact calculation depends on timing
        assert!(effective > 70.0, "expected significantly more than 48, got {}", effective);
    }

    #[test]
    fn effective_hours_with_transition() {
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 13:30 ET (peak)
        // End: Monday March 16, 2026 at 15:30 ET (2 hours, includes transition at 14:00)
        let start = et_to_utc(2026, 3, 16, 13, 30);
        let reset = et_to_utc(2026, 3, 16, 15, 30);

        let effective = effective_hours_remaining_from(start, reset, &promos);

        // 0.5h peak * 1x + 1.5h off-peak * 2x = 0.5 + 3.0 = 3.5
        assert!((effective - 3.5).abs() < 0.1, "expected ~3.5, got {}", effective);
    }

    #[test]
    fn find_transition_detects_peak_to_offpeak() {
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 13:00 ET (peak)
        // End: Monday March 16, 2026 at 15:00 ET
        let start = et_to_utc(2026, 3, 16, 13, 0);
        let end = et_to_utc(2026, 3, 16, 15, 0);

        let transition = find_next_transition(start, end, &promos);
        assert!(transition.is_some());

        let (t, before, after) = transition.unwrap();
        // Transition should be at 14:00 ET
        let t_et = to_eastern(t);
        assert_eq!(t_et.hour(), 14);
        assert_eq!(t_et.minute(), 0);
        assert!((before - 1.0).abs() < 1e-9);
        assert!((after - 2.0).abs() < 1e-9);
    }

    #[test]
    fn find_transition_detects_offpeak_to_peak() {
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 7:00 ET (off-peak)
        // End: Monday March 16, 2026 at 9:00 ET
        let start = et_to_utc(2026, 3, 16, 7, 0);
        let end = et_to_utc(2026, 3, 16, 9, 0);

        let transition = find_next_transition(start, end, &promos);
        assert!(transition.is_some());

        let (t, before, after) = transition.unwrap();
        // Transition should be at 08:00 ET
        let t_et = to_eastern(t);
        assert_eq!(t_et.hour(), 8);
        assert_eq!(t_et.minute(), 0);
        assert!((before - 2.0).abs() < 1e-9);
        assert!((after - 1.0).abs() < 1e-9);
    }

    #[test]
    fn no_transition_returns_none() {
        let promos = vec![test_promo()];

        // Entirely within peak hours
        let start = et_to_utc(2026, 3, 16, 9, 0);
        let end = et_to_utc(2026, 3, 16, 11, 0);

        let transition = find_next_transition(start, end, &promos);
        assert!(transition.is_none());
    }
}
