//! Peak/off-peak schedule calculator
//!
//! Handles:
//! - Peak hour detection (8AM-2PM ET weekdays)
//! - Promotion loading and multiplier calculation
//! - Effective hours remaining accounting for off-peak multipliers

use chrono::{DateTime, Datelike, Timelike, Utc};
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
        log::debug!(
            "[schedule] no promotions file at {}, returning empty",
            path.display()
        );
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
pub fn is_promo_active_at(t: DateTime<Utc>, promo: &Promotion) -> bool {
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

    // The real subscription windows (src/config.rs); spelled out here so the
    // tests stay independent of config-module internals.
    const REAL_WINDOWS: [&str; 3] = ["five_hour", "seven_day", "weekly_scoped"];

    // Test promotion: March 15-25, 2026 with 2x off-peak for weekly_scoped only
    fn test_promo() -> Promotion {
        Promotion {
            name: "March 2026 Promo".to_string(),
            start_date: "2026-03-15".to_string(),
            end_date: "2026-03-25".to_string(),
            peak_start_hour_et: 8,
            peak_end_hour_et: 14,
            offpeak_multiplier: 2.0,
            applies_to: vec!["weekly_scoped".to_string()],
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
        assert!((get_multiplier_at(t, &promos, "weekly_scoped") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_off_peak_during_promo_is_2x() {
        let promos = vec![test_promo()];
        // Monday March 16, 2026 at 6:00 AM ET (off-peak, during promo)
        let t = et_to_utc(2026, 3, 16, 6, 0);
        assert!((get_multiplier_at(t, &promos, "weekly_scoped") - 2.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_after_promo_ends_is_1() {
        let promos = vec![test_promo()];
        // March 26, 2026 at 6:00 AM ET (after promo ended on 25th)
        let t = et_to_utc(2026, 3, 26, 6, 0);
        assert!((get_multiplier_at(t, &promos, "weekly_scoped") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_before_promo_starts_is_1() {
        let promos = vec![test_promo()];
        // March 14, 2026 at 6:00 AM ET (before promo starts on 15th)
        let t = et_to_utc(2026, 3, 14, 6, 0);
        assert!((get_multiplier_at(t, &promos, "weekly_scoped") - 1.0).abs() < 1e-9);
    }

    /// Regression test: applies_to filtering — only listed windows get the boost
    #[test]
    fn multiplier_applies_to_filtering() {
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];
        // Monday March 16, 2026 at 6:00 AM ET (off-peak, during promo)
        let t = et_to_utc(2026, 3, 16, 6, 0);

        // Listed window gets 2x
        assert!(
            (get_multiplier_at(t, &promos, "weekly_scoped") - 2.0).abs() < 1e-9,
            "weekly_scoped should get 2x (in applies_to)"
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
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 6:00 AM ET (off-peak)
        // End: Wednesday March 18, 2026 at 6:00 AM ET (48 hours later)
        let start = et_to_utc(2026, 3, 16, 6, 0);
        let reset = start + Duration::hours(48);

        // weekly_scoped gets the 2x boost
        let effective = effective_hours_remaining_from(start, reset, &promos, "weekly_scoped");
        assert!(effective > 48.0, "expected > 48, got {}", effective);
        assert!(
            effective > 70.0,
            "expected significantly more than 48, got {}",
            effective
        );

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
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 13:30 ET (peak)
        // End: Monday March 16, 2026 at 15:30 ET (2 hours, includes transition at 14:00)
        let start = et_to_utc(2026, 3, 16, 13, 30);
        let reset = et_to_utc(2026, 3, 16, 15, 30);

        let effective = effective_hours_remaining_from(start, reset, &promos, "weekly_scoped");

        // 0.5h peak * 1x + 1.5h off-peak * 2x = 0.5 + 3.0 = 3.5
        assert!(
            (effective - 3.5).abs() < 0.1,
            "expected ~3.5, got {}",
            effective
        );
    }

    #[test]
    fn find_transition_detects_peak_to_offpeak() {
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 13:00 ET (peak)
        // End: Monday March 16, 2026 at 15:00 ET
        let start = et_to_utc(2026, 3, 16, 13, 0);
        let end = et_to_utc(2026, 3, 16, 15, 0);

        let transition = find_next_transition(start, end, &promos, "weekly_scoped");
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
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 7:00 ET (off-peak)
        // End: Monday March 16, 2026 at 9:00 ET
        let start = et_to_utc(2026, 3, 16, 7, 0);
        let end = et_to_utc(2026, 3, 16, 9, 0);

        let transition = find_next_transition(start, end, &promos, "weekly_scoped");
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
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // Peak-to-off-peak boundary
        let start = et_to_utc(2026, 3, 16, 13, 0);
        let end = et_to_utc(2026, 3, 16, 15, 0);

        // weekly_scoped sees a transition
        assert!(
            find_next_transition(start, end, &promos, "weekly_scoped").is_some(),
            "weekly_scoped should see transition"
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

        let transition = find_next_transition(start, end, &promos, "weekly_scoped");
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
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // Start: Monday March 16, 2026 at 6:00 PM ET (off-peak)
        // Reset: Wednesday March 18, 2026 at 10:00 AM ET (peak)
        // Total: 40 hours, with most hours off-peak
        let start = et_to_utc(2026, 3, 16, 18, 0);
        let reset = start + Duration::hours(40);

        let effective = effective_hours_remaining_from(start, reset, &promos, "weekly_scoped");

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
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // Monday March 16, 2026 at 7:00 ET (off-peak, 1 hour before peak)
        let now = et_to_utc(2026, 3, 16, 7, 0);
        // Deadline: 2 hours later
        let deadline = now + Duration::hours(2);

        let transition = next_transition_from(now, deadline, &promos, "weekly_scoped");
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

        let transition = next_transition_from(now, deadline, &promos, "weekly_scoped");
        assert!(transition.is_none());
    }

    #[test]
    fn next_transition_detects_losing_bonus_offpeak_to_peak() {
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // 07:35 ET during promo - 25 minutes before peak starts
        let now = et_to_utc(2026, 3, 16, 7, 35);
        // Look ahead 1 hour
        let deadline = now + Duration::hours(1);

        let transition = next_transition_from(now, deadline, &promos, "weekly_scoped");
        assert!(transition.is_some());

        let t = transition.unwrap();
        // Transition is losing the 2x bonus (off-peak -> peak)
        assert!(
            t.multiplier_after < t.multiplier_before,
            "should be losing bonus"
        );
        assert_eq!(t.minutes_until, 25); // 25 minutes until 08:00
    }

    #[test]
    fn next_transition_detects_gaining_bonus_peak_to_offpeak() {
        // test_promo applies_to: ["weekly_scoped"]
        let promos = vec![test_promo()];

        // 13:30 ET during promo - 30 minutes before peak ends
        let now = et_to_utc(2026, 3, 16, 13, 30);
        // Look ahead 1 hour
        let deadline = now + Duration::hours(1);

        let transition = next_transition_from(now, deadline, &promos, "weekly_scoped");
        assert!(transition.is_some());

        let t = transition.unwrap();
        // Transition is gaining the 2x bonus (peak -> off-peak)
        assert!(
            t.multiplier_after > t.multiplier_before,
            "should be gaining bonus"
        );
        assert_eq!(t.minutes_until, 30); // 30 minutes until 14:00
    }

    // --- Timezone boundaries: peak window pinned in UTC, both DST sides ---
    //
    // The peak window is defined in ET wall-clock; its UTC position moves with
    // DST (08:00 ET = 12:00 UTC in EDT, 13:00 UTC in EST). Every earlier test
    // constructs times via et_to_utc, which would pass even if the conversion
    // were pinned to a fixed UTC-4 offset — these pin the actual UTC instants.

    #[test]
    fn peak_boundaries_pinned_in_utc_during_edt() {
        // Monday March 16, 2026 (EDT = UTC-4): peak is 12:00-18:00 UTC
        assert!(
            !is_peak_at(utc_at(2026, 3, 16, 11, 59)),
            "11:59 UTC = 7:59 ET, off-peak"
        );
        assert!(
            is_peak_at(utc_at(2026, 3, 16, 12, 0)),
            "12:00 UTC = 8:00 ET, peak"
        );
        assert!(
            is_peak_at(utc_at(2026, 3, 16, 17, 59)),
            "17:59 UTC = 1:59 PM ET, peak"
        );
        assert!(
            !is_peak_at(utc_at(2026, 3, 16, 18, 0)),
            "18:00 UTC = 2:00 PM ET, off-peak"
        );
    }

    #[test]
    fn peak_boundaries_pinned_in_utc_during_est() {
        // Monday January 5, 2026 (EST = UTC-5): peak is 13:00-19:00 UTC
        // If the ET conversion were fixed at UTC-4, the 13:00Z instant would
        // read as 9:00 AM ET (peak) instead of 8:00 AM ET — same thing here,
        // but 12:59Z would read 8:59 AM ET (peak) instead of 7:59 AM (off-peak).
        assert!(
            !is_peak_at(utc_at(2026, 1, 5, 12, 59)),
            "12:59 UTC = 7:59 AM EST, off-peak"
        );
        assert!(
            is_peak_at(utc_at(2026, 1, 5, 13, 0)),
            "13:00 UTC = 8:00 AM EST, peak"
        );
        assert!(
            is_peak_at(utc_at(2026, 1, 5, 18, 59)),
            "18:59 UTC = 1:59 PM EST, peak"
        );
        assert!(
            !is_peak_at(utc_at(2026, 1, 5, 19, 0)),
            "19:00 UTC = 2:00 PM EST, off-peak"
        );
    }

    #[test]
    fn dst_transition_sundays_are_off_peak_all_day() {
        // DST transitions land on Sundays (weekends are 2x all day, no peak
        // window), so the schedule never consults an ambiguous or nonexistent
        // local hour. Pin both 2026 transition days via unambiguous UTC times.

        // Spring forward: Sunday March 8, 2026, 23-hour day.
        // 06:00Z = 1:00 AM EST (exists); 07:00Z = 3:00 AM EDT (2 AM does not);
        // 12:00Z = 8:00 AM EDT — would be peak if this were a weekday.
        for (label, t) in [
            ("1:00 AM EST", utc_at(2026, 3, 8, 6, 0)),
            ("3:00 AM EDT", utc_at(2026, 3, 8, 7, 0)),
            ("8:00 AM EDT", utc_at(2026, 3, 8, 12, 0)),
        ] {
            assert!(is_weekend(t), "March 8 should be a weekend day");
            assert!(
                !is_peak_at(t),
                "{} on spring-forward Sunday should be off-peak",
                label
            );
        }

        // Fall back: Sunday November 1, 2026, 25-hour day.
        // 09:00Z = 5:00 AM EDT; 10:30Z = 6:30 AM EST — either side of the
        // repeated 1-2 AM hour, both off-peak.
        for (label, t) in [
            ("5:00 AM EDT", utc_at(2026, 11, 1, 9, 0)),
            ("6:30 AM EST", utc_at(2026, 11, 1, 10, 30)),
        ] {
            assert!(is_weekend(t), "November 1 should be a weekend day");
            assert!(
                !is_peak_at(t),
                "{} on fall-back Sunday should be off-peak",
                label
            );
        }
    }

    #[test]
    fn weekend_to_weekday_is_one_continuous_offpeak_stretch() {
        // Friday 14:00 ET through Monday 08:00 ET is a single off-peak span:
        // Friday afternoon/evening + all weekend + Monday pre-8AM. No part of
        // it should register a peak hour or an internal multiplier transition.
        let friday_2pm = et_to_utc(2026, 3, 20, 14, 0);
        let monday_noon = et_to_utc(2026, 3, 23, 12, 0);

        for hour in (0..42).step_by(3) {
            let t = friday_2pm + Duration::hours(hour);
            assert!(
                !is_peak_at(t),
                "{} ET in the Fri 2pm -> Mon stretch should be off-peak",
                to_eastern(t)
            );
        }

        // The whole stretch carries one constant multiplier, so the next
        // transition is Monday 08:00 ET — nothing inside the weekend itself.
        let promos = vec![test_promo()];
        let sat_start = et_to_utc(2026, 3, 21, 0, 0);
        let sun_end = et_to_utc(2026, 3, 22, 23, 0);
        assert!(
            find_next_transition(sat_start, sun_end, &promos, "weekly_scoped").is_none(),
            "Saturday -> Sunday should have no transitions at all"
        );

        let (t, before, after) = find_next_transition(
            et_to_utc(2026, 3, 22, 12, 0),
            et_to_utc(2026, 3, 23, 10, 0),
            &promos,
            "weekly_scoped",
        )
        .expect("Monday 08:00 ET should be the first transition after the weekend");
        let t_et = to_eastern(t);
        assert_eq!(
            (t_et.weekday(), t_et.hour(), t_et.minute()),
            (chrono::Weekday::Mon, 8, 0)
        );
        assert!((before - 2.0).abs() < 1e-9, "weekend carries the 2x bonus");
        assert!((after - 1.0).abs() < 1e-9, "Monday 8 AM returns to peak 1x");
        assert_eq!(monday_noon.weekday(), chrono::Weekday::Mon); // guard on the anchor
    }

    // --- Window transitions at the promotion date boundaries ---

    #[test]
    fn find_transition_detects_promo_end_at_midnight_et() {
        let promos = vec![test_promo()]; // ends 2026-03-25 (exclusive)

        // Tuesday 2026-03-24 20:00 ET (off-peak, 2x) -> Wednesday 08:00 ET.
        // The multiplier drops 2x -> 1x at 2026-03-25 00:00 ET even though the
        // clock never enters peak hours — the promo end is itself a transition.
        let (t, before, after) = find_next_transition(
            et_to_utc(2026, 3, 24, 20, 0),
            et_to_utc(2026, 3, 25, 8, 0),
            &promos,
            "weekly_scoped",
        )
        .expect("promo end at midnight ET should surface as a transition");
        let t_et = to_eastern(t);
        assert_eq!(
            (t_et.hour(), t_et.minute()),
            (0, 0),
            "promo ends at 00:00 ET"
        );
        assert_eq!(t_et.date_naive().to_string(), "2026-03-25");
        assert!((before - 2.0).abs() < 1e-9);
        assert!((after - 1.0).abs() < 1e-9);
    }

    #[test]
    fn find_transition_detects_promo_start_at_midnight_et() {
        let promos = vec![test_promo()]; // starts 2026-03-15 (inclusive)

        // Saturday 2026-03-14 22:00 ET (off-peak, 1x) -> Sunday 02:00 ET.
        // The bonus appears at midnight with no peak-window involvement.
        let (t, before, after) = find_next_transition(
            et_to_utc(2026, 3, 14, 22, 0),
            et_to_utc(2026, 3, 15, 2, 0),
            &promos,
            "weekly_scoped",
        )
        .expect("promo start at midnight ET should surface as a transition");
        let t_et = to_eastern(t);
        assert_eq!((t_et.hour(), t_et.minute()), (0, 0));
        assert_eq!(t_et.date_naive().to_string(), "2026-03-15");
        assert!((before - 1.0).abs() < 1e-9);
        assert!((after - 2.0).abs() < 1e-9);
    }

    #[test]
    fn promo_date_boundaries_are_start_inclusive_end_exclusive() {
        let promos = vec![test_promo()]; // 2026-03-15 ..< 2026-03-25

        // All four instants are off-peak, so the multiplier isolates the date logic.
        assert!(
            (get_multiplier_at(et_to_utc(2026, 3, 14, 23, 59), &promos, "weekly_scoped") - 1.0)
                .abs()
                < 1e-9,
            "23:59 the day before start is outside the promo"
        );
        assert!(
            (get_multiplier_at(et_to_utc(2026, 3, 15, 0, 0), &promos, "weekly_scoped") - 2.0).abs()
                < 1e-9,
            "00:00 on the start date is the first active minute"
        );
        assert!(
            (get_multiplier_at(et_to_utc(2026, 3, 24, 23, 59), &promos, "weekly_scoped") - 2.0)
                .abs()
                < 1e-9,
            "23:59 the day before end is still inside the promo"
        );
        assert!(
            (get_multiplier_at(et_to_utc(2026, 3, 25, 0, 0), &promos, "weekly_scoped") - 1.0).abs()
                < 1e-9,
            "00:00 on the end date is already past the promo (end exclusive)"
        );
    }

    #[test]
    fn find_transition_none_when_deadline_not_after_start() {
        let promos = vec![test_promo()];
        let now = et_to_utc(2026, 3, 16, 13, 0); // peak ends at 14:00

        assert!(find_next_transition(now, now, &promos, "weekly_scoped").is_none());
        assert!(
            find_next_transition(now, now - Duration::hours(1), &promos, "weekly_scoped").is_none()
        );
    }

    // --- Missing / malformed schedules degrade to the 1x flat model ---

    #[test]
    fn empty_promotion_list_yields_flat_1x() {
        let promos: Vec<Promotion> = vec![];
        // Monday 2026-03-16 06:00 ET: off-peak, would be 2x if a promo applied.
        let t = et_to_utc(2026, 3, 16, 6, 0);
        assert!((get_multiplier_at(t, &promos, "weekly_scoped") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn malformed_dates_inside_valid_json_fall_back_to_1x() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("promotions.json");
        // Parses as JSON and every field is present — but the dates are
        // unparseable. is_promo_active_at must warn and return false rather
        // than panic, degrading to the documented 1x flat model.
        let json = r#"[
            {
                "name": "Broken Promo",
                "start_date": "2026-13-45",
                "end_date": "not-a-date",
                "offpeak_multiplier": 2.0,
                "applies_to": ["weekly_scoped"]
            }
        ]"#;
        std::fs::write(&path, json).unwrap();

        let promos = load_promotions(&path);
        assert_eq!(promos.len(), 1, "file itself is valid JSON and must load");

        let t = et_to_utc(2026, 3, 16, 6, 0); // off-peak
        assert!(!is_promo_active_at(t, &promos[0]));
        assert!((get_multiplier_at(t, &promos, "weekly_scoped") - 1.0).abs() < 1e-9);

        // The forecast sees no bonus: effective hours equal raw hours.
        let reset = t + Duration::hours(12);
        let effective = effective_hours_remaining_from(t, reset, &promos, "weekly_scoped");
        assert!(
            (effective - 12.0).abs() < 0.1,
            "malformed dates must forecast as 1x, got {}",
            effective
        );
    }

    #[test]
    fn empty_applies_to_never_matches_any_window() {
        let promo = Promotion {
            applies_to: vec![],
            ..test_promo()
        };
        let promos = vec![promo];
        let t = et_to_utc(2026, 3, 16, 6, 0); // off-peak, inside the promo range

        for window in ["weekly_scoped", "five_hour", "seven_day"] {
            assert!(
                (get_multiplier_at(t, &promos, window) - 1.0).abs() < 1e-9,
                "window {} must stay 1x with an empty applies_to",
                window
            );
            assert!(find_next_transition(t, t + Duration::hours(2), &promos, window).is_none());
        }
    }

    #[test]
    fn load_promotions_from_empty_array_and_from_directory() {
        let dir = tempfile::TempDir::new().unwrap();

        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, "[]").unwrap();
        assert!(load_promotions(&empty).is_empty());

        // A directory at the configured path fails fs::read_to_string — the
        // warn-and-empty path, not a panic.
        assert!(load_promotions(dir.path()).is_empty());
    }

    // --- Pinning tests: fallback paths not covered above (claudego-2d46409e) ---

    /// Regression: `current_multiplier` is the live-clock entry point the bead
    /// names; with the fallback empty schedule it must be flat 1x for every
    /// real window at whatever "now" happens to be. Deterministic despite
    /// Utc::now() because an empty promotion list can never match.
    #[test]
    fn current_multiplier_with_empty_schedule_is_flat_1x() {
        let promos: Vec<Promotion> = vec![];
        for window in REAL_WINDOWS {
            assert!(
                (current_multiplier(&promos, window) - 1.0).abs() < 1e-9,
                "current_multiplier with no promotions must be 1.0 for {}",
                window
            );
        }
    }

    /// Regression: the empty-schedule fallback is flat 1x for *every* window at
    /// *every* class of instant — peak, off-peak, both peak boundaries, the
    /// Fri 14:00 weekend edge, and midnight — and produces no transitions
    /// anywhere (the Fri 14:00 -> Mon 08:00 stretch included), so the
    /// governor's pre-scaling path sees a None transition and a raw-hours
    /// forecast.
    #[test]
    fn empty_schedule_returns_1x_for_every_window_and_instant_class() {
        let promos: Vec<Promotion> = vec![];
        let instants = [
            ("weekday peak", et_to_utc(2026, 3, 16, 10, 0)),
            ("weekday off-peak", et_to_utc(2026, 3, 16, 6, 0)),
            ("peak start boundary", et_to_utc(2026, 3, 16, 8, 0)),
            ("peak end boundary", et_to_utc(2026, 3, 16, 14, 0)),
            ("friday 14:00 edge", et_to_utc(2026, 3, 20, 14, 0)),
            ("weekend noon", et_to_utc(2026, 3, 21, 12, 0)),
            ("midnight", et_to_utc(2026, 3, 17, 0, 0)),
        ];

        for window in REAL_WINDOWS {
            for (label, t) in instants {
                assert!(
                    (get_multiplier_at(t, &promos, window) - 1.0).abs() < 1e-9,
                    "{} at {} must be 1x with no promotions",
                    window,
                    label
                );
            }

            // The whole Fri 14:00 -> Mon 08:00 stretch carries one constant
            // 1x multiplier: no transition inside it, none before its end.
            let fri_2pm = et_to_utc(2026, 3, 20, 14, 0);
            let mon_8am = et_to_utc(2026, 3, 23, 8, 0);
            assert!(
                find_next_transition(fri_2pm, mon_8am, &promos, window).is_none(),
                "empty schedule must have no transition across the weekend stretch for {}",
                window
            );
            assert!(
                next_transition_from(fri_2pm, mon_8am, &promos, window).is_none(),
                "next_transition_from must be None with no promotions for {}",
                window
            );

            // The forecast sees no bonus: exactly the raw wall hours
            // (Fri 14:00 -> Mon 08:00 = 10 + 24 + 24 + 8 = 66h).
            let effective =
                effective_hours_remaining_from(fri_2pm, mon_8am, &promos, window);
            assert!(
                (effective - 66.0).abs() < 1e-6,
                "empty schedule must forecast raw 66h for {}, got {}",
                window,
                effective
            );
        }
    }

    /// Regression: a zero-length promotion (start == end, never satisfied by
    /// the start-inclusive/end-exclusive comparison) degrades to the flat 1x
    /// model without panicking — no multiplier, no transitions, raw forecast.
    #[test]
    fn zero_length_promo_resolves_to_flat_1x_without_panic() {
        let promo = Promotion {
            start_date: "2026-03-16".to_string(),
            end_date: "2026-03-16".to_string(),
            ..test_promo()
        };
        let promos = vec![promo];

        // Every instant of the would-be promo day stays 1x — the off-peak
        // ones isolate the date logic from the peak window.
        for (label, t) in [
            ("00:00", et_to_utc(2026, 3, 16, 0, 0)),
            ("06:00 off-peak", et_to_utc(2026, 3, 16, 6, 0)),
            ("13:59 peak", et_to_utc(2026, 3, 16, 13, 59)),
            ("23:59", et_to_utc(2026, 3, 16, 23, 59)),
        ] {
            assert!(
                !is_promo_active_at(t, &promos[0]),
                "zero-length promo must never be active ({})",
                label
            );
            assert!(
                (get_multiplier_at(t, &promos, "weekly_scoped") - 1.0).abs() < 1e-9,
                "zero-length promo must leave {} at 1x",
                label
            );
        }

        // Walking the whole day finds no transition and forecasts raw hours.
        let day_start = et_to_utc(2026, 3, 16, 0, 0);
        assert!(
            find_next_transition(
                day_start,
                day_start + Duration::hours(24),
                &promos,
                "weekly_scoped"
            )
            .is_none(),
            "zero-length promo must not surface a transition"
        );
        let effective = effective_hours_remaining_from(
            day_start,
            day_start + Duration::hours(24),
            &promos,
            "weekly_scoped",
        );
        assert!(
            (effective - 24.0).abs() < 1e-6,
            "zero-length promo must forecast raw 24h, got {}",
            effective
        );
    }

    /// Regression: an inverted promotion range (end before start) satisfies no
    /// instant, so it degrades to flat 1x without panicking — same shape as
    /// the zero-length case but across a multi-day span.
    #[test]
    fn inverted_promo_range_resolves_to_flat_1x_without_panic() {
        let promo = Promotion {
            start_date: "2026-03-20".to_string(),
            end_date: "2026-03-16".to_string(), // ends before it starts
            ..test_promo()
        };
        let promos = vec![promo];

        for (label, t) in [
            ("inside the inverted span", et_to_utc(2026, 3, 17, 6, 0)),
            ("late in the inverted span", et_to_utc(2026, 3, 19, 23, 59)),
            ("weekend inside the span", et_to_utc(2026, 3, 21, 12, 0)),
        ] {
            assert!(
                !is_promo_active_at(t, &promos[0]),
                "inverted range must never be active ({})",
                label
            );
            assert!(
                (get_multiplier_at(t, &promos, "weekly_scoped") - 1.0).abs() < 1e-9,
                "inverted range must leave {} at 1x",
                label
            );
        }

        let span_start = et_to_utc(2026, 3, 16, 0, 0);
        assert!(
            find_next_transition(
                span_start,
                span_start + Duration::hours(120),
                &promos,
                "weekly_scoped"
            )
            .is_none(),
            "inverted range must not surface a transition"
        );
        let effective = effective_hours_remaining_from(
            span_start,
            span_start + Duration::hours(24),
            &promos,
            "weekly_scoped",
        );
        assert!(
            (effective - 24.0).abs() < 1e-6,
            "inverted range must forecast raw 24h, got {}",
            effective
        );
    }

    /// Regression: the per-promotion peak-hour fields are inert — the peak
    /// window comes from the module constants, so even degenerate values
    /// (inverted or zero-length) change nothing and panic nothing.
    #[test]
    fn degenerate_peak_hour_fields_are_inert() {
        let inverted = Promotion {
            peak_start_hour_et: 14,
            peak_end_hour_et: 8, // inverted "window"
            ..test_promo()
        };
        let zero_length = Promotion {
            peak_start_hour_et: 8,
            peak_end_hour_et: 8, // empty "window"
            ..test_promo()
        };
        let baseline = vec![test_promo()];

        let instants = [
            ("off-peak", et_to_utc(2026, 3, 16, 6, 0)),
            ("peak", et_to_utc(2026, 3, 16, 10, 0)),
            ("13:59 peak edge", et_to_utc(2026, 3, 16, 13, 59)),
            ("14:00 off-peak edge", et_to_utc(2026, 3, 16, 14, 0)),
        ];
        for promo in [inverted, zero_length] {
            let promos = vec![promo];
            for (label, t) in instants {
                assert_eq!(
                    get_multiplier_at(t, &promos, "weekly_scoped"),
                    get_multiplier_at(t, &baseline, "weekly_scoped"),
                    "degenerate peak fields must not change the multiplier at {}",
                    label
                );
            }
        }
    }

    /// Regression: valid JSON of the wrong shape (not an array of promotion
    /// objects) hits the same warn-and-empty parse-failure path as malformed
    /// JSON rather than panicking or partially loading.
    #[test]
    fn load_promotions_from_wrong_shape_json_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        for (label, contents) in [
            ("object", r#"{"name": "lone promo"}"#),
            ("null", "null"),
            ("string", r#""just a string""#),
            ("number", "42"),
            ("array of non-objects", r#"[1, 2, 3]"#),
        ] {
            let path = dir.path().join(format!("wrong-shape-{}.json", label));
            std::fs::write(&path, contents).unwrap();
            assert!(
                load_promotions(&path).is_empty(),
                "wrong-shape JSON ({}) must fall back to an empty schedule",
                label
            );
        }
    }

    /// Regression: a file that exists but is not valid UTF-8 fails
    /// fs::read_to_string — the same warn-and-empty read-failure path a
    /// directory triggers — rather than panicking.
    #[test]
    fn load_promotions_from_non_utf8_file_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("binary-promotions.json");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0x81, 0x01]).unwrap();

        assert!(path.exists(), "the file must exist to reach the read path");
        assert!(load_promotions(&path).is_empty());
    }

    // --- Deterministic forecasts: exact effective-hour values ---

    #[test]
    fn forecast_doc_example_40h_with_12h_peak_is_exactly_68() {
        let promos = vec![test_promo()];

        // Monday 2026-03-16 08:00 ET + 40h -> Wednesday 00:00 ET.
        // Peak: Mon 08-14 (6h) + Tue 08-14 (6h) = 12h; off-peak: 28h.
        // 28 * 2.0 + 12 * 1.0 = 68.0 effective hours.
        let start = et_to_utc(2026, 3, 16, 8, 0);
        let reset = start + Duration::hours(40);
        let effective = effective_hours_remaining_from(start, reset, &promos, "weekly_scoped");
        assert!(
            (effective - 68.0).abs() < 1e-6,
            "expected exactly 68.0 effective hours, got {:.6}",
            effective
        );

        // A window the promo does not apply to sees the raw 40h.
        let unboosted = effective_hours_remaining_from(start, reset, &promos, "five_hour");
        assert!((unboosted - 40.0).abs() < 1e-6);
    }

    #[test]
    fn forecast_across_promo_end_is_exactly_16_over_12_wall_hours() {
        let promos = vec![test_promo()];

        // Tuesday 2026-03-24 20:00 ET -> Wednesday 08:00 ET (12 wall hours).
        // Tue 20:00-00:00: 4h off-peak at 2x = 8.0
        // Wed 00:00-08:00: 8h off-peak at 1x (promo expired) = 8.0
        let start = et_to_utc(2026, 3, 24, 20, 0);
        let reset = et_to_utc(2026, 3, 25, 8, 0);
        let effective = effective_hours_remaining_from(start, reset, &promos, "weekly_scoped");
        assert!(
            (effective - 16.0).abs() < 1e-6,
            "expected exactly 16.0 effective hours across the promo end, got {:.6}",
            effective
        );

        // The same span a week earlier — promo still active throughout —
        // is 4h at 2x + 8h at 2x = 24.0, isolating the expiry effect.
        let start_pre = et_to_utc(2026, 3, 17, 20, 0);
        let reset_pre = et_to_utc(2026, 3, 18, 8, 0);
        let effective_pre =
            effective_hours_remaining_from(start_pre, reset_pre, &promos, "weekly_scoped");
        assert!(
            (effective_pre - 24.0).abs() < 1e-6,
            "expected exactly 24.0 while the promo holds, got {:.6}",
            effective_pre
        );
    }

    #[test]
    fn forecast_24h_from_mid_afternoon_is_exactly_42() {
        let promos = vec![test_promo()];

        // Monday 2026-03-16 16:00 ET + 24h -> Tuesday 16:00 ET.
        // Off-peak: Mon 16:00-Tue 08:00 (16h) + Tue 14:00-16:00 (2h) = 18h.
        // Peak: Tue 08:00-14:00 = 6h. 18 * 2.0 + 6 * 1.0 = 42.0.
        let start = et_to_utc(2026, 3, 16, 16, 0);
        let reset = start + Duration::hours(24);
        let effective = effective_hours_remaining_from(start, reset, &promos, "weekly_scoped");
        assert!(
            (effective - 42.0).abs() < 1e-6,
            "expected exactly 42.0 effective hours, got {:.6}",
            effective
        );
    }

    #[test]
    fn forecast_is_zero_when_reset_is_not_after_start() {
        let promos = vec![test_promo()];
        let start = et_to_utc(2026, 3, 16, 6, 0);

        assert_eq!(
            effective_hours_remaining_from(start, start, &promos, "weekly_scoped"),
            0.0
        );
        assert_eq!(
            effective_hours_remaining_from(
                start,
                start - Duration::hours(1),
                &promos,
                "weekly_scoped"
            ),
            0.0
        );
    }

    // --- Helper for pinning UTC instants (DST tests) ---

    fn utc_at(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
    }
}
