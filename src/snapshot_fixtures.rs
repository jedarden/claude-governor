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
/// - `seven_day_sonnet_pct`: 7-day window utilization percentage (Sonnet only)
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
    seven_day_sonnet_pct: f64,
) -> PrevUsageSnapshot {
    PrevUsageSnapshot {
        taken_at,
        five_hour_pct,
        seven_day_pct,
        seven_day_sonnet_pct,
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
/// assert_eq!(snapshot.seven_day_sonnet_pct, 38.7);
/// ```
pub fn baseline_snapshot() -> PrevUsageSnapshot {
    // Wednesday, March 18, 2026 at 10:00:00 UTC
    let taken_at = "2026-03-18T10:00:00Z".parse().unwrap();

    PrevUsageSnapshot {
        taken_at,
        five_hour_pct: 12.5,
        seven_day_pct: 45.2,
        seven_day_sonnet_pct: 38.7,
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
        seven_day_sonnet_pct: 40.3, // +1.6% from baseline
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
        seven_day_sonnet_pct: 46.1, // +7.4% from baseline
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
        seven_day_sonnet_pct: 0.5,
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
        seven_day_sonnet_pct: 94.2,
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
        seven_day_sonnet_pct: 42.8, // Still accumulating
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
        assert_eq!(snapshot.seven_day_sonnet_pct, 38.7);

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
        assert!(curr.seven_day_sonnet_pct > prev.seven_day_sonnet_pct);
    }

    #[test]
    fn test_snapshot_after_7d_is_7_days_later() {
        let prev = baseline_snapshot();
        let curr = snapshot_after_7d();

        let elapsed = curr.taken_at.signed_duration_since(prev.taken_at);
        assert_eq!(elapsed.num_days(), 7);

        // Verify utilization increased
        assert!(curr.seven_day_pct > prev.seven_day_pct);
        assert!(curr.seven_day_sonnet_pct > prev.seven_day_sonnet_pct);
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
        assert!(snapshot.seven_day_sonnet_pct < 1.0);
    }

    #[test]
    fn test_high_utilization_snapshot_has_high_values() {
        let snapshot = high_utilization_snapshot();

        assert!(snapshot.five_hour_pct > 80.0);
        assert!(snapshot.seven_day_pct > 90.0);
        assert!(snapshot.seven_day_sonnet_pct > 90.0);
    }

    #[test]
    fn test_post_reset_snapshot_shows_dramatic_drop() {
        let prev = high_utilization_snapshot();
        let curr = post_reset_snapshot();

        // 5-hour window should show dramatic drop (reset)
        assert!(curr.five_hour_pct < prev.five_hour_pct - 50.0);

        // 7-day windows should not have reset (lower drop)
        assert!(curr.seven_day_pct < prev.seven_day_pct);
        assert!(curr.seven_day_sonnet_pct < prev.seven_day_sonnet_pct);
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
        assert_eq!(snapshot.seven_day_sonnet_pct, 15.0);
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
            assert!(snapshot.seven_day_sonnet_pct >= 0.0 && snapshot.seven_day_sonnet_pct <= 100.0,
                "seven_day_sonnet_pct out of range: {}", snapshot.seven_day_sonnet_pct);
        }
    }
}
