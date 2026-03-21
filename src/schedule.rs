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

#[cfg(test)]
use tempfile;

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

/// Get the active promotion multiplier at a specific time for a specific window
///
/// Returns the off-peak multiplier if a promotion is active, the time is off-peak,
/// AND the promotion's `applies_to` list includes `window`.
/// Returns 1.0 if the time is peak, no promotion is active, or no promotion applies to `window`.
pub fn get_multiplier_at(t: DateTime<Utc>, promotions: &[Promotion], window: &str) -> f64 {
    // If peak hours, always 1.0
    if is_peak_at(t) {
        return 1.0;
    }

    // Check for active promotion that applies to this window
    for promo in promotions {
        if promo.applies_to.iter().any(|w| w == window) && is_promo_active_at(t, promo) {
            return promo.offpeak_multiplier;
        }
    }

    1.0
}

/// Get the current multiplier for a specific window
pub fn current_multiplier(promotions: &[Promotion], window: &str) -> f64 {
    get_multiplier_at(Utc::now(), promotions, window)
}

/// Check whether any promotion is currently active (in its date range) at time t,
/// regardless of peak/off-peak status.
pub fn is_any_promo_active_at(t: DateTime<Utc>, promotions: &[Promotion]) -> bool {
    promotions.iter().any(|p| is_promo_active_at(t, p))
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
/// multiplier for `window` at each step. Only promotions whose `applies_to`
/// includes `window` contribute a multiplier > 1.0.
///
/// Example: 40 hours remaining with 30 hours off-peak during 2x promo
/// = 30 * 2 + 10 * 1 = 70 effective hours
pub fn effective_hours_remaining(
    reset_time: DateTime<Utc>,
    promotions: &[Promotion],
    window: &str,
) -> f64 {
    effective_hours_remaining_from(Utc::now(), reset_time, promotions, window)
}

/// Calculate effective hours remaining from a specific start time for a specific window
pub fn effective_hours_remaining_from(
    start_time: DateTime<Utc>,
    reset_time: DateTime<Utc>,
    promotions: &[Promotion],
    window: &str,
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

        let multiplier = get_multiplier_at(current, promotions, window);
        effective_hours += step_hours * multiplier;

        current = step_end;
    }

    effective_hours
}

/// Find the next promotion transition time between now and a deadline for a specific window
///
/// Returns (transition_time, multiplier_before, multiplier_after) if a transition exists,
/// None if the multiplier stays constant for `window`.
pub fn find_next_transition(
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    promotions: &[Promotion],
    window: &str,
) -> Option<(DateTime<Utc>, f64, f64)> {
    if end_time <= start_time {
        return None;
    }

    let mut current = start_time;
    let initial_mult = get_multiplier_at(current, promotions, window);

    // Walk in 1-minute steps to find first change
    while current < end_time {
        let next_time = current + chrono::Duration::minutes(1);
        let next_mult = get_multiplier_at(next_time, promotions, window);

        if (next_mult - initial_mult).abs() > 1e-9 {
            return Some((next_time, initial_mult, next_mult));
        }

        current = next_time;
    }

    None
}

/// Information about an upcoming peak/off-peak transition
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// When the transition occurs (UTC)
    pub at: DateTime<Utc>,
    /// Multiplier before the transition
    pub multiplier_before: f64,
    /// Multiplier after the transition
    pub multiplier_after: f64,
    /// Minutes until the transition
    pub minutes_until: i64,
}

/// Get the next upcoming peak/off-peak transition for a specific window
///
/// Looks ahead from now until the given deadline (typically the window reset time).
/// Returns `None` if no transition occurs before the deadline.
///
/// This is the primary API for the governor loop's pre-scaling logic.
pub fn next_transition(
    deadline: DateTime<Utc>,
    promotions: &[Promotion],
    window: &str,
) -> Option<Transition> {
    let now = Utc::now();
    let (at, before, after) = find_next_transition(now, deadline, promotions, window)?;
    let minutes_until = (at - now).num_minutes();

    Some(Transition {
        at,
        multiplier_before: before,
        multiplier_after: after,
        minutes_until,
    })
}

/// Get the next upcoming peak/off-peak transition from a specific start time (for testing)
///
/// This is the same as `next_transition` but accepts an explicit `now` parameter
/// for deterministic testing.
pub fn next_transition_from(
    now: DateTime<Utc>,
    deadline: DateTime<Utc>,
    promotions: &[Promotion],
    window: &str,
) -> Option<Transition> {
    let (at, before, after) = find_next_transition(now, deadline, promotions, window)?;
    let minutes_until = (at - now).num_minutes();

    Some(Transition {
        at,
        multiplier_before: before,
        multiplier_after: after,
        minutes_until,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    // Test promotion: March 15-25, 2026 with 2x off-peak for seven_day_sonnet only
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
        assert!((get_multiplier_at(t, &promos, "seven_day_sonnet") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_off_peak_during_promo_is_2x() {
        let promos = vec![test_promo()];
        // Monday March 16, 2026 at 6:00 AM ET (off-peak, during promo)
        let t = et_to_utc(2026, 3, 16, 6, 0);
        assert!((get_multiplier_at(t, &promos, "seven_day_sonnet") - 2.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_after_promo_ends_is_1() {
        let promos = vec![test_promo()];
        // March 26, 2026 at 6:00 AM ET (after promo ended on 25th)
        let t = et_to_utc(2026, 3, 26, 6, 0);
        assert!((get_multiplier_at(t, &promos, "seven_day_sonnet") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_before_promo_starts_is_1() {
        let promos = vec![test_promo()];
        // March 14, 2026 at 6:00 AM ET (before promo starts on 15th)
        let t = et_to_utc(2026, 3, 14, 6, 0);
        assert!((get_multiplier_at(t, &promos, "seven_day_sonnet") - 1.0).abs() < 1e-9);
    }

    /// Regression test: applies_to filtering — only listed windows get the boost
    #[test]
    fn multiplier_applies_to_filtering() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];
        // Monday March 16, 2026 at 6:00 AM ET (off-peak, during promo)
        let t = et_to_utc(2026, 3, 16, 6, 0);

        // Listed window gets 2x
        assert!(
            (get_multiplier_at(t, &promos, "seven_day_sonnet") - 2.0).abs() < 1e-9,
            "seven_day_sonnet should get 2x (in applies_to)"
        );
        // Unlisted windows always get 1.0x
        assert!(
            (get_multiplier_at(t, &promos, "five_hour") - 1.0).abs() < 1e-9,
            "five_hour should get 1.0x (not in applies_to)"
        );
        assert!(
            (get_multiplier_at(t, &promos, "seven_day") - 1.0).abs() < 1e-9,
            "seven_day should get 1.0x (not in applies_to)"
        );
    }

    #[test]
    fn effective_hours_with_no_promo_equals_raw() {
        let start = et_to_utc(2026, 3, 10, 0, 0);
        let reset = start + Duration::hours(40);
        let promos: Vec<Promotion> = vec![]; // No promo active

        let effective = effective_hours_remaining_from(start, reset, &promos, "five_hour");
        assert!((effective - 40.0).abs() < 0.1);
    }

    #[test]
    fn effective_hours_with_offpeak_promo_is_greater() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 6:00 AM ET (off-peak)
        // End: Wednesday March 18, 2026 at 6:00 AM ET (48 hours later)
        let start = et_to_utc(2026, 3, 16, 6, 0);
        let reset = start + Duration::hours(48);

        // seven_day_sonnet gets the 2x boost
        let effective = effective_hours_remaining_from(start, reset, &promos, "seven_day_sonnet");
        assert!(effective > 48.0, "expected > 48, got {}", effective);
        assert!(effective > 70.0, "expected significantly more than 48, got {}", effective);

        // five_hour is NOT in applies_to: effective hours == raw hours
        let effective_5h = effective_hours_remaining_from(start, reset, &promos, "five_hour");
        assert!(
            (effective_5h - 48.0).abs() < 0.1,
            "five_hour should equal raw 48h (not in applies_to), got {}",
            effective_5h
        );
    }

    #[test]
    fn effective_hours_with_transition() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 13:30 ET (peak)
        // End: Monday March 16, 2026 at 15:30 ET (2 hours, includes transition at 14:00)
        let start = et_to_utc(2026, 3, 16, 13, 30);
        let reset = et_to_utc(2026, 3, 16, 15, 30);

        let effective = effective_hours_remaining_from(start, reset, &promos, "seven_day_sonnet");

        // 0.5h peak * 1x + 1.5h off-peak * 2x = 0.5 + 3.0 = 3.5
        assert!((effective - 3.5).abs() < 0.1, "expected ~3.5, got {}", effective);
    }

    #[test]
    fn find_transition_detects_peak_to_offpeak() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 13:00 ET (peak)
        // End: Monday March 16, 2026 at 15:00 ET
        let start = et_to_utc(2026, 3, 16, 13, 0);
        let end = et_to_utc(2026, 3, 16, 15, 0);

        let transition = find_next_transition(start, end, &promos, "seven_day_sonnet");
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
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 7:00 ET (off-peak)
        // End: Monday March 16, 2026 at 9:00 ET
        let start = et_to_utc(2026, 3, 16, 7, 0);
        let end = et_to_utc(2026, 3, 16, 9, 0);

        let transition = find_next_transition(start, end, &promos, "seven_day_sonnet");
        assert!(transition.is_some());

        let (t, before, after) = transition.unwrap();
        // Transition should be at 08:00 ET
        let t_et = to_eastern(t);
        assert_eq!(t_et.hour(), 8);
        assert_eq!(t_et.minute(), 0);
        assert!((before - 2.0).abs() < 1e-9);
        assert!((after - 1.0).abs() < 1e-9);
    }

    /// Regression: windows not in applies_to see no transition (multiplier always 1.0)
    #[test]
    fn no_transition_for_excluded_window() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // Peak-to-off-peak boundary
        let start = et_to_utc(2026, 3, 16, 13, 0);
        let end = et_to_utc(2026, 3, 16, 15, 0);

        // seven_day_sonnet sees a transition
        assert!(
            find_next_transition(start, end, &promos, "seven_day_sonnet").is_some(),
            "seven_day_sonnet should see transition"
        );
        // five_hour does NOT (promo doesn't apply)
        assert!(
            find_next_transition(start, end, &promos, "five_hour").is_none(),
            "five_hour should NOT see transition (not in applies_to)"
        );
        // seven_day does NOT
        assert!(
            find_next_transition(start, end, &promos, "seven_day").is_none(),
            "seven_day should NOT see transition (not in applies_to)"
        );
    }

    #[test]
    fn no_transition_returns_none() {
        let promos = vec![test_promo()];

        // Entirely within peak hours
        let start = et_to_utc(2026, 3, 16, 9, 0);
        let end = et_to_utc(2026, 3, 16, 11, 0);

        let transition = find_next_transition(start, end, &promos, "seven_day_sonnet");
        assert!(transition.is_none());
    }

    // --- Bead spec boundary: 2:01 PM ---

    #[test]
    fn weekday_at_2_01pm_is_off_peak() {
        // Monday March 16, 2026 at 2:01 PM ET -> off-peak
        let t = et_to_utc(2026, 3, 16, 14, 1);
        assert!(!is_peak_at(t));
    }

    // --- Bead spec: 40h reset with 30h off-peak should be > 40 ---

    #[test]
    fn effective_hours_40h_with_30h_offpeak_exceeds_40() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 6:00 PM ET (off-peak)
        // Reset: Wednesday March 18, 2026 at 10:00 AM ET (peak)
        // Total: 40 hours, with most hours off-peak
        let start = et_to_utc(2026, 3, 16, 18, 0);
        let reset = start + Duration::hours(40);

        let effective = effective_hours_remaining_from(start, reset, &promos, "seven_day_sonnet");

        // With 2x off-peak multiplier, effective hours should exceed raw 40h
        assert!(
            effective > 40.0,
            "expected > 40.0 effective hours, got {:.1}",
            effective
        );
    }

    // --- Load promotions from file ---

    #[test]
    fn load_promotions_from_nonexistent_file_returns_empty() {
        let path = Path::new("/tmp/nonexistent-promotions-xyz.json");
        let promos = load_promotions(path);
        assert!(promos.is_empty());
    }

    #[test]
    fn load_promotions_from_valid_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("promotions.json");
        let json = r#"[
            {
                "name": "Test Promo",
                "start_date": "2026-03-01",
                "end_date": "2026-04-01",
                "offpeak_multiplier": 2.0,
                "applies_to": ["seven_day"]
            }
        ]"#;
        std::fs::write(&path, json).unwrap();

        let promos = load_promotions(&path);
        assert_eq!(promos.len(), 1);
        assert_eq!(promos[0].name, "Test Promo");
        assert!((promos[0].offpeak_multiplier - 2.0).abs() < 1e-9);
        // Defaults should be applied
        assert_eq!(promos[0].peak_start_hour_et, 8);
        assert_eq!(promos[0].peak_end_hour_et, 14);
    }

    #[test]
    fn load_promotions_from_invalid_json_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bad-promotions.json");
        std::fs::write(&path, "not valid json").unwrap();

        let promos = load_promotions(&path);
        assert!(promos.is_empty());
    }

    // --- next_transition tests ---

    #[test]
    fn next_transition_returns_transition_info() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // Monday March 16, 2026 at 7:00 ET (off-peak, 1 hour before peak)
        let now = et_to_utc(2026, 3, 16, 7, 0);
        // Deadline: 2 hours later
        let deadline = now + Duration::hours(2);

        let transition = next_transition_from(now, deadline, &promos, "seven_day_sonnet");
        assert!(transition.is_some());

        let t = transition.unwrap();
        // Transition should be at 08:00 ET
        let t_et = to_eastern(t.at);
        assert_eq!(t_et.hour(), 8);
        assert_eq!(t_et.minute(), 0);
        assert!((t.multiplier_before - 2.0).abs() < 1e-9);
        assert!((t.multiplier_after - 1.0).abs() < 1e-9);
        assert_eq!(t.minutes_until, 60); // 1 hour = 60 minutes
    }

    #[test]
    fn next_transition_none_when_no_transition_in_window() {
        let promos = vec![test_promo()];

        // Entirely within peak hours (no transition)
        let now = et_to_utc(2026, 3, 16, 9, 0);
        let deadline = et_to_utc(2026, 3, 16, 11, 0);

        let transition = next_transition_from(now, deadline, &promos, "seven_day_sonnet");
        assert!(transition.is_none());
    }

    #[test]
    fn next_transition_detects_losing_bonus_offpeak_to_peak() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // 07:35 ET during promo - 25 minutes before peak starts
        let now = et_to_utc(2026, 3, 16, 7, 35);
        // Look ahead 1 hour
        let deadline = now + Duration::hours(1);

        let transition = next_transition_from(now, deadline, &promos, "seven_day_sonnet");
        assert!(transition.is_some());

        let t = transition.unwrap();
        // Transition is losing the 2x bonus (off-peak -> peak)
        assert!(t.multiplier_after < t.multiplier_before, "should be losing bonus");
        assert_eq!(t.minutes_until, 25); // 25 minutes until 08:00
    }

    #[test]
    fn next_transition_detects_gaining_bonus_peak_to_offpeak() {
        // test_promo applies_to: ["seven_day_sonnet"]
        let promos = vec![test_promo()];

        // 13:30 ET during promo - 30 minutes before peak ends
        let now = et_to_utc(2026, 3, 16, 13, 30);
        // Look ahead 1 hour
        let deadline = now + Duration::hours(1);

        let transition = next_transition_from(now, deadline, &promos, "seven_day_sonnet");
        assert!(transition.is_some());

        let t = transition.unwrap();
        // Transition is gaining the 2x bonus (peak -> off-peak)
        assert!(t.multiplier_after > t.multiplier_before, "should be gaining bonus");
        assert_eq!(t.minutes_until, 30); // 30 minutes until 14:00
    }
}
