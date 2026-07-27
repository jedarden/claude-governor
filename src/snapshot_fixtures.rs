//! Test fixtures for snapshot delta computation
//!
//! This module provides realistic fixtures for testing consecutive API poll
//! snapshot scenarios and delta calculations. The fixtures model actual API
//! response data with realistic timestamps and utilization percentages.
//!
//! Fixtures cover:
//! - Baseline snapshot with typical starting values
//! - Consecutive snapshot (5 hours later) with increased usage
//! - Consecutive snapshot (7 days later) with increased usage
//! - Consecutive snapshot (7 days later, same weekday) with increased usage

use chrono::{DateTime, Datelike, Utc};
use crate::state::PrevUsageSnapshot;

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// Create a PrevUsageSnapshot with realistic API response values.
///
/// # Arguments
/// - `taken_at`: Timestamp when the snapshot was taken
/// - `five_hour_pct`: 5-hour window utilization percentage
/// - `seven_day_pct`: 7-day window utilization percentage (all models)
/// - `weekly_scoped_pct`: 7-day window utilization percentage (Sonnet only)
///
/// # Returns
/// A `PrevUsageSnapshot` struct with the specified values.
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::make_snapshot;
/// use chrono::Utc;
///
/// let now = Utc::now();
/// let snapshot = make_snapshot(now, 25.5, 45.0, 38.2);
/// assert_eq!(snapshot.five_hour_pct, 25.5);
/// assert_eq!(snapshot.taken_at, now);
/// ```
pub fn make_snapshot(
    taken_at: DateTime<Utc>,
    five_hour_pct: f64,
    seven_day_pct: f64,
    weekly_scoped_pct: f64,
) -> PrevUsageSnapshot {
    PrevUsageSnapshot {
        taken_at,
        five_hour_pct,
        seven_day_pct,
        weekly_scoped_pct,
    }
}

// ---------------------------------------------------------------------------
// Baseline fixture
// ---------------------------------------------------------------------------

/// Baseline snapshot with typical starting utilization values.
///
/// Represents a starting point with low-to-moderate usage across all windows.
/// Useful as the "previous" snapshot in delta computation tests.
///
/// # Timing
/// - Wednesday, March 18, 2026 at 10:00:00 UTC
/// - Chosen to allow realistic "7 days later, same weekday" progression
///
/// # Values
/// - 5-hour window: 12.5% (low usage, early in a 5-hour window)
/// - 7-day window: 45.2% (moderate usage, mid-week)
/// - 7-day Sonnet: 38.7% (moderate usage, slightly lower than all-models)
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::baseline_snapshot;
///
/// let snapshot = baseline_snapshot();
/// assert_eq!(snapshot.five_hour_pct, 12.5);
/// assert_eq!(snapshot.seven_day_pct, 45.2);
/// assert_eq!(snapshot.weekly_scoped_pct, 38.7);
/// ```
pub fn baseline_snapshot() -> PrevUsageSnapshot {
    // Wednesday, March 18, 2026 at 10:00:00 UTC
    let taken_at = "2026-03-18T10:00:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 12.5,
        seven_day_pct: 45.2,
        weekly_scoped_pct: 38.7,
    }
}

// ---------------------------------------------------------------------------
// 5-hour delta fixture
// ---------------------------------------------------------------------------

/// Consecutive snapshot taken 5 hours after baseline.
///
/// Represents increased utilization after 5 hours of active workloads.
/// This models a short-term delta scenario for testing p5h delta computation.
///
/// # Timing
/// - Wednesday, March 18, 2026 at 15:00:00 UTC (5 hours after baseline)
///
/// # Values
/// - 5-hour window: 18.2% (delta: +5.7% - increased usage over 5 hours)
/// - 7-day window: 46.8% (delta: +1.6% - gradual increase)
/// - 7-day Sonnet: 40.3% (delta: +1.6% - gradual increase)
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::{baseline_snapshot, snapshot_after_5h};
///
/// let prev = baseline_snapshot();
/// let curr = snapshot_after_5h();
/// let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
/// assert_eq!(elapsed.num_hours(), 5);
/// ```
pub fn snapshot_after_5h() -> PrevUsageSnapshot {
    // Wednesday, March 18, 2026 at 15:00:00 UTC (5 hours after baseline)
    let taken_at = "2026-03-18T15:00:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 18.2,      // +5.7% from baseline
        seven_day_pct: 46.8,       // +1.6% from baseline
        weekly_scoped_pct: 40.3, // +1.6% from baseline
    }
}

// ---------------------------------------------------------------------------
// 7-day delta fixture
// ---------------------------------------------------------------------------

/// Consecutive snapshot taken 7 days after baseline.
///
/// Represents increased utilization after a full week of operation.
/// This models a medium-term delta scenario for testing p7d delta computation.
///
/// # Timing
/// - Wednesday, March 25, 2026 at 10:00:00 UTC (7 days after baseline)
///
/// # Values
/// - 5-hour window: 15.8% (new 5-hour window, similar to baseline)
/// - 7-day window: 52.4% (delta: +7.2% - accumulated week of usage)
/// - 7-day Sonnet: 46.1% (delta: +7.4% - accumulated week of usage)
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::{baseline_snapshot, snapshot_after_7d};
///
/// let prev = baseline_snapshot();
/// let curr = snapshot_after_7d();
/// let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
/// assert_eq!(elapsed.num_days(), 7);
/// ```
pub fn snapshot_after_7d() -> PrevUsageSnapshot {
    // Wednesday, March 25, 2026 at 10:00:00 UTC (7 days after baseline)
    let taken_at = "2026-03-25T10:00:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 15.8,       // New 5-hour window (reset occurred)
        seven_day_pct: 52.4,       // +7.2% from baseline
        weekly_scoped_pct: 46.1, // +7.4% from baseline
    }
}

// ---------------------------------------------------------------------------
// 7-day same-weekday delta fixture
// ---------------------------------------------------------------------------

/// Consecutive snapshot taken 7 days after baseline (same weekday).
///
/// Represents increased utilization after a full week, maintaining the same
/// weekday for pattern consistency. This models a weekly recurring usage pattern.
///
/// # Timing
/// - Wednesday, March 25, 2026 at 10:00:00 UTC (7 days after baseline, same weekday)
///
/// # Values
/// - 5-hour window: 15.8% (new 5-hour window, similar to baseline)
/// - 7-day window: 52.4% (delta: +7.2% - accumulated week of usage)
/// - 7-day Sonnet: 46.1% (delta: +7.4% - accumulated week of usage)
///
/// # Note
/// This fixture is identical to `snapshot_after_7d` because the baseline was
/// chosen on a Wednesday, so 7 days later is also a Wednesday. This is intentional
/// to support both naming conventions for the same scenario.
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::{baseline_snapshot, snapshot_after_7ds};
/// use chrono::{Datelike, Weekday};
///
/// let prev = baseline_snapshot();
/// let curr = snapshot_after_7ds();
///
/// let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
/// assert_eq!(elapsed.num_days(), 7);
/// assert_eq!(prev.taken_at.weekday(), curr.taken_at.weekday());
/// assert_eq!(prev.taken_at.weekday(), Weekday::Wed);
/// ```
pub fn snapshot_after_7ds() -> PrevUsageSnapshot {
    snapshot_after_7d()
}

// ---------------------------------------------------------------------------
// Edge case fixtures
// ---------------------------------------------------------------------------

/// Snapshot with near-zero utilization (idle system).
///
/// Models a system with minimal activity - useful for testing delta computation
/// with small values and edge cases.
///
/// # Timing
/// - Wednesday, March 19, 2026 at 14:30:00 UTC
///
/// # Values
/// - All windows: 0.2% - 0.5% (minimal activity)
pub fn idle_snapshot() -> PrevUsageSnapshot {
    let taken_at = "2026-03-19T14:30:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 0.3,
        seven_day_pct: 0.2,
        weekly_scoped_pct: 0.5,
    }
}

/// Snapshot with high utilization (near capacity).
///
/// Models a system under heavy load - useful for testing delta computation
/// with high values and cutoff risk scenarios.
///
/// # Timing
/// - Thursday, March 20, 2026 at 18:00:00 UTC
///
/// # Values
/// - 5-hour window: 82.4% (high but not critical)
/// - 7-day window: 91.7% (approaching cutoff)
/// - 7-day Sonnet: 94.2% (critical, near cutoff)
pub fn high_utilization_snapshot() -> PrevUsageSnapshot {
    let taken_at = "2026-03-20T18:00:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 82.4,
        seven_day_pct: 91.7,
        weekly_scoped_pct: 94.2,
    }
}

/// Snapshot after a window reset (utilization dropped).
///
/// Models the scenario where a window has reset, causing utilization to
/// drop dramatically. Useful for testing negative delta handling.
///
/// # Timing
/// - Monday, March 23, 2026 at 04:00:00 UTC (after 5-hour window reset at 03:59:59)
///
/// # Values
/// - 5-hour window: 2.1% (just reset from previous high value)
/// - 7-day window: 48.3% (moderate, not yet reset)
/// - 7-day Sonnet: 42.8% (moderate, not yet reset)
pub fn post_reset_snapshot() -> PrevUsageSnapshot {
    // Early Monday after window reset
    let taken_at = "2026-03-23T04:00:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 2.1,        // Just reset
        seven_day_pct: 48.3,       // Still accumulating
        weekly_scoped_pct: 42.8, // Still accumulating
    }
}

// ---------------------------------------------------------------------------
// Composite fixture pairs for delta testing
// ---------------------------------------------------------------------------

/// Fixture pair: baseline and 5-hour later.
///
/// Returns a tuple of (previous, current) snapshots for testing 5-hour delta.
pub fn snapshot_pair_5h() -> (PrevUsageSnapshot, PrevUsageSnapshot) {
    (baseline_snapshot(), snapshot_after_5h())
}

/// Fixture pair: baseline and 7-day later.
///
/// Returns a tuple of (previous, current) snapshots for testing 7-day delta.
pub fn snapshot_pair_7d() -> (PrevUsageSnapshot, PrevUsageSnapshot) {
    (baseline_snapshot(), snapshot_after_7d())
}

/// Fixture pair: baseline and 7-day later (same weekday).
///
/// Returns a tuple of (previous, current) snapshots for testing 7-day same-weekday delta.
pub fn snapshot_pair_7ds() -> (PrevUsageSnapshot, PrevUsageSnapshot) {
    (baseline_snapshot(), snapshot_after_7ds())
}

/// Fixture pair: high utilization and post-reset.
///
/// Returns a tuple of (previous, current) snapshots for testing negative delta
/// (window reset) scenarios.
pub fn snapshot_pair_reset() -> (PrevUsageSnapshot, PrevUsageSnapshot) {
    (high_utilization_snapshot(), post_reset_snapshot())
}

// ---------------------------------------------------------------------------
// Window presence/absence fixtures for structural inactivity testing
// ---------------------------------------------------------------------------

/// Snapshot representing weekly_scoped window present with real data.
///
/// Models a normal poll where the weekly_scoped window is actively reporting
/// utilization (the window exists and is enabled for the account). This is the
/// baseline "healthy" state where the window can legitimately participate in
/// binding-window candidacy.
///
/// # Timing
/// - Wednesday, March 18, 2026 at 12:00:00 UTC
///
/// # Values
/// - 5-hour window: 15.3% (normal operation)
/// - 7-day window: 48.7% (normal operation)
/// - weekly_scoped: 72.5% (active model-scoped cap, reporting real utilization)
///
/// # Use Case
/// This fixture verifies that when weekly_scoped IS present with data, it:
/// - Is NOT excluded from binding-window candidacy
/// - CAN be selected as the binding window (if it has the highest risk_score)
/// - Contributes normally to capacity forecasts
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::weekly_scoped_present_snapshot;
///
/// let snapshot = weekly_scoped_present_snapshot();
/// assert!(snapshot.weekly_scoped_pct > 0.0, "weekly_scoped should report utilization");
/// assert_eq!(snapshot.weekly_scoped_pct, 72.5);
/// ```
pub fn weekly_scoped_present_snapshot() -> PrevUsageSnapshot {
    let taken_at = "2026-03-18T12:00:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 15.3,
        seven_day_pct: 48.7,
        weekly_scoped_pct: 72.5, // Active model-scoped cap with real utilization
    }
}

/// Snapshot representing weekly_scoped window absent from API response.
///
/// Models a poll where the weekly_scoped window is completely missing (null) from
/// the API response. This occurs when the account has no active model-scoped
/// weekly cap, or when the platform temporarily stops reporting it.
///
/// # Timing
/// - Wednesday, March 18, 2026 at 12:05:00 UTC (5 minutes after present snapshot)
///
/// # Values
/// - 5-hour window: 15.8% (slight increase from previous)
/// - 7-day window: 49.1% (slight increase from previous)
/// - weekly_scoped: 0.0% (window absent from API, treated as non-binding)
///
/// # Use Case
/// This fixture, when used consecutively 3+ times, simulates the scenario where
/// weekly_scoped is structurally inactive and should be excluded from binding-window
/// candidacy. The governor should stop waiting for burn-rate data that cannot arrive
/// and select the tightest REMAINING window as binding.
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::weekly_scoped_absent_snapshot;
///
/// let snapshot = weekly_scoped_absent_snapshot();
/// assert_eq!(snapshot.weekly_scoped_pct, 0.0, "weekly_scoped should be 0 when absent");
/// ```
pub fn weekly_scoped_absent_snapshot() -> PrevUsageSnapshot {
    // 5 minutes after the present snapshot (consecutive poll scenario)
    let taken_at = "2026-03-18T12:05:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 15.8,     // Slight increase from previous
        seven_day_pct: 49.1,     // Slight increase from previous
        weekly_scoped_pct: 0.0, // Window absent from API (null response)
    }
}

/// Fixture pair: weekly_scoped present → absent (first consecutive absence).
///
/// Returns a tuple of (previous, current) snapshots representing the transition
/// from an active weekly_scoped window to its first absence from the API.
///
/// # Use Case
/// This simulates the FIRST poll where weekly_scoped goes missing. After this
/// transition, the consecutive-absence counter would be 1 (not yet >=
/// MIN_CONSECUTIVE_ABSENT, so the window is NOT yet excluded).
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::snapshot_pair_weekly_scoped_first_absence;
///
/// let (prev, curr) = snapshot_pair_weekly_scoped_first_absence();
/// assert!(prev.weekly_scoped_pct > 0.0, "previous should have utilization");
/// assert_eq!(curr.weekly_scoped_pct, 0.0, "current should show absence");
pub fn snapshot_pair_weekly_scoped_first_absence() -> (PrevUsageSnapshot, PrevUsageSnapshot) {
    (weekly_scoped_present_snapshot(), weekly_scoped_absent_snapshot())
}

/// Fixture sequence: 3 consecutive polls with weekly_scoped absent.
///
/// Returns a vector of 4 snapshots representing:
/// - Poll 0: weekly_scoped present with data (baseline)
/// - Poll 1: weekly_scoped absent (first absence)
/// - Poll 2: weekly_scoped absent (second consecutive absence)
/// - Poll 3: weekly_scoped absent (third consecutive absence)
///
/// # Timing
/// Each poll is 60 seconds after the previous, matching the governor's default
/// polling interval. Total elapsed: 3 minutes.
///
/// # Use Case
/// This sequence tests the structural-inactivity predicate. After poll 3, the
/// consecutive-absence counter for weekly_scoped reaches MIN_CONSECUTIVE_ABSENT (3),
/// so the window should be excluded from binding-window candidacy starting with
/// the next reconciliation cycle.
///
/// # Values
/// - Poll 0 (baseline): weekly_scoped = 72.5%, five_hour = 15.3%, seven_day = 48.7%
/// - Poll 1 (absent #1): weekly_scoped = 0.0%, five_hour = 16.2%, seven_day = 49.3%
/// - Poll 2 (absent #2): weekly_scoped = 0.0%, five_hour = 17.1%, seven_day = 49.9%
/// - Poll 3 (absent #3): weekly_scoped = 0.0%, five_hour = 18.0%, seven_day = 50.5%
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::weekly_scoped_absent_3_consecutive_polls;
///
/// let polls = weekly_scoped_absent_3_consecutive_polls();
/// assert_eq!(polls.len(), 4, "should have 4 polls (baseline + 3 absences)");
///
/// // Verify the pattern: present → absent, absent, absent
/// assert!(polls[0].weekly_scoped_pct > 0.0, "poll 0 should have data");
/// assert_eq!(polls[1].weekly_scoped_pct, 0.0, "poll 1 should show absence");
/// assert_eq!(polls[2].weekly_scoped_pct, 0.0, "poll 2 should show absence");
/// assert_eq!(polls[3].weekly_scoped_pct, 0.0, "poll 3 should show absence");
pub fn weekly_scoped_absent_3_consecutive_polls() -> Vec<PrevUsageSnapshot> {
    let baseline = weekly_scoped_present_snapshot();
    let taken_at = baseline.taken_at;

    // Poll 1: first absence (60 seconds after baseline)
    let poll1 = PrevUsageSnapshot {
        taken_at: taken_at + chrono::Duration::seconds(60),
        five_hour_pct: 16.2,
        seven_day_pct: 49.3,
        weekly_scoped_pct: 0.0, // Absent
    };

    // Poll 2: second consecutive absence (60 seconds after poll 1)
    let poll2 = PrevUsageSnapshot {
        taken_at: poll1.taken_at + chrono::Duration::seconds(60),
        five_hour_pct: 17.1,
        seven_day_pct: 49.9,
        weekly_scoped_pct: 0.0, // Absent
    };

    // Poll 3: third consecutive absence (60 seconds after poll 2)
    let poll3 = PrevUsageSnapshot {
        taken_at: poll2.taken_at + chrono::Duration::seconds(60),
        five_hour_pct: 18.0,
        seven_day_pct: 50.5,
        weekly_scoped_pct: 0.0, // Absent
    };

    vec![baseline, poll1, poll2, poll3]
}

/// Fixture sequence: 3 consecutive polls with weekly_scoped present with data.
///
/// Returns a vector of 4 snapshots representing normal operation where weekly_scoped
/// is present and reporting utilization across 3 consecutive polls.
///
/// # Timing
/// Each poll is 60 seconds after the previous, matching the governor's default
/// polling interval. Total elapsed: 3 minutes.
///
/// # Use Case
/// This sequence verifies that when weekly_scoped IS consistently present with
/// real data, it remains eligible for binding-window candidacy throughout. The
/// consecutive-absence counter stays at 0, so the window is never excluded.
///
/// # Values
/// - Poll 0 (baseline): weekly_scoped = 72.5%, five_hour = 15.3%, seven_day = 48.7%
/// - Poll 1: weekly_scoped = 73.1%, five_hour = 16.2%, seven_day = 49.3%
/// - Poll 2: weekly_scoped = 73.8%, five_hour = 17.1%, seven_day = 49.9%
/// - Poll 3: weekly_scoped = 74.5%, five_hour = 18.0%, seven_day = 50.5%
///
/// # Example
/// ```rust
/// use claude_governor::snapshot_fixtures::weekly_scoped_present_3_consecutive_polls;
///
/// let polls = weekly_scoped_present_3_consecutive_polls();
/// assert_eq!(polls.len(), 4, "should have 4 polls");
///
/// // Verify all polls have weekly_scoped data
/// for (i, poll) in polls.iter().enumerate() {
///     assert!(poll.weekly_scoped_pct > 0.0, "poll {} should have data", i);
/// }
pub fn weekly_scoped_present_3_consecutive_polls() -> Vec<PrevUsageSnapshot> {
    let baseline = weekly_scoped_present_snapshot();
    let taken_at = baseline.taken_at;

    // Poll 1: present with increased utilization (60 seconds after baseline)
    let poll1 = PrevUsageSnapshot {
        taken_at: taken_at + chrono::Duration::seconds(60),
        five_hour_pct: 16.2,
        seven_day_pct: 49.3,
        weekly_scoped_pct: 73.1, // Present with data
    };

    // Poll 2: present with increased utilization (60 seconds after poll 1)
    let poll2 = PrevUsageSnapshot {
        taken_at: poll1.taken_at + chrono::Duration::seconds(60),
        five_hour_pct: 17.1,
        seven_day_pct: 49.9,
        weekly_scoped_pct: 73.8, // Present with data
    };

    // Poll 3: present with increased utilization (60 seconds after poll 2)
    let poll3 = PrevUsageSnapshot {
        taken_at: poll2.taken_at + chrono::Duration::seconds(60),
        five_hour_pct: 18.0,
        seven_day_pct: 50.5,
        weekly_scoped_pct: 74.5, // Present with data
    };

    vec![baseline, poll1, poll2, poll3]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_baseline_snapshot_has_realistic_values() {
        let snapshot = baseline_snapshot();

        assert_eq!(snapshot.five_hour_pct, 12.5);
        assert_eq!(snapshot.seven_day_pct, 45.2);
        assert_eq!(snapshot.weekly_scoped_pct, 38.7);

        // Verify timestamp is correct
        let expected_time: DateTime<Utc> = "2026-03-18T10:00:00Z".parse().unwrap();
        assert_eq!(snapshot.taken_at, expected_time);

        // Verify it's a Wednesday
        assert_eq!(snapshot.taken_at.weekday(), chrono::Weekday::Wed);
    }

    #[test]
    fn test_snapshot_after_5h_is_5_hours_later() {
        let prev = baseline_snapshot();
        let curr = snapshot_after_5h();

        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_hours(), 5);
        assert_eq!(elapsed.num_minutes(), 300);

        // Verify utilization increased
        assert!(curr.five_hour_pct > prev.five_hour_pct);
        assert!(curr.seven_day_pct > prev.seven_day_pct);
        assert!(curr.weekly_scoped_pct > prev.weekly_scoped_pct);
    }

    #[test]
    fn test_snapshot_after_7d_is_7_days_later() {
        let prev = baseline_snapshot();
        let curr = snapshot_after_7d();

        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_days(), 7);

        // Verify utilization increased
        assert!(curr.seven_day_pct > prev.seven_day_pct);
        assert!(curr.weekly_scoped_pct > prev.weekly_scoped_pct);
    }

    #[test]
    fn test_snapshot_after_7ds_is_same_weekday() {
        let prev = baseline_snapshot();
        let curr = snapshot_after_7ds();

        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_days(), 7);

        // Same weekday (both Wednesday)
        assert_eq!(prev.taken_at.weekday(), curr.taken_at.weekday());
        assert_eq!(prev.taken_at.weekday(), chrono::Weekday::Wed);
    }

    #[test]
    fn test_idle_snapshot_has_low_values() {
        let snapshot = idle_snapshot();

        assert!(snapshot.five_hour_pct < 1.0);
        assert!(snapshot.seven_day_pct < 1.0);
        assert!(snapshot.weekly_scoped_pct < 1.0);
    }

    #[test]
    fn test_high_utilization_snapshot_has_high_values() {
        let snapshot = high_utilization_snapshot();

        assert!(snapshot.five_hour_pct > 80.0);
        assert!(snapshot.seven_day_pct > 90.0);
        assert!(snapshot.weekly_scoped_pct > 90.0);
    }

    #[test]
    fn test_post_reset_snapshot_shows_dramatic_drop() {
        let prev = high_utilization_snapshot();
        let curr = post_reset_snapshot();

        // 5-hour window should show dramatic drop (reset)
        assert!(curr.five_hour_pct < prev.five_hour_pct - 50.0);

        // 7-day windows should not have reset (lower drop)
        assert!(curr.seven_day_pct < prev.seven_day_pct);
        assert!(curr.weekly_scoped_pct < prev.weekly_scoped_pct);
    }

    #[test]
    fn test_snapshot_pair_5h_returns_correct_pair() {
        let (prev, curr) = snapshot_pair_5h();

        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_hours(), 5);
    }

    #[test]
    fn test_snapshot_pair_7d_returns_correct_pair() {
        let (prev, curr) = snapshot_pair_7d();

        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_days(), 7);
    }

    #[test]
    fn test_snapshot_pair_7ds_returns_correct_pair() {
        let (prev, curr) = snapshot_pair_7ds();

        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_days(), 7);

        // Same weekday
        assert_eq!(prev.taken_at.weekday(), curr.taken_at.weekday());
    }

    #[test]
    fn test_snapshot_pair_reset_returns_negative_delta_scenario() {
        let (prev, curr) = snapshot_pair_reset();

        // Should show negative delta in 5-hour window
        let delta_5h = curr.five_hour_pct - prev.five_hour_pct;
        assert!(delta_5h < 0.0, "5-hour delta should be negative after reset");
    }

    #[test]
    fn test_make_snapshot_creates_valid_snapshot() {
        let now = Utc::now();
        let snapshot = make_snapshot(now, 10.0, 20.0, 15.0);

        assert_eq!(snapshot.taken_at, now);
        assert_eq!(snapshot.five_hour_pct, 10.0);
        assert_eq!(snapshot.seven_day_pct, 20.0);
        assert_eq!(snapshot.weekly_scoped_pct, 15.0);
    }

    #[test]
    fn test_all_fixtures_have_valid_percentage_ranges() {
        let fixtures = vec![
            baseline_snapshot(),
            snapshot_after_5h(),
            snapshot_after_7d(),
            snapshot_after_7ds(),
            idle_snapshot(),
            high_utilization_snapshot(),
            post_reset_snapshot(),
        ];

        for snapshot in fixtures {
            // All percentages should be in valid range [0, 100]
            assert!(snapshot.five_hour_pct >= 0.0 && snapshot.five_hour_pct <= 100.0,
                "five_hour_pct out of range: {}", snapshot.five_hour_pct);
            assert!(snapshot.seven_day_pct >= 0.0 && snapshot.seven_day_pct <= 100.0,
                "seven_day_pct out of range: {}", snapshot.seven_day_pct);
            assert!(snapshot.weekly_scoped_pct >= 0.0 && snapshot.weekly_scoped_pct <= 100.0,
                "weekly_scoped_pct out of range: {}", snapshot.weekly_scoped_pct);
        }
    }

    // ---------------------------------------------------------------------------
    // Consecutive snapshot positive delta tests
    // ---------------------------------------------------------------------------

    /// Floating-point tolerance for delta comparisons.
    ///
    /// Using 1e-9 (0.000000001) provides sufficient precision for percentage
    /// comparisons while accommodating floating-point arithmetic imprecision.
    const DELTA_TOLERANCE: f64 = 1e-9;

    /// Test consecutive snapshots with +10% increase for all three window types.
    ///
    /// This test verifies that when consecutive snapshots show a realistic 10% increase
    /// in utilization across all windows, the delta computation correctly identifies
    /// the positive changes.
    ///
    /// # Test Scenario
    /// - Baseline: 12.5% (5h), 45.2% (7d), 38.7% (7ds)
    /// - Current: +10% increase → 13.75% (5h), 49.72% (7d), 42.57% (7ds)
    ///
    /// # Expected Deltas
    /// - p5h_delta = +1.25% (13.75 - 12.5)
    /// - p7d_delta = +4.52% (49.72 - 45.2)
    /// - p7ds_delta = +3.87% (42.57 - 38.7)
    #[test]
    fn test_consecutive_snapshots_positive_10_percent_increase() {
        let prev = baseline_snapshot();

        // Create current snapshot with +10% increase across all windows
        let curr = make_snapshot(
            prev.taken_at + chrono::Duration::hours(5),
            prev.five_hour_pct * 1.10,        // 12.5% → 13.75%
            prev.seven_day_pct * 1.10,        // 45.2% → 49.72%
            prev.weekly_scoped_pct * 1.10, // 38.7% → 42.57%
        );

        // Compute deltas using the standard formula: delta = current - previous
        let delta_5h = curr.five_hour_pct - prev.five_hour_pct;
        let delta_7d = curr.seven_day_pct - prev.seven_day_pct;
        let delta_7ds = curr.weekly_scoped_pct - prev.weekly_scoped_pct;

        // Verify all deltas are positive (usage increased)
        assert!(delta_5h > 0.0, "5h delta should be positive with +10% increase");
        assert!(delta_7d > 0.0, "7d delta should be positive with +10% increase");
        assert!(delta_7ds > 0.0, "7ds delta should be positive with +10% increase");

        // Verify exact delta values (within floating-point tolerance)
        let expected_5h = 1.25;   // 12.5 * 0.10
        let expected_7d = 4.52;   // 45.2 * 0.10
        let expected_7ds = 3.87;  // 38.7 * 0.10

        assert!((delta_5h - expected_5h).abs() < DELTA_TOLERANCE,
            "5h delta should be +{expected_5h}% (10% of baseline), got {delta_5h}");
        assert!((delta_7d - expected_7d).abs() < DELTA_TOLERANCE,
            "7d delta should be +{expected_7d}% (10% of baseline), got {delta_7d}");
        assert!((delta_7ds - expected_7ds).abs() < DELTA_TOLERANCE,
            "7ds delta should be +{expected_7ds}% (10% of baseline), got {delta_7ds}");

        // Verify current snapshot values are exactly 10% higher
        assert!((curr.five_hour_pct - prev.five_hour_pct * 1.10).abs() < DELTA_TOLERANCE,
            "Current 5h should be 10% higher than previous");
        assert!((curr.seven_day_pct - prev.seven_day_pct * 1.10).abs() < DELTA_TOLERANCE,
            "Current 7d should be 10% higher than previous");
        assert!((curr.weekly_scoped_pct - prev.weekly_scoped_pct * 1.10).abs() < DELTA_TOLERANCE,
            "Current 7ds should be 10% higher than previous");
    }

    /// Test consecutive snapshots with +25% increase for all three window types.
    ///
    /// This test verifies that when consecutive snapshots show a realistic 25% increase
    /// in utilization across all windows, the delta computation correctly identifies
    /// the positive changes.
    ///
    /// # Test Scenario
    /// - Baseline: 12.5% (5h), 45.2% (7d), 38.7% (7ds)
    /// - Current: +25% increase → 15.625% (5h), 56.5% (7d), 48.375% (7ds)
    ///
    /// # Expected Deltas
    /// - p5h_delta = +3.125% (15.625 - 12.5)
    /// - p7d_delta = +11.3% (56.5 - 45.2)
    /// - p7ds_delta = +9.675% (48.375 - 38.7)
    #[test]
    fn test_consecutive_snapshots_positive_25_percent_increase() {
        let prev = baseline_snapshot();

        // Create current snapshot with +25% increase across all windows
        let curr = make_snapshot(
            prev.taken_at + chrono::Duration::hours(5),
            prev.five_hour_pct * 1.25,        // 12.5% → 15.625%
            prev.seven_day_pct * 1.25,        // 45.2% → 56.5%
            prev.weekly_scoped_pct * 1.25, // 38.7% → 48.375%
        );

        // Compute deltas using the standard formula: delta = current - previous
        let delta_5h = curr.five_hour_pct - prev.five_hour_pct;
        let delta_7d = curr.seven_day_pct - prev.seven_day_pct;
        let delta_7ds = curr.weekly_scoped_pct - prev.weekly_scoped_pct;

        // Verify all deltas are positive (usage increased)
        assert!(delta_5h > 0.0, "5h delta should be positive with +25% increase");
        assert!(delta_7d > 0.0, "7d delta should be positive with +25% increase");
        assert!(delta_7ds > 0.0, "7ds delta should be positive with +25% increase");

        // Verify exact delta values (within floating-point tolerance)
        let expected_5h = 3.125;  // 12.5 * 0.25
        let expected_7d = 11.3;    // 45.2 * 0.25
        let expected_7ds = 9.675; // 38.7 * 0.25

        assert!((delta_5h - expected_5h).abs() < DELTA_TOLERANCE,
            "5h delta should be +{expected_5h}% (25% of baseline), got {delta_5h}");
        assert!((delta_7d - expected_7d).abs() < DELTA_TOLERANCE,
            "7d delta should be +{expected_7d}% (25% of baseline), got {delta_7d}");
        assert!((delta_7ds - expected_7ds).abs() < DELTA_TOLERANCE,
            "7ds delta should be +{expected_7ds}% (25% of baseline), got {delta_7ds}");

        // Verify current snapshot values are exactly 25% higher
        assert!((curr.five_hour_pct - prev.five_hour_pct * 1.25).abs() < DELTA_TOLERANCE,
            "Current 5h should be 25% higher than previous");
        assert!((curr.seven_day_pct - prev.seven_day_pct * 1.25).abs() < DELTA_TOLERANCE,
            "Current 7d should be 25% higher than previous");
        assert!((curr.weekly_scoped_pct - prev.weekly_scoped_pct * 1.25).abs() < DELTA_TOLERANCE,
            "Current 7ds should be 25% higher than previous");
    }

    /// Test consecutive snapshots with +50% increase for all three window types.
    ///
    /// This test verifies that when consecutive snapshots show a realistic 50% increase
    /// in utilization across all windows, the delta computation correctly identifies
    /// the positive changes. This represents a significant usage spike scenario.
    ///
    /// # Test Scenario
    /// - Baseline: 12.5% (5h), 45.2% (7d), 38.7% (7ds)
    /// - Current: +50% increase → 18.75% (5h), 67.8% (7d), 58.05% (7ds)
    ///
    /// # Expected Deltas
    /// - p5h_delta = +6.25% (18.75 - 12.5)
    /// - p7d_delta = +22.6% (67.8 - 45.2)
    /// - p7ds_delta = +19.35% (58.05 - 38.7)
    #[test]
    fn test_consecutive_snapshots_positive_50_percent_increase() {
        let prev = baseline_snapshot();

        // Create current snapshot with +50% increase across all windows
        let curr = make_snapshot(
            prev.taken_at + chrono::Duration::hours(5),
            prev.five_hour_pct * 1.50,        // 12.5% → 18.75%
            prev.seven_day_pct * 1.50,        // 45.2% → 67.8%
            prev.weekly_scoped_pct * 1.50, // 38.7% → 58.05%
        );

        // Compute deltas using the standard formula: delta = current - previous
        let delta_5h = curr.five_hour_pct - prev.five_hour_pct;
        let delta_7d = curr.seven_day_pct - prev.seven_day_pct;
        let delta_7ds = curr.weekly_scoped_pct - prev.weekly_scoped_pct;

        // Verify all deltas are positive (usage increased significantly)
        assert!(delta_5h > 0.0, "5h delta should be positive with +50% increase");
        assert!(delta_7d > 0.0, "7d delta should be positive with +50% increase");
        assert!(delta_7ds > 0.0, "7ds delta should be positive with +50% increase");

        // Verify exact delta values (within floating-point tolerance)
        let expected_5h = 6.25;   // 12.5 * 0.50
        let expected_7d = 22.6;   // 45.2 * 0.50
        let expected_7ds = 19.35; // 38.7 * 0.50

        assert!((delta_5h - expected_5h).abs() < DELTA_TOLERANCE,
            "5h delta should be +{expected_5h}% (50% of baseline), got {delta_5h}");
        assert!((delta_7d - expected_7d).abs() < DELTA_TOLERANCE,
            "7d delta should be +{expected_7d}% (50% of baseline), got {delta_7d}");
        assert!((delta_7ds - expected_7ds).abs() < DELTA_TOLERANCE,
            "7ds delta should be +{expected_7ds}% (50% of baseline), got {delta_7ds}");

        // Verify current snapshot values are exactly 50% higher
        assert!((curr.five_hour_pct - prev.five_hour_pct * 1.50).abs() < DELTA_TOLERANCE,
            "Current 5h should be 50% higher than previous");
        assert!((curr.seven_day_pct - prev.seven_day_pct * 1.50).abs() < DELTA_TOLERANCE,
            "Current 7d should be 50% higher than previous");
        assert!((curr.weekly_scoped_pct - prev.weekly_scoped_pct * 1.50).abs() < DELTA_TOLERANCE,
            "Current 7ds should be 50% higher than previous");

        // Verify the +50% increase is significant (larger than +10% and +25%)
        assert!(delta_5h > 3.125, "50% increase 5h delta should exceed 25% increase delta");
        assert!(delta_7d > 11.3, "50% increase 7d delta should exceed 25% increase delta");
        assert!(delta_7ds > 9.675, "50% increase 7ds delta should exceed 25% increase delta");
    }

    /// Test consecutive snapshots with mixed realistic increases across window types.
    ///
    /// This test verifies that when consecutive snapshots show different increases
    /// for each window type (realistic scenario where windows accumulate usage at
    /// different rates), the delta computation correctly handles each window independently.
    ///
    /// # Test Scenario
    /// - Baseline: 12.5% (5h), 45.2% (7d), 38.7% (7ds)
    /// - Current: Mixed increases → +15% (5h), +20% (7d), +30% (7ds)
    ///
    /// # Expected Deltas
    /// - p5h_delta = +1.875% (15% of baseline)
    /// - p7d_delta = +9.04% (20% of baseline)
    /// - p7ds_delta = +11.61% (30% of baseline)
    #[test]
    fn test_consecutive_snapshots_mixed_realistic_increases() {
        let prev = baseline_snapshot();

        // Create current snapshot with different increases per window
        let curr = make_snapshot(
            prev.taken_at + chrono::Duration::hours(5),
            prev.five_hour_pct * 1.15,        // 12.5% → 14.375% (+15%)
            prev.seven_day_pct * 1.20,        // 45.2% → 54.24% (+20%)
            prev.weekly_scoped_pct * 1.30, // 38.7% → 50.31% (+30%)
        );

        // Compute deltas using the standard formula: delta = current - previous
        let delta_5h = curr.five_hour_pct - prev.five_hour_pct;
        let delta_7d = curr.seven_day_pct - prev.seven_day_pct;
        let delta_7ds = curr.weekly_scoped_pct - prev.weekly_scoped_pct;

        // Verify all deltas are positive (usage increased)
        assert!(delta_5h > 0.0, "5h delta should be positive with +15% increase");
        assert!(delta_7d > 0.0, "7d delta should be positive with +20% increase");
        assert!(delta_7ds > 0.0, "7ds delta should be positive with +30% increase");

        // Verify exact delta values (within floating-point tolerance)
        let expected_5h = 1.875;  // 12.5 * 0.15
        let expected_7d = 9.04;   // 45.2 * 0.20
        let expected_7ds = 11.61;  // 38.7 * 0.30

        assert!((delta_5h - expected_5h).abs() < DELTA_TOLERANCE,
            "5h delta should be +{expected_5h}% (15% of baseline), got {delta_5h}");
        assert!((delta_7d - expected_7d).abs() < DELTA_TOLERANCE,
            "7d delta should be +{expected_7d}% (20% of baseline), got {delta_7d}");
        assert!((delta_7ds - expected_7ds).abs() < DELTA_TOLERANCE,
            "7ds delta should be +{expected_7ds}% (30% of baseline), got {delta_7ds}");

        // Verify each window increased by its expected percentage
        assert!((curr.five_hour_pct - prev.five_hour_pct * 1.15).abs() < DELTA_TOLERANCE,
            "Current 5h should be 15% higher than previous");
        assert!((curr.seven_day_pct - prev.seven_day_pct * 1.20).abs() < DELTA_TOLERANCE,
            "Current 7d should be 20% higher than previous");
        assert!((curr.weekly_scoped_pct - prev.weekly_scoped_pct * 1.30).abs() < DELTA_TOLERANCE,
            "Current 7ds should be 30% higher than previous");

        // Verify deltas increase with the percentage increase
        assert!(delta_7ds > delta_7d, "30% increase delta should exceed 20% increase delta");
        assert!(delta_7d > delta_5h, "20% increase delta should exceed 15% increase delta");
    }

    /// Test that existing fixture snapshots produce correct positive deltas.
    ///
    /// This test verifies that the pre-existing fixture snapshots (baseline and
    /// after_5h, after_7d, after_7ds) produce the documented positive deltas when
    /// used consecutively. This ensures the fixtures are self-consistent and
    /// accurately represent real usage increases.
    #[test]
    fn test_existing_fixture_snapshots_produce_correct_positive_deltas() {
        // Test baseline → after_5h (5-hour window increase)
        let (prev_5h, curr_5h) = snapshot_pair_5h();
        let delta_5h = curr_5h.five_hour_pct - prev_5h.five_hour_pct;
        let delta_7d = curr_5h.seven_day_pct - prev_5h.seven_day_pct;
        let delta_7ds = curr_5h.weekly_scoped_pct - prev_5h.weekly_scoped_pct;

        assert!(delta_5h > 0.0, "5h window should show positive increase after 5 hours");
        assert!((delta_5h - 5.7).abs() < DELTA_TOLERANCE,
            "5h delta should be +5.7% (18.2 - 12.5), got {delta_5h}");

        assert!(delta_7d > 0.0, "7d window should show positive increase after 5 hours");
        assert!((delta_7d - 1.6).abs() < DELTA_TOLERANCE,
            "7d delta should be +1.6% (46.8 - 45.2), got {delta_7d}");

        assert!(delta_7ds > 0.0, "7ds window should show positive increase after 5 hours");
        assert!((delta_7ds - 1.6).abs() < DELTA_TOLERANCE,
            "7ds delta should be +1.6% (40.3 - 38.7), got {delta_7ds}");

        // Test baseline → after_7d (7-day window increase)
        let (prev_7d, curr_7d) = snapshot_pair_7d();
        let delta_5h_7d = curr_7d.five_hour_pct - prev_7d.five_hour_pct;
        let delta_7d_7d = curr_7d.seven_day_pct - prev_7d.seven_day_pct;
        let delta_7ds_7d = curr_7d.weekly_scoped_pct - prev_7d.weekly_scoped_pct;

        // 5-hour window reset, so we verify it changed (not necessarily positive)
        assert!((delta_5h_7d - 3.3).abs() < DELTA_TOLERANCE,
            "5h delta after 7d should be +3.3% (15.8 - 12.5), got {delta_5h_7d}");

        assert!(delta_7d_7d > 0.0, "7d window should show positive increase after 7 days");
        assert!((delta_7d_7d - 7.2).abs() < DELTA_TOLERANCE,
            "7d delta should be +7.2% (52.4 - 45.2), got {delta_7d_7d}");

        assert!(delta_7ds_7d > 0.0, "7ds window should show positive increase after 7 days");
        assert!((delta_7ds_7d - 7.4).abs() < DELTA_TOLERANCE,
            "7ds delta should be +7.4% (46.1 - 38.7), got {delta_7ds_7d}");

        // Test baseline → after_7ds (same as after_7d)
        let (prev_7ds, curr_7ds) = snapshot_pair_7ds();
        let delta_7d_7ds = curr_7ds.seven_day_pct - prev_7ds.seven_day_pct;
        let delta_7ds_7ds = curr_7ds.weekly_scoped_pct - prev_7ds.weekly_scoped_pct;

        assert!((delta_7d_7ds - delta_7d_7d).abs() < DELTA_TOLERANCE,
            "7ds snapshot should produce same 7d delta as 7d snapshot");
        assert!((delta_7ds_7ds - delta_7ds_7d).abs() < DELTA_TOLERANCE,
            "7ds snapshot should produce same 7ds delta as 7d snapshot");
    }

    /// Test delta computation accuracy with extreme percentage increases.
    ///
    /// This test verifies that the delta computation handles extreme increases
    /// (up to 100% doubling) accurately, ensuring the formula works correctly
    /// even at the boundaries of realistic usage scenarios.
    #[test]
    fn test_delta_computation_accuracy_with_extreme_increases() {
        let prev = baseline_snapshot();

        // Test +75% increase (high but below doubling)
        let curr_75 = make_snapshot(
            prev.taken_at + chrono::Duration::hours(5),
            prev.five_hour_pct * 1.75,
            prev.seven_day_pct * 1.75,
            prev.weekly_scoped_pct * 1.75,
        );

        let delta_5h_75 = curr_75.five_hour_pct - prev.five_hour_pct;
        let delta_7d_75 = curr_75.seven_day_pct - prev.seven_day_pct;
        let delta_7ds_75 = curr_75.weekly_scoped_pct - prev.weekly_scoped_pct;

        assert!((delta_5h_75 - 9.375).abs() < DELTA_TOLERANCE,
            "75% increase 5h delta should be +9.375%, got {delta_5h_75}");
        assert!((delta_7d_75 - 33.9).abs() < DELTA_TOLERANCE,
            "75% increase 7d delta should be +33.9%, got {delta_7d_75}");
        assert!((delta_7ds_75 - 29.025).abs() < DELTA_TOLERANCE,
            "75% increase 7ds delta should be +29.025%, got {delta_7ds_75}");

        // Test +100% increase (doubling)
        let curr_100 = make_snapshot(
            prev.taken_at + chrono::Duration::hours(5),
            prev.five_hour_pct * 2.0,
            prev.seven_day_pct * 2.0,
            prev.weekly_scoped_pct * 2.0,
        );

        let delta_5h_100 = curr_100.five_hour_pct - prev.five_hour_pct;
        let delta_7d_100 = curr_100.seven_day_pct - prev.seven_day_pct;
        let delta_7ds_100 = curr_100.weekly_scoped_pct - prev.weekly_scoped_pct;

        assert!((delta_5h_100 - 12.5).abs() < DELTA_TOLERANCE,
            "100% increase 5h delta should be +12.5%, got {delta_5h_100}");
        assert!((delta_7d_100 - 45.2).abs() < DELTA_TOLERANCE,
            "100% increase 7d delta should be +45.2%, got {delta_7d_100}");
        assert!((delta_7ds_100 - 38.7).abs() < DELTA_TOLERANCE,
            "100% increase 7ds delta should be +38.7%, got {delta_7ds_100}");

        // Verify 100% increase is exactly double the 50% increase
        assert!((delta_5h_100 - 2.0 * 6.25).abs() < DELTA_TOLERANCE,
            "100% increase delta should be exactly 2x 50% increase delta");
        assert!((delta_7d_100 - 2.0 * 22.6).abs() < DELTA_TOLERANCE,
            "100% increase delta should be exactly 2x 50% increase delta");
        assert!((delta_7ds_100 - 2.0 * 19.35).abs() < DELTA_TOLERANCE,
            "100% increase delta should be exactly 2x 50% increase delta");
    }

    /// Test that delta computation is consistent across multiple consecutive polls.
    ///
    /// This test verifies that when we have three consecutive snapshots (prev → mid → curr),
    /// the delta computation is consistent when computing step-by-step (prev→mid, mid→curr)
    /// versus computing the total delta (prev→curr). This ensures the delta formula
    /// maintains additive properties.
    #[test]
    fn test_delta_computation_consistency_across_consecutive_polls() {
        let prev = baseline_snapshot();

        // Create three consecutive snapshots
        let mid = make_snapshot(
            prev.taken_at + chrono::Duration::hours(5),
            prev.five_hour_pct * 1.25,        // +25% at step 1
            prev.seven_day_pct * 1.20,        // +20% at step 1
            prev.weekly_scoped_pct * 1.15, // +15% at step 1
        );

        let curr = make_snapshot(
            mid.taken_at + chrono::Duration::hours(5),
            mid.five_hour_pct * 1.20,        // Additional +20% at step 2
            mid.seven_day_pct * 1.25,        // Additional +25% at step 2
            mid.weekly_scoped_pct * 1.30, // Additional +30% at step 2
        );

        // Compute step-by-step deltas
        let delta_5h_step1 = mid.five_hour_pct - prev.five_hour_pct;
        let delta_7d_step1 = mid.seven_day_pct - prev.seven_day_pct;
        let delta_7ds_step1 = mid.weekly_scoped_pct - prev.weekly_scoped_pct;

        let delta_5h_step2 = curr.five_hour_pct - mid.five_hour_pct;
        let delta_7d_step2 = curr.seven_day_pct - mid.seven_day_pct;
        let delta_7ds_step2 = curr.weekly_scoped_pct - mid.weekly_scoped_pct;

        // Compute total delta (prev → curr)
        let delta_5h_total = curr.five_hour_pct - prev.five_hour_pct;
        let delta_7d_total = curr.seven_day_pct - prev.seven_day_pct;
        let delta_7ds_total = curr.weekly_scoped_pct - prev.weekly_scoped_pct;

        // Verify additivity: total = step1 + step2
        assert!((delta_5h_total - (delta_5h_step1 + delta_5h_step2)).abs() < DELTA_TOLERANCE,
            "Total 5h delta should equal sum of step deltas");
        assert!((delta_7d_total - (delta_7d_step1 + delta_7d_step2)).abs() < DELTA_TOLERANCE,
            "Total 7d delta should equal sum of step deltas");
        assert!((delta_7ds_total - (delta_7ds_step1 + delta_7ds_step2)).abs() < DELTA_TOLERANCE,
            "Total 7ds delta should equal sum of step deltas");

        // Verify the compounded percentages
        // 5h: 1.25 * 1.20 = 1.50 (total +50%)
        assert!((curr.five_hour_pct - prev.five_hour_pct * 1.50).abs() < DELTA_TOLERANCE,
            "Compounded 5h increase should be +50%");
        // 7d: 1.20 * 1.25 = 1.50 (total +50%)
        assert!((curr.seven_day_pct - prev.seven_day_pct * 1.50).abs() < DELTA_TOLERANCE,
            "Compounded 7d increase should be +50%");
        // 7ds: 1.15 * 1.30 = 1.495 (total +49.5%)
        assert!((curr.weekly_scoped_pct - prev.weekly_scoped_pct * 1.495).abs() < DELTA_TOLERANCE,
            "Compounded 7ds increase should be +49.5%");
    }

    // ---------------------------------------------------------------------------
    // Tests for window presence/absence fixtures
    // ---------------------------------------------------------------------------

    #[test]
    fn test_weekly_scoped_present_snapshot_has_real_utilization() {
        let snapshot = weekly_scoped_present_snapshot();

        // Verify weekly_scoped has real utilization data (not zero)
        assert!(snapshot.weekly_scoped_pct > 0.0,
            "weekly_scoped_present_snapshot should have non-zero utilization");
        assert_eq!(snapshot.weekly_scoped_pct, 72.5,
            "weekly_scoped should match the expected utilization");

        // Verify other windows have realistic values
        assert_eq!(snapshot.five_hour_pct, 15.3);
        assert_eq!(snapshot.seven_day_pct, 48.7);

        // Verify timestamp
        let expected_time: DateTime<Utc> = "2026-03-18T12:00:00Z".parse().unwrap();
        assert_eq!(snapshot.taken_at, expected_time);
    }

    #[test]
    fn test_weekly_scoped_absent_snapshot_has_zero_utilization() {
        let snapshot = weekly_scoped_absent_snapshot();

        // Verify weekly_scoped is zero (absent from API)
        assert_eq!(snapshot.weekly_scoped_pct, 0.0,
            "weekly_scoped_absent_snapshot should have 0% utilization (absent from API)");

        // Verify other windows still have values (they're present)
        assert_eq!(snapshot.five_hour_pct, 15.8);
        assert_eq!(snapshot.seven_day_pct, 49.1);

        // Verify timestamp is 5 minutes after the present snapshot
        let present_time = weekly_scoped_present_snapshot().taken_at;
        let elapsed = snapshot.taken_at.signed_duration_since(present_time);
        assert_eq!(elapsed.num_minutes(), 5,
            "absent snapshot should be 5 minutes after present snapshot");
    }

    #[test]
    fn test_snapshot_pair_weekly_scoped_first_absence_documents_transition() {
        let (prev, curr) = snapshot_pair_weekly_scoped_first_absence();

        // Verify previous has data
        assert!(prev.weekly_scoped_pct > 0.0,
            "previous snapshot should have utilization data");
        assert_eq!(prev.weekly_scoped_pct, 72.5);

        // Verify current shows absence
        assert_eq!(curr.weekly_scoped_pct, 0.0,
            "current snapshot should show absence (0%)");

        // Verify this represents the first absence (consecutive count would be 1)
        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_minutes(), 5,
            "transition should occur over 5 minutes (one poll interval)");
    }

    #[test]
    fn test_weekly_scoped_absent_3_consecutive_polls_has_correct_structure() {
        let polls = weekly_scoped_absent_3_consecutive_polls();

        // Verify we have exactly 4 polls
        assert_eq!(polls.len(), 4,
            "should have 4 polls (baseline + 3 consecutive absences)");

        // Verify poll 0: present with data
        assert!(polls[0].weekly_scoped_pct > 0.0,
            "poll 0 (baseline) should have utilization data");
        assert_eq!(polls[0].weekly_scoped_pct, 72.5);

        // Verify polls 1-3: all absent
        for (i, poll) in polls.iter().enumerate().skip(1) {
            assert_eq!(poll.weekly_scoped_pct, 0.0,
                "poll {} should show absence (0%)", i);
        }

        // Verify timestamps are 60 seconds apart (consecutive polls)
        for i in 1..polls.len() {
            let elapsed = polls[i].taken_at.signed_duration_since(polls[i-1].taken_at);
            assert_eq!(elapsed.num_seconds(), 60,
                "poll {} should be 60 seconds after poll {}", i, i-1);
        }

        // Verify other windows show gradual increase (realistic usage)
        assert!(polls[3].five_hour_pct > polls[0].five_hour_pct,
            "5-hour window should show increase over 3 minutes");
        assert!(polls[3].seven_day_pct > polls[0].seven_day_pct,
            "7-day window should show increase over 3 minutes");
    }

    #[test]
    fn test_weekly_scoped_present_3_consecutive_polls_has_correct_structure() {
        let polls = weekly_scoped_present_3_consecutive_polls();

        // Verify we have exactly 4 polls
        assert_eq!(polls.len(), 4,
            "should have 4 polls (baseline + 3 consecutive present polls)");

        // Verify ALL polls have weekly_scoped data
        for (i, poll) in polls.iter().enumerate() {
            assert!(poll.weekly_scoped_pct > 0.0,
                "poll {} should have utilization data (present)", i);
        }

        // Verify timestamps are 60 seconds apart (consecutive polls)
        for i in 1..polls.len() {
            let elapsed = polls[i].taken_at.signed_duration_since(polls[i-1].taken_at);
            assert_eq!(elapsed.num_seconds(), 60,
                "poll {} should be 60 seconds after poll {}", i, i-1);
        }

        // Verify weekly_scoped shows gradual increase (realistic usage)
        assert!(polls[3].weekly_scoped_pct > polls[0].weekly_scoped_pct,
            "weekly_scoped should show increase over 3 minutes");

        // Verify the progression: 72.5 → 73.1 → 73.8 → 74.5
        assert_eq!(polls[0].weekly_scoped_pct, 72.5);
        assert_eq!(polls[1].weekly_scoped_pct, 73.1);
        assert_eq!(polls[2].weekly_scoped_pct, 73.8);
        assert_eq!(polls[3].weekly_scoped_pct, 74.5);
    }

    #[test]
    fn test_weekly_scoped_absent_vs_present_sequences_are_mutually_exclusive() {
        // The absent sequence: poll 0 has data, polls 1-3 are absent
        let absent_polls = weekly_scoped_absent_3_consecutive_polls();
        assert!(absent_polls[0].weekly_scoped_pct > 0.0,
            "absent sequence: poll 0 should have data");
        assert_eq!(absent_polls[1].weekly_scoped_pct, 0.0,
            "absent sequence: poll 1 should be absent");
        assert_eq!(absent_polls[2].weekly_scoped_pct, 0.0,
            "absent sequence: poll 2 should be absent");
        assert_eq!(absent_polls[3].weekly_scoped_pct, 0.0,
            "absent sequence: poll 3 should be absent");

        // The present sequence: ALL polls have data
        let present_polls = weekly_scoped_present_3_consecutive_polls();
        for (i, poll) in present_polls.iter().enumerate() {
            assert!(poll.weekly_scoped_pct > 0.0,
                "present sequence: poll {} should have data", i);
        }

        // Verify the scenarios are truly different
        assert_ne!(absent_polls[3].weekly_scoped_pct, present_polls[3].weekly_scoped_pct,
            "final poll state should differ between scenarios");
    }

    #[test]
    fn test_weekly_scoped_absent_reaches_min_consecutive_threshold() {
        let polls = weekly_scoped_absent_3_consecutive_polls();

        // Simulate the consecutive-absence counter
        let mut consecutive_absent_count = 0;
        let min_threshold = 3; // MIN_CONSECUTIVE_ABSENT from governor.rs

        for (i, poll) in polls.iter().enumerate() {
            if poll.weekly_scoped_pct == 0.0 {
                consecutive_absent_count += 1;
            } else {
                consecutive_absent_count = 0; // Reset on presence
            }

            // Verify the counter reaches exactly 3 after the final poll
            if i == 3 {
                assert_eq!(consecutive_absent_count, min_threshold,
                    "after 3 consecutive absences, counter should reach MIN_CONSECUTIVE_ABSENT (3)");
            }
        }

        // Final verification: counter is 3, meaning window should be excluded
        assert_eq!(consecutive_absent_count, 3,
            "consecutive absent count should be exactly 3 at end of sequence");
    }

    #[test]
    fn test_weekly_scoped_present_never_reaches_absent_threshold() {
        let polls = weekly_scoped_present_3_consecutive_polls();

        // Simulate the consecutive-absence counter
        let mut consecutive_absent_count = 0;
        let min_threshold = 3; // MIN_CONSECUTIVE_ABSENT from governor.rs

        for poll in polls.iter() {
            if poll.weekly_scoped_pct == 0.0 {
                consecutive_absent_count += 1;
            } else {
                consecutive_absent_count = 0; // Reset on presence
            }

            // Verify the counter NEVER reaches 3
            assert!(consecutive_absent_count < min_threshold,
                "consecutive absent count should stay below threshold when window is present");
        }

        // Final verification: counter is 0, meaning window should NOT be excluded
        assert_eq!(consecutive_absent_count, 0,
            "consecutive absent count should be 0 when window is always present");
    }
}
