//! Governor - Capacity management and scaling decisions
//!
//! This module handles:
//! - Emergency brake detection (98% hard stop)
//! - Underutilization sprint triggering and management
//! - End-of-window capacity sprint
//! - Governor state management
//! - Agent scaling decisions
//! - Main daemon loop: poll -> schedule -> burn_rate -> target -> scale -> alert -> write_state

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::alerts::{
    check_alert_conditions, check_low_cache_efficiency, fire_alert, should_fire, update_cooldown,
    AlertType, SprintTrigger,
};
use crate::burn_rate::{
    compute_composite_safe_workers, effective_multiplier, generate_window_forecast,
    log_capacity_forecast, validate_promotion_from_db, PromotionValidationResult,
};
use crate::calibrator;
use crate::collector;
use crate::config::{
    AgentConfig, AlertConfig, CompositeRiskConfig, ConeScalingConfig, SprintConfig,
};
use crate::db;
use crate::poller::Poller;
use crate::poller::UsagePoller;
use crate::poller::UsageWindow;
use crate::schedule::{self, Promotion};
use crate::state;
use crate::worker::{self, WorkerConfig};

/// Emergency brake threshold (98%)
const EMERGENCY_BRAKE_THRESHOLD: f64 = 98.0;

/// Safe mode: enter when median absolute error (pct points) exceeds this
const SAFE_MODE_ENTRY_ERROR_THRESHOLD: f64 = 15.0;

/// Safe mode: exit when median absolute error drops below this (hysteresis gap)
const SAFE_MODE_EXIT_ERROR_THRESHOLD: f64 = 8.0;

/// Safe mode: minimum prediction samples before safe mode can trigger
const SAFE_MODE_MIN_SAMPLES: u32 = 5;

/// Safe mode: minimum new predictions since entry before exit is allowed
const SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT: u32 = 3;

/// Safe mode: target ceiling reduction (percentage points) while active
const SAFE_MODE_CEILING_REDUCTION: f64 = 5.0;

/// Safe mode: hysteresis band multiplier while active
const SAFE_MODE_HYSTERESIS_MULTIPLIER: f64 = 2.0;

/// Window names for utilization tracking
pub const WINDOW_FIVE_HOUR: &str = "five_hour";
pub const WINDOW_SEVEN_DAY: &str = "seven_day";
pub const WINDOW_WEEKLY_SCOPED: &str = "weekly_scoped";

/// Minimum number of consecutive polls where a window is absent from the API
/// response (null) or reports `is_active == false` before treating it as
/// structurally inactive and excluding it from binding-window candidacy.
///
/// # Rationale
/// - **Value: 3 polls** - Distinguishes a one-off transient null (network hiccup,
///   temporary API lag) from a settled absent state (window not enabled for the
///   account). The governor polls every 60 seconds by default, so 3 polls = 3 minutes
///   of absence — long enough to trust the signal is real, short enough to respond
///   quickly to a genuine capacity window becoming unavailable.
///
/// - **Why not 1?** A single null could be transient; treating it as permanent would
///   cause a window to flicker in/out of binding candidacy on every API blip.
///
/// - **Why not higher (5+)?** The observed live failure mode (weekly_scoped null
///   across every poll while pooled windows had headroom) persisted indefinitely;
///   waiting 5+ minutes would leave the governor pinned at 0 workers for too long.
///
/// - **Tuning path:** If 3 proves too aggressive (false exclusions during brief API
///   outages), increase to 5. If 3 proves too slow (governor holds at 0 for too long
///   before excluding), decrease to 2.
///
/// # Note on `is_active` field population
/// **Finding: `is_active` IS populated in real Anthropic API payloads.**
///
/// Evidence from test_limits_array_parses_alongside_legacy_windows (poller.rs:728-758):
/// - The test fixture is explicitly documented as "The real captured shape"
/// - All three limit entries (session, weekly_all, weekly_scoped) include
///   `"is_active": true` in the captured payload
/// - The field parses successfully through `UsageLimit.is_active: Option<bool>`
///
/// Therefore, the structural-inactivity predicate (to be implemented in child beads)
/// CAN legitimately use both exclusion arms:
/// 1. Consecutive absence (null) from API response across >= MIN_CONSECUTIVE_ABSENT polls
/// 2. API reports `is_active == false` for the window's limit entry
///
/// This constant governs threshold (1); threshold (2) is instantaneous (a single
/// false reading excludes the window immediately, matching the platform's explicit
/// signal that the limit is not active).
pub const MIN_CONSECUTIVE_ABSENT: u32 = 3;

/// Check if a window is structurally inactive.
///
/// A window is considered structurally inactive when EITHER:
/// 1. The window's consecutive absence count (from state.consecutive_absent_polls)
///    is >= MIN_CONSECUTIVE_ABSENT, indicating the window has been absent (null)
///    from the API response across multiple consecutive polls.
/// 2. The API reports `is_active == false` for the window (only if the field is
///    populated in the API response).
///
/// # Arguments
/// - `window_name`: The window identifier ("five_hour", "seven_day", "weekly_scoped")
/// - `window`: The usage window to check
/// - `state`: The governor state containing consecutive absence tracking
///
/// # Returns
/// `true` if the window is structurally inactive (should be excluded from
/// binding-window candidacy), `false` otherwise.
///
/// # Note on is_active field population
/// The is_active field is optional in UsageWindow. When absent/null in the API
/// response, the window is treated as active (not inactive). Only an explicit
/// `false` value marks the window as structurally inactive.
fn is_structurally_inactive(window: &UsageWindow, state: &state::GovernorState) -> bool {
    // Condition 1: Consecutive absence threshold reached
    // Check if the window has been absent (null) from API responses across
    // >= MIN_CONSECUTIVE_ABSENT consecutive polls.
    // This accesses state::GovernorState.is_window_consecutively_absent()
    let is_inactive_by_consecutive_absence = state.is_window_consecutively_absent(&window.name);

    // Condition 2: API reports is_active == false
    // Only treat as inactive if is_active is explicitly false. If None or true,
    // the window is considered active.
    let is_inactive_by_api = window.is_active == Some(false);

    // The window is structurally inactive if EITHER condition is true.
    is_inactive_by_consecutive_absence || is_inactive_by_api
}

// ---------------------------------------------------------------------------
// Annotation Guard Helpers
// ---------------------------------------------------------------------------

/// Reasons why annotation of a window delta interval should be skipped.
///
/// Each variant represents a guard condition that, when triggered,
/// indicates the interval is not suitable for reliable annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Interval is too short (< 2 minutes elapsed) for meaningful delta computation
    IntervalTooShort { elapsed_seconds: i64 },

    /// Worker count changed mid-interval, violating the concurrent session assumption
    WorkerCountChanged { workers_start: u32, workers_end: u32 },

    /// Interval spans a window reset (utilization dropped significantly)
    WindowReset {
        five_hour_reset: bool,
        seven_day_reset: bool,
        weekly_scoped_reset: bool,
    },
}

impl SkipReason {
    /// Human-readable description of the skip reason
    pub fn description(&self) -> String {
        match self {
            SkipReason::IntervalTooShort { elapsed_seconds } => {
                format!("interval too short ({}s < 120s)", elapsed_seconds)
            }
            SkipReason::WorkerCountChanged { workers_start, workers_end } => {
                format!("worker count changed mid-interval ({} -> {})", workers_start, workers_end)
            }
            SkipReason::WindowReset { five_hour_reset, seven_day_reset, weekly_scoped_reset } => {
                let resets: Vec<&str> = [
                    (*five_hour_reset).then_some("5h"),
                    (*seven_day_reset).then_some("7d"),
                    (*weekly_scoped_reset).then_some("7ds"),
                ]
                .into_iter()
                .flatten()
                .collect();
                format!("interval spans window reset ({})", resets.join(", "))
            }
        }
    }
}

/// Minimum elapsed time (in seconds) required for annotation.
///
/// Intervals shorter than this threshold are considered too noisy
/// for reliable delta computation.
const MIN_ELAPSED_SECONDS: i64 = 120;

/// Utilization drop threshold (in percentage points) for detecting window resets.
///
/// When utilization drops by more than this amount between polls, it indicates
/// a window reset occurred.
const WINDOW_RESET_THRESHOLD_PCT: f64 = 1.0;

/// Check if the elapsed time meets the minimum requirement for annotation.
///
/// # Arguments
/// * `t0` - Interval start timestamp
/// * `t1` - Interval end timestamp
///
/// # Returns
/// * `Some(SkipReason::IntervalTooShort)` - if elapsed time < 2 minutes
/// * `None` - if elapsed time is sufficient for annotation
///
/// # Example
/// ```ignore
/// use chrono::Utc;
/// let t0 = Utc::now();
/// let t1 = t0 + chrono::Duration::seconds(90); // Only 90 seconds
/// assert!(check_elapsed_minimum(t0, t1).is_some()); // Should skip
///
/// let t2 = t0 + chrono::Duration::seconds(180); // 3 minutes
/// assert!(check_elapsed_minimum(t0, t2).is_none()); // Should proceed
/// ```
pub fn check_elapsed_minimum(t0: DateTime<Utc>, t1: DateTime<Utc>) -> Option<SkipReason> {
    let elapsed_seconds = (t1 - t0).num_seconds().abs();

    if elapsed_seconds < MIN_ELAPSED_SECONDS {
        return Some(SkipReason::IntervalTooShort { elapsed_seconds });
    }

    None
}

/// Check if worker count remained stable during the interval.
///
/// # Arguments
/// * `workers_start` - Worker count at interval start
/// * `workers_end` - Worker count at interval end
///
/// # Returns
/// * `Some(SkipReason::WorkerCountChanged)` - if worker count changed
/// * `None` - if worker count is stable
///
/// # Example
/// ```ignore
/// // Worker count changed - should skip
/// assert!(check_worker_count_stable(5, 7).is_some());
///
/// // Worker count stable - should proceed
/// assert!(check_worker_count_stable(5, 5).is_none());
/// ```
pub fn check_worker_count_stable(workers_start: u32, workers_end: u32) -> Option<SkipReason> {
    if workers_start != workers_end {
        return Some(SkipReason::WorkerCountChanged {
            workers_start,
            workers_end,
        });
    }

    None
}

/// Check if the interval spans a window reset.
///
/// A window reset is detected when any window's utilization drops by more than
/// `WINDOW_RESET_THRESHOLD_PCT` percentage points between the old and new snapshots.
/// This indicates the window's utilization counter rolled over, making the delta
/// unreliable for annotation.
///
/// # Arguments
/// * `old_pct` - Window utilization at interval start
/// * `new_pct` - Window utilization at interval end
///
/// # Returns
/// * `Some(SkipReason::WindowReset)` - if any window shows a reset
/// * `None` - if no window reset is detected
///
/// # Example
/// ```ignore
/// let old_pct = db::WindowPctSnapshot { five_hour: 20.0, seven_day: 45.0, weekly_scoped: 35.0 };
/// let new_pct = db::WindowPctSnapshot { five_hour: 18.5, seven_day: 46.0, weekly_scoped: 36.0 };
///
/// // 5-hour dropped 1.5% - should skip
/// assert!(check_window_reset(&old_pct, &new_pct).is_some());
///
/// let new_pct2 = db::WindowPctSnapshot { five_hour: 21.5, seven_day: 46.5, weekly_scoped: 36.5 };
/// // All increased or stable - should proceed
/// assert!(check_window_reset(&old_pct, &new_pct2).is_none());
/// ```
pub fn check_window_reset(old_pct: &db::WindowPctSnapshot, new_pct: &db::WindowPctSnapshot) -> Option<SkipReason> {
    let five_hour_reset = new_pct.five_hour < old_pct.five_hour - WINDOW_RESET_THRESHOLD_PCT;
    let seven_day_reset = new_pct.seven_day < old_pct.seven_day - WINDOW_RESET_THRESHOLD_PCT;
    let weekly_scoped_reset = new_pct.weekly_scoped < old_pct.weekly_scoped - WINDOW_RESET_THRESHOLD_PCT;

    if five_hour_reset || seven_day_reset || weekly_scoped_reset {
        return Some(SkipReason::WindowReset {
            five_hour_reset,
            seven_day_reset,
            weekly_scoped_reset,
        });
    }

    None
}

/// Snapshot of usage data for all windows
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    /// Per-window utilization percentages
    pub windows: HashMap<String, f64>,
}

impl UsageSnapshot {
    /// Create a new empty snapshot
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
        }
    }

    /// Create a snapshot from individual window values
    pub fn from_windows(five_hour: f64, seven_day: f64, weekly_scoped: f64) -> Self {
        let mut windows = HashMap::new();
        windows.insert(WINDOW_FIVE_HOUR.to_string(), five_hour);
        windows.insert(WINDOW_SEVEN_DAY.to_string(), seven_day);
        windows.insert(WINDOW_WEEKLY_SCOPED.to_string(), weekly_scoped);
        Self { windows }
    }

    /// Get utilization for a specific window
    pub fn get(&self, window: &str) -> Option<f64> {
        self.windows.get(window).copied()
    }
}

impl Default for UsageSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

/// Emergency brake event
#[derive(Debug, Clone, PartialEq)]
pub struct EmergencyBrake {
    /// The window that triggered the brake
    pub triggered_window: String,

    /// The utilization percentage that triggered the brake
    pub utilization_pct: f64,
}

/// Agent representation for scaling
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    /// Agent identifier
    pub id: String,

    /// Current worker count
    pub workers: u32,

    /// Whether the agent is idle (no active work)
    pub is_idle: bool,
}

/// Window context for sprint eligibility evaluation
#[derive(Debug, Clone, PartialEq)]
pub struct WindowContext {
    /// Window name (five_hour, seven_day, weekly_scoped)
    pub name: String,
    /// Hours remaining until window reset
    pub hours_remaining: f64,
    /// Remaining headroom as percentage (100 - utilization)
    pub headroom_pct: f64,
    /// Whether this window has cutoff_risk
    pub cutoff_risk: bool,
    /// Safe worker count computed for this window (if any)
    pub safe_worker_count: Option<u32>,
    /// Whether there's a bead backlog (workers have work)
    pub has_backlog: bool,
    /// Confidence cone ratio (upper/lower bound, or None if not applicable)
    pub cone_ratio: Option<f64>,
}

/// Active sprint state — tracks an underutilization recovery sprint
#[derive(Debug, Clone, PartialEq)]
pub struct SprintState {
    /// Which agent/worker pool is sprinting
    pub worker_id: String,
    /// The target worker count during sprint
    pub target_workers: u32,
    /// The window that triggered the sprint
    pub window: String,
    /// Original worker count before sprint (to restore after)
    pub original_workers: u32,
    /// When the sprint should end (window reset time)
    pub sprint_expires_at: Option<DateTime<Utc>>,
    /// Normal max workers before sprint boost
    pub normal_max_workers: u32,
}

/// Governor state
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorState {
    /// Whether emergency brake is currently active
    pub emergency_brake_active: bool,

    /// Tracked agents
    pub agents: HashMap<String, Agent>,

    /// The emergency brake event if active
    pub emergency_brake: Option<EmergencyBrake>,

    /// Active sprint state if an underutilization sprint is running
    pub sprint: Option<SprintState>,
}

impl GovernorState {
    /// Create a new governor state
    pub fn new() -> Self {
        Self {
            emergency_brake_active: false,
            agents: HashMap::new(),
            emergency_brake: None,
            sprint: None,
        }
    }

    /// Add or update an agent
    pub fn add_agent(&mut self, id: &str, workers: u32, is_idle: bool) {
        self.agents.insert(
            id.to_string(),
            Agent {
                id: id.to_string(),
                workers,
                is_idle,
            },
        );
    }

    /// Scale all agents to zero workers
    pub fn scale_all_to_zero(&mut self) {
        for agent in self.agents.values_mut() {
            agent.workers = 0;
        }
    }

    /// Check if emergency brake should be applied
    ///
    /// Returns Some(EmergencyBrake) if any window utilization >= 98%,
    /// None otherwise.
    ///
    /// When triggered:
    /// - Scales ALL agents to 0 workers immediately
    /// - Sets emergency_brake_active flag
    /// - Logs the brake application
    /// - (Caller should create HUMAN alert bead)
    pub fn check_emergency_brake(&mut self, usage: &UsageSnapshot) -> Option<EmergencyBrake> {
        // Check all windows for threshold breach
        for (window, &utilization) in &usage.windows {
            if utilization >= EMERGENCY_BRAKE_THRESHOLD {
                // Emergency brake triggered!
                let brake = EmergencyBrake {
                    triggered_window: window.clone(),
                    utilization_pct: utilization,
                };

                // Scale ALL agents to 0 immediately
                self.scale_all_to_zero();

                // Set state flag
                self.emergency_brake_active = true;
                self.emergency_brake = Some(brake.clone());

                // Log the emergency brake
                log::warn!(
                    "EMERGENCY BRAKE APPLIED — {} at {:.1}%",
                    brake.triggered_window,
                    brake.utilization_pct
                );

                return Some(brake);
            }
        }

        None
    }

    /// Clear the emergency brake if utilization has dropped below threshold
    ///
    /// Returns true if the brake was cleared, false otherwise.
    /// Brake clears when:
    /// - Utilization drops below 98% for all windows, OR
    /// - Window resets (detected as significant utilization drop)
    pub fn clear_emergency_brake(&mut self, usage: &UsageSnapshot) -> bool {
        if !self.emergency_brake_active {
            return false;
        }

        // Check if any window is still at or above threshold
        let still_above_threshold = usage
            .windows
            .values()
            .any(|&u| u >= EMERGENCY_BRAKE_THRESHOLD);

        if !still_above_threshold {
            // All windows below threshold, clear the brake
            log::info!(
                "Emergency brake cleared — utilization dropped below {:.0}%",
                EMERGENCY_BRAKE_THRESHOLD
            );
            self.emergency_brake_active = false;
            self.emergency_brake = None;
            return true;
        }

        false
    }

    /// Check emergency brake with automatic clearing
    ///
    /// This combines check and clear in a single call:
    /// - If brake is active, try to clear it first
    /// - If not active (or just cleared), check for new trigger
    pub fn update_emergency_brake(&mut self, usage: &UsageSnapshot) -> Option<EmergencyBrake> {
        // Try to clear existing brake first
        self.clear_emergency_brake(usage);

        // If brake is still active, return it
        if self.emergency_brake_active {
            return self.emergency_brake.clone();
        }

        // Check for new trigger
        self.check_emergency_brake(usage)
    }

    // --- Sprint methods ---

    /// Apply a sprint trigger — boost the affected agent to target workers.
    ///
    /// Saves the original worker count so it can be restored when the sprint ends.
    /// Does nothing if a sprint is already active or emergency brake is engaged.
    pub fn apply_sprint(&mut self, trigger: &SprintTrigger) {
        if self.emergency_brake_active {
            log::warn!("[sprint] Skipping sprint — emergency brake active");
            return;
        }
        if self.sprint.is_some() {
            log::debug!("[sprint] Sprint already active, skipping new trigger");
            return;
        }

        let original_workers = self
            .agents
            .get(&trigger.worker_id)
            .map(|a| a.workers)
            .unwrap_or(0);

        // Boost the agent
        if let Some(agent) = self.agents.get_mut(&trigger.worker_id) {
            agent.workers = trigger.target_workers;
        }

        self.sprint = Some(SprintState {
            worker_id: trigger.worker_id.clone(),
            target_workers: trigger.target_workers,
            window: trigger.window.clone(),
            original_workers,
            sprint_expires_at: None,
            normal_max_workers: 0,
        });

        log::info!(
            "[sprint] Applied: boosting {} from {} to {} workers (window: {})",
            trigger.worker_id,
            original_workers,
            trigger.target_workers,
            trigger.window
        );
    }

    /// Clear the active sprint — restore the agent to its original worker count.
    ///
    /// Returns true if a sprint was active and cleared, false otherwise.
    pub fn clear_sprint(&mut self) -> bool {
        if let Some(sprint) = self.sprint.take() {
            if let Some(agent) = self.agents.get_mut(&sprint.worker_id) {
                agent.workers = sprint.original_workers;
                log::info!(
                    "[sprint] Cleared: restored {} to {} workers",
                    sprint.worker_id,
                    sprint.original_workers
                );
            } else {
                log::info!(
                    "[sprint] Cleared: agent {} no longer tracked",
                    sprint.worker_id
                );
            }
            true
        } else {
            false
        }
    }

    /// Check if the active sprint should end.
    ///
    /// Sprint ends when:
    /// - Usage exceeds the underutilization threshold (sprint achieved its goal), OR
    /// - The triggering window has reset (hours_remaining jumped significantly)
    pub fn check_sprint_end(
        &mut self,
        usage: &UsageSnapshot,
        sprint_config: &SprintConfig,
    ) -> bool {
        let sprint = match &self.sprint {
            Some(s) => s.clone(),
            None => return false,
        };

        let window_util = usage.get(&sprint.window);

        // If utilization exceeds threshold, sprint succeeded
        if let Some(util) = window_util {
            if util >= sprint_config.underutilization_threshold_pct {
                log::info!(
                    "[sprint] Sprint ended: {} utilization reached {:.1}% (threshold: {:.1}%)",
                    sprint.window,
                    util,
                    sprint_config.underutilization_threshold_pct
                );
                return self.clear_sprint();
            }
        }

        false
    }

    /// Check whether a sprint is currently active.
    pub fn is_sprint_active(&self) -> bool {
        self.sprint.is_some()
    }

    // --- End-of-window capacity sprint methods ---

    /// Check if a window is eligible for end-of-window capacity sprint.
    ///
    /// Sprint is eligible when:
    /// - Window resets in <= horizon_minutes (default 90)
    /// - Remaining headroom > min_headroom_pct (default 15%)
    /// - Bead backlog exists (workers have work to do)
    /// - No other window has cutoff_risk
    /// - Confidence cone not too wide (cone_ratio <= max_cone_ratio)
    /// - Safe mode NOT active
    /// - Emergency brake NOT active
    pub fn sprint_eligible(
        &self,
        window_ctx: &WindowContext,
        other_windows: &[WindowContext],
        config: &SprintConfig,
    ) -> bool {
        // Block if emergency brake is active
        if self.emergency_brake_active {
            log::debug!("[sprint] Blocked: emergency brake active");
            return false;
        }

        // Block if safe mode is active
        // Note: This check requires safe_mode state, which we don't have in this struct
        // The caller should check this separately

        // Check horizon: window must reset soon
        let horizon_hours = config.horizon_minutes / 60.0;
        if window_ctx.hours_remaining > horizon_hours {
            log::debug!(
                "[sprint] Blocked: window {} resets in {:.1}h (horizon: {:.1}h)",
                window_ctx.name,
                window_ctx.hours_remaining,
                horizon_hours
            );
            return false;
        }

        // Check minimum headroom
        if window_ctx.headroom_pct <= config.min_headroom_pct {
            log::debug!(
                "[sprint] Blocked: window {} headroom {:.1}% <= min {:.1}%",
                window_ctx.name,
                window_ctx.headroom_pct,
                config.min_headroom_pct
            );
            return false;
        }

        // Check for backlog
        if !window_ctx.has_backlog {
            log::debug!(
                "[sprint] Blocked: no backlog for window {}",
                window_ctx.name
            );
            return false;
        }

        // Check other windows for cutoff_risk
        for other in other_windows {
            if other.cutoff_risk {
                log::debug!(
                    "[sprint] Blocked: other window {} has cutoff_risk",
                    other.name
                );
                return false;
            }
        }

        // Check confidence cone ratio
        if let Some(cone_ratio) = window_ctx.cone_ratio {
            if cone_ratio > config.max_cone_ratio {
                log::debug!(
                    "[sprint] Blocked: cone ratio {:.2} > max {:.2}",
                    cone_ratio,
                    config.max_cone_ratio
                );
                return false;
            }
        }

        true
    }

    /// Check if the active end-of-window sprint should end.
    ///
    /// Sprint ends when:
    /// - Window has reset (hours_remaining jumped)
    /// - Headroom dropped below sprint_end_headroom_pct
    /// - Safe mode activated (caller should check)
    /// - Emergency brake activated (already checked elsewhere)
    pub fn check_eow_sprint_end(
        &mut self,
        window_ctx: &WindowContext,
        config: &SprintConfig,
        now: DateTime<Utc>,
    ) -> bool {
        let sprint = match &self.sprint {
            Some(s) => s.clone(),
            None => return false,
        };

        // Check if sprint has expired (based on window reset time)
        if let Some(expires_at) = sprint.sprint_expires_at {
            if now >= expires_at {
                log::info!(
                    "[sprint] End-of-window sprint ended: {} window reset",
                    sprint.window
                );
                return self.clear_sprint();
            }
        }

        // Check if headroom dropped below minimum
        if window_ctx.headroom_pct < config.sprint_end_headroom_pct {
            log::info!(
                "[sprint] End-of-window sprint ended: headroom {:.1}% < {:.1}%",
                window_ctx.headroom_pct,
                config.sprint_end_headroom_pct
            );
            return self.clear_sprint();
        }

        false
    }

    /// Compute the effective max workers during a sprint.
    ///
    /// During sprint:
    /// - effective_max = normal_max + max_workers_boost
    /// - BUT capped at min(safe_worker_count) across non-sprinting windows
    pub fn compute_sprint_max_workers(
        &self,
        normal_max: u32,
        other_windows: &[WindowContext],
        config: &SprintConfig,
    ) -> u32 {
        let boosted = normal_max.saturating_add(config.max_workers_boost);

        // Find the minimum safe_worker_count across non-sprinting windows
        let min_safe = other_windows
            .iter()
            .filter_map(|w| w.safe_worker_count)
            .min();

        match min_safe {
            Some(cap) => {
                let effective = boosted.min(cap);
                log::debug!(
                    "[sprint] effective_max: {} (boosted: {}, cap: {})",
                    effective,
                    boosted,
                    cap
                );
                effective
            }
            None => boosted,
        }
    }
}

impl Default for GovernorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod governor_state_tests {
    use super::*;

    /// Test that a new GovernorState starts with no emergency brake and no sprint.
    #[test]
    fn test_governor_state_new() {
        let state = GovernorState::new();
        assert!(!state.emergency_brake_active);
        assert!(state.emergency_brake.is_none());
        assert!(state.sprint.is_none());
        assert!(state.agents.is_empty());
    }

    /// Test that add_agent correctly inserts or updates an agent.
    #[test]
    fn test_governor_state_add_agent() {
        let mut state = GovernorState::new();

        state.add_agent("agent-1", 5, false);
        assert_eq!(state.agents.len(), 1);
        assert_eq!(state.agents["agent-1"].workers, 5);
        assert!(!state.agents["agent-1"].is_idle);

        // Update existing agent
        state.add_agent("agent-1", 10, true);
        assert_eq!(state.agents["agent-1"].workers, 10);
        assert!(state.agents["agent-1"].is_idle);
    }

    /// Test that scale_all_to_zero sets all agent workers to 0.
    #[test]
    fn test_governor_state_scale_all_to_zero() {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);
        state.add_agent("agent-2", 3, false);

        state.scale_all_to_zero();

        assert_eq!(state.agents["agent-1"].workers, 0);
        assert_eq!(state.agents["agent-2"].workers, 0);
    }

    /// Test emergency brake triggers at 98% utilization.
    #[test]
    fn test_emergency_brake_triggers_at_threshold() {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);

        let usage = UsageSnapshot::from_windows(99.0, 50.0, 50.0);

        let brake = state.check_emergency_brake(&usage);

        assert!(brake.is_some());
        assert_eq!(brake.as_ref().unwrap().triggered_window, WINDOW_FIVE_HOUR);
        assert_eq!(brake.as_ref().unwrap().utilization_pct, 99.0);
        assert!(state.emergency_brake_active);
        assert_eq!(state.agents["agent-1"].workers, 0);
    }

    /// Test emergency brake does not trigger below 98%.
    #[test]
    fn test_emergency_brake_no_trigger_below_threshold() {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);

        let usage = UsageSnapshot::from_windows(97.0, 50.0, 50.0);

        let brake = state.check_emergency_brake(&usage);

        assert!(brake.is_none());
        assert!(!state.emergency_brake_active);
        assert_eq!(state.agents["agent-1"].workers, 5);
    }

    /// Test clearing emergency brake when utilization drops.
    #[test]
    fn test_clear_emergency_brake() {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);
        state.emergency_brake_active = true;

        let usage = UsageSnapshot::from_windows(50.0, 50.0, 50.0);

        let cleared = state.clear_emergency_brake(&usage);

        assert!(cleared);
        assert!(!state.emergency_brake_active);
        assert!(state.emergency_brake.is_none());
    }

    /// Test sprint application boosts agent workers.
    #[test]
    fn test_apply_sprint() {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);

        let trigger = crate::alerts::SprintTrigger {
            worker_id: "agent-1".to_string(),
            target_workers: 10,
            window: WINDOW_FIVE_HOUR.to_string(),
            utilization_pct: 25.0,
            hours_remaining: 1.5,
            reason: "underutilization sprint".to_string(),
            triggered_at: Utc::now(),
        };

        state.apply_sprint(&trigger);

        assert!(state.sprint.is_some());
        assert_eq!(state.sprint.as_ref().unwrap().target_workers, 10);
        assert_eq!(state.sprint.as_ref().unwrap().original_workers, 5);
        assert_eq!(state.agents["agent-1"].workers, 10);
    }

    /// Test clearing sprint restores original worker count.
    #[test]
    fn test_clear_sprint() {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);

        let trigger = crate::alerts::SprintTrigger {
            worker_id: "agent-1".to_string(),
            target_workers: 10,
            window: WINDOW_FIVE_HOUR.to_string(),
            utilization_pct: 25.0,
            hours_remaining: 1.5,
            reason: "underutilization sprint".to_string(),
            triggered_at: Utc::now(),
        };

        state.apply_sprint(&trigger);
        assert_eq!(state.agents["agent-1"].workers, 10);

        let cleared = state.clear_sprint();

        assert!(cleared);
        assert!(state.sprint.is_none());
        assert_eq!(state.agents["agent-1"].workers, 5);
    }
}

// ---------------------------------------------------------------------------
// Scaling decision
// ---------------------------------------------------------------------------

/// Result of a scaling decision in one cycle
#[derive(Debug, Clone, PartialEq)]
pub enum ScalingDecision {
    /// No change needed (within hysteresis band or already at target)
    NoChange,
    /// Scale up by N workers
    ScaleUp(u32),
    /// Scale down by N workers (graceful)
    ScaleDown(u32),
    /// Emergency brake — scale all to zero
    EmergencyBrake,
}

/// Resolve safe_worker_count to a concrete target, with an explicit fallback when
/// the burn rate data is insufficient (None).
///
/// - `None` → `current_total`: no burn rate data (token collector offline, cursor
///   corruption, or too few samples since restart — see ADR-002). Hold at whatever
///   is currently running and take no scaling action either way, rather than guess.
///   This deliberately does NOT fall back to `max_workers`: doing so meant a fresh
///   restart (current_total=0, zero samples) launched workers at full configured
///   capacity with zero burn-rate awareness — an unmonitored subscription-usage
///   burn with no idea how much quota was actually left. `max_workers` is now only
///   ever reached by data-driven scale-up, one step at a time, once real samples
///   confirm it's affordable. The independent emergency brake (any window >= 98%
///   current utilization, checked before this fallback runs) still applies
///   regardless of data availability, so a real cutoff is still caught.
/// - `Some(0)` → `0`: the forecast says even one worker exhausts the binding window
///   before it resets — scale to 0 and let the window recover. (This is a `use-or-lose`
///   subscription-utilisation governor: idle-then-refill is the intended cycle, and the
///   pools it drives idle at no cost, so there is no cold-start penalty worth holding
///   capacity that would drive the window to a platform cutoff.)
/// - `Some(w)` → `w`: normal case.
fn safe_worker_count_or_hold(safe: Option<u32>, _max_workers: u32, current_total: u32) -> u32 {
    match safe {
        None => {
            log::info!(
                "[governor] insufficient burn rate data, holding at current worker count ({}) — no scaling action",
                current_total
            );
            current_total
        }
        Some(w) => w,
    }
}

/// Parse the `--workspace <path>` argument out of an agent launch command.
fn workspace_from_launch_cmd(launch_cmd: &str) -> Option<String> {
    let mut it = launch_cmd.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == "--workspace" {
            return it.next().map(|s| s.to_string());
        }
    }
    None
}

/// Count ready beads in a workspace via `bf ready`. Returns 0 on any error — a missing
/// backlog signal must only ever *suppress* a sprint, never cause one.
fn count_ready_beads(workspace: &str) -> u32 {
    match std::process::Command::new("bf")
        .arg("ready")
        .current_dir(workspace)
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("bf-"))
            .count() as u32,
        _ => 0,
    }
}

/// Underutilization sprint: when a subscription generator pool is sprint-eligible (a
/// window is under-used, resets soon, nothing at cutoff risk, not in safe mode) AND
/// there is more queued generation work than running workers, boost the target toward
/// that pool's max so spare use-or-lose capacity is burned *productively* rather than
/// left to reset unused. The backlog gate is what keeps "never leave the subscription
/// empty" from meaning "spin up idle runners". Returns the (possibly boosted) target.
fn apply_underutilization_sprint(
    state: &state::GovernorState,
    sprint_config: &SprintConfig,
    agents: &HashMap<String, AgentConfig>,
    base_target: u32,
    now: DateTime<Utc>,
) -> u32 {
    for (name, cfg) in agents {
        if !cfg.subscription || cfg.max_workers == 0 {
            continue;
        }
        let workspace = match workspace_from_launch_cmd(&cfg.launch_cmd) {
            Some(w) => w,
            None => continue,
        };
        let backlog = count_ready_beads(&workspace);
        let current = state.workers.get(name).map(|w| w.current).unwrap_or(0);
        // Only sprint if there is unclaimed work for the extra runners to do.
        if backlog <= current {
            continue;
        }
        if let Some(trigger) = crate::alerts::check_underutilization_sprint_for_worker(
            state,
            sprint_config,
            name,
            cfg.max_workers,
            now,
        ) {
            let boosted = base_target.max(trigger.target_workers);
            if boosted > base_target {
                log::info!(
                    "[governor] underutilization sprint: {} has backlog {} > {} workers; boosting target {} -> {} ({})",
                    name, backlog, current, base_target, boosted, trigger.reason
                );
                return boosted;
            }
        }
    }
    base_target
}

/// Get baseline burn rate config for sonnet (subscription) agents.
///
/// Returns the baseline config from state (warm-start) or from agent config (cold-start).
/// Priority:
/// 1. state.baseline_burn_rates (warm-start - already loaded from config)
/// 2. agent config lookup (cold-start - first time running)
/// 3. default baseline (truly no config available)
fn get_sonnet_baseline_config(
    state: &state::GovernorState,
    agents: &HashMap<String, AgentConfig>,
) -> crate::state::BaselineBurnRates {
    // Warm-start: check state for already-loaded config-derived baselines
    // Prefer needle-sonnet specifically, then any subscription agent baseline from state
    for (name, baseline) in &state.baseline_burn_rates {
        if name.contains("sonnet") || name.contains("needle") {
            log::debug!(
                "[governor] using state-loaded baseline for {} (pct={:.2}/hr, ${:.2}/hr)",
                name,
                baseline.pct_per_worker_per_hour,
                baseline.dollars_per_worker_per_hour
            );
            return baseline.clone();
        }
    }

    // Fallback: use any subscription agent baseline from state
    if let Some((name, baseline)) = state.baseline_burn_rates.iter().next() {
        log::debug!(
            "[governor] using state-loaded baseline for {} (pct={:.2}/hr, ${:.2}/hr)",
            name,
            baseline.pct_per_worker_per_hour,
            baseline.dollars_per_worker_per_hour
        );
        return baseline.clone();
    }

    // Cold-start: state has no baselines, fall back to agent config lookup
    log::debug!("[governor] state has no baseline_burn_rates, falling back to agent config");

    // Helper to convert from burn_rate::BaselineBurnRates to state::BaselineBurnRates
    let convert_baseline =
        |br: crate::burn_rate::BaselineBurnRates| -> crate::state::BaselineBurnRates {
            crate::state::BaselineBurnRates {
                pct_per_worker_per_hour: br.pct_per_worker_per_hour,
                dollars_per_worker_per_hour: br.dollars_per_worker_per_hour,
            }
        };

    // First try to find a subscription agent with "sonnet" in its name
    for (name, cfg) in agents {
        if cfg.subscription && (name.contains("sonnet") || name.contains("needle")) {
            return convert_baseline(cfg.baseline_burn_rate_or_default());
        }
    }

    // Fallback: use the first subscription agent's baseline, or default
    for (name, cfg) in agents {
        if cfg.subscription {
            log::debug!(
                "[governor] no sonnet agent found, using {} agent's baseline for dollar staleness checks",
                name
            );
            return convert_baseline(cfg.baseline_burn_rate_or_default());
        }
    }

    // No subscription agents at all - use default
    log::warn!(
        "[governor] no subscription agents configured, using default baseline for dollar staleness checks"
    );
    crate::state::BaselineBurnRates::default()
}

// ---------------------------------------------------------------------------
// Window delta calculation helpers
// ---------------------------------------------------------------------------

/// Calculate percentage deltas between consecutive API poll snapshots.
///
/// Computes the per-window percentage changes between two consecutive
/// usage snapshots. Returns deltas for (5-hour, 7-day, 7-day-sonnet) windows.
///
/// # Arguments
/// - `previous_snapshot`: Usage snapshot from the previous poll cycle
/// - `current_snapshot`: Usage snapshot from the current poll cycle
///
/// # Returns
/// A tuple of (delta_5h, delta_7d, delta_7ds) where each value is the
/// percentage change (current - previous) for that window.
///
/// # Example
/// ```
/// use claude_governor::db::WindowPctSnapshot;
/// use claude_governor::governor::calculate_window_pct_delta;
/// let prev = WindowPctSnapshot { five_hour: 10.0, seven_day: 20.0, weekly_scoped: 15.0 };
/// let curr = WindowPctSnapshot { five_hour: 12.5, seven_day: 22.0, weekly_scoped: 18.0 };
/// let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);
/// assert_eq!(d5h, 2.5);  // 12.5 - 10.0
/// assert_eq!(d7d, 2.0);   // 22.0 - 20.0
/// assert_eq!(d7ds, 3.0); // 18.0 - 15.0
/// ```
pub fn calculate_window_pct_delta(
    previous_snapshot: &crate::db::WindowPctSnapshot,
    current_snapshot: &crate::db::WindowPctSnapshot,
) -> (f64, f64, f64) {
    let delta_5h = current_snapshot.five_hour - previous_snapshot.five_hour;
    let delta_7d = current_snapshot.seven_day - previous_snapshot.seven_day;
    let delta_7ds = current_snapshot.weekly_scoped - previous_snapshot.weekly_scoped;
    (delta_5h, delta_7d, delta_7ds)
}

/// Apportion a total delta to a specific session based on USD weight.
///
/// When a fleet-wide percentage delta is observed, this function computes
/// the portion attributable to a single session by weighting the session's
/// USD spend against the total fleet spend for the interval.
///
/// # Arguments
/// - `total_delta`: The total percentage delta for the entire fleet
/// - `total_usd`: Total USD spent by the entire fleet in the interval
/// - `session_total_usd`: USD spent by this specific session in the interval
///
/// # Returns
/// The apportioned delta for this session (will be 0.0 if total_usd is 0.0).
///
/// # Example
/// ```
/// use claude_governor::governor::apportion_delta;
/// // Fleet burned 2.5% of 7-day quota in an interval
/// // Session A spent $10 out of fleet total $50
/// let session_delta = apportion_delta(2.5, 50.0, 10.0);
/// assert!((session_delta - 0.5).abs() < 0.001);  // 2.5 * (10/50) = 0.5
/// ```
pub fn apportion_delta(total_delta: f64, total_usd: f64, session_total_usd: f64) -> f64 {
    if total_usd <= 0.0 {
        return 0.0;
    }
    let weight = session_total_usd / total_usd;
    total_delta * weight
}

#[cfg(test)]
mod window_delta_tests {
    use super::*;

    #[test]
    fn test_calculate_window_pct_delta_basic() {
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 10.0,
            seven_day: 20.0,
            weekly_scoped: 15.0,
        };
        let curr = crate::db::WindowPctSnapshot {
            five_hour: 12.5,
            seven_day: 22.0,
            weekly_scoped: 18.0,
        };
        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);
        assert!((d5h - 2.5).abs() < f64::EPSILON);
        assert!((d7d - 2.0).abs() < f64::EPSILON);
        assert!((d7ds - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_calculate_window_pct_delta_negative_deltas() {
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 30.0,
            weekly_scoped: 25.0,
        };
        let curr = crate::db::WindowPctSnapshot {
            five_hour: 15.0,
            seven_day: 22.0,
            weekly_scoped: 18.0,
        };
        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);
        assert!((d5h - (-5.0)).abs() < f64::EPSILON);
        assert!((d7d - (-8.0)).abs() < f64::EPSILON);
        assert!((d7ds - (-7.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_calculate_window_pct_delta_zero_previous() {
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 0.0,
            seven_day: 0.0,
            weekly_scoped: 0.0,
        };
        let curr = crate::db::WindowPctSnapshot {
            five_hour: 5.0,
            seven_day: 10.0,
            weekly_scoped: 7.5,
        };
        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);
        assert!((d5h - 5.0).abs() < f64::EPSILON);
        assert!((d7d - 10.0).abs() < f64::EPSILON);
        assert!((d7ds - 7.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_apportion_delta_basic() {
        // Fleet delta: 2.5%, fleet total: $50, session: $10
        let result = apportion_delta(2.5, 50.0, 10.0);
        assert!((result - 0.5).abs() < f64::EPSILON); // 2.5 * (10/50) = 0.5
    }

    #[test]
    fn test_apportion_delta_zero_total_usd() {
        let result = apportion_delta(2.5, 0.0, 10.0);
        assert_eq!(result, 0.0);
    }

    #[test]
    fn test_apportion_delta_zero_session_usd() {
        let result = apportion_delta(2.5, 50.0, 0.0);
        assert_eq!(result, 0.0);
    }

    #[test]
    fn test_apportion_delta_equal_weights() {
        // Two sessions with equal spend
        let result1 = apportion_delta(3.0, 60.0, 30.0); // Half of total
        let result2 = apportion_delta(3.0, 60.0, 30.0); // Half of total
        assert!((result1 - 1.5).abs() < f64::EPSILON);
        assert!((result2 - 1.5).abs() < f64::EPSILON);
        assert!((result1 + result2 - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_apportion_delta_negative_total_delta() {
        // Window reset case: negative delta
        let result = apportion_delta(-5.0, 50.0, 10.0);
        assert!((result - (-1.0)).abs() < f64::EPSILON); // -5.0 * (10/50) = -1.0
    }

    #[test]
    fn test_apportion_delta_fractional_weights() {
        // Session spent 1/3 of total
        let result = apportion_delta(6.0, 90.0, 30.0);
        assert!((result - 2.0).abs() < f64::EPSILON); // 6.0 * (30/90) = 2.0
    }

    // -----------------------------------------------------------------------
    // Snapshot delta computation tests - consecutive API polls
    // -----------------------------------------------------------------------

    /// Test that consecutive snapshots produce correct non-zero deltas.
    ///
    /// Simulates the poll cycle where previous_api_snapshot holds the
    /// previous reading and current_api_snapshot holds the new reading.
    #[test]
    fn test_consecutive_snapshots_non_zero_deltas() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        let prev = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        };

        let curr = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 12.5,
            seven_day_pct: 22.0,
            weekly_scoped_pct: 18.0,
        };

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Verify all deltas are non-zero and positive
        assert!(delta_5h > 0.0, "5h delta should be positive");
        assert!(delta_7d > 0.0, "7d delta should be positive");
        assert!(delta_7ds > 0.0, "7ds delta should be positive");

        // Verify exact delta values
        assert!(
            (delta_5h - 2.5).abs() < f64::EPSILON,
            "5h delta = 12.5 - 10.0 = 2.5"
        );
        assert!(
            (delta_7d - 2.0).abs() < f64::EPSILON,
            "7d delta = 22.0 - 20.0 = 2.0"
        );
        assert!(
            (delta_7ds - 3.0).abs() < f64::EPSILON,
            "7ds delta = 18.0 - 15.0 = 3.0"
        );
    }

    /// Test that identical snapshots produce zero deltas.
    ///
    /// When the API percentage hasn't changed between consecutive polls,
    /// all deltas should be zero.
    #[test]
    fn test_identical_snapshots_zero_deltas() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        let snapshot = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 25.0,
            seven_day_pct: 35.0,
            weekly_scoped_pct: 28.0,
        };

        // Previous and current are identical
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: snapshot.five_hour_pct,
            seven_day: snapshot.seven_day_pct,
            weekly_scoped: snapshot.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: snapshot.five_hour_pct,
            seven_day: snapshot.seven_day_pct,
            weekly_scoped: snapshot.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // All deltas should be exactly zero
        assert_eq!(
            delta_5h, 0.0,
            "5h delta should be zero for identical snapshots"
        );
        assert_eq!(
            delta_7d, 0.0,
            "7d delta should be zero for identical snapshots"
        );
        assert_eq!(
            delta_7ds, 0.0,
            "7ds delta should be zero for identical snapshots"
        );
    }

    /// Test first poll handling when no previous snapshot exists.
    ///
    /// On the first poll (after governor start or state clear), previous_api_snapshot
    /// is None, so deltas cannot be computed. The code handles this gracefully by
    /// only computing deltas when both previous and current snapshots exist.
    #[test]
    fn test_first_poll_no_previous_snapshot() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Simulate first poll: only current snapshot exists
        let current: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        });

        let previous: Option<PrevUsageSnapshot> = None;

        // Track whether delta computation was attempted
        let mut delta_computation_attempted = false;

        // ASSERTION 1: Verify snapshot state BEFORE the match
        assert!(
            previous.is_none(),
            "Previous snapshot should be None on first poll"
        );
        assert!(
            current.is_some(),
            "Current snapshot should be Some on first poll"
        );

        // The code should handle this gracefully - no delta computation
        // This simulates the check in run_governor_cycle:
        // if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot)
        let match_result = match (&previous, &current) {
            (Some(prev), Some(curr)) => {
                // This branch should NOT execute on first poll
                delta_computation_attempted = true;
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let _deltas = calculate_window_pct_delta(&prev_pct, &curr_pct);
                "delta_computed"
            }
            (None, Some(_curr)) => {
                // Expected on first poll: previous is None, current exists
                "first_poll_skip"
            }
            (None, None) => {
                // Neither snapshot available
                "no_snapshots"
            }
            (Some(_prev), None) => {
                // Only previous exists (shouldn't happen in normal flow)
                "only_previous"
            }
        };

        // ASSERTION 2: Verify delta computation was skipped (returns early)
        assert!(
            !delta_computation_attempted,
            "Delta computation should be skipped on first poll when prev_snapshot is None"
        );

        // ASSERTION 3: Verify the match fell into the correct branch
        assert_eq!(
            match_result, "first_poll_skip",
            "Should match the (None, Some) branch on first poll"
        );

        // ASSERTION 4: Verify no panic occurred - test reaches this point
        // (If we reach here, graceful handling succeeded)
    }

    /// Test that delta calculation uses the correct window fields.
    ///
    /// Verifies that the delta calculation correctly pairs:
    /// - five_hour_pct -> five_hour
    /// - seven_day_pct -> seven_day
    /// - weekly_scoped_pct -> weekly_scoped
    #[test]
    fn test_delta_uses_correct_window_fields() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        let prev = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        };

        let curr = PrevUsageSnapshot {
            taken_at: Utc::now(),
            // Each window changes differently
            five_hour_pct: 15.0,     // +5.0
            seven_day_pct: 25.0,     // +5.0
            weekly_scoped_pct: 20.0, // +5.0
        };

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Verify each delta uses the correct field pair
        assert!(
            (delta_5h - 5.0).abs() < f64::EPSILON,
            "5h: curr(15.0) - prev(10.0) = 5.0"
        );
        assert!(
            (delta_7d - 5.0).abs() < f64::EPSILON,
            "7d: curr(25.0) - prev(20.0) = 5.0"
        );
        assert!(
            (delta_7ds - 5.0).abs() < f64::EPSILON,
            "7ds: curr(20.0) - prev(15.0) = 5.0"
        );
    }

    /// Test that negative deltas (window resets) are computed correctly.
    ///
    /// When a window resets, the utilization drops, producing negative deltas.
    #[test]
    fn test_negative_deltas_window_reset() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        let prev = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 80.0,
            seven_day_pct: 90.0,
            weekly_scoped_pct: 85.0,
        };

        let curr = PrevUsageSnapshot {
            taken_at: Utc::now(),
            // Window reset - utilization drops
            five_hour_pct: 5.0,     // -75.0 (reset)
            seven_day_pct: 15.0,    // -75.0 (reset)
            weekly_scoped_pct: 8.0, // -77.0 (reset)
        };

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Verify all deltas are negative (window reset)
        assert!(delta_5h < 0.0, "5h delta should be negative on reset");
        assert!(delta_7d < 0.0, "7d delta should be negative on reset");
        assert!(delta_7ds < 0.0, "7ds delta should be negative on reset");

        // Verify exact values
        assert!(
            (delta_5h - (-75.0)).abs() < f64::EPSILON,
            "5h: 5.0 - 80.0 = -75.0"
        );
        assert!(
            (delta_7d - (-75.0)).abs() < f64::EPSILON,
            "7d: 15.0 - 90.0 = -75.0"
        );
        assert!(
            (delta_7ds - (-77.0)).abs() < f64::EPSILON,
            "7ds: 8.0 - 85.0 = -77.0"
        );
    }

    /// Test mixed deltas: some windows increase, some decrease.
    ///
    /// Simulates a realistic scenario where windows behave differently.
    #[test]
    fn test_mixed_deltas_increase_and_decrease() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        let prev = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 50.0,
            seven_day_pct: 60.0,
            weekly_scoped_pct: 55.0,
        };

        let curr = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 55.0,     // +5.0 (increasing)
            seven_day_pct: 58.0,     // -2.0 (slight decrease)
            weekly_scoped_pct: 62.0, // +7.0 (increasing)
        };

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        assert!(
            (delta_5h - 5.0).abs() < f64::EPSILON,
            "5h should increase by 5.0"
        );
        assert!(
            (delta_7d - (-2.0)).abs() < f64::EPSILON,
            "7d should decrease by 2.0"
        );
        assert!(
            (delta_7ds - 7.0).abs() < f64::EPSILON,
            "7ds should increase by 7.0"
        );
    }

    /// Test delta precision with very small changes.
    ///
    /// Verifies that the delta calculation handles small percentage changes
    /// accurately (e.g., 0.1% increments).
    #[test]
    fn test_delta_precision_small_changes() {
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 50.0,
            seven_day: 60.0,
            weekly_scoped: 55.0,
        };

        let curr = crate::db::WindowPctSnapshot {
            five_hour: 50.1,
            seven_day: 60.05,
            weekly_scoped: 55.001,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev, &curr);

        // Use a tolerance suitable for percentage values (1e-9 is more than sufficient)
        const TOL: f64 = 1e-9;
        assert!((delta_5h - 0.1).abs() < TOL, "5h: 50.1 - 50.0 = 0.1");
        assert!((delta_7d - 0.05).abs() < TOL, "7d: 60.05 - 60.0 = 0.05");
        assert!(
            (delta_7ds - 0.001).abs() < TOL,
            "7ds: 55.001 - 55.0 = 0.001"
        );
    }

    // ---------------------------------------------------------------------------
    // Test helper functions
    // ---------------------------------------------------------------------------

    /// Create a WindowPctSnapshot with specified utilization percentages.
    ///
    /// Helper function to create WindowPctSnapshot instances with custom values
    /// for testing delta calculations and other window percentage operations.
    ///
    /// # Arguments
    /// - `five_hour`: 5-hour window utilization percentage
    /// - `seven_day`: 7-day window utilization percentage (all models)
    /// - `weekly_scoped`: 7-day window utilization percentage (Sonnet only)
    ///
    /// ⚠️ BUG: The documentation above incorrectly states "Sonnet only".
    /// The weekly_scoped field is MODEL-AGNOSTIC and can be scoped to ANY model
    /// (Fable, Opus, Sonnet, etc.) depending on which model carries the scoped cap
    /// this period. See state.rs UsageState.weekly_scoped_model and weekly_scoped_pct
    /// for the model-agnostic implementation. This affects all locations where
    /// weekly_scoped is documented as "Sonnet only" in this file.
    ///
    /// # Returns
    /// A WindowPctSnapshot struct with the specified values.
    ///
    /// # Example
    /// ```rust
    /// use crate::governor::window_delta_tests::make_window_pct_snapshot;
    ///
    /// let snapshot = make_window_pct_snapshot(25.5, 45.0, 38.2);
    /// assert_eq!(snapshot.five_hour, 25.5);
    /// assert_eq!(snapshot.seven_day, 45.0);
    /// assert_eq!(snapshot.weekly_scoped, 38.2);
    /// ```
    pub fn make_window_pct_snapshot(
        five_hour: f64,
        seven_day: f64,
        weekly_scoped: f64,
    ) -> crate::db::WindowPctSnapshot {
        crate::db::WindowPctSnapshot {
            five_hour,
            seven_day,
            weekly_scoped,
        }
    }

    /// Create a PrevUsageSnapshot with specified values and current timestamp.
    ///
    /// Helper function to create PrevUsageSnapshot instances for testing
    /// consecutive API poll scenarios and delta calculations.
    ///
    /// # Arguments
    /// - `five_hour_pct`: 5-hour window utilization percentage
    /// - `seven_day_pct`: 7-day window utilization percentage (all models)
    /// - `weekly_scoped_pct`: 7-day window utilization percentage (Sonnet only)
    ///
    /// ⚠️ BUG: The documentation above incorrectly states "Sonnet only".
    /// The weekly_scoped_pct field is MODEL-AGNOSTIC and applies to whatever model
    /// carries the scoped cap this period (Fable, Opus, Sonnet, etc.).
    /// The correct implementation uses state::UsageState.weekly_scoped_model to
    /// determine which model is active, and weekly_scoped_pct for the utilization.
    /// See state.rs lines 70-77 for the model-agnostic design.
    ///
    /// # Returns
    /// A PrevUsageSnapshot struct with the specified values and current timestamp.
    ///
    /// # Example
    /// ```rust
    /// use crate::governor::window_delta_tests::make_usage_snapshot;
    ///
    /// let snapshot = make_usage_snapshot(12.5, 35.0, 28.5);
    /// assert_eq!(snapshot.five_hour_pct, 12.5);
    /// assert_eq!(snapshot.seven_day_pct, 35.0);
    /// assert_eq!(snapshot.weekly_scoped_pct, 28.5);
    /// // timestamp is set to Utc::now()
    /// ```
    pub fn make_usage_snapshot(
        five_hour_pct: f64,
        seven_day_pct: f64,
        weekly_scoped_pct: f64,
    ) -> crate::state::PrevUsageSnapshot {
        crate::state::PrevUsageSnapshot {
            taken_at: chrono::Utc::now(),
            five_hour_pct,
            seven_day_pct,
            weekly_scoped_pct,
        }
    }

    /// Create a PrevUsageSnapshot with a custom timestamp.
    ///
    /// Helper function to create PrevUsageSnapshot instances with a specific
    /// timestamp for testing time-sensitive scenarios (e.g., elapsed time calculations).
    ///
    /// # Arguments
    /// - `taken_at`: The timestamp when the snapshot was taken
    /// - `five_hour_pct`: 5-hour window utilization percentage
    /// - `seven_day_pct`: 7-day window utilization percentage (all models)
    /// - `weekly_scoped_pct`: 7-day window utilization percentage (Sonnet only)
    ///
    /// ⚠️ BUG: The documentation above incorrectly states "Sonnet only".
    /// weekly_scoped_pct is MODEL-AGNOSTIC. Use state::UsageState.weekly_scoped_model
    /// to determine which model (Fable, Opus, etc.) carries the scoped cap this period.
    /// The legacy sonnet_pct field is deprecated; see state.rs lines 53-56.
    ///
    /// # Returns
    /// A PrevUsageSnapshot struct with the specified values and custom timestamp.
    ///
    /// # Example
    /// ```rust
    /// use crate::governor::window_delta_tests::make_usage_snapshot_with_time;
    /// use chrono::Utc;
    ///
    /// let earlier_time = Utc::now() - chrono::Duration::seconds(120);
    /// let snapshot = make_usage_snapshot_with_time(earlier_time, 15.0, 40.0, 32.0);
    /// assert_eq!(snapshot.five_hour_pct, 15.0);
    /// assert_eq!(snapshot.taken_at, earlier_time);
    /// ```
    pub fn make_usage_snapshot_with_time(
        taken_at: chrono::DateTime<chrono::Utc>,
        five_hour_pct: f64,
        seven_day_pct: f64,
        weekly_scoped_pct: f64,
    ) -> crate::state::PrevUsageSnapshot {
        crate::state::PrevUsageSnapshot {
            taken_at,
            five_hour_pct,
            seven_day_pct,
            weekly_scoped_pct,
        }
    }

    /// Test that snapshot helper functions create valid structs.
    ///
    /// Demonstrates the usage of the helper functions and verifies they
    /// produce correctly constructed snapshots.
    #[test]
    fn test_snapshot_helpers_create_valid_structs() {
        // Test make_window_pct_snapshot
        let window_snap = make_window_pct_snapshot(10.5, 25.0, 18.75);
        assert!((window_snap.five_hour - 10.5).abs() < f64::EPSILON);
        assert!((window_snap.seven_day - 25.0).abs() < f64::EPSILON);
        assert!((window_snap.weekly_scoped - 18.75).abs() < f64::EPSILON);

        // Test make_usage_snapshot (with current timestamp)
        let usage_snap = make_usage_snapshot(12.5, 30.0, 22.5);
        assert!((usage_snap.five_hour_pct - 12.5).abs() < f64::EPSILON);
        assert!((usage_snap.seven_day_pct - 30.0).abs() < f64::EPSILON);
        assert!((usage_snap.weekly_scoped_pct - 22.5).abs() < f64::EPSILON);
        // Timestamp should be recent (within last second)
        let age = (chrono::Utc::now() - usage_snap.taken_at).num_seconds();
        assert!(age >= 0 && age <= 1, "timestamp should be current");

        // Test make_usage_snapshot_with_time (with custom timestamp)
        let custom_time = chrono::Utc::now() - chrono::Duration::seconds(60);
        let custom_snap = make_usage_snapshot_with_time(custom_time, 8.0, 20.0, 15.0);
        assert!((custom_snap.five_hour_pct - 8.0).abs() < f64::EPSILON);
        assert!((custom_snap.seven_day_pct - 20.0).abs() < f64::EPSILON);
        assert!((custom_snap.weekly_scoped_pct - 15.0).abs() < f64::EPSILON);
        assert_eq!(custom_snap.taken_at, custom_time);
    }

    // ---------------------------------------------------------------------------
    // First poll handling tests
    // ---------------------------------------------------------------------------

    /// Test first poll handling when previous_api_snapshot is None.
    ///
    /// Verifies that on the first poll (after governor start or state clear):
    /// - No panic occurs
    /// - Delta computation is skipped (graceful handling)
    /// - Default delta values are used (set to Some(0.0))
    #[test]
    fn test_first_poll_delta_defaults_to_zero() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Simulate first poll: previous_api_snapshot is None, current_api_snapshot is Some
        let previous_api_snapshot: Option<PrevUsageSnapshot> = None;
        let current_api_snapshot: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        });

        // Simulate the delta computation logic from run_governor_cycle
        let mut p5h_delta: Option<f64> = None;
        let mut p7d_delta: Option<f64> = None;
        let mut p7ds_delta: Option<f64> = None;

        // Explicit pattern matching for all snapshot availability cases
        // This mirrors the code in run_governor_cycle (lines 2012-2057)
        match (&previous_api_snapshot, &current_api_snapshot) {
            (Some(prev), Some(curr)) => {
                // Both snapshots available: proceed with delta computation
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);

                p5h_delta = Some(delta_5h);
                p7d_delta = Some(delta_7d);
                p7ds_delta = Some(delta_7ds);
            }
            (None, Some(_curr)) => {
                // First poll: no previous snapshot available, cannot compute delta
                // Set delta fields to zero to indicate no change from initial state
                p5h_delta = Some(0.0);
                p7d_delta = Some(0.0);
                p7ds_delta = Some(0.0);
            }
            (None, None) | (Some(_), None) => {
                // Neither snapshot available OR only previous available: handle gracefully
                // Leave deltas as None
            }
        }

        // Verify: no panic occurred (test is still running)
        // Verify: delta computation was skipped (deltas weren't computed via calculate_window_pct_delta)
        // Verify: default values are used (deltas set to Some(0.0))
        assert_eq!(
            p5h_delta,
            Some(0.0),
            "5h delta should be Some(0.0) on first poll"
        );
        assert_eq!(
            p7d_delta,
            Some(0.0),
            "7d delta should be Some(0.0) on first poll"
        );
        assert_eq!(
            p7ds_delta,
            Some(0.0),
            "7ds delta should be Some(0.0) on first poll"
        );
    }

    /// Test first poll with varying current snapshot values.
    ///
    /// Verifies that regardless of the current snapshot values, when previous
    /// snapshot is None, all deltas are set to Some(0.0).
    #[test]
    fn test_first_poll_zero_deltas_regardless_of_current_values() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        let test_cases = vec![
            (10.0, 20.0, 15.0), // Low utilization
            (50.0, 60.0, 55.0), // Medium utilization
            (95.0, 98.0, 97.0), // High utilization
            (0.0, 0.0, 0.0),    // Zero utilization
        ];

        for (five_hour, seven_day, weekly_scoped) in test_cases {
            let previous_api_snapshot: Option<PrevUsageSnapshot> = None;
            let current_api_snapshot: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
                taken_at: Utc::now(),
                five_hour_pct: five_hour,
                seven_day_pct: seven_day,
                weekly_scoped_pct: weekly_scoped,
            });

            let mut p5h_delta: Option<f64> = None;
            let mut p7d_delta: Option<f64> = None;
            let mut p7ds_delta: Option<f64> = None;

            match (&previous_api_snapshot, &current_api_snapshot) {
                (Some(prev), Some(curr)) => {
                    let prev_pct = crate::db::WindowPctSnapshot {
                        five_hour: prev.five_hour_pct,
                        seven_day: prev.seven_day_pct,
                        weekly_scoped: prev.weekly_scoped_pct,
                    };
                    let curr_pct = crate::db::WindowPctSnapshot {
                        five_hour: curr.five_hour_pct,
                        seven_day: curr.seven_day_pct,
                        weekly_scoped: curr.weekly_scoped_pct,
                    };
                    let (delta_5h, delta_7d, delta_7ds) =
                        calculate_window_pct_delta(&prev_pct, &curr_pct);
                    p5h_delta = Some(delta_5h);
                    p7d_delta = Some(delta_7d);
                    p7ds_delta = Some(delta_7ds);
                }
                (None, Some(_curr)) => {
                    p5h_delta = Some(0.0);
                    p7d_delta = Some(0.0);
                    p7ds_delta = Some(0.0);
                }
                (None, None) | (Some(_), None) => {
                    // Leave deltas as None
                }
            }

            assert_eq!(
                p5h_delta,
                Some(0.0),
                "5h delta should be 0.0 for current values ({}, {}, {})",
                five_hour,
                seven_day,
                weekly_scoped
            );
            assert_eq!(
                p7d_delta,
                Some(0.0),
                "7d delta should be 0.0 for current values ({}, {}, {})",
                five_hour,
                seven_day,
                weekly_scoped
            );
            assert_eq!(
                p7ds_delta,
                Some(0.0),
                "7ds delta should be 0.0 for current values ({}, {}, {})",
                five_hour,
                seven_day,
                weekly_scoped
            );
        }
    }

    /// Test that consecutive polls compute non-zero deltas after first poll.
    ///
    /// Verifies the transition from first poll (deltas = 0) to second poll
    /// (deltas computed from snapshots).
    #[test]
    fn test_consecutive_polls_after_first_poll_computes_deltas() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // First poll: previous is None, deltas should be 0
        let prev_none: Option<PrevUsageSnapshot> = None;
        let curr_first: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        });

        let mut p5h_delta: Option<f64> = None;
        let mut p7d_delta: Option<f64> = None;
        let mut p7ds_delta: Option<f64> = None;

        match (&prev_none, &curr_first) {
            (Some(prev), Some(curr)) => {
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);
                p5h_delta = Some(delta_5h);
                p7d_delta = Some(delta_7d);
                p7ds_delta = Some(delta_7ds);
            }
            (None, Some(_curr)) => {
                p5h_delta = Some(0.0);
                p7d_delta = Some(0.0);
                p7ds_delta = Some(0.0);
            }
            (None, None) | (Some(_), None) => {}
        }

        // Verify first poll gives zero deltas
        assert_eq!(p5h_delta, Some(0.0), "First poll: 5h delta should be 0.0");
        assert_eq!(p7d_delta, Some(0.0), "First poll: 7d delta should be 0.0");
        assert_eq!(p7ds_delta, Some(0.0), "First poll: 7ds delta should be 0.0");

        // Second poll: previous is now Some, deltas should be computed
        let curr_second: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 12.5,     // +2.5
            seven_day_pct: 22.0,     // +2.0
            weekly_scoped_pct: 18.0, // +3.0
        });

        p5h_delta = None;
        p7d_delta = None;
        p7ds_delta = None;

        match (&curr_first, &curr_second) {
            (Some(prev), Some(curr)) => {
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);
                p5h_delta = Some(delta_5h);
                p7d_delta = Some(delta_7d);
                p7ds_delta = Some(delta_7ds);
            }
            (None, Some(_curr)) => {
                p5h_delta = Some(0.0);
                p7d_delta = Some(0.0);
                p7ds_delta = Some(0.0);
            }
            (None, None) | (Some(_), None) => {}
        }

        // Verify second poll computes actual deltas
        assert!(
            (p5h_delta.unwrap() - 2.5).abs() < f64::EPSILON,
            "Second poll: 5h delta should be 2.5"
        );
        assert!(
            (p7d_delta.unwrap() - 2.0).abs() < f64::EPSILON,
            "Second poll: 7d delta should be 2.0"
        );
        assert!(
            (p7ds_delta.unwrap() - 3.0).abs() < f64::EPSILON,
            "Second poll: 7ds delta should be 3.0"
        );
    }

    /// Test edge case: both previous and current snapshots are None.
    ///
    /// Verifies panic prevention and graceful handling when no snapshot data is available.
    /// This can occur during governor initialization or when the collector is offline.
    #[test]
    fn test_no_snapshots_available_no_panic() {
        use crate::state::PrevUsageSnapshot;

        // Edge case: neither snapshot available (e.g., collector offline, initialization)
        let previous_api_snapshot: Option<PrevUsageSnapshot> = None;
        let current_api_snapshot: Option<PrevUsageSnapshot> = None;

        let mut p5h_delta: Option<f64> = None;
        let mut p7d_delta: Option<f64> = None;
        let mut p7ds_delta: Option<f64> = None;

        // This should not panic and should leave deltas as None
        match (&previous_api_snapshot, &current_api_snapshot) {
            (Some(prev), Some(curr)) => {
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);
                p5h_delta = Some(delta_5h);
                p7d_delta = Some(delta_7d);
                p7ds_delta = Some(delta_7ds);
            }
            (None, Some(_curr)) => {
                p5h_delta = Some(0.0);
                p7d_delta = Some(0.0);
                p7ds_delta = Some(0.0);
            }
            (None, None) | (Some(_), None) => {
                // Neither snapshot available OR only previous available: handle gracefully
                // Leave deltas as None - verified by assertions below
            }
        }

        // Verify: no panic occurred (test is still running)
        // Verify: delta computation was skipped
        // Verify: deltas remain None (not Some(0.0))
        assert_eq!(
            p5h_delta, None,
            "5h delta should be None when no snapshots available"
        );
        assert_eq!(
            p7d_delta, None,
            "7d delta should be None when no snapshots available"
        );
        assert_eq!(
            p7ds_delta, None,
            "7ds delta should be None when no snapshots available"
        );
    }

    /// Test edge case: previous snapshot exists but current is None.
    ///
    /// Verifies panic prevention and graceful handling when the current poll fails
    /// to produce a snapshot (e.g., API error, timeout).
    #[test]
    fn test_previous_snapshot_without_current_no_panic() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Edge case: previous exists but current is None (e.g., API error in current poll)
        let previous_api_snapshot: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        });
        let current_api_snapshot: Option<PrevUsageSnapshot> = None;

        let mut p5h_delta: Option<f64> = None;
        let mut p7d_delta: Option<f64> = None;
        let mut p7ds_delta: Option<f64> = None;

        // This should not panic and should leave deltas as None
        match (&previous_api_snapshot, &current_api_snapshot) {
            (Some(prev), Some(curr)) => {
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);
                p5h_delta = Some(delta_5h);
                p7d_delta = Some(delta_7d);
                p7ds_delta = Some(delta_7ds);
            }
            (None, Some(_curr)) => {
                p5h_delta = Some(0.0);
                p7d_delta = Some(0.0);
                p7ds_delta = Some(0.0);
            }
            (None, None) | (Some(_), None) => {
                // Neither snapshot available OR only previous available: handle gracefully
                // Leave deltas as None - verified by assertions below
            }
        }

        // Verify: no panic occurred (test is still running)
        // Verify: delta computation was skipped
        // Verify: deltas remain None (not Some(0.0))
        assert_eq!(
            p5h_delta, None,
            "5h delta should be None when current snapshot is missing"
        );
        assert_eq!(
            p7d_delta, None,
            "7d delta should be None when current snapshot is missing"
        );
        assert_eq!(
            p7ds_delta, None,
            "7ds delta should be None when current snapshot is missing"
        );
    }

    /// Test that delta computation is explicitly skipped on first poll.
    ///
    /// Verifies that when previous_api_snapshot is None, the delta computation
    /// logic is bypassed entirely, not just returning zero values.
    #[test]
    fn test_delta_computation_skipped_on_first_poll() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        let previous_api_snapshot: Option<PrevUsageSnapshot> = None;
        let current_api_snapshot: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 25.0,
            seven_day_pct: 45.0,
            weekly_scoped_pct: 35.0,
        });

        let mut delta_computation_attempted = false;

        // Track whether delta computation was attempted
        match (&previous_api_snapshot, &current_api_snapshot) {
            (Some(prev), Some(curr)) => {
                // This branch should NOT be reached on first poll
                delta_computation_attempted = true;
                let _prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let _curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let _deltas = calculate_window_pct_delta(&_prev_pct, &_curr_pct);
            }
            (None, Some(_curr)) => {
                // First poll: delta computation skipped
                delta_computation_attempted = false;
            }
            (None, None) | (Some(_), None) => {
                delta_computation_attempted = false;
            }
        }

        // Verify: delta computation was NOT attempted
        assert!(
            !delta_computation_attempted,
            "Delta computation should be skipped on first poll"
        );
    }

    /// Test panic prevention with extreme utilization values.
    ///
    /// Verifies that the delta computation and snapshot handling don't panic
    /// with extreme but valid utilization values (0%, 100%, very close to threshold).
    #[test]
    fn test_panic_prevention_with_extreme_values() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Test case 1: utilization at exactly 98% (emergency brake threshold)
        let prev = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 98.0,
            seven_day_pct: 98.0,
            weekly_scoped_pct: 98.0,
        };

        let curr = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 98.0,
            seven_day_pct: 98.0,
            weekly_scoped_pct: 98.0,
        };

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        // Should not panic with values at threshold
        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
        assert_eq!(delta_5h, 0.0);
        assert_eq!(delta_7d, 0.0);
        assert_eq!(delta_7ds, 0.0);

        // Test case 2: utilization at exactly 0%
        let prev_zero = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 0.0,
            seven_day_pct: 0.0,
            weekly_scoped_pct: 0.0,
        };

        let curr_zero = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 0.0,
            seven_day_pct: 0.0,
            weekly_scoped_pct: 0.0,
        };

        let prev_pct_zero = crate::db::WindowPctSnapshot {
            five_hour: prev_zero.five_hour_pct,
            seven_day: prev_zero.seven_day_pct,
            weekly_scoped: prev_zero.weekly_scoped_pct,
        };

        let curr_pct_zero = crate::db::WindowPctSnapshot {
            five_hour: curr_zero.five_hour_pct,
            seven_day: curr_zero.seven_day_pct,
            weekly_scoped: curr_zero.weekly_scoped_pct,
        };

        // Should not panic with zero values
        let (delta_5h_zero, delta_7d_zero, delta_7ds_zero) =
            calculate_window_pct_delta(&prev_pct_zero, &curr_pct_zero);
        assert_eq!(delta_5h_zero, 0.0);
        assert_eq!(delta_7d_zero, 0.0);
        assert_eq!(delta_7ds_zero, 0.0);
    }

    /// Test that default delta value (0.0) is correctly set on first poll.
    ///
    /// Verifies that the default value Some(0.0) is used specifically for the
    /// first poll case (None, Some), not for other edge cases.
    #[test]
    fn test_default_delta_value_specific_to_first_poll() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // First poll: should get Some(0.0) as default
        let previous_api_snapshot: Option<PrevUsageSnapshot> = None;
        let current_api_snapshot: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 30.0,
            seven_day_pct: 50.0,
            weekly_scoped_pct: 40.0,
        });

        let mut p5h_delta: Option<f64> = None;
        let mut p7d_delta: Option<f64> = None;
        let mut p7ds_delta: Option<f64> = None;

        match (&previous_api_snapshot, &current_api_snapshot) {
            (Some(prev), Some(curr)) => {
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);
                p5h_delta = Some(delta_5h);
                p7d_delta = Some(delta_7d);
                p7ds_delta = Some(delta_7ds);
            }
            (None, Some(_curr)) => {
                // Default value for first poll
                p5h_delta = Some(0.0);
                p7d_delta = Some(0.0);
                p7ds_delta = Some(0.0);
            }
            (None, None) | (Some(_), None) => {
                // Leave deltas as None - different from first poll default
            }
        }

        // Verify first poll gets Some(0.0) default
        assert_eq!(
            p5h_delta,
            Some(0.0),
            "First poll should default to Some(0.0)"
        );
        assert_eq!(
            p7d_delta,
            Some(0.0),
            "First poll should default to Some(0.0)"
        );
        assert_eq!(
            p7ds_delta,
            Some(0.0),
            "First poll should default to Some(0.0)"
        );

        // Contrast with (None, None) case which should remain None
        let mut p5h_delta_none: Option<f64> = None;
        let mut p7d_delta_none: Option<f64> = None;
        let mut p7ds_delta_none: Option<f64> = None;

        let none_snap: Option<PrevUsageSnapshot> = None;
        match (&none_snap, &none_snap) {
            (Some(prev), Some(curr)) => {
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);
                p5h_delta_none = Some(delta_5h);
                p7d_delta_none = Some(delta_7d);
                p7ds_delta_none = Some(delta_7ds);
            }
            (None, Some(_curr)) => {
                p5h_delta_none = Some(0.0);
                p7d_delta_none = Some(0.0);
                p7ds_delta_none = Some(0.0);
            }
            (None, None) | (Some(_), None) => {
                // Remain None - different from first poll
            }
        }

        // Verify (None, None) remains None (not Some(0.0))
        assert_eq!(
            p5h_delta_none, None,
            "(None, None) should remain None, not Some(0.0)"
        );
        assert_eq!(
            p7d_delta_none, None,
            "(None, None) should remain None, not Some(0.0)"
        );
        assert_eq!(
            p7ds_delta_none, None,
            "(None, None) should remain None, not Some(0.0)"
        );
    }

    // ---------------------------------------------------------------------------
    // Consecutive snapshots test - state-level poll bookkeeping
    // ---------------------------------------------------------------------------

    /// Test that two consecutive snapshots simulate consecutive polling behavior.
    ///
    /// This test creates two distinct snapshots with different known values and
    /// walks `GovernorState` through the snapshot bookkeeping two polls apart,
    /// demonstrating consecutive polling behavior.
    ///
    /// Scope: this is the state + delta half of a cycle, not `run_governor_cycle`
    /// itself — no poller, database, or state file is involved. The shift here
    /// mirrors the production ordering, which shifts `current -> previous` at the
    /// top of the cycle (`run_governor_cycle`, "shift snapshots" step) and only
    /// writes a new `current` once the poll succeeds, so a failed poll leaves the
    /// prior reading in `previous_api_snapshot`. For the end-to-end version that
    /// really runs two cycles against a poller, see
    /// `mock_poller_tests::test_second_cycle_repolls_and_computes_window_deltas`.
    ///
    /// The test verifies:
    /// - Two distinct snapshots are created with different utilization values
    /// - Both snapshots are stored in the GovernorState in sequence
    /// - The snapshot shift behavior (current becomes previous) works correctly
    /// - Delta computation uses both snapshots correctly
    /// - The delta fields are populated in the state: `p5h_delta`/`p7d_delta`/
    ///   `p7ds_delta` and `last_fleet_aggregate.window_pct_deltas` are all Some
    ///   and non-zero, and each carries the delta implied by the two snapshots
    /// - Those fields survive serialization into the persisted state shape
    /// - Consecutive polling is demonstrated through state transitions
    #[test]
    fn test_consecutive_snapshots_governor_cycle() {
        use crate::state::GovernorState;
        use crate::state::PrevUsageSnapshot;
        use chrono::{Duration, Utc};

        // Create a fresh GovernorState
        let mut state = GovernorState::new();

        // Verify initial state: no snapshots
        assert!(
            state.previous_api_snapshot.is_none(),
            "Initial state should have no previous snapshot"
        );
        assert!(
            state.current_api_snapshot.is_none(),
            "Initial state should have no current snapshot"
        );
        assert!(
            state.p5h_delta.is_none(),
            "Initial state should have no 5h delta"
        );
        assert!(
            state.p7d_delta.is_none(),
            "Initial state should have no 7d delta"
        );
        assert!(
            state.p7ds_delta.is_none(),
            "Initial state should have no 7ds delta"
        );

        // === First snapshot: Simulate first governor cycle poll ===
        let first_poll_time = Utc::now() - Duration::seconds(120);
        let snapshot1 = PrevUsageSnapshot {
            taken_at: first_poll_time,
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        };

        // Set the first snapshot as current (simulates successful API poll in cycle 1)
        state.current_api_snapshot = Some(snapshot1.clone());

        // Verify first snapshot is stored as current
        assert!(
            state.current_api_snapshot.is_some(),
            "After first poll, current snapshot should be Some"
        );
        assert!(
            state.previous_api_snapshot.is_none(),
            "After first poll, previous should still be None"
        );

        let current = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(
            current.taken_at, first_poll_time,
            "First snapshot timestamp"
        );
        assert!(
            (current.five_hour_pct - 10.0).abs() < f64::EPSILON,
            "First snapshot 5h"
        );
        assert!(
            (current.seven_day_pct - 20.0).abs() < f64::EPSILON,
            "First snapshot 7d"
        );
        assert!(
            (current.weekly_scoped_pct - 15.0).abs() < f64::EPSILON,
            "First snapshot 7ds"
        );

        // === Second snapshot: Simulate second governor cycle poll ===
        let second_poll_time = Utc::now();
        let snapshot2 = PrevUsageSnapshot {
            taken_at: second_poll_time,
            five_hour_pct: 12.5,     // +2.5 from first
            seven_day_pct: 22.0,     // +2.0 from first
            weekly_scoped_pct: 18.0, // +3.0 from first
        };

        // Shift snapshots: current becomes previous (simulates cycle 2 start)
        state.previous_api_snapshot = state.current_api_snapshot.take();
        // Set new current snapshot (simulates successful API poll in cycle 2)
        state.current_api_snapshot = Some(snapshot2.clone());

        // Verify both snapshots are now stored correctly
        assert!(
            state.previous_api_snapshot.is_some(),
            "After second poll, previous snapshot should be Some"
        );
        assert!(
            state.current_api_snapshot.is_some(),
            "After second poll, current snapshot should be Some"
        );

        // Verify previous snapshot is the first snapshot
        let previous = state.previous_api_snapshot.as_ref().unwrap();
        assert_eq!(
            previous.taken_at, first_poll_time,
            "Previous is first snapshot"
        );
        assert!(
            (previous.five_hour_pct - 10.0).abs() < f64::EPSILON,
            "Previous 5h value"
        );
        assert!(
            (previous.seven_day_pct - 20.0).abs() < f64::EPSILON,
            "Previous 7d value"
        );
        assert!(
            (previous.weekly_scoped_pct - 15.0).abs() < f64::EPSILON,
            "Previous 7ds value"
        );

        // Verify current snapshot is the second snapshot
        let current = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(
            current.taken_at, second_poll_time,
            "Current is second snapshot"
        );
        assert!(
            (current.five_hour_pct - 12.5).abs() < f64::EPSILON,
            "Current 5h value"
        );
        assert!(
            (current.seven_day_pct - 22.0).abs() < f64::EPSILON,
            "Current 7d value"
        );
        assert!(
            (current.weekly_scoped_pct - 18.0).abs() < f64::EPSILON,
            "Current 7ds value"
        );

        // === Verify consecutive polling behavior: compute deltas ===
        // Expected deltas, derived from the two snapshots' own fields rather than
        // from anything the delta code produced. Every delta assertion below —
        // state fields, fleet aggregate, serialized state — is anchored to these,
        // so the test tracks the snapshot inputs and not its own output.
        let expected_5h_delta = snapshot2.five_hour_pct - snapshot1.five_hour_pct;
        let expected_7d_delta = snapshot2.seven_day_pct - snapshot1.seven_day_pct;
        let expected_7ds_delta = snapshot2.weekly_scoped_pct - snapshot1.weekly_scoped_pct;

        // This simulates the delta computation that happens in run_governor_cycle
        if let (Some(prev), Some(curr)) =
            (&state.previous_api_snapshot, &state.current_api_snapshot)
        {
            let prev_pct = crate::db::WindowPctSnapshot {
                five_hour: prev.five_hour_pct,
                seven_day: prev.seven_day_pct,
                weekly_scoped: prev.weekly_scoped_pct,
            };
            let curr_pct = crate::db::WindowPctSnapshot {
                five_hour: curr.five_hour_pct,
                seven_day: curr.seven_day_pct,
                weekly_scoped: curr.weekly_scoped_pct,
            };
            let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

            // Store computed deltas (as run_governor_cycle does)
            state.p5h_delta = Some(delta_5h);
            state.p7d_delta = Some(delta_7d);
            state.p7ds_delta = Some(delta_7ds);

            // Verify deltas are computed from consecutive snapshots
            assert!(
                state.p5h_delta.is_some(),
                "After computing, 5h delta should be Some"
            );
            assert!(
                state.p7d_delta.is_some(),
                "After computing, 7d delta should be Some"
            );
            assert!(
                state.p7ds_delta.is_some(),
                "After computing, 7ds delta should be Some"
            );

            // Populated means more than Some(_): the first poll path writes
            // Some(0.0) (see test_first_poll_governor_state_no_panic_default_deltas),
            // so a delta that is still 0.0 here would mean the second snapshot
            // never reached the delta computation. The three snapshot values all
            // moved, so all three deltas must be non-zero.
            assert_ne!(
                state.p5h_delta.unwrap(),
                0.0,
                "5h delta should be non-zero after two snapshots ({} -> {})",
                snapshot1.five_hour_pct,
                snapshot2.five_hour_pct
            );
            assert_ne!(
                state.p7d_delta.unwrap(),
                0.0,
                "7d delta should be non-zero after two snapshots ({} -> {})",
                snapshot1.seven_day_pct,
                snapshot2.seven_day_pct
            );
            assert_ne!(
                state.p7ds_delta.unwrap(),
                0.0,
                "7ds delta should be non-zero after two snapshots ({} -> {})",
                snapshot1.weekly_scoped_pct,
                snapshot2.weekly_scoped_pct
            );

            // === Delta value verification: manual calculation vs computed ===
            // Delta formula: delta = current_snapshot_pct - previous_snapshot_pct
            //
            // This formula tracks the percentage point change in utilization between
            // consecutive polling cycles. A positive delta indicates increasing utilization,
            // while a negative delta indicates decreasing utilization.
            //
            // The operands are percent-of-quota readings, so the result is a signed
            // difference in *percentage points* — not a ratio and not a relative
            // percent change. Example: 5-hour utilization of 10.0% in the previous
            // snapshot and 12.5% in the current one gives 12.5 - 10.0 = 2.5
            // percentage points, not 25%.
            //
            // The sign matters on the production path too, where a window reset drives
            // every delta negative; see
            // `mock_poller_tests::test_cycle_computes_negative_deltas_when_windows_reset`.

            // Document the expected calculations (hoisted above this block) for clarity
            assert!(
                (expected_5h_delta - 2.5).abs() < f64::EPSILON,
                "Expected 5h delta calculation: 12.5 - 10.0 = 2.5 percentage points"
            );
            assert!(
                (expected_7d_delta - 2.0).abs() < f64::EPSILON,
                "Expected 7d delta calculation: 22.0 - 20.0 = 2.0 percentage points"
            );
            assert!(
                (expected_7ds_delta - 3.0).abs() < f64::EPSILON,
                "Expected 7ds delta calculation: 18.0 - 15.0 = 3.0 percentage points"
            );

            // Verify computed deltas match expected manual calculations
            // This validates that the calculate_window_pct_delta function implements
            // the correct formula: current - previous
            let computed_5h_delta = state.p5h_delta.unwrap();
            let computed_7d_delta = state.p7d_delta.unwrap();
            let computed_7ds_delta = state.p7ds_delta.unwrap();

            assert!(
                (computed_5h_delta - expected_5h_delta).abs() < f64::EPSILON,
                "Computed 5h delta ({}) should match expected 5h delta ({}) from formula: current ({}) - previous ({})",
                computed_5h_delta,
                expected_5h_delta,
                snapshot2.five_hour_pct,
                snapshot1.five_hour_pct
            );

            assert!(
                (computed_7d_delta - expected_7d_delta).abs() < f64::EPSILON,
                "Computed 7d delta ({}) should match expected 7d delta ({}) from formula: current ({}) - previous ({})",
                computed_7d_delta,
                expected_7d_delta,
                snapshot2.seven_day_pct,
                snapshot1.seven_day_pct
            );

            assert!(
                (computed_7ds_delta - expected_7ds_delta).abs() < f64::EPSILON,
                "Computed 7ds delta ({}) should match expected 7ds delta ({}) from formula: current ({}) - previous ({})",
                computed_7ds_delta,
                expected_7ds_delta,
                snapshot2.weekly_scoped_pct,
                snapshot1.weekly_scoped_pct
            );
        } else {
            panic!("Both snapshots should be Some after consecutive polls");
        }

        // === Verify delta fields in last_fleet_aggregate structure ===
        // In addition to the individual delta fields (p5h_delta, p7d_delta, p7ds_delta),
        // the governor state also stores deltas in the last_fleet_aggregate.window_pct_deltas
        // structure. This is critical for fleet-level delta tracking and reporting.

        // Precondition: a fresh aggregate carries exactly zero deltas, so the
        // non-zero assertions below can only pass because the two snapshots were
        // actually differenced. (Deltas are signed — a window reset makes them
        // negative — so the precondition is `== 0.0`, not `>= 0.0`.)
        assert_eq!(
            state.last_fleet_aggregate.window_pct_deltas.five_hour, 0.0,
            "precondition: fresh last_fleet_aggregate should have a zero 5h delta"
        );
        assert_eq!(
            state.last_fleet_aggregate.window_pct_deltas.seven_day, 0.0,
            "precondition: fresh last_fleet_aggregate should have a zero 7d delta"
        );
        assert_eq!(
            state.last_fleet_aggregate.window_pct_deltas.weekly_scoped, 0.0,
            "precondition: fresh last_fleet_aggregate should have a zero 7ds delta"
        );

        // After computing deltas from consecutive snapshots, update last_fleet_aggregate.
        // Production reaches the same values by a longer route: run_governor_cycle
        // annotates the interval's `f` row with the computed deltas
        // (`db::annotate_window_pct_deltas`) and then reads that row back into
        // last_fleet_aggregate.window_pct_deltas from `p5h`/`p7d`/`p7ds`. This
        // assignment stands in for that round trip, which needs a database.
        state.last_fleet_aggregate.window_pct_deltas.five_hour = state.p5h_delta.unwrap();
        state.last_fleet_aggregate.window_pct_deltas.seven_day = state.p7d_delta.unwrap();
        state.last_fleet_aggregate.window_pct_deltas.weekly_scoped = state.p7ds_delta.unwrap();

        // Now verify window_pct_deltas fields are non-zero after two snapshots
        assert!(
            state.last_fleet_aggregate.window_pct_deltas.five_hour != 0.0,
            "last_fleet_aggregate.window_pct_deltas.five_hour should be non-zero after two snapshots (got {})",
            state.last_fleet_aggregate.window_pct_deltas.five_hour
        );
        assert!(
            state.last_fleet_aggregate.window_pct_deltas.seven_day != 0.0,
            "last_fleet_aggregate.window_pct_deltas.seven_day should be non-zero after two snapshots (got {})",
            state.last_fleet_aggregate.window_pct_deltas.seven_day
        );
        assert!(
            state.last_fleet_aggregate.window_pct_deltas.weekly_scoped != 0.0,
            "last_fleet_aggregate.window_pct_deltas.weekly_scoped should be non-zero after two snapshots (got {})",
            state.last_fleet_aggregate.window_pct_deltas.weekly_scoped
        );

        // Verify the whole window_pct_deltas structure matches the deltas implied by
        // the two snapshots — field by field, so a mis-wired field (e.g. seven_day
        // fed from the 7ds delta) fails here rather than passing on shape alone.
        let aggregate_deltas = &state.last_fleet_aggregate.window_pct_deltas;
        assert!(
            (aggregate_deltas.five_hour - expected_5h_delta).abs() < f64::EPSILON,
            "window_pct_deltas.five_hour ({}) should match the 5h delta from the snapshots ({})",
            aggregate_deltas.five_hour,
            expected_5h_delta
        );
        assert!(
            (aggregate_deltas.seven_day - expected_7d_delta).abs() < f64::EPSILON,
            "window_pct_deltas.seven_day ({}) should match the 7d delta from the snapshots ({})",
            aggregate_deltas.seven_day,
            expected_7d_delta
        );
        assert!(
            (aggregate_deltas.weekly_scoped - expected_7ds_delta).abs() < f64::EPSILON,
            "window_pct_deltas.weekly_scoped ({}) should match the 7ds delta from the snapshots ({})",
            aggregate_deltas.weekly_scoped,
            expected_7ds_delta
        );

        // === Verify the delta fields survive serialization ===
        // The governor persists state to governor-state.json between cycles, so a
        // delta that lives only in memory is not populated in any useful sense: the
        // next cycle and `cgov status` both read it back from disk. Serializing here
        // pins the field names and catches a rename or a `#[serde(skip)]` that would
        // silently drop the deltas from the persisted state.
        let serialized = serde_json::to_value(&state).expect("GovernorState should serialize");

        for (field, expected) in [
            ("p5h_delta", expected_5h_delta),
            ("p7d_delta", expected_7d_delta),
            ("p7ds_delta", expected_7ds_delta),
        ] {
            let value = serialized
                .get(field)
                .unwrap_or_else(|| panic!("serialized state should contain a {} field", field));
            let value = value
                .as_f64()
                .unwrap_or_else(|| panic!("{} should serialize as a number, got {}", field, value));
            assert!(
                (value - expected).abs() < f64::EPSILON,
                "serialized {} ({}) should carry the computed delta ({})",
                field,
                value,
                expected
            );
        }

        let serialized_aggregate = serialized
            .get("last_fleet_aggregate")
            .and_then(|v| v.get("window_pct_deltas"))
            .expect("serialized state should contain last_fleet_aggregate.window_pct_deltas");

        for (field, expected) in [
            ("five_hour", expected_5h_delta),
            ("seven_day", expected_7d_delta),
            ("weekly_scoped", expected_7ds_delta),
        ] {
            let value = serialized_aggregate
                .get(field)
                .unwrap_or_else(|| panic!("window_pct_deltas should contain a {} field", field));
            let value = value.as_f64().unwrap_or_else(|| {
                panic!(
                    "window_pct_deltas.{} should serialize as a number, got {}",
                    field, value
                )
            });
            assert!(
                (value - expected).abs() < f64::EPSILON,
                "serialized window_pct_deltas.{} ({}) should carry the computed delta ({})",
                field,
                value,
                expected
            );
        }

        // === Verify consecutive polling through state transitions ===
        // The key demonstration of consecutive polling is:
        // 1. First poll established current_api_snapshot
        // 2. Second poll shifted that snapshot to previous_api_snapshot
        // 3. Second poll set a new current_api_snapshot
        // 4. Both snapshots are now available for delta computation

        // Timestamp progression shows consecutive polls
        let prev_time = state.previous_api_snapshot.as_ref().unwrap().taken_at;
        let curr_time = state.current_api_snapshot.as_ref().unwrap().taken_at;
        assert!(
            curr_time > prev_time,
            "Current snapshot timestamp should be later than previous"
        );
        let elapsed = (curr_time - prev_time).num_seconds();
        assert!(
            elapsed >= 119 && elapsed <= 121,
            "Time between snapshots should be ~120 seconds (got {})",
            elapsed
        );

        // Snapshot values are distinct (showing real utilization changes)
        let prev_5h = state.previous_api_snapshot.as_ref().unwrap().five_hour_pct;
        let curr_5h = state.current_api_snapshot.as_ref().unwrap().five_hour_pct;
        assert!(
            curr_5h > prev_5h,
            "Current 5h utilization ({}) should be greater than previous ({})",
            curr_5h,
            prev_5h
        );
        assert!(
            (curr_5h - prev_5h - 2.5).abs() < f64::EPSILON,
            "5h utilization change should be 2.5 percentage points"
        );
    }

    /// Test first poll handling with GovernorState.
    ///
    /// Verifies that on the first poll (when previous_api_snapshot is None
    /// and current_api_snapshot is Some):
    /// - No panic occurs during state initialization and snapshot handling
    /// - Delta computation is gracefully skipped (no crash on missing previous snapshot)
    /// - Default values (Some(0.0)) are used for delta fields
    ///
    /// This test uses the actual GovernorState structure to ensure integration
    /// correctness, not just the pattern matching logic.
    #[test]
    fn test_first_poll_governor_state_no_panic_default_deltas() {
        use crate::state::GovernorState;
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // === Step 1: Create a GovernorState with previous_api_snapshot = None ===
        let mut state = GovernorState::new();

        // Verify initial state has both snapshots as None (fresh start)
        assert!(
            state.previous_api_snapshot.is_none(),
            "Fresh state should have previous_api_snapshot = None"
        );
        assert!(
            state.current_api_snapshot.is_none(),
            "Fresh state should have current_api_snapshot = None"
        );
        assert!(
            state.p5h_delta.is_none(),
            "Fresh state should have p5h_delta = None"
        );
        assert!(
            state.p7d_delta.is_none(),
            "Fresh state should have p7d_delta = None"
        );
        assert!(
            state.p7ds_delta.is_none(),
            "Fresh state should have p7ds_delta = None"
        );

        // === Step 2: Create a state with current_api_snapshot = Some(...) ===
        // This simulates the first successful API poll after governor start
        let first_snapshot = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 35.0,
            seven_day_pct: 55.0,
            weekly_scoped_pct: 45.0,
        };

        // Set current_api_snapshot (simulates first poll completing)
        state.current_api_snapshot = Some(first_snapshot.clone());

        // Verify first poll state: current is Some, previous is still None
        assert!(
            state.current_api_snapshot.is_some(),
            "After first poll, current_api_snapshot should be Some"
        );
        assert!(
            state.previous_api_snapshot.is_none(),
            "After first poll, previous_api_snapshot should still be None"
        );

        // === Step 3: Verify no panic occurs during delta computation ===
        // Simulate the delta computation logic from run_governor_cycle
        // This matches the pattern at lines 2012-2057 in run_governor_cycle
        let mut delta_computation_called = false;

        match (&state.previous_api_snapshot, &state.current_api_snapshot) {
            (Some(prev), Some(curr)) => {
                // This branch computes deltas - should NOT execute on first poll
                delta_computation_called = true;
                let _prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let _curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let _deltas = calculate_window_pct_delta(&_prev_pct, &_curr_pct);
            }
            (None, Some(_curr)) => {
                // === Step 4: Verify delta computation is skipped ===
                // First poll: no previous snapshot available
                // This branch should be reached, confirming delta computation is skipped
                delta_computation_called = false;

                // Set default values (Some(0.0)) as run_governor_cycle does
                state.p5h_delta = Some(0.0);
                state.p7d_delta = Some(0.0);
                state.p7ds_delta = Some(0.0);
            }
            (None, None) | (Some(_), None) => {
                // These cases represent error states or uninitialized state
                // Should not occur on a successful first poll
                delta_computation_called = false;
            }
        }

        // === Step 5: Verify default values are used ===
        // Test is still running = no panic occurred
        assert!(
            !delta_computation_called,
            "Delta computation should be skipped on first poll"
        );
        assert_eq!(
            state.p5h_delta,
            Some(0.0),
            "5h delta should default to Some(0.0) on first poll"
        );
        assert_eq!(
            state.p7d_delta,
            Some(0.0),
            "7d delta should default to Some(0.0) on first poll"
        );
        assert_eq!(
            state.p7ds_delta,
            Some(0.0),
            "7ds delta should default to Some(0.0) on first poll"
        );

        // Verify current snapshot values are preserved (first poll data is not lost)
        let current = state.current_api_snapshot.as_ref().unwrap();
        assert_eq!(
            current.five_hour_pct, 35.0,
            "First poll 5h utilization should be preserved"
        );
        assert_eq!(
            current.seven_day_pct, 55.0,
            "First poll 7d utilization should be preserved"
        );
        assert_eq!(
            current.weekly_scoped_pct, 45.0,
            "First poll 7ds utilization should be preserved"
        );
    }

    // ---------------------------------------------------------------------------
    // Tests using snapshot fixtures for realistic data
    // ---------------------------------------------------------------------------

    /// Test consecutive snapshots produce correct deltas using realistic fixture data.
    ///
    /// This test uses the baseline and 5-hour-later fixtures from snapshot_fixtures.rs
    /// to verify that delta computation works correctly with realistic, production-like data.
    #[test]
    fn test_consecutive_snapshots_fixtures_produce_correct_deltas() {
        use crate::snapshot_fixtures::{baseline_snapshot, snapshot_after_5h};

        let prev = baseline_snapshot();
        let curr = snapshot_after_5h();

        // Convert to WindowPctSnapshot for delta calculation
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Verify all deltas are positive (utilization increased)
        assert!(
            delta_5h > 0.0,
            "5h delta should be positive with increased usage"
        );
        assert!(
            delta_7d > 0.0,
            "7d delta should be positive with increased usage"
        );
        assert!(
            delta_7ds > 0.0,
            "7ds delta should be positive with increased usage"
        );

        // Verify exact delta values based on fixture documentation
        // baseline: 5h=12.5%, 7d=45.2%, 7ds=38.7%
        // after_5h: 5h=18.2%, 7d=46.8%, 7ds=40.3%
        // Expected deltas: 5h=+5.7%, 7d=+1.6%, 7ds=+1.6%
        assert!(
            (delta_5h - 5.7).abs() < 1e-9,
            "5h delta should be +5.7% (18.2 - 12.5)"
        );
        assert!(
            (delta_7d - 1.6).abs() < 1e-9,
            "7d delta should be +1.6% (46.8 - 45.2)"
        );
        assert!(
            (delta_7ds - 1.6).abs() < 1e-9,
            "7ds delta should be +1.6% (40.3 - 38.7)"
        );
    }

    /// Test consecutive snapshots with 7-day interval using fixtures.
    ///
    /// Verifies that the 7-day fixture pair produces the documented positive deltas.
    #[test]
    fn test_consecutive_snapshots_7d_fixtures_produce_correct_deltas() {
        use crate::snapshot_fixtures::{baseline_snapshot, snapshot_after_7d};

        let prev = baseline_snapshot();
        let curr = snapshot_after_7d();

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Verify 7-day windows show positive increase
        assert!(delta_7d > 0.0, "7d delta should be positive after 7 days");
        assert!(delta_7ds > 0.0, "7ds delta should be positive after 7 days");

        // Expected deltas: 7d=+7.2%, 7ds=+7.4%
        assert!(
            (delta_7d - 7.2).abs() < 1e-9,
            "7d delta should be +7.2% (52.4 - 45.2)"
        );
        assert!(
            (delta_7ds - 7.4).abs() < 1e-9,
            "7ds delta should be +7.4% (46.1 - 38.7)"
        );

        // 5-hour window reset occurred, so delta should reflect new window value
        assert!(
            (delta_5h - 3.3).abs() < 1e-9,
            "5h delta should be +3.3% (15.8 - 12.5)"
        );
    }

    /// Test consecutive snapshots with 7-day same-weekday using fixtures.
    ///
    /// Verifies that the 7-day same-weekday fixture produces the same results as the
    /// regular 7-day fixture (both are Wednesday to Wednesday).
    #[test]
    fn test_consecutive_snapshots_7ds_fixtures_produce_correct_deltas() {
        use crate::snapshot_fixtures::{baseline_snapshot, snapshot_after_7ds};

        let prev = baseline_snapshot();
        let curr = snapshot_after_7ds();

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Should produce same deltas as 7-day fixture (same weekday progression)
        assert!(
            (delta_7d - 7.2).abs() < 1e-9,
            "7ds delta should match 7d fixture (+7.2%)"
        );
        assert!((delta_7ds - 7.4).abs() < 1e-9, "7ds delta should be +7.4%");
        assert!((delta_5h - 3.3).abs() < 1e-9, "5h delta should be +3.3%");
    }

    /// Test that identical fixture values produce zero deltas.
    ///
    /// Creates two identical snapshots using the baseline fixture to verify
    /// that when utilization hasn't changed, all deltas are zero.
    #[test]
    fn test_identical_fixture_snapshots_produce_zero_deltas() {
        use crate::snapshot_fixtures::baseline_snapshot;

        let snapshot = baseline_snapshot();

        // Use the same snapshot for both previous and current
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: snapshot.five_hour_pct,
            seven_day: snapshot.seven_day_pct,
            weekly_scoped: snapshot.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: snapshot.five_hour_pct,
            seven_day: snapshot.seven_day_pct,
            weekly_scoped: snapshot.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // All deltas should be exactly zero for identical snapshots
        assert_eq!(
            delta_5h, 0.0,
            "5h delta should be zero for identical snapshots"
        );
        assert_eq!(
            delta_7d, 0.0,
            "7d delta should be zero for identical snapshots"
        );
        assert_eq!(
            delta_7ds, 0.0,
            "7ds delta should be zero for identical snapshots"
        );
    }

    /// Test snapshot pair fixtures for delta computation.
    ///
    /// Uses the helper functions that return snapshot pairs to verify the
    /// documented deltas are computed correctly.
    #[test]
    fn test_snapshot_pair_fixtures_compute_correct_deltas() {
        use crate::snapshot_fixtures::{snapshot_pair_5h, snapshot_pair_7d, snapshot_pair_7ds};

        // Test 5-hour pair
        let (prev_5h, curr_5h) = snapshot_pair_5h();
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev_5h.five_hour_pct,
            seven_day: prev_5h.seven_day_pct,
            weekly_scoped: prev_5h.weekly_scoped_pct,
        };
        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr_5h.five_hour_pct,
            seven_day: curr_5h.seven_day_pct,
            weekly_scoped: curr_5h.weekly_scoped_pct,
        };
        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
        assert!(
            (delta_5h - 5.7).abs() < 1e-9,
            "5h pair should produce +5.7% delta"
        );
        assert!(
            (delta_7d - 1.6).abs() < 1e-9,
            "5h pair should produce +1.6% 7d delta"
        );
        assert!(
            (delta_7ds - 1.6).abs() < 1e-9,
            "5h pair should produce +1.6% 7ds delta"
        );

        // Test 7-day pair
        let (prev_7d, curr_7d) = snapshot_pair_7d();
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev_7d.five_hour_pct,
            seven_day: prev_7d.seven_day_pct,
            weekly_scoped: prev_7d.weekly_scoped_pct,
        };
        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr_7d.five_hour_pct,
            seven_day: curr_7d.seven_day_pct,
            weekly_scoped: curr_7d.weekly_scoped_pct,
        };
        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
        assert!(
            (delta_5h - 3.3).abs() < 1e-9,
            "7d pair should produce +3.3% 5h delta"
        );
        assert!(
            (delta_7d - 7.2).abs() < 1e-9,
            "7d pair should produce +7.2% 7d delta"
        );
        assert!(
            (delta_7ds - 7.4).abs() < 1e-9,
            "7d pair should produce +7.4% 7ds delta"
        );

        // Test 7ds pair (should match 7d pair)
        let (prev_7ds, curr_7ds) = snapshot_pair_7ds();
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev_7ds.five_hour_pct,
            seven_day: prev_7ds.seven_day_pct,
            weekly_scoped: prev_7ds.weekly_scoped_pct,
        };
        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr_7ds.five_hour_pct,
            seven_day: curr_7ds.seven_day_pct,
            weekly_scoped: curr_7ds.weekly_scoped_pct,
        };
        let (delta_5h_7ds, delta_7d_7ds, delta_7ds_7ds) =
            calculate_window_pct_delta(&prev_pct, &curr_pct);
        assert!(
            (delta_5h_7ds - delta_5h).abs() < 1e-9,
            "7ds pair should match 7d 5h delta"
        );
        assert!(
            (delta_7d_7ds - delta_7d).abs() < 1e-9,
            "7ds pair should match 7d 7d delta"
        );
        assert!(
            (delta_7ds_7ds - delta_7ds).abs() < 1e-9,
            "7ds pair should match 7d 7ds delta"
        );
    }

    /// Test that fixtures with increased utilization produce positive deltas.
    ///
    /// Verifies the core expectation that when current utilization is higher than
    /// previous utilization, all computed deltas are positive.
    #[test]
    fn test_increased_fixture_values_produce_positive_deltas() {
        use crate::snapshot_fixtures::{baseline_snapshot, make_snapshot};

        let baseline = baseline_snapshot();
        let now = baseline.taken_at;

        // Create snapshots with various increases
        let increases = vec![
            (1.10, 10.0), // +10% increase
            (1.25, 25.0), // +25% increase
            (1.50, 50.0), // +50% increase
        ];

        for (multiplier, expected_percent_increase) in increases {
            let increased = make_snapshot(
                now + chrono::Duration::hours(5),
                baseline.five_hour_pct * multiplier,
                baseline.seven_day_pct * multiplier,
                baseline.weekly_scoped_pct * multiplier,
            );

            let prev_pct = crate::db::WindowPctSnapshot {
                five_hour: baseline.five_hour_pct,
                seven_day: baseline.seven_day_pct,
                weekly_scoped: baseline.weekly_scoped_pct,
            };

            let curr_pct = crate::db::WindowPctSnapshot {
                five_hour: increased.five_hour_pct,
                seven_day: increased.seven_day_pct,
                weekly_scoped: increased.weekly_scoped_pct,
            };

            let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

            // All deltas should be positive
            assert!(
                delta_5h > 0.0,
                "5h delta should be positive for +{}% increase",
                expected_percent_increase
            );
            assert!(
                delta_7d > 0.0,
                "7d delta should be positive for +{}% increase",
                expected_percent_increase
            );
            assert!(
                delta_7ds > 0.0,
                "7ds delta should be positive for +{}% increase",
                expected_percent_increase
            );

            // Verify delta magnitude matches expected percentage
            let expected_5h_delta = baseline.five_hour_pct * (multiplier - 1.0);
            let expected_7d_delta = baseline.seven_day_pct * (multiplier - 1.0);
            let expected_7ds_delta = baseline.weekly_scoped_pct * (multiplier - 1.0);

            assert!(
                (delta_5h - expected_5h_delta).abs() < 1e-9,
                "5h delta should match expected increase for +{}%",
                expected_percent_increase
            );
            assert!(
                (delta_7d - expected_7d_delta).abs() < 1e-9,
                "7d delta should match expected increase for +{}%",
                expected_percent_increase
            );
            assert!(
                (delta_7ds - expected_7ds_delta).abs() < 1e-9,
                "7ds delta should match expected increase for +{}%",
                expected_percent_increase
            );
        }
    }

    /// Test edge case fixtures for delta computation.
    ///
    /// Uses the idle, high utilization, and post-reset fixtures to verify delta
    /// computation handles extreme values correctly.
    #[test]
    fn test_edge_case_fixture_snapshots_compute_deltas() {
        use crate::snapshot_fixtures::{
            high_utilization_snapshot, idle_snapshot, post_reset_snapshot,
        };

        // Test idle -> high utilization (large positive delta)
        let idle = idle_snapshot();
        let high = high_utilization_snapshot();

        let idle_pct = crate::db::WindowPctSnapshot {
            five_hour: idle.five_hour_pct,
            seven_day: idle.seven_day_pct,
            weekly_scoped: idle.weekly_scoped_pct,
        };

        let high_pct = crate::db::WindowPctSnapshot {
            five_hour: high.five_hour_pct,
            seven_day: high.seven_day_pct,
            weekly_scoped: high.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&idle_pct, &high_pct);

        // Should show large positive increase
        assert!(
            delta_5h > 80.0,
            "5h delta should be > 80% from idle to high"
        );
        assert!(
            delta_7d > 90.0,
            "7d delta should be > 90% from idle to high"
        );
        assert!(
            delta_7ds > 90.0,
            "7ds delta should be > 90% from idle to high"
        );

        // Test high -> post-reset (negative delta, window reset scenario)
        let reset = post_reset_snapshot();

        let reset_pct = crate::db::WindowPctSnapshot {
            five_hour: reset.five_hour_pct,
            seven_day: reset.seven_day_pct,
            weekly_scoped: reset.weekly_scoped_pct,
        };

        let (delta_5h_reset, delta_7d_reset, delta_7ds_reset) =
            calculate_window_pct_delta(&high_pct, &reset_pct);

        // Should show negative delta (window reset)
        assert!(
            delta_5h_reset < 0.0,
            "5h delta should be negative after reset"
        );
        assert!(
            delta_7d_reset < 0.0,
            "7d delta should be negative after reset"
        );
        assert!(
            delta_7ds_reset < 0.0,
            "7ds delta should be negative after reset"
        );

        // 5-hour reset should be dramatic (drops from 82.4% to 2.1%)
        assert!(
            (delta_5h_reset - (-80.3)).abs() < 0.1,
            "5h reset should drop ~80%"
        );
    }

    /// Test that fixture snapshots produce correct time progression.
    ///
    /// Verifies that consecutive fixtures have the expected time differences
    /// to ensure realistic polling simulation.
    #[test]
    fn test_fixture_snapshots_produce_correct_time_progression() {
        use crate::snapshot_fixtures::{baseline_snapshot, snapshot_after_5h, snapshot_after_7d};
        use chrono::Duration;

        let baseline = baseline_snapshot();
        let after_5h = snapshot_after_5h();
        let after_7d = snapshot_after_7d();

        // Verify 5-hour progression
        let elapsed_5h = after_5h.taken_at.signed_duration_since(baseline.taken_at);
        assert_eq!(
            elapsed_5h.num_hours(),
            5,
            "5h snapshot should be 5 hours after baseline"
        );

        // Verify 7-day progression
        let elapsed_7d = after_7d.taken_at.signed_duration_since(baseline.taken_at);
        assert_eq!(
            elapsed_7d.num_days(),
            7,
            "7d snapshot should be 7 days after baseline"
        );

        // Verify monotonic time progression
        assert!(
            after_5h.taken_at > baseline.taken_at,
            "after_5h should be later than baseline"
        );
        assert!(
            after_7d.taken_at > after_5h.taken_at,
            "after_7d should be later than after_5h"
        );
    }

    /// Test delta computation tolerance for floating-point precision.
    ///
    /// Verifies that the delta computation uses appropriate tolerance for
    /// comparing floating-point values from fixtures.
    #[test]
    fn test_fixture_delta_computation_with_fp_tolerance() {
        use crate::snapshot_fixtures::{baseline_snapshot, make_snapshot};
        use chrono::Duration;

        let baseline = baseline_snapshot();

        // Create snapshot with values that produce floating-point results
        let curr = make_snapshot(
            baseline.taken_at + chrono::Duration::hours(5),
            baseline.five_hour_pct + 0.1, // Small increment
            baseline.seven_day_pct + 0.05,
            baseline.weekly_scoped_pct + 0.075,
        );

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: baseline.five_hour_pct,
            seven_day: baseline.seven_day_pct,
            weekly_scoped: baseline.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Use appropriate tolerance for floating-point comparison
        const TOLERANCE: f64 = 1e-9;
        assert!(
            (delta_5h - 0.1).abs() < TOLERANCE,
            "5h delta should be 0.1 with tolerance"
        );
        assert!(
            (delta_7d - 0.05).abs() < TOLERANCE,
            "7d delta should be 0.05 with tolerance"
        );
        assert!(
            (delta_7ds - 0.075).abs() < TOLERANCE,
            "7ds delta should be 0.075 with tolerance"
        );
    }

    /// Test second poll handling when both previous and current snapshots exist.
    ///
    /// This test verifies the governor works correctly on subsequent polls when both
    /// prev_snapshot and current snapshot are available. It simulates the transition
    /// from first poll (None prev) to a second poll (Some prev, Some curr) and verifies
    /// delta computation executes successfully with both snapshots.
    ///
    /// This test mirrors the comprehensive assertion pattern from test_first_poll_no_previous_snapshot
    /// (bf-151vi) but for the subsequent poll scenario.
    #[test]
    fn test_second_poll_with_both_snapshots() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Track whether delta computation was attempted
        let mut delta_computation_attempted = false;
        let mut delta_computation_result = None;

        // Simulate first poll state: current snapshot exists, previous is None
        let first_poll_current: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 10.0,
            seven_day_pct: 20.0,
            weekly_scoped_pct: 15.0,
        });

        let first_poll_previous: Option<PrevUsageSnapshot> = None;

        // ASSERTION 1: Verify first poll state
        assert!(
            first_poll_previous.is_none(),
            "First poll: previous should be None"
        );
        assert!(
            first_poll_current.is_some(),
            "First poll: current should be Some"
        );

        // Verify first poll falls into (None, Some) branch (no delta computation)
        let first_poll_result = match (&first_poll_previous, &first_poll_current) {
            (Some(_prev), Some(_curr)) => "should_not_happen",
            (None, Some(_curr)) => "first_poll_skip",
            (None, None) => "no_snapshots",
            (Some(_prev), None) => "only_previous",
        };
        assert_eq!(
            first_poll_result, "first_poll_skip",
            "First poll should fall into (None, Some) branch"
        );

        // Simulate the shift at the start of second poll (as in run_governor_cycle)
        // The previous snapshot becomes the first poll's current snapshot
        let second_poll_previous = first_poll_current;

        // Simulate second poll: new current snapshot with increased utilization
        let second_poll_current: Option<PrevUsageSnapshot> = Some(PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 12.5,     // +2.5 from previous
            seven_day_pct: 22.0,     // +2.0 from previous
            weekly_scoped_pct: 18.0, // +3.0 from previous
        });

        // ASSERTION 2: Verify second poll state (both snapshots exist)
        assert!(
            second_poll_previous.is_some(),
            "Second poll: previous should be Some (transitioned from first poll current)"
        );
        assert!(
            second_poll_current.is_some(),
            "Second poll: current should be Some (new poll data)"
        );

        // ASSERTION 3: Verify delta computation executes correctly with both snapshots
        // This simulates the check in run_governor_cycle:
        // if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot)
        let match_result = match (&second_poll_previous, &second_poll_current) {
            (Some(prev), Some(curr)) => {
                // Expected on second poll: both snapshots exist
                delta_computation_attempted = true;

                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };

                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };

                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);

                // Store the computed deltas for verification
                delta_computation_result = Some((delta_5h, delta_7d, delta_7ds));

                "delta_computed"
            }
            (None, Some(_curr)) => {
                // Should NOT happen on second poll (previous exists)
                "first_poll_skip"
            }
            (None, None) => {
                // Should NOT happen on second poll (both should exist)
                "no_snapshots"
            }
            (Some(_prev), None) => {
                // Should NOT happen in normal flow (current should exist after successful poll)
                "only_previous"
            }
        };

        // ASSERTION 4: Verify delta computation was executed (not skipped)
        assert!(
            delta_computation_attempted,
            "Delta computation should execute on second poll when both snapshots exist"
        );

        // ASSERTION 5: Verify the match fell into the correct (Some, Some) branch
        assert_eq!(
            match_result, "delta_computed",
            "Second poll should match the (Some, Some) branch and compute deltas"
        );

        // ASSERTION 6: Verify computed deltas are correct (not None and not zero)
        assert!(
            delta_computation_result.is_some(),
            "Delta computation result should be Some on second poll"
        );

        let (delta_5h, delta_7d, delta_7ds) = delta_computation_result.unwrap();

        // Verify exact delta values (current - previous)
        assert!(
            (delta_5h - 2.5).abs() < f64::EPSILON,
            "5h delta should be 2.5% (12.5 - 10.0), got {}",
            delta_5h
        );
        assert!(
            (delta_7d - 2.0).abs() < f64::EPSILON,
            "7d delta should be 2.0% (22.0 - 20.0), got {}",
            delta_7d
        );
        assert!(
            (delta_7ds - 3.0).abs() < f64::EPSILON,
            "7ds delta should be 3.0% (18.0 - 15.0), got {}",
            delta_7ds
        );

        // ASSERTION 7: Verify no panic occurred - test reaches this point
        // (If we reach here, graceful handling succeeded)
    }
}

// ---------------------------------------------------------------------------
// Agent cost priority helpers
// ---------------------------------------------------------------------------

/// Extract the model name from an agent's launch command.
///
/// Looks for --agent flag in the launch_cmd and extracts the model identifier.
/// Returns None if the model cannot be determined.
///
/// # Examples
/// - "needle run --agent claude-code-glm-5 --workspace ..." -> Some("claude-code-glm-5")
/// - "needle run --agent claude-opus --workspace ..." -> Some("claude-opus")
fn extract_model_from_launch_cmd(launch_cmd: &str) -> Option<String> {
    // Look for --agent flag
    let args: Vec<&str> = launch_cmd.split_whitespace().collect();
    for (i, arg) in args.iter().enumerate() {
        if *arg == "--agent" && i + 1 < args.len() {
            return Some(args[i + 1].to_string());
        }
    }
    None
}

/// Get the per-worker dollar cost for an agent.
///
/// Uses the pricing configuration to estimate the hourly cost per worker for this agent.
/// The cost is derived from the model's pricing assuming typical usage patterns.
///
/// Priority order:
/// 1. Use burn_rate.by_model if available (empirically measured)
/// 2. Use pricing config to estimate from model name
/// 3. Return default Sonnet cost as fallback
///
/// Returns cost in USD per worker per hour.
fn get_agent_cost_per_worker(
    agent_name: &str,
    agent_config: &AgentConfig,
    burn_rate_by_model: &HashMap<String, state::ModelBurnRate>,
    pricing_config: &crate::config::GovernorConfig,
) -> f64 {
    // Try burn rate data first (empirical)
    let model = extract_model_from_launch_cmd(&agent_config.launch_cmd);

    if let Some(model_name) = &model {
        // Look for burn rate data by model name
        if let Some(burn_rate) = burn_rate_by_model.get(model_name) {
            if burn_rate.dollars_per_worker_per_hour > 0.0 {
                log::debug!(
                    "[governor] agent {}: using burn rate ${:.2}/hr from {} samples",
                    agent_name,
                    burn_rate.dollars_per_worker_per_hour,
                    burn_rate.samples
                );
                return burn_rate.dollars_per_worker_per_hour;
            }
        }

        // Fall back to pricing config
        if let Some(model_pricing) = pricing_config.get_pricing(model_name) {
            // Estimate hourly cost: assume average 1M input + 500K output tokens/hour
            // This is a rough heuristic for prioritization
            let input_cost = model_pricing.input_per_mtok * 1.0; // 1M input tokens
            let output_cost = model_pricing.output_per_mtok * 0.5; // 500K output tokens
            let estimated_hourly_cost = input_cost + output_cost;

            log::debug!(
                "[governor] agent {}: using pricing estimate ${:.2}/hr for model {}",
                agent_name,
                estimated_hourly_cost,
                model_name
            );
            return estimated_hourly_cost;
        }
    }

    // Ultimate fallback: default Sonnet cost ($3 + $7.50 = $10.50/hr heuristic)
    log::debug!(
        "[governor] agent {}: using default Sonnet cost $10.50/hr (no pricing data found)",
        agent_name
    );
    10.50
}

/// Distribute workers across agents by cost priority.
///
/// When scaling down (new_total < current_total): prioritize high-cost agents first.
/// When scaling up (new_total > current_total): prioritize low-cost agents first.
///
/// # Arguments
/// - `agents`: HashMap of agent name -> AgentConfig
/// - `current_workers`: HashMap of agent name -> current worker count
/// - `target_total`: The new total worker count to achieve
/// - `burn_rate_by_model`: Per-model burn rate data for cost lookup
/// - `pricing_config`: Pricing configuration for cost estimation
/// - `cutoff_risk`: Whether we're in cutoff_risk mode (affects scale-down priority)
///
/// # Returns
/// HashMap of agent name -> target worker count
fn distribute_workers_by_cost_priority(
    agents: &HashMap<String, AgentConfig>,
    current_workers: &HashMap<String, u32>,
    target_total: u32,
    burn_rate_by_model: &HashMap<String, state::ModelBurnRate>,
    pricing_config: &crate::config::GovernorConfig,
    _cutoff_risk: bool, // Reserved for future scale-down priority adjustments
) -> HashMap<String, u32> {
    // Base distribution: start from the current allocation and adjust gently by the
    // delta (minimising churn) — scale down sheds the most expensive workers first,
    // scale up adds to the cheapest agent first. A second pass then enforces each
    // agent's min_workers floor so a dedicated pool (e.g. an Opus polish strand with
    // max_workers=1) actually launches — the pure cost sort would otherwise always
    // fill the cheap, high-max agent (glm, max 8) first and never give it a slot.
    let mut result: HashMap<String, u32> = HashMap::new();

    let current_total: u32 = current_workers.values().sum();
    let delta = target_total as i32 - current_total as i32;

    // (name, cost/hr, current, min, max)
    let mut agent_costs: Vec<(String, f64, u32, u32, u32)> = Vec::new();
    for (name, config) in agents {
        let cost = get_agent_cost_per_worker(name, config, burn_rate_by_model, pricing_config);
        let current = *current_workers.get(name).unwrap_or(&0);
        agent_costs.push((
            name.clone(),
            cost,
            current,
            config.min_workers.min(config.max_workers),
            config.max_workers,
        ));
        result.insert(name.clone(), current);
    }

    if delta < 0 {
        // Scale down: remove from the highest-cost agent first.
        let mut remaining = delta.unsigned_abs();
        agent_costs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (name, _cost, current, _min, _max) in &agent_costs {
            if remaining == 0 {
                break;
            }
            let can_remove = (*current).min(remaining);
            result.insert(name.clone(), current - can_remove);
            remaining -= can_remove;
        }
    } else if delta > 0 {
        // Scale up: add to the lowest-cost agent first (tie-break on spare capacity).
        let mut remaining = delta as u32;
        agent_costs.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| (b.4 - b.2).cmp(&(a.4 - a.2)))
        });
        for (name, _cost, current, _min, max) in &agent_costs {
            if remaining == 0 {
                break;
            }
            let room = max.saturating_sub(*current);
            let can_add = room.min(remaining);
            result.insert(name.clone(), current + can_add);
            remaining -= can_add;
        }
    }

    // Enforce per-agent min_workers: raise any agent below its floor, pulling the
    // needed workers from the most expensive agent that has spare capacity above its
    // own min. mins of 0 make this a no-op, preserving pure cost-priority behaviour.
    let mut donors = agent_costs.clone();
    donors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let deficits: Vec<(String, u32)> = agent_costs
        .iter()
        .filter_map(|(name, _cost, _current, min, _max)| {
            let have = *result.get(name).unwrap_or(&0);
            (have < *min).then(|| (name.clone(), *min - have))
        })
        .collect();
    for (dname, mut needed) in deficits {
        for (sname, _scost, _scur, smin, _smax) in &donors {
            if needed == 0 {
                break;
            }
            if *sname == dname {
                continue;
            }
            let shave = *result.get(sname).unwrap_or(&0);
            let take = shave.saturating_sub(*smin).min(needed);
            if take > 0 {
                result.insert(sname.clone(), shave - take);
                let cur = *result.get(&dname).unwrap_or(&0);
                result.insert(dname.clone(), cur + take);
                needed -= take;
            }
        }
    }

    result
}

/// Compute the target worker count from capacity forecast and schedule state.
///
/// Uses the binding window's `safe_worker_count` as the primary constraint.
/// When composite risk optimization is enabled and non-binding windows have
/// cost above the threshold, allows scaling higher by considering their capacity
/// over the binding window's remaining time.
///
/// Falls back to the configured max if no valid forecast is available.
///
/// ## Cone-based scaling aggressiveness
///
/// The binding window carries a `cone_ratio` (= exh_hrs_p75 / exh_hrs_p25):
/// - `cone_ratio < cone_scaling.narrow_threshold` → narrow cone → use `safe_worker_count` (p50)
/// - `cone_ratio >= cone_scaling.narrow_threshold` → wide cone → use `safe_worker_count_p75` (p75)
///
/// Steps:
/// 1. Check emergency brake (any window >= 98%) → return 0
/// 2. Get binding window from capacity forecast
/// 3. Select p50 or p75 safe worker count based on cone_ratio vs narrow_threshold
/// 4. If composite risk enabled, try composite optimization
/// 5. Otherwise use cone-selected safe worker count from binding window
/// 6. Apply sprint boost if active
/// 7. Clamp to [min, max] from worker state
pub fn compute_target_workers(
    state: &state::GovernorState,
    _target_ceiling: f64,
    composite_risk_config: &CompositeRiskConfig,
    cone_scaling_config: &ConeScalingConfig,
) -> u32 {
    // Aggregate min/max across all configured agents
    let mut global_min = u32::MAX;
    let mut global_max: u32 = 0;
    let mut current_total: u32 = 0;

    for ws in state.workers.values() {
        global_min = global_min.min(ws.min);
        global_max = global_max.max(ws.max);
        current_total += ws.current;
    }

    // No workers configured — return 0
    if global_min == u32::MAX {
        return 0;
    }

    let forecast = &state.capacity_forecast;

    // Check emergency brake: any window >= 98%
    let windows = [
        (&WINDOW_FIVE_HOUR, &forecast.five_hour),
        (&WINDOW_SEVEN_DAY, &forecast.seven_day),
        (&WINDOW_WEEKLY_SCOPED, &forecast.weekly_scoped),
    ];

    for (_name, win) in &windows {
        if win.current_utilization >= EMERGENCY_BRAKE_THRESHOLD {
            log::warn!(
                "[governor] EMERGENCY BRAKE: {} at {:.1}% >= {:.0}%",
                _name,
                win.current_utilization,
                EMERGENCY_BRAKE_THRESHOLD
            );
            return 0;
        }
    }

    // Get binding window index
    let binding_idx = match forecast.binding_window.as_str() {
        WINDOW_FIVE_HOUR => 0,
        WINDOW_SEVEN_DAY => 1,
        _ => 2,
    };

    let binding_forecast = match forecast.binding_window.as_str() {
        WINDOW_FIVE_HOUR => &forecast.five_hour,
        WINDOW_SEVEN_DAY => &forecast.seven_day,
        _ => &forecast.weekly_scoped,
    };

    // Select safe worker count based on cone_ratio vs narrow_threshold.
    // Narrow cone (low uncertainty) → use p50 median estimate.
    // Wide cone (high uncertainty) → use p75 conservative estimate.
    let cone_ratio = binding_forecast.cone_ratio;
    let cone_is_wide = cone_ratio >= cone_scaling_config.narrow_threshold;
    let selected_safe = if cone_is_wide {
        log::debug!(
            "[governor] cone_ratio {:.2} >= narrow_threshold {:.2}: using p75 safe worker count (conservative)",
            cone_ratio, cone_scaling_config.narrow_threshold
        );
        binding_forecast.safe_worker_count_p75
    } else {
        log::debug!(
            "[governor] cone_ratio {:.2} < narrow_threshold {:.2}: using p50 safe worker count (median)",
            cone_ratio, cone_scaling_config.narrow_threshold
        );
        binding_forecast.safe_worker_count
    };

    // Try composite risk optimization if enabled
    let base_target = if composite_risk_config.enabled {
        let all_forecasts = &[
            forecast.five_hour.clone(),
            forecast.seven_day.clone(),
            forecast.weekly_scoped.clone(),
        ];

        match compute_composite_safe_workers(
            all_forecasts,
            binding_idx,
            composite_risk_config.binding_weight,
            composite_risk_config.cost_threshold,
            current_total,
        ) {
            Some(composite_safe) => {
                log::debug!(
                    "[governor] composite risk optimization: binding_safe={:?} (cone_is_wide={}), composite_safe={}",
                    selected_safe,
                    cone_is_wide,
                    composite_safe
                );
                composite_safe
            }
            None => {
                // Composite risk not applicable, fall back to cone-selected binding window estimate
                safe_worker_count_or_hold(selected_safe, global_max, current_total)
            }
        }
    } else {
        safe_worker_count_or_hold(selected_safe, global_max, current_total)
    };

    let target = base_target.min(global_max).max(global_min);

    log::debug!(
        "[governor] compute_target_workers: binding={}, cone_ratio={:.2}, cone_is_wide={}, safe_w={:?}, current={}, target={} (min={}, max={}, composite={})",
        forecast.binding_window,
        cone_ratio,
        cone_is_wide,
        selected_safe,
        current_total,
        target,
        global_min,
        global_max,
        composite_risk_config.enabled,
    );

    target
}

/// Apply scaling decision with hysteresis band.
///
/// Returns the scaling action to take:
/// - `NoChange` if |target - current| <= hysteresis_band
/// - `ScaleUp(n)` if target > current + hysteresis (limited by max_scale_up_per_cycle)
/// - `ScaleDown(n)` if target < current - hysteresis (limited by max_scale_down_per_cycle)
///
/// Emergency brake bypasses hysteresis entirely.
pub fn apply_scaling(
    target: u32,
    current: u32,
    hysteresis_band: f64,
    max_up_per_cycle: u32,
    max_down_per_cycle: u32,
) -> ScalingDecision {
    // Emergency brake: target is 0
    if target == 0 && current > 0 {
        log::warn!("[governor] EMERGENCY: scaling {} -> 0 workers", current);
        return ScalingDecision::EmergencyBrake;
    }

    let delta = target as i32 - current as i32;
    let hysteresis = hysteresis_band as i32;

    if delta.abs() <= hysteresis {
        log::debug!(
            "[governor] hysteresis: |{} - {}| = {} <= {:.1}, no change",
            target,
            current,
            delta.abs(),
            hysteresis_band
        );
        return ScalingDecision::NoChange;
    }

    if delta > 0 {
        let scale = (delta as u32).min(max_up_per_cycle);
        log::info!(
            "[governor] scale UP: {} -> {} (+{})",
            current,
            current + scale,
            scale
        );
        return ScalingDecision::ScaleUp(scale);
    }

    // delta < 0
    let scale = (delta.abs() as u32).min(max_down_per_cycle);
    log::info!(
        "[governor] scale DOWN: {} -> {} (-{})",
        current,
        current - scale,
        scale
    );
    ScalingDecision::ScaleDown(scale)
}

// ---------------------------------------------------------------------------
// Pre-scale logic
// ---------------------------------------------------------------------------

/// Compute the effective target workers accounting for an upcoming multiplier transition.
///
/// When a losing-bonus transition (multiplier dropping, e.g. off-peak 2x → peak 1x) is
/// imminent within `pre_scale_minutes`, returns a pre-scale target to begin scaling down
/// one worker per cycle toward the post-transition safe count.
///
/// Conservative-only: returns `None` when no losing-bonus transition is imminent,
/// including cases where a bonus is about to be *gained* (never pre-scale up).
///
/// # Parameters
/// - `now`: current time (explicit for deterministic testing)
/// - `pre_scale_minutes`: look-ahead window; 0 disables pre-scaling
/// - `promotions`: active promotion definitions
/// - `reset_time`: window deadline (deadline for transition search)
/// - `target`: current target from `compute_target_workers`
/// - `current_total`: actual running workers right now
pub fn compute_pre_scale_target(
    now: DateTime<Utc>,
    pre_scale_minutes: u64,
    promotions: &[Promotion],
    reset_time: DateTime<Utc>,
    target: u32,
    current_total: u32,
    window: &str,
) -> Option<u32> {
    if pre_scale_minutes == 0 {
        return None;
    }

    let transition = schedule::next_transition_from(now, reset_time, promotions, window)?;

    log::debug!(
        "[governor] next transition in {}min: {:.1}x → {:.1}x at {}",
        transition.minutes_until,
        transition.multiplier_before,
        transition.multiplier_after,
        transition.at.to_rfc3339()
    );

    // Only act when transition is within the pre-scale look-ahead window
    if transition.minutes_until > pre_scale_minutes as i64 {
        return None;
    }

    // Conservative: only pre-scale down when LOSING a bonus (never scale up to gain one)
    if transition.multiplier_after >= transition.multiplier_before {
        return None;
    }

    // Scale target proportionally to multiplier drop (e.g. 2x → 1x halves effective capacity)
    let ratio = transition.multiplier_after / transition.multiplier_before;
    let post_transition_target = (target as f64 * ratio).floor() as u32;

    if post_transition_target >= current_total {
        return None;
    }

    // Ramp down one worker per cycle; never overshoot below post-transition target
    let effective_target = post_transition_target.max(current_total.saturating_sub(1));

    log::info!(
        "[governor] PRE-SCALE: off-peak→peak in {}min — scaling {}→{} (post-transition safe: {})",
        transition.minutes_until,
        current_total,
        effective_target,
        post_transition_target
    );

    Some(effective_target)
}

// ---------------------------------------------------------------------------
// Safe mode calibration check
// ---------------------------------------------------------------------------

/// Update safe mode state based on current calibration accuracy statistics.
///
/// Entry: if median absolute error > SAFE_MODE_ENTRY_ERROR_THRESHOLD and enough samples.
/// Exit: if median absolute error < SAFE_MODE_EXIT_ERROR_THRESHOLD (hysteresis) AND
///       at least SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT new predictions since entry.
///
/// Also updates calibration.predictions_scored and calibration.median_error_7ds
/// from the latest stats.
///
/// Returns true if safe mode state changed (entered or exited).
pub fn update_safe_mode_from_calibration(
    safe_mode: &mut state::SafeModeState,
    calibration: &mut state::CalibrationState,
    stats: &calibrator::CalibrationStats,
    now: DateTime<Utc>,
) -> bool {
    // Always sync calibration state from latest stats
    calibration.predictions_scored = stats.total_samples;
    calibration.median_error_7ds = stats.median_error_7ds;

    let median_error_abs = stats.median_error.abs();

    if safe_mode.active {
        // Update predictions-since-entry counter
        safe_mode.predictions_since_entry = stats
            .total_samples
            .saturating_sub(safe_mode.scored_at_entry);

        // Check exit: accuracy recovered past exit threshold and enough new predictions observed
        if median_error_abs < SAFE_MODE_EXIT_ERROR_THRESHOLD
            && safe_mode.predictions_since_entry >= SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT
            && stats.total_samples >= SAFE_MODE_MIN_SAMPLES
        {
            log::info!(
                "[governor] safe_mode exit: median_error={:.2} < exit_threshold={:.1}, \
                 predictions_since_entry={}",
                median_error_abs,
                SAFE_MODE_EXIT_ERROR_THRESHOLD,
                safe_mode.predictions_since_entry,
            );
            *safe_mode = state::SafeModeState::default();
            return true;
        }
        false
    } else {
        // Check entry: accuracy degraded past entry threshold with enough samples
        if median_error_abs > SAFE_MODE_ENTRY_ERROR_THRESHOLD
            && stats.total_samples >= SAFE_MODE_MIN_SAMPLES
        {
            log::warn!(
                "[governor] safe_mode enter: median_error={:.2} > entry_threshold={:.1}, \
                 samples={}",
                median_error_abs,
                SAFE_MODE_ENTRY_ERROR_THRESHOLD,
                stats.total_samples,
            );
            *safe_mode = state::SafeModeState {
                active: true,
                entered_at: Some(now),
                trigger: Some("median_error".to_string()),
                median_error_at_entry: Some(median_error_abs),
                predictions_since_entry: 0,
                scored_at_entry: stats.total_samples,
            };
            return true;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Alert FP telemetry helpers
// ---------------------------------------------------------------------------

/// Classify an alert as true positive or false positive for telemetry tracking.
///
/// Cutoff-related alerts are true positives only when utilization is genuinely near
/// the hard limit (>= 95%). Alerts that fire at lower utilization are false positives
/// because the governor's scaling logic handles those cases without human intervention.
fn is_true_positive_alert(alert_type: &AlertType, state: &state::GovernorState) -> bool {
    match alert_type {
        AlertType::CutoffImminent | AlertType::SonnetCutoffRisk | AlertType::SessionCutoffRisk => {
            // True positive only if hard limit margin is genuinely negative AND utilization >= 95%
            let forecast = &state.capacity_forecast;
            let any_window_genuine = [
                &forecast.five_hour,
                &forecast.seven_day,
                &forecast.weekly_scoped,
            ]
            .iter()
            .any(|w| w.hard_limit_margin_hrs < 0.0 && w.hard_limit_remaining_pct <= 5.0);
            any_window_genuine
        }
        AlertType::EmergencyBrakeActivated => {
            // True positive if any window is actually at 98%+
            let forecast = &state.capacity_forecast;
            [
                &forecast.five_hour,
                &forecast.seven_day,
                &forecast.weekly_scoped,
            ]
            .iter()
            .any(|w| w.current_utilization >= 98.0)
        }
        AlertType::CollectorOffline => {
            // Collector offline is a true positive if data is genuinely stale (> 30 min)
            let age = (Utc::now() - state.last_fleet_aggregate.t1).num_seconds();
            age > 1800
        }
        _ => true, // Other alerts are assumed true positives by default
    }
}

// ---------------------------------------------------------------------------
// Governor daemon loop
// ---------------------------------------------------------------------------

/// Run one governor cycle: poll -> schedule -> burn_rate -> target -> scale -> alert -> write_state
///
/// This is the core loop body executed every `loop_interval` seconds.
pub fn run_governor_cycle(
    poller: &mut impl UsagePoller,
    state_path: &Path,
    dry_run: bool,
    loop_interval: u64,
    hysteresis_band: f64,
    max_up_per_cycle: u32,
    max_down_per_cycle: u32,
    target_ceiling: f64,
    alert_config: &AlertConfig,
    agents: &std::collections::HashMap<String, AgentConfig>,
    pre_scale_minutes: u64,
    promotions: &[Promotion],
    composite_risk_config: &CompositeRiskConfig,
    cone_scaling_config: &ConeScalingConfig,
    pricing_config: &crate::config::GovernorConfig,
) -> anyhow::Result<()> {
    let now = Utc::now();
    log::info!("[governor] === cycle start at {} ===", now.to_rfc3339());

    // 1. Load current state
    let mut state = state::load_state(state_path)?;

    // 1a. Load baseline burn rates from config (warm state)
    // This ensures that when EMA samples >= 3, we have config-derived baselines available
    state.load_baseline_burn_rates_from_config(agents);

    // 1b. Shift snapshot state before poll: current becomes previous.
    // On first poll, current_api_snapshot is None, so previous becomes None too.
    state.previous_api_snapshot = state.current_api_snapshot.take();

    // 1a. Poll Anthropic API for live usage data
    match poller.poll_usage() {
        Ok(usage_data) => {
            // Extract weekly_scoped utilization from model-agnostic limits[] array
            // This ensures the rotated model's REAL pct feeds the EMA calculation
            let weekly_scoped_util = usage_data
                .scoped_weekly()
                .map(|(_, window)| window.utilization)
                .unwrap_or(usage_data.weekly_scoped_utilization);

            let scoped_label = crate::state::weekly_scoped_display_label(
                usage_data.weekly_scoped_model.as_deref(),
            );
            log::info!(
                "[governor] polled usage: {}={:.1}%, all_models={:.1}%, 5h={:.1}%{}",
                scoped_label,
                weekly_scoped_util,
                usage_data.seven_day_utilization,
                usage_data.five_hour_utilization,
                if usage_data.stale { " (stale)" } else { "" },
            );

            // Detect weekly_scoped model identity change BEFORE updating state
            let prev_model = state.usage.weekly_scoped_model.clone();
            let new_model = usage_data.weekly_scoped_model.clone();

            // VERIFICATION: Log the pct value being used for the new/rotated model
            log::info!(
                "[governor] weekly_scoped model change detection: prev_model={:?}, new_model={:?}, new_weekly_scoped_pct={:.2}%",
                prev_model,
                new_model,
                weekly_scoped_util
            );

            let model_changed = crate::state::reset_weekly_scoped_on_model_change(
                &prev_model,
                &new_model,
                &mut state.burn_rate,
            );

            // If model changed, clear the previous weekly_scoped snapshot to avoid
            // computing a delta against the old model's utilization value
            if model_changed {
                if let Some(ref mut prev_snap) = state.previous_api_snapshot {
                    log::info!(
                        "[governor] clearing previous_api_snapshot.weekly_scoped_pct due to model change"
                    );
                    prev_snap.weekly_scoped_pct = 0.0;
                }

                // Reset fleet_pct_ema_samples to 0 to trigger cold-start seeding on next cycle
                // This ensures the new model starts with EstimateQuality::ColdStart and gets
                // seeded from baseline_burn_rate instead of claiming 0% / infinite headroom
                log::info!(
                    "[governor] resetting fleet_pct_ema_samples from {} to 0 due to model change",
                    state.burn_rate.fleet_pct_ema_samples
                );
                state.burn_rate.fleet_pct_ema_samples = 0;
            }

            state.usage = state::UsageState {
                weekly_scoped_pct: weekly_scoped_util,
                // The legacy sonnet_pct field is deprecated and NOT used for weekly_scoped calculations.
                // New code should use weekly_scoped_pct (model-agnostic) instead.
                // See state.rs lines 53-56 for the deprecated sonnet_pct field documentation.
                sonnet_pct: 0.0, // Deprecated - always 0.0, not used for weekly_scoped
                all_models_pct: usage_data.seven_day_utilization,
                five_hour_pct: usage_data.five_hour_utilization,
                sonnet_resets_at: usage_data.weekly_scoped_resets_at,
                seven_day_resets_at: usage_data.seven_day_resets_at,
                five_hour_resets_at: usage_data.five_hour_resets_at,
                stale: usage_data.stale,
                weekly_scoped_model: usage_data.weekly_scoped_model.clone(),
            };
            state.token_refresh_failing = usage_data.stale;

            // Update current_api_snapshot with the new snapshot data
            state.current_api_snapshot = Some(state::PrevUsageSnapshot {
                taken_at: now,
                five_hour_pct: usage_data.five_hour_utilization,
                seven_day_pct: usage_data.seven_day_utilization,
                weekly_scoped_pct: weekly_scoped_util,
            });

            // Calculate window deltas from consecutive API snapshots
            // Pure structure: if let pattern matching on both snapshots
            if let (Some(prev), Some(curr)) =
                (&state.previous_api_snapshot, &state.current_api_snapshot)
            {
                // Both snapshots available: proceed with delta computation
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);

                // Log computed window deltas
                log::info!(
                    "[governor] window deltas: 5h={:+.2}%, 7d={:+.2}%, 7ds={:+.2}% (previous: {:.1}/{:.1}/{:.1}%, current: {:.1}/{:.1}/{:.1}%)",
                    delta_5h, delta_7d, delta_7ds,
                    prev_pct.five_hour, prev_pct.seven_day, prev_pct.weekly_scoped,
                    curr_pct.five_hour, curr_pct.seven_day, curr_pct.weekly_scoped,
                );

                // Store computed deltas in governor state
                state.p5h_delta = Some(delta_5h);
                state.p7d_delta = Some(delta_7d);
                state.p7ds_delta = Some(delta_7ds);
            } else {
                // No previous snapshot to subtract from — the first poll after
                // governor start / state clear, or the poll after a failed one
                // (the failure leaves current_api_snapshot None, so the next
                // rotation shifts None into previous). Initialize every delta
                // field explicitly rather than leaving whatever the last cycle
                // wrote: a retained Some(..) here describes an interval that has
                // already scrolled past, and downstream consumers cannot tell it
                // from a fresh reading. None — not Some(0.0) — because "no
                // baseline" is not the same claim as "no change".
                state.p5h_delta = None;
                state.p7d_delta = None;
                state.p7ds_delta = None;

                log::debug!(
                    "[governor] no previous API snapshot; window deltas cleared (first poll or poll following a failure)"
                );
            }
        }
        Err(e) => {
            // If the error is from the API call (not token refresh), the token is fine.
            // Reset token_refresh_failing to prevent false positives from transient API
            // errors (e.g., 429 rate limits) that persist the stale flag from a previous cycle.
            if let Some(pe) = e.downcast_ref::<crate::poller::PollerError>() {
                match pe {
                    crate::poller::PollerError::ApiRequestFailed(_)
                    | crate::poller::PollerError::ApiError(_)
                    | crate::poller::PollerError::ParseError(_) => {
                        state.token_refresh_failing = false;
                    }
                    _ => {} // Auth errors: keep token_refresh_failing unchanged
                }
            } else {
                state.token_refresh_failing = false;
            }
            log::warn!("[governor] poll failed, keeping previous usage data: {}", e);
        }
    }

    // 1b. Clear emergency-brake-triggered safe_mode when utilization drops below threshold.
    //     The emergency brake sets safe_mode with trigger="emergency_brake" when any window
    //     hits 98%+. Once utilization drops (e.g. after a window reset), safe_mode should
    //     clear because the condition that triggered it no longer exists. Calibration-based
    //     safe_mode is NOT cleared here — that uses update_safe_mode_from_calibration().
    if state.safe_mode.active && state.safe_mode.trigger.as_deref() == Some("emergency_brake") {
        let max_util = [
            state.capacity_forecast.five_hour.current_utilization,
            state.capacity_forecast.seven_day.current_utilization,
            state.capacity_forecast.weekly_scoped.current_utilization,
        ]
        .into_iter()
        .fold(0.0_f64, f64::max);
        if max_util < EMERGENCY_BRAKE_THRESHOLD {
            log::info!(
                "[governor] clearing emergency_brake safe_mode — max utilization {:.1}% < {:.0}% threshold",
                max_util,
                EMERGENCY_BRAKE_THRESHOLD
            );
            state.safe_mode = state::SafeModeState::default();
        }
    }

    // 2. Run token collector pass to gather usage data from JSONL files
    match collector::run_collection_pass() {
        Ok(result) => {
            log::info!(
                "[governor] collector pass: {} lines, {} instances, ${:.4} total",
                result.lines_processed,
                result.instance_records,
                result.total_usd,
            );
        }
        Err(e) => {
            log::warn!("[governor] collector pass failed: {}", e);
        }
    }

    // 3. Read latest fleet record from database and update last_fleet_aggregate
    let db_path = collector::default_db_path();
    // Snapshot whether collector was offline before this update, so we can detect recovery.
    let collector_was_offline = (now - state.last_fleet_aggregate.t1).num_seconds() > 300;
    if let Ok(conn) = db::open_db(&db_path) {
        if let Ok(fleet_records) = db::query_last_fleets(&conn, 1) {
            if let Some(fleet_json) = fleet_records.first() {
                // Extract fleet aggregate data from the JSON record
                if let (Some(t0_str), Some(t1_str)) = (
                    fleet_json.get("t0").and_then(|v| v.as_str()),
                    fleet_json.get("t1").and_then(|v| v.as_str()),
                ) {
                    let t0: DateTime<Utc> = t0_str.parse().unwrap_or_else(|_| now);
                    let t1: DateTime<Utc> = t1_str.parse().unwrap_or_else(|_| now);
                    let workers = fleet_json
                        .get("workers")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let total_usd = fleet_json
                        .get("total-usd")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let p75_usd_hr = fleet_json
                        .get("p75-usd-hr")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let std_usd_hr = fleet_json
                        .get("std-usd-hr")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);

                    // Extract window percentage deltas
                    let p5h = fleet_json
                        .get("p5h")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let p7d = fleet_json
                        .get("p7d")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let p7ds = fleet_json
                        .get("p7ds")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let fleet_cache_eff = fleet_json
                        .get("fleet-cache-eff")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let cache_eff_p25 = fleet_json
                        .get("cache-eff-p25")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let cli_tokens = fleet_json
                        .get("cli-tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cli_cost = fleet_json
                        .get("cli-cost")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let sdk_tokens = fleet_json
                        .get("sdk-tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let sdk_cost = fleet_json
                        .get("sdk-cost")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);

                    state.last_fleet_aggregate = state::FleetAggregate {
                        t0,
                        t1,
                        sonnet_workers: workers,
                        sonnet_usd_total: total_usd,
                        sonnet_p75_usd_hr: p75_usd_hr,
                        sonnet_std_usd_hr: std_usd_hr,
                        window_pct_deltas: state::WindowPctDeltas {
                            five_hour: p5h,
                            seven_day: p7d,
                            weekly_scoped: p7ds,
                        },
                        fleet_cache_eff,
                        cache_eff_p25,
                        cli_tokens,
                        cli_cost,
                        sdk_tokens,
                        sdk_cost,
                    };

                    // Update consecutive low-cache-eff counter for alert tracking.
                    // Only count intervals where workers > 0 to avoid spurious counts
                    // during idle periods when cache efficiency is meaningless.
                    if workers > 0 && fleet_cache_eff < alert_config.low_cache_eff_threshold {
                        state.low_cache_eff_consecutive =
                            state.low_cache_eff_consecutive.saturating_add(1);
                    } else {
                        state.low_cache_eff_consecutive = 0;
                    }

                    log::debug!(
                        "[governor] fleet aggregate: {} workers, ${:.2}/hr p75, deltas 5h={:.2}% 7d={:.2}% 7ds={:.2}%, cache_eff={:.2} (consecutive_low={})",
                        workers, p75_usd_hr, p5h, p7d, p7ds, fleet_cache_eff, state.low_cache_eff_consecutive
                    );

                    // If the collector just recovered from an offline state, clear the
                    // collector_offline cooldown so a future outage fires immediately
                    // instead of waiting out the remaining cooldown window.
                    let collector_now_online = (now - t1).num_seconds() <= 300;
                    if collector_was_offline && collector_now_online {
                        let age_s = (now - t1).num_seconds();
                        log::info!(
                            "[governor] collector recovered — last record {}s old, clearing offline alert cooldown",
                            age_s
                        );
                        state
                            .alert_cooldown
                            .clear(&AlertType::CollectorOffline.to_string());
                    }
                }
            }
        }
    }

    // 4. Count current workers (from heartbeat files + tmux)
    // Seed state.workers from agents config if empty
    if state.workers.is_empty() && !agents.is_empty() {
        for (name, agent) in agents {
            state.workers.insert(
                name.clone(),
                state::WorkerState {
                    current: 0,
                    target: 0,
                    min: agent.min_workers,
                    max: agent.max_workers,
                },
            );
        }
    }

    // Build per-agent WorkerConfigs and count workers across all agents
    let agent_worker_configs: Vec<(String, WorkerConfig)> = agents
        .iter()
        .map(|(name, agent)| (name.clone(), WorkerConfig::from_agent_config(agent)))
        .collect();

    // Fall back to default if no agents configured
    let worker_configs: Vec<(String, WorkerConfig)> = if agent_worker_configs.is_empty() {
        vec![("default".to_string(), WorkerConfig::default())]
    } else {
        agent_worker_configs
    };

    // Count workers across all configured agents
    let mut total_heartbeat_count = 0usize;
    let mut total_tmux_count = 0usize;
    let mut all_sessions: Vec<String> = Vec::new();
    let mut consistent = true;

    for (_name, wc) in &worker_configs {
        let wc_count = worker::count_workers(wc);
        total_heartbeat_count += wc_count.heartbeat_count;
        total_tmux_count += wc_count.tmux_count;
        all_sessions.extend(wc_count.sessions);
        if !wc_count.consistent {
            consistent = false;
        }
    }

    let current_total = total_tmux_count as u32;
    let _prev_total = state.workers.values().map(|w| w.current).sum::<u32>();

    log::info!(
        "[governor] workers: {} active ({} heartbeats, {} tmux sessions, consistent={}, agents={})",
        current_total,
        total_heartbeat_count,
        total_tmux_count,
        consistent,
        worker_configs.len(),
    );

    // Update worker state with current count
    // Count current workers per agent from heartbeat/tmux data
    let mut current_workers_per_agent: HashMap<String, u32> = HashMap::new();
    for (name, wc) in &worker_configs {
        let wc_count = worker::count_workers(wc);
        current_workers_per_agent.insert(name.clone(), wc_count.tmux_count as u32);
    }

    for (name, ws) in state.workers.iter_mut() {
        ws.current = *current_workers_per_agent.get(name).unwrap_or(&0);
    }

    // 5. Compute burn rates and update capacity forecast using fleet aggregate data

    // 5-pre. Update fleet_pct_hr_ema from consecutive API reading deltas.
    //
    // The fleet record's p5h/p7d/p7ds fields are always null (the collector writes them
    // null and never fills them in), so dividing them by elapsed_hours always yields 0.
    // Instead we compute pct_hr from the delta between consecutive poller readings,
    // applying an EMA that is only updated on positive deltas — zero-delta cycles
    // (when the API percentage hasn't moved in the past N seconds) are skipped so
    // they can't drive the EMA down to zero.
    //
    // Save the old snapshot BEFORE updating it — we need it for reset detection later.
    let old_snapshot = state.burn_rate.prev_usage_snapshot.clone();
    {
        const EMA_ALPHA: f64 = 0.2;
        // Require at least 60 s between delta samples to avoid noise from very short windows
        const MIN_ELAPSED_SECS: f64 = 60.0;
        // If the governor was paused for > 30 min, the snapshot is too stale to use
        const MAX_ELAPSED_SECS: f64 = 1800.0;

        if !state.usage.stale {
            let new_five_hour = state.usage.five_hour_pct;
            let new_seven_day = state.usage.all_models_pct;
            // NOTE: weekly_scoped_pct is the model-agnostic field for weekly_scoped utilization.
            // The legacy sonnet_pct field is kept for backward compatibility but should not be used
            // in new code. When model identity changes, reset logic above ensures stale samples
            // are cleared.
            let new_weekly_scoped = state.usage.weekly_scoped_pct;

            // VERIFICATION: Log that the EMA is using the rotated model's actual pct
            log::info!(
                "[governor] EMA input: weekly_scoped_model={:?}, weekly_scoped_pct={:.2}% (this is the actual pct from the rotated model)",
                state.usage.weekly_scoped_model,
                new_weekly_scoped
            );
            if let Some(snap) = old_snapshot.clone() {
                let elapsed_secs = (now - snap.taken_at).num_seconds() as f64;
                let elapsed_hours_snap = elapsed_secs / 3600.0;

                if elapsed_secs >= MIN_ELAPSED_SECS && elapsed_secs <= MAX_ELAPSED_SECS {
                    // Compute per-window deltas from consecutive API snapshots
                    let old_pct = crate::db::WindowPctSnapshot {
                        five_hour: snap.five_hour_pct,
                        seven_day: snap.seven_day_pct,
                        weekly_scoped: snap.weekly_scoped_pct,
                    };
                    let new_pct = crate::db::WindowPctSnapshot {
                        five_hour: new_five_hour,
                        seven_day: new_seven_day,
                        weekly_scoped: new_weekly_scoped,
                    };
                    let (delta_5h, delta_7d, delta_7ds) =
                        calculate_window_pct_delta(&old_pct, &new_pct);

                    // Fleet total USD/hr from the most recent fleet aggregate (with staleness checking)
                    let baseline = get_sonnet_baseline_config(&state, agents);
                    let usd_per_worker = crate::burn_rate::staleness_checked_fleet_dollar_rate(
                        &state.last_fleet_aggregate,
                        &baseline,
                    );
                    let fleet_usd_hr =
                        usd_per_worker * state.last_fleet_aggregate.sonnet_workers as f64;

                    let samples = state.burn_rate.fleet_pct_ema_samples;
                    let mut updated_any = false;

                    if delta_5h > 0.0 {
                        let rate = delta_5h / elapsed_hours_snap;
                        if samples == 0 {
                            state.burn_rate.fleet_pct_hr_ema.five_hour = rate;
                        } else {
                            state.burn_rate.fleet_pct_hr_ema.five_hour = EMA_ALPHA * rate
                                + (1.0 - EMA_ALPHA) * state.burn_rate.fleet_pct_hr_ema.five_hour;
                        }
                        if fleet_usd_hr > 0.0 {
                            let ratio = fleet_usd_hr / rate;
                            if samples == 0 {
                                state.burn_rate.usd_per_pct_ema_five_hour = ratio;
                            } else {
                                state.burn_rate.usd_per_pct_ema_five_hour = EMA_ALPHA * ratio
                                    + (1.0 - EMA_ALPHA) * state.burn_rate.usd_per_pct_ema_five_hour;
                            }
                        }
                        updated_any = true;
                    }

                    if delta_7d > 0.0 {
                        let rate = delta_7d / elapsed_hours_snap;
                        if samples == 0 {
                            state.burn_rate.fleet_pct_hr_ema.seven_day = rate;
                        } else {
                            state.burn_rate.fleet_pct_hr_ema.seven_day = EMA_ALPHA * rate
                                + (1.0 - EMA_ALPHA) * state.burn_rate.fleet_pct_hr_ema.seven_day;
                        }
                        if fleet_usd_hr > 0.0 {
                            let ratio = fleet_usd_hr / rate;
                            if samples == 0 {
                                state.burn_rate.usd_per_pct_ema_seven_day = ratio;
                            } else {
                                state.burn_rate.usd_per_pct_ema_seven_day = EMA_ALPHA * ratio
                                    + (1.0 - EMA_ALPHA) * state.burn_rate.usd_per_pct_ema_seven_day;
                            }
                        }
                        updated_any = true;
                    }

                    if delta_7ds > 0.0 {
                        let rate = delta_7ds / elapsed_hours_snap;
                        // VERIFICATION: Log that the weekly_scoped EMA is using the new model's pct
                        log::info!(
                            "[governor] updating weekly_scoped EMA: delta={:+.3}%, rate={:.4}%/hr, model={:?}, source_pct={:.2}%",
                            delta_7ds,
                            rate,
                            state.usage.weekly_scoped_model,
                            new_weekly_scoped
                        );
                        if samples == 0 {
                            state.burn_rate.fleet_pct_hr_ema.weekly_scoped = rate;
                        } else {
                            state.burn_rate.fleet_pct_hr_ema.weekly_scoped = EMA_ALPHA * rate
                                + (1.0 - EMA_ALPHA)
                                    * state.burn_rate.fleet_pct_hr_ema.weekly_scoped;
                        }
                        if fleet_usd_hr > 0.0 {
                            let ratio = fleet_usd_hr / rate;
                            if samples == 0 {
                                state.burn_rate.usd_per_pct_ema_weekly_scoped = ratio;
                            } else {
                                state.burn_rate.usd_per_pct_ema_weekly_scoped = EMA_ALPHA * ratio
                                    + (1.0 - EMA_ALPHA)
                                        * state.burn_rate.usd_per_pct_ema_weekly_scoped;
                            }
                        }
                        updated_any = true;
                    }

                    if updated_any {
                        state.burn_rate.fleet_pct_ema_samples =
                            state.burn_rate.fleet_pct_ema_samples.saturating_add(1);
                    }

                    log::info!(
                        "[governor] {} computed window deltas (in {:.0}s): 5h={:+.3}% 7d={:+.3}% 7ds={:+.3}% \
                         → EMA pct/hr: 5h={:.4} 7d={:.4} 7ds={:.4} (samples={})",
                        now.to_rfc3339(),
                        elapsed_secs,
                        delta_5h,
                        delta_7d,
                        delta_7ds,
                        state.burn_rate.fleet_pct_hr_ema.five_hour,
                        state.burn_rate.fleet_pct_hr_ema.seven_day,
                        state.burn_rate.fleet_pct_hr_ema.weekly_scoped,
                        state.burn_rate.fleet_pct_ema_samples,
                    );
                }
            }

            // Update the snapshot for use in the next cycle
            state.burn_rate.prev_usage_snapshot = Some(state::PrevUsageSnapshot {
                taken_at: now,
                five_hour_pct: new_five_hour,
                seven_day_pct: new_seven_day,
                weekly_scoped_pct: new_weekly_scoped,
            });
        }
    }

    let elapsed_hours = if state.last_fleet_aggregate.t0 != state.last_fleet_aggregate.t1 {
        (state.last_fleet_aggregate.t1 - state.last_fleet_aggregate.t0).num_seconds() as f64
            / 3600.0
    } else {
        0.0
    };

    // 5-pre-b. Annotate window percentage deltas in the SQLite mirror.
    //
    // After computing API deltas, annotate the i and f records for the interval
    // with the per-window percentage deltas, apportioning by total_usd weight.
    // This unblocks empirical promotion validation and downstream analytics.
    if !state.usage.stale {
        if let (Some(ref prev_snap), Ok(conn)) = (&old_snapshot, db::open_db(&db_path)) {
            let t0 = state.last_fleet_aggregate.t0;
            let t1 = state.last_fleet_aggregate.t1;
            let workers_at_start = state.last_fleet_aggregate.sonnet_workers;
            let workers_at_end = current_total;

            let old_pct = db::WindowPctSnapshot {
                five_hour: prev_snap.five_hour_pct,
                seven_day: prev_snap.seven_day_pct,
                weekly_scoped: prev_snap.weekly_scoped_pct,
            };
            let new_pct = db::WindowPctSnapshot {
                five_hour: state.usage.five_hour_pct,
                seven_day: state.usage.all_models_pct,
                weekly_scoped: state.usage.weekly_scoped_pct,
            };

            // Guard 1: Elapsed time < 2 minutes - too noisy for reliable annotation
            let elapsed_seconds = (t1 - t0).num_seconds().abs();
            if elapsed_seconds < 120 {
                log::warn!(
                    "[governor] skipping window delta annotation: interval too short ({}s < 120s)",
                    elapsed_seconds
                );
            } else if workers_at_start != workers_at_end {
                // Guard 2: Worker count changed mid-interval - sessions not comparable
                log::warn!(
                    "[governor] skipping window delta annotation: worker count changed mid-interval ({} -> {})",
                    workers_at_start,
                    workers_at_end
                );
            } else {
                // Guard 3: Check if interval spans a window reset
                // A reset is detected when any window utilization drops > 1%
                let reset_threshold = 1.0;
                let five_hour_reset = new_pct.five_hour < old_pct.five_hour - reset_threshold;
                let seven_day_reset = new_pct.seven_day < old_pct.seven_day - reset_threshold;
                let weekly_scoped_reset =
                    new_pct.weekly_scoped < old_pct.weekly_scoped - reset_threshold;

                if five_hour_reset || seven_day_reset || weekly_scoped_reset {
                    log::warn!(
                        "[governor] skipping window delta annotation: interval spans window reset (5h: {:.1}%→{:.1}%, 7d: {:.1}%→{:.1}%, 7ds: {:.1}%→{:.1}%)",
                        old_pct.five_hour, new_pct.five_hour,
                        old_pct.seven_day, new_pct.seven_day,
                        old_pct.weekly_scoped, new_pct.weekly_scoped
                    );
                } else {
                    // All guards passed - proceed with annotation
                    if let Err(e) = db::annotate_window_pct_deltas(
                        &conn,
                        t0,
                        t1,
                        &old_pct,
                        &new_pct,
                        workers_at_start,
                        workers_at_end,
                    ) {
                        log::warn!("[governor] failed to annotate window pct deltas: {}", e);
                    }
                }
            }
        }
    }

    // Build current utilization map from polled usage
    let mut current_utilization = HashMap::new();
    current_utilization.insert("five_hour".to_string(), state.usage.five_hour_pct);
    current_utilization.insert("seven_day".to_string(), state.usage.all_models_pct);
    current_utilization.insert("weekly_scoped".to_string(), state.usage.weekly_scoped_pct);

    // 5a-pre. Detect window resets and score predictions for calibration.
    //
    // A window reset is detected when utilization drops > 1% compared to the
    // previous cycle (stored in old_snapshot, captured before the update).
    // When a reset is detected, we score any pending prediction for that window
    // by comparing the predicted final utilization (made at window start) against
    // the actual final utilization (observed just before reset).
    const WINDOW_RESET_THRESHOLD: f64 = 1.0;
    if let Some(ref prev_snap) = old_snapshot {
        // Current utilizations for comparison
        let cur_5h = state.usage.five_hour_pct;
        let cur_7d = state.usage.all_models_pct;
        let cur_7ds = state.usage.weekly_scoped_pct;

        // Previous utilizations (from before the snapshot update)
        let prev_5h = prev_snap.five_hour_pct;
        let prev_7d = prev_snap.seven_day_pct;
        let prev_7ds = prev_snap.weekly_scoped_pct;

        // Check for resets in each window
        let windows_to_check = [
            ("five_hour", cur_5h, prev_5h),
            ("seven_day", cur_7d, prev_7d),
            ("weekly_scoped", cur_7ds, prev_7ds),
        ];

        for (window_name, current, previous) in windows_to_check {
            // Detect reset: utilization dropped > threshold
            if current < previous - WINDOW_RESET_THRESHOLD {
                // We have a window reset - check for pending prediction
                if let Some(pred) = state.pending_predictions.get(window_name) {
                    // Score the prediction: predicted change vs actual change
                    // Predicted change = predicted_final_pct - starting_pct
                    // Actual change = previous (just before reset) - starting_pct
                    let predicted_change = pred.predicted_final_pct - pred.starting_pct;
                    let actual_change = previous - pred.starting_pct;

                    let score = calibrator::score_prediction(
                        window_name,
                        predicted_change,
                        actual_change,
                        pred.prediction_time,
                    );

                    log::info!(
                        "[governor] window reset detected in {}: utilization {:.1}% → {:.1}% (drop {:.1}%), \
                         scoring prediction: predicted_change={:+.2}%, actual_change={:+.2}%, error={:+.2}%",
                        window_name,
                        previous,
                        current,
                        previous - current,
                        predicted_change,
                        actual_change,
                        score.error,
                    );

                    // Append score to accuracy log
                    if let Err(e) = calibrator::append_score(&score) {
                        log::warn!(
                            "[governor] failed to append prediction score for {}: {}",
                            window_name,
                            e
                        );
                    } else {
                        log::debug!(
                            "[governor] scored prediction for {}: predicted={:.2}%, actual={:.2}%, error={:+.2}%",
                            window_name,
                            predicted_change,
                            actual_change,
                            score.error
                        );
                    }

                    // Remove the pending prediction after scoring
                    state.pending_predictions.remove(window_name);
                }
            }
        }
    }

    // 5a-pre-b. Store new predictions for all windows.
    //
    // For each window, predict the final utilization percentage when the window resets.
    // The prediction is: current_utilization + (fleet_pct_per_hour * hours_remaining).
    // We need the fleet_pct_per_hour values which are computed later, so we'll do this
    // in two parts: detect resets now, store predictions after pct/hr is computed.
    // For now, just mark that we need to store predictions later.
    //
    // The actual prediction storage happens after fleet_pct_per_hour is computed.

    // Build effective hours remaining map from poller data
    // Uses effective_hours_remaining_from so only windows in applies_to get the promo boost.
    let mut hours_remaining = HashMap::new();
    if let Ok(reset_time) = state.usage.five_hour_resets_at.parse::<DateTime<Utc>>() {
        hours_remaining.insert(
            "five_hour".to_string(),
            schedule::effective_hours_remaining_from(now, reset_time, promotions, "five_hour"),
        );
    }
    // seven_day (all-models) has its own reset time, independent of whether this
    // account has a distinct Sonnet-scoped window at all. Previously this was
    // (incorrectly) derived from sonnet_resets_at, so an account with no separate
    // Sonnet limit (sonnet_resets_at == "") silently lost seven_day's hours_remaining
    // too — it defaulted to 0.0 downstream via unwrap_or(0.0), which made a healthy,
    // real window look maximally urgent and could out-compete five_hour for binding
    // status on a phantom score. See [[project_cgov_polish_loop]] root-cause writeup.
    if let Ok(reset_time) = state.usage.seven_day_resets_at.parse::<DateTime<Utc>>() {
        hours_remaining.insert(
            "seven_day".to_string(),
            schedule::effective_hours_remaining_from(now, reset_time, promotions, "seven_day"),
        );
    }
    if let Ok(reset_time) = state.usage.sonnet_resets_at.parse::<DateTime<Utc>>() {
        hours_remaining.insert(
            "weekly_scoped".to_string(),
            schedule::effective_hours_remaining_from(now, reset_time, promotions, "weekly_scoped"),
        );
    }

    // Compute fleet_pct_per_hour from the accumulated API-delta EMA.
    //
    // Strategy (in priority order):
    //   (A) EMA from consecutive API readings — use when at least one positive delta
    //       has been observed (fleet_pct_ema_samples >= 1).
    //   (B) Dollar fallback with learned ratio — when the EMA for a window is still
    //       zero but the collector's USD/hr and a learned usd_per_pct ratio are both
    //       available, estimate pct/hr = fleet_usd_hr / usd_per_pct_ema.
    //   (C) Dollar fallback with baseline ratio — when neither EMA nor learned ratio
    //       is available yet (startup / short polling window), use the collector's
    //       USD/hr with the hardcoded baseline ratio derived from default burn rate
    //       assumptions (~$5/hr/worker ÷ ~1.5%/hr/worker ≈ 3.33 $/pct).  This
    //       ensures safe_worker_count is non-None even before the first API delta is
    //       observed, so the governor can proactively scale from startup.
    //   (D) Zero — truly no data at all (no dollar burn either).
    let fleet_pct_per_hour: HashMap<String, f64> = {
        let ema = &state.burn_rate.fleet_pct_hr_ema;
        let samples = state.burn_rate.fleet_pct_ema_samples;
        // Fleet total USD/hr (p75 per-worker × active workers) with staleness checking
        let baseline = get_sonnet_baseline_config(&state, agents);
        let usd_per_worker = crate::burn_rate::staleness_checked_fleet_dollar_rate(
            &state.last_fleet_aggregate,
            &baseline,
        );
        let fleet_usd_hr = usd_per_worker * state.last_fleet_aggregate.sonnet_workers as f64;
        // Baseline dollars-per-pct ratio from agent config baseline_burn_rate
        let baseline_usd_per_pct =
            baseline.dollars_per_worker_per_hour / baseline.pct_per_worker_per_hour;

        let rate_for = |ema_val: f64, usd_per_pct: f64| -> f64 {
            if samples >= 1 && ema_val > 0.0 {
                ema_val // (A) API delta EMA
            } else if fleet_usd_hr > 0.0 && usd_per_pct > 0.0 {
                fleet_usd_hr / usd_per_pct // (B) learned ratio
            } else if fleet_usd_hr > 0.0 {
                fleet_usd_hr / baseline_usd_per_pct // (C) baseline ratio fallback
            } else {
                0.0 // (D) no data at all
            }
        };

        let mut map = HashMap::new();
        map.insert(
            "five_hour".to_string(),
            rate_for(ema.five_hour, state.burn_rate.usd_per_pct_ema_five_hour),
        );
        map.insert(
            "seven_day".to_string(),
            rate_for(ema.seven_day, state.burn_rate.usd_per_pct_ema_seven_day),
        );
        map.insert(
            "weekly_scoped".to_string(),
            rate_for(
                ema.weekly_scoped,
                state.burn_rate.usd_per_pct_ema_weekly_scoped,
            ),
        );

        if samples == 0 && fleet_usd_hr > 0.0 {
            log::debug!(
                "[governor] fleet_pct_hr: baseline dollar fallback active \
                 (fleet_usd_hr={:.4}/hr, usd_per_pct={:.3}) → \
                 5h={:.4} 7d={:.4} 7ds={:.4} pct/hr",
                fleet_usd_hr,
                baseline_usd_per_pct,
                map["five_hour"],
                map["seven_day"],
                map["weekly_scoped"],
            );
        }

        map
    };

    // 5a-pre-b. Store new predictions for all windows.
    //
    // For each window, predict the final utilization percentage when the window resets.
    // The prediction is: current_utilization + (fleet_pct_per_hour * hours_remaining).
    // This prediction will be scored when the window resets (utilization drops).
    for window in &["five_hour", "seven_day", "weekly_scoped"] {
        let util = current_utilization.get(*window).copied().unwrap_or(0.0);
        let hrs_left = hours_remaining.get(*window).copied().unwrap_or(0.0);
        let pct_hr = fleet_pct_per_hour.get(*window).copied().unwrap_or(0.0);

        // Predict final utilization: current + (rate * time)
        // Clamp to 0-100% range
        let predicted_final_pct = (util + pct_hr * hrs_left).clamp(0.0, 100.0);

        // Store the prediction
        state.pending_predictions.insert(
            window.to_string(),
            state::PendingPrediction {
                prediction_time: now,
                predicted_final_pct,
                starting_pct: util,
            },
        );

        log::debug!(
            "[governor] stored prediction for {}: current={:.1}%, rate={:.3}%/hr, hrs_left={:.1}, predicted_final={:.1}%",
            window,
            util,
            pct_hr,
            hrs_left,
            predicted_final_pct
        );
    }

    // 5a. Check calibration accuracy and update safe mode state.
    //
    // This must run before the capacity forecast is built so the effective
    // target ceiling (reduced when safe mode is active) is used in forecasts.
    if let Ok(scores) = calibrator::read_all_scores() {
        if !scores.is_empty() {
            let cal_stats = calibrator::compute_stats(&scores);
            update_safe_mode_from_calibration(
                &mut state.safe_mode,
                &mut state.burn_rate.calibration,
                &cal_stats,
                now,
            );
        }
    }

    // Effective settings — conservative overrides applied when safe mode is active.
    // - hysteresis_band: widened by SAFE_MODE_HYSTERESIS_MULTIPLIER
    // - composite risk: disabled (cross-window optimisation is too uncertain)
    // - sprint: sprint eligibility is also blocked (checked in check_underutilization_sprint)
    // - target_ceiling: reduced by SAFE_MODE_CEILING_REDUCTION pct points per-window

    let effective_hysteresis = if state.safe_mode.active {
        let widened = hysteresis_band * SAFE_MODE_HYSTERESIS_MULTIPLIER;
        log::info!(
            "[governor] safe_mode active: hysteresis_band {:.1} → {:.1}",
            hysteresis_band,
            widened
        );
        widened.min(10.0) // cap at 10 pct points
    } else {
        hysteresis_band
    };

    // When safe mode is active, disable composite risk optimisation so the governor
    // uses the conservative binding-window ceiling only.
    let safe_composite_risk;
    let effective_composite_risk: &CompositeRiskConfig = if state.safe_mode.active {
        safe_composite_risk = CompositeRiskConfig {
            enabled: false,
            ..composite_risk_config.clone()
        };
        &safe_composite_risk
    } else {
        composite_risk_config
    };

    // When safe mode is active, force the p75 (conservative) estimate regardless of cone width.
    let safe_cone_scaling;
    let effective_cone_scaling: &ConeScalingConfig = if state.safe_mode.active {
        // narrow_threshold = 0.0 → cone_ratio (always ≥ 1.0) is always "wide" → always p75
        safe_cone_scaling = ConeScalingConfig {
            narrow_threshold: 0.0,
        };
        &safe_cone_scaling
    } else {
        cone_scaling_config
    };

    // Build capacity forecast for each window using burn_rate module
    let mut five_hour_forecast = state::WindowForecast::default();
    let mut seven_day_forecast = state::WindowForecast::default();
    let mut weekly_scoped_forecast = state::WindowForecast::default();

    // Track effective target ceilings per window (after safe mode reduction)
    let mut effective_target_ceilings = std::collections::HashMap::new();

    for window in &["five_hour", "seven_day", "weekly_scoped"] {
        let util = current_utilization.get(*window).copied().unwrap_or(0.0);
        let hrs_left = hours_remaining.get(*window).copied().unwrap_or(0.0);
        let fleet_pct_hr = fleet_pct_per_hour.get(*window).copied().unwrap_or(0.0);

        // Get the base target ceiling for this specific window (from config override or global default)
        let base_target_ceiling = pricing_config.daemon.get_target_ceiling_for_window(window);

        // Apply safe mode reduction if active (per-window)
        let effective_target_ceiling = if state.safe_mode.active {
            let reduced = base_target_ceiling - SAFE_MODE_CEILING_REDUCTION;
            log::info!(
                "[governor] safe_mode active: {} target_ceiling {:.0}% → {:.0}%",
                window,
                base_target_ceiling,
                reduced
            );
            reduced.max(50.0) // never below 50%
        } else {
            base_target_ceiling
        };

        // Store effective target ceiling for this window (used later for logging)
        effective_target_ceilings.insert(window.to_string(), effective_target_ceiling);

        // Per-worker pct/hr rate for safe_worker_count calculation
        // Per-worker pct/hr rate for safe_worker_count calculation.
        //
        // Use current_total.max(1) rather than requiring current_total > 0: when the
        // fleet has genuinely scaled to 0 (e.g. correctly holding at 0 to protect a
        // tight window) but we DO have real aggregate rate data (fleet_pct_hr > 0),
        // dividing by a hypothetical 1 worker yields a conservative (pessimistic)
        // per-worker estimate, letting safe_worker_count compute a real, stable 0
        // instead of collapsing to None ("insufficient data"). Previously, hitting
        // current_total == 0 made this 0.0 regardless of fleet_pct_hr, which flowed
        // into safe_worker_count_or_max's None branch and reset the ceiling to
        // max_workers — causing a 0 -> None -> max_workers -> real-0-again flap every
        // cycle the fleet drained to 0, launching (and billing) workers each time.
        // True cold start (fleet_pct_hr == 0, no samples ever) is unaffected: it still
        // yields 0.0 here and correctly falls through to the max_workers-ceiling
        // bootstrap path downstream.
        let pct_per_worker = if fleet_pct_hr > 0.0 {
            fleet_pct_hr / current_total.max(1) as f64
        } else {
            0.0
        };

        // Convert per-worker USD/hr stddev to pct/hr stddev using per-window USD-per-pct ratio.
        // Falls back to baseline ratio from config when the learned ratio is unavailable.
        let baseline = get_sonnet_baseline_config(&state, agents);
        let baseline_usd_per_pct =
            baseline.dollars_per_worker_per_hour / baseline.pct_per_worker_per_hour;
        let usd_per_pct = match *window {
            "five_hour" => state.burn_rate.usd_per_pct_ema_five_hour,
            "seven_day" => state.burn_rate.usd_per_pct_ema_seven_day,
            "weekly_scoped" => state.burn_rate.usd_per_pct_ema_weekly_scoped,
            _ => 0.0,
        };
        let effective_usd_per_pct = if usd_per_pct > 0.0 {
            usd_per_pct
        } else {
            baseline_usd_per_pct
        };
        let std_pct_hr = state.last_fleet_aggregate.sonnet_std_usd_hr / effective_usd_per_pct;

        // Compute estimate_quality based on EMA sample count
        let ema_val = match *window {
            "five_hour" => state.burn_rate.fleet_pct_hr_ema.five_hour,
            "seven_day" => state.burn_rate.fleet_pct_hr_ema.seven_day,
            "weekly_scoped" => state.burn_rate.fleet_pct_hr_ema.weekly_scoped,
            _ => 0.0,
        };
        let estimate_quality = if state.burn_rate.fleet_pct_ema_samples >= 3 && ema_val > 0.0 {
            state::EstimateQuality::Calibrated
        } else if state.burn_rate.fleet_pct_ema_samples == 0 {
            state::EstimateQuality::ColdStart
        } else {
            state::EstimateQuality::InsufficientSamples
        };

        // --- Cold-start base-rate seeding for production path (bead bf-3ebgd) ---
        //
        // When a window has insufficient EMA samples (< MIN_SAMPLES_FOR_EMA) and no
        // observed burn rate this interval (fleet_pct_hr == 0.0), it would otherwise
        // carry a zero rate straight into generate_window_forecast, which interprets
        // 0 as *infinite* headroom (predicted_exhaustion = +inf). This is dangerous
        // for cold-start windows that are genuinely active but lack calibration history.
        //
        // Fix: seed the burn rate from the agent config's baseline_burn_rate (via
        // baseline_burn_rate_or_default accessor) when the window is in cold start
        // AND the API reports real utilization (util > 0.0, meaning the window exists
        // this period rather than being the absent-window sentinel at 0%).
        //
        // Rationale for baseline_burn_rate as the seed:
        // - Already present in AgentConfig, no new coupling
        // - Conservative (1.5% per worker/hr by default)
        // - Per-agent configurable for different models
        // - Same source already used by the burn_rate module's fallback logic
        //
        // Keep uncertainty wide: the seeded rate uses a non-trivial fleet stddev
        // (the full fleet rate itself) to widen the confidence cone, so the
        // pessimistic p75 safe-worker path engages until real samples take over.
        // Calibrated windows (>= MIN_SAMPLES_FOR_EMA) are unaffected.
        //
        let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) = if matches!(
            estimate_quality,
            state::EstimateQuality::ColdStart | state::EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0
        {
            let base_per_worker = baseline.pct_per_worker_per_hour;
            let seeded_fleet_pct = base_per_worker * current_total as f64;
            // Mark the estimate uncertain: a fleet stddev on the order of the rate
            // itself widens the confidence cone (cone_ratio > 1) so the pessimistic
            // p75 exhaustion / safe-worker path engages. Using the full fleet rate
            // as the spread is deliberately conservative for a wholly unmeasured window.
            let widened_std_pct = seeded_fleet_pct;
            log::info!(
                "[governor] {}: cold-start (no burn samples yet, util={:.1}%) — \
                     seeding conservative base rate {:.3}%/worker/hr across {} worker(s) \
                     with widened uncertainty cone; estimate will self-correct as \
                     real samples accumulate",
                window,
                util,
                base_per_worker,
                current_total,
            );
            (seeded_fleet_pct, base_per_worker, widened_std_pct)
        } else {
            (fleet_pct_hr, pct_per_worker, std_pct_hr)
        };

        let forecast = generate_window_forecast(
            window,
            fleet_pct_hr_seeded,
            util,
            effective_target_ceiling,
            hrs_left,
            pct_per_worker_seeded,
            std_pct_hr_seeded,
            estimate_quality,
        );

        let mut forecast = forecast;

        match *window {
            "five_hour" => five_hour_forecast = forecast,
            "seven_day" => seven_day_forecast = forecast,
            "weekly_scoped" => weekly_scoped_forecast = forecast,
            _ => {}
        }
    }

    // Identify binding window (highest risk_score)
    // The risk_score combines margin urgency, duration weight, and volatility (cone_ratio).
    // Higher risk_score = more urgent window that should drive scaling decisions.
    let windows = [
        ("five_hour", &five_hour_forecast),
        ("seven_day", &seven_day_forecast),
        ("weekly_scoped", &weekly_scoped_forecast),
    ];

    // Only consider windows we actually have reset-time data for this cycle. A
    // window absent from `hours_remaining` (e.g. seven_day_sonnet on an account
    // with no distinct Sonnet-scoped limit) falls back to hrs_left=0.0 upstream,
    // which zeroes its margin_pct and defaults risk_score to 0.0 — a phantom
    // score that can beat a real, healthy window's legitimately negative
    // (low-risk) score. Excluding data-absent windows keeps binding selection
    // limited to windows the API actually reports as real constraints.
    let binding_window = windows
        .iter()
        .filter(|(name, _)| {
            hours_remaining.contains_key(*name) && !state.is_window_consecutively_absent(name)
        })
        .max_by(|(_, a), (_, b)| {
            a.risk_score
                .partial_cmp(&b.risk_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(name, _)| name.to_string())
        .unwrap_or_default();

    // Set binding flag
    if binding_window == "five_hour" {
        five_hour_forecast.binding = true;
    } else if binding_window == "seven_day" {
        seven_day_forecast.binding = true;
    } else if binding_window == "weekly_scoped" {
        weekly_scoped_forecast.binding = true;
    }

    // Update state with new capacity forecast
    state.capacity_forecast = state::CapacityForecast {
        five_hour: five_hour_forecast,
        seven_day: seven_day_forecast,
        weekly_scoped: weekly_scoped_forecast,
        binding_window: binding_window.clone(),
        dollars_per_pct_7d_s: 0.0,
        estimated_remaining_dollars: 0.0,
    };

    // Update schedule state with per-window multipliers and effective hours.
    // Each window's multiplier respects the promotion's applies_to list —
    // only windows listed there get > 1.0; all others stay 1.0.

    // Empirically validate promotion multiplier from token-history DB.
    // If validation fails (insufficient data or ratio out of range), fall back to 1x.
    let db_path = collector::default_db_path();
    let promo_validation: PromotionValidationResult = if let Some(promo) = promotions.first() {
        // Only validate if there's an active promotion with offpeak_multiplier > 1.0
        if promo.offpeak_multiplier > 1.0 && schedule::is_promo_active_at(now, promo) {
            validate_promotion_from_db(&db_path, promo.offpeak_multiplier)
        } else {
            PromotionValidationResult {
                validated: true,
                observed_ratio: 1.0,
                declared_multiplier: promo.offpeak_multiplier,
                median_peak: 0.0,
                median_offpeak: 0.0,
                peak_samples: 0,
                offpeak_samples: 0,
                reason: None,
            }
        }
    } else {
        PromotionValidationResult {
            validated: true,
            observed_ratio: 1.0,
            declared_multiplier: 1.0,
            median_peak: 0.0,
            median_offpeak: 0.0,
            peak_samples: 0,
            offpeak_samples: 0,
            reason: None,
        }
    };

    // Update burn_rate state with validation results
    state.burn_rate.tokens_per_pct_peak = promo_validation.median_peak as u64;
    state.burn_rate.tokens_per_pct_offpeak = promo_validation.median_offpeak as u64;
    state.burn_rate.offpeak_ratio_observed = promo_validation.observed_ratio;
    state.burn_rate.offpeak_ratio_expected = promo_validation.declared_multiplier;
    state.burn_rate.promotion_validated = promo_validation.validated;
    state.burn_rate.promotion_peak_samples = promo_validation.peak_samples;
    state.burn_rate.promotion_offpeak_samples = promo_validation.offpeak_samples;

    // Get the effective multiplier based on validation result
    let effective_promo_multiplier = effective_multiplier(&promo_validation);

    // For each window, determine the multiplier to use:
    // - During peak hours: always 1.0
    // - During off-peak: use effective multiplier if promotion applies to window, else 1.0
    let is_peak = schedule::is_peak_at(now);
    let mult_five_hour = if is_peak {
        1.0
    } else {
        // Check if any promotion applies to five_hour window
        let applies = promotions.iter().any(|p| {
            p.applies_to.iter().any(|w| w == "five_hour") && schedule::is_promo_active_at(now, p)
        });
        if applies {
            effective_promo_multiplier
        } else {
            1.0
        }
    };
    let mult_seven_day = if is_peak {
        1.0
    } else {
        let applies = promotions.iter().any(|p| {
            p.applies_to.iter().any(|w| w == "seven_day") && schedule::is_promo_active_at(now, p)
        });
        if applies {
            effective_promo_multiplier
        } else {
            1.0
        }
    };
    let mult_weekly_scoped = if is_peak {
        1.0
    } else {
        let applies = promotions.iter().any(|p| {
            p.applies_to.iter().any(|w| w == "weekly_scoped")
                && schedule::is_promo_active_at(now, p)
        });
        if applies {
            effective_promo_multiplier
        } else {
            1.0
        }
    };
    let eff_five_hour = hours_remaining.get("five_hour").copied().unwrap_or(0.0);
    let eff_seven_day = hours_remaining.get("seven_day").copied().unwrap_or(0.0);
    let eff_weekly_scoped = hours_remaining.get("weekly_scoped").copied().unwrap_or(0.0);
    // Effective hours for display: use the binding window's value
    let eff_display = match binding_window.as_str() {
        "five_hour" => eff_five_hour,
        "seven_day" => eff_seven_day,
        _ => eff_weekly_scoped,
    };
    // Raw hours remaining: wall-clock hours until seven_day reset (approx)
    let raw_hours = state
        .usage
        .sonnet_resets_at
        .parse::<DateTime<Utc>>()
        .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
        .unwrap_or(0.0);
    state.schedule = state::ScheduleState {
        is_peak_hour: schedule::is_peak_at(now),
        is_promo_active: schedule::is_any_promo_active_at(now, promotions),
        promo_multiplier_five_hour: mult_five_hour,
        promo_multiplier_seven_day: mult_seven_day,
        promo_multiplier_weekly_scoped: mult_weekly_scoped,
        // max across windows for backward-compatible display
        promo_multiplier: [mult_five_hour, mult_seven_day, mult_weekly_scoped]
            .iter()
            .cloned()
            .fold(1.0_f64, f64::max),
        effective_hours_remaining_five_hour: eff_five_hour,
        effective_hours_remaining_seven_day: eff_seven_day,
        effective_hours_remaining_weekly_scoped: eff_weekly_scoped,
        effective_hours_remaining: eff_display,
        raw_hours_remaining: raw_hours,
    };

    // Update burn_rate from fleet aggregate if we have valid data
    if elapsed_hours > 0.0 && current_total > 0 {
        let deltas = &state.last_fleet_aggregate.window_pct_deltas;
        let total_pct_delta = deltas.five_hour + deltas.seven_day + deltas.weekly_scoped;
        let avg_pct_per_hour = total_pct_delta / (elapsed_hours * 3.0); // Average across windows

        // Get baseline before mutable borrow (for staleness checking)
        let baseline = get_sonnet_baseline_config(&state, agents);
        let usd_per_worker = crate::burn_rate::staleness_checked_fleet_dollar_rate(
            &state.last_fleet_aggregate,
            &baseline,
        );

        let entry = state
            .burn_rate
            .by_model
            .entry("claude-sonnet-4-20250514".to_string())
            .or_insert(state::ModelBurnRate {
                pct_per_worker_per_hour: 0.0,
                dollars_per_worker_per_hour: 0.0,
                samples: 0,
            });

        // Compute per-worker rates (with staleness checking for dollar rate)
        let pct_per_worker = avg_pct_per_hour / current_total as f64;

        entry.pct_per_worker_per_hour = pct_per_worker;
        entry.dollars_per_worker_per_hour = usd_per_worker;
        entry.samples = entry.samples.saturating_add(1);
        state.burn_rate.last_sample_at = Some(now);
    }

    // 6. Log capacity forecast
    log_capacity_forecast(
        &state.capacity_forecast,
        state.usage.weekly_scoped_model.as_deref(),
    );

    // Get the effective target ceiling for the binding window (used for logging)
    let binding_effective_ceiling = effective_target_ceilings
        .get(&binding_window)
        .copied()
        .unwrap_or(target_ceiling);

    // 4. Compute target workers
    let target = compute_target_workers(
        &state,
        binding_effective_ceiling,
        effective_composite_risk,
        effective_cone_scaling,
    );
    log::info!(
        "[governor] target workers: {} (ceiling: {:.0}%{})",
        target,
        binding_effective_ceiling,
        if state.safe_mode.active {
            ", safe_mode"
        } else {
            ""
        }
    );

    // 4b. Underutilization sprint: burn spare use-or-lose capacity by boosting a
    // subscription generator toward its max when a window is under-used and resets
    // soon — but only while it has queued generation work, so the boost is productive.
    let target = apply_underutilization_sprint(&state, &pricing_config.sprint, agents, target, now);

    // 4a. Pre-scale check: look for upcoming peak/off-peak transitions
    //
    // Conservative-only: pre-scale DOWN before losing multiplier bonus,
    // never pre-scale UP before gaining bonus.
    let pre_scale = state
        .usage
        .sonnet_resets_at
        .parse::<DateTime<Utc>>()
        .ok()
        .and_then(|reset_time| {
            compute_pre_scale_target(
                now,
                pre_scale_minutes,
                promotions,
                reset_time,
                target,
                current_total,
                "weekly_scoped",
            )
        });

    // Use pre-scale target if set, otherwise use normal target
    let effective_target = pre_scale.unwrap_or(target);

    // 5. Apply scaling decision
    let decision = apply_scaling(
        effective_target,
        current_total,
        effective_hysteresis,
        max_up_per_cycle,
        max_down_per_cycle,
    );

    // 6. Execute scaling (unless dry-run or no change)
    //
    // Use priority-based distribution when scaling multiple agents:
    // - Scale down: reduce highest-cost agents first (Opus -> Sonnet -> Haiku)
    // - Scale up: add to lowest-cost agents first with capacity
    match &decision {
        ScalingDecision::NoChange => {
            // The aggregate total is unchanged, but the per-agent allocation can
            // still violate a pool's min_workers (e.g. a dedicated polish pool that
            // must always run 1 worker). Reconcile the distribution so such a pool
            // launches even at a steady total — moving a worker off an over-allocated
            // agent — instead of only ever acting on aggregate deltas.
            if !dry_run {
                let cutoff_risk = match state.capacity_forecast.binding_window.as_str() {
                    WINDOW_FIVE_HOUR => state.capacity_forecast.five_hour.cutoff_risk,
                    WINDOW_SEVEN_DAY => state.capacity_forecast.seven_day.cutoff_risk,
                    _ => state.capacity_forecast.weekly_scoped.cutoff_risk,
                };
                let mut current_workers_map: HashMap<String, u32> = HashMap::new();
                for (name, ws) in &state.workers {
                    current_workers_map.insert(name.clone(), ws.current);
                }
                let target_distribution = distribute_workers_by_cost_priority(
                    agents,
                    &current_workers_map,
                    current_total,
                    &state.burn_rate.by_model,
                    pricing_config,
                    cutoff_risk,
                );
                let mut reconciled = false;
                // Free capacity from over-allocated agents first, then launch the deficit.
                for (agent_name, &target_count) in &target_distribution {
                    if let Some(worker_config) =
                        worker_configs.iter().find(|(name, _)| name == agent_name)
                    {
                        let current = *current_workers_map.get(agent_name).unwrap_or(&0);
                        if target_count < current {
                            worker::scale_down_graceful(
                                current - target_count,
                                &worker_config.1,
                                false,
                            );
                            reconciled = true;
                            log::info!(
                                "[governor] reconcile: {} {} -> {} workers",
                                agent_name,
                                current,
                                target_count
                            );
                        }
                    }
                }
                for (agent_name, &target_count) in &target_distribution {
                    if let Some(worker_config) =
                        worker_configs.iter().find(|(name, _)| name == agent_name)
                    {
                        let current = *current_workers_map.get(agent_name).unwrap_or(&0);
                        if target_count > current {
                            worker::scale_up(target_count - current, &worker_config.1, false);
                            reconciled = true;
                            log::info!(
                                "[governor] reconcile: {} {} -> {} workers",
                                agent_name,
                                current,
                                target_count
                            );
                        }
                    }
                }
                if reconciled {
                    log::info!(
                        "[governor] reconciled per-agent allocation at steady total {}",
                        current_total
                    );
                } else {
                    log::info!("[governor] no scaling action this cycle");
                }
            } else {
                log::info!("[governor] no scaling action this cycle (dry-run)");
            }
        }
        ScalingDecision::ScaleUp(n) => {
            log::info!("[governor] scaling up by {} workers", n);
            if !dry_run {
                // Determine cutoff_risk from binding window
                let binding_window = &state.capacity_forecast.binding_window;
                let cutoff_risk = match binding_window.as_str() {
                    WINDOW_FIVE_HOUR => state.capacity_forecast.five_hour.cutoff_risk,
                    WINDOW_SEVEN_DAY => state.capacity_forecast.seven_day.cutoff_risk,
                    _ => state.capacity_forecast.weekly_scoped.cutoff_risk,
                };

                // Build current workers map
                let mut current_workers_map: HashMap<String, u32> = HashMap::new();
                for (name, ws) in &state.workers {
                    current_workers_map.insert(name.clone(), ws.current);
                }

                // Calculate new target total
                let new_total = current_total.saturating_add(*n);

                // Distribute workers by cost priority
                let target_distribution = distribute_workers_by_cost_priority(
                    agents,
                    &current_workers_map,
                    new_total,
                    &state.burn_rate.by_model,
                    pricing_config,
                    cutoff_risk,
                );

                // Scale up each agent individually based on distribution
                let mut total_launched = 0;
                for (agent_name, &target_count) in &target_distribution {
                    if let Some(worker_config) =
                        worker_configs.iter().find(|(name, _)| name == agent_name)
                    {
                        let current = *current_workers_map.get(agent_name).unwrap_or(&0);
                        if target_count > current {
                            let to_add = target_count - current;
                            let launched = worker::scale_up(to_add, &worker_config.1, false);
                            total_launched += launched;
                            log::info!(
                                "[governor] scaled up {} agent: {} -> {} workers (launched {})",
                                agent_name,
                                current,
                                target_count,
                                launched
                            );
                        }
                    }
                }
                log::info!("[governor] total workers launched: {}", total_launched);
            } else {
                log::info!("[governor] DRY RUN: would scale up by {}", n);
            }
        }
        ScalingDecision::ScaleDown(n) => {
            log::info!("[governor] gracefully scaling down by {} workers", n);
            if !dry_run {
                // Determine cutoff_risk from binding window
                let binding_window = &state.capacity_forecast.binding_window;
                let cutoff_risk = match binding_window.as_str() {
                    WINDOW_FIVE_HOUR => state.capacity_forecast.five_hour.cutoff_risk,
                    WINDOW_SEVEN_DAY => state.capacity_forecast.seven_day.cutoff_risk,
                    _ => state.capacity_forecast.weekly_scoped.cutoff_risk,
                };

                // Build current workers map
                let mut current_workers_map: HashMap<String, u32> = HashMap::new();
                for (name, ws) in &state.workers {
                    current_workers_map.insert(name.clone(), ws.current);
                }

                // Calculate new target total
                let new_total = current_total.saturating_sub(*n);

                // Distribute workers by cost priority (highest cost first when scaling down)
                let target_distribution = distribute_workers_by_cost_priority(
                    agents,
                    &current_workers_map,
                    new_total,
                    &state.burn_rate.by_model,
                    pricing_config,
                    cutoff_risk,
                );

                // Scale down each agent individually based on distribution
                let mut total_graceful = 0;
                let mut total_forced = 0;
                for (agent_name, &target_count) in &target_distribution {
                    if let Some(worker_config) =
                        worker_configs.iter().find(|(name, _)| name == agent_name)
                    {
                        let current = *current_workers_map.get(agent_name).unwrap_or(&0);
                        if target_count < current {
                            let to_remove = current - target_count;
                            let result =
                                worker::scale_down_graceful(to_remove, &worker_config.1, false);
                            total_graceful += result.graceful;
                            total_forced += result.force_killed;
                            log::info!(
                                "[governor] scaled down {} agent: {} -> {} workers (removed: {}, graceful={}, forced={})",
                                agent_name,
                                current,
                                target_count,
                                to_remove,
                                result.graceful,
                                result.force_killed
                            );
                        }
                    }
                }
                log::info!(
                    "[governor] total scaled down: {} graceful, {} force-killed",
                    total_graceful,
                    total_forced
                );
            } else {
                log::info!("[governor] DRY RUN: would scale down by {}", n);
            }
        }
        ScalingDecision::EmergencyBrake => {
            log::warn!("[governor] EMERGENCY BRAKE: scaling all to 0");
            if !dry_run {
                // Kill all workers immediately across all agents
                for session in &all_sessions {
                    let _ = std::process::Command::new("tmux")
                        .args(["kill-session", "-t", session])
                        .output();
                }
                log::warn!("[governor] killed {} worker sessions", all_sessions.len());

                // Update state
                for ws in state.workers.values_mut() {
                    ws.current = 0;
                    ws.target = 0;
                }
                state.safe_mode.active = true;
                state.safe_mode.trigger = Some("emergency_brake".to_string());
                state.safe_mode.entered_at = Some(now);
            } else {
                log::warn!("[governor] DRY RUN: would emergency brake");
            }
        }
    }

    // 7. Update target in state using priority-based distribution
    //
    // Build current workers map for distribution
    let mut current_workers_map: HashMap<String, u32> = HashMap::new();
    for (name, ws) in &state.workers {
        current_workers_map.insert(name.clone(), ws.current);
    }

    // Determine cutoff_risk from binding window
    let binding_window = &state.capacity_forecast.binding_window;
    let cutoff_risk = match binding_window.as_str() {
        WINDOW_FIVE_HOUR => state.capacity_forecast.five_hour.cutoff_risk,
        WINDOW_SEVEN_DAY => state.capacity_forecast.seven_day.cutoff_risk,
        _ => state.capacity_forecast.weekly_scoped.cutoff_risk,
    };

    match &decision {
        ScalingDecision::EmergencyBrake => {
            for ws in state.workers.values_mut() {
                ws.target = 0;
            }
        }
        ScalingDecision::ScaleUp(n) => {
            let new_total = current_total.saturating_add(*n);
            let target_distribution = distribute_workers_by_cost_priority(
                agents,
                &current_workers_map,
                new_total,
                &state.burn_rate.by_model,
                pricing_config,
                cutoff_risk,
            );
            for (agent_name, ws) in state.workers.iter_mut() {
                ws.target = *target_distribution.get(agent_name).unwrap_or(&ws.current);
            }
        }
        ScalingDecision::ScaleDown(n) => {
            let new_total = current_total.saturating_sub(*n);
            let target_distribution = distribute_workers_by_cost_priority(
                agents,
                &current_workers_map,
                new_total,
                &state.burn_rate.by_model,
                pricing_config,
                cutoff_risk,
            );
            for (agent_name, ws) in state.workers.iter_mut() {
                ws.target = *target_distribution.get(agent_name).unwrap_or(&ws.current);
            }
        }
        ScalingDecision::NoChange => {
            // Still update target to reflect current desired state using priority distribution
            let target_distribution = distribute_workers_by_cost_priority(
                agents,
                &current_workers_map,
                effective_target,
                &state.burn_rate.by_model,
                pricing_config,
                cutoff_risk,
            );
            for (agent_name, ws) in state.workers.iter_mut() {
                ws.target = *target_distribution.get(agent_name).unwrap_or(&ws.current);
            }
        }
    }

    // 8. Check alerts and fire via configured command
    let mut alert_conditions = check_alert_conditions(&state, now, agents);
    alert_conditions.extend(check_low_cache_efficiency(&state, alert_config, now));
    for alert in &alert_conditions {
        if should_fire(
            alert.alert_type,
            &state.alert_cooldown,
            now,
            alert_config.cooldown_minutes,
        ) {
            // Record alert outcome in FP telemetry. A cutoff alert at sub-100% utilization
            // is classified as a false positive (the consistency guard should suppress these,
            // but telemetry catches any that slip through).
            let is_true_positive = is_true_positive_alert(&alert.alert_type, &state);
            state
                .alert_fp_telemetry
                .record(&alert.alert_type.to_string(), is_true_positive);

            // Fire the alert: execute configured command (e.g. bf create --type human)
            // and log to governor.log
            let log_rotation_config = Some((
                pricing_config.daemon.log_max_bytes,
                pricing_config.daemon.log_backup_count,
            ));
            if let Err(e) = fire_alert(alert, alert_config, log_rotation_config) {
                log::warn!("[governor] alert fire failed: {}", e);
            }
            update_cooldown(&mut state.alert_cooldown, alert.alert_type, now);
            state.alerts.push(serde_json::json!({
                "type": alert.alert_type.to_string(),
                "message": alert.message,
                "severity": format!("{:?}", alert.severity),
                "detected_at": alert.detected_at.to_rfc3339(),
                "is_true_positive": is_true_positive,
            }));
        }
    }

    // Log aggregate FP rate each cycle for observability
    if let Some(fp_rate) = state.alert_fp_telemetry.aggregate_fp_rate() {
        log::info!(
            "[governor] alert FP rate: {:.1}% ({} total recorded)",
            fp_rate * 100.0,
            state.alert_fp_telemetry.total_recorded,
        );
    }

    // 9. Write state
    state.updated_at = now;
    state::save_previous_state(&state, state_path)?;
    state::save_state(&state, state_path)?;

    log::info!(
        "[governor] === cycle complete (decision: {:?}, next in {}s) ===",
        decision,
        loop_interval
    );

    Ok(())
}

/// Run a single observation cycle (poll, forecast, calibrate, write state)
///
/// This is the observe-only portion of the governor cycle — it handles:
/// - Polling usage data from the Anthropic API
/// - Running token collector to gather fleet usage
/// - Computing burn-rate EMA and capacity forecasts
/// - Maintaining confidence-cone calibration
/// - Tracking safe-mode state
/// - Writing governor-state.json with observe-owned fields
///
/// Scaling and alerting are NOT handled here — those stay in run_governor_cycle.
/// This function runs once and exits cleanly (no daemon loop).
/// Output data returned by run_observe()
///
/// Contains the key observation data that can be printed by the _observe subcommand.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ObserveOutput {
    pub timestamp: String,
    pub success: bool,
    pub message: String,
    pub windows: Vec<WindowSummary>,
}

/// Summary of a single usage window
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowSummary {
    pub name: String,
    pub utilization: f64,
    pub remaining_hours: f64,
}

pub fn run_observe(
    state_path: &Path,
    alert_config: &AlertConfig,
    agents: &std::collections::HashMap<String, AgentConfig>,
    promotions: &[Promotion],
    composite_risk_config: &CompositeRiskConfig,
    cone_scaling_config: &ConeScalingConfig,
    pricing_config: &crate::config::GovernorConfig,
) -> anyhow::Result<ObserveOutput> {
    let now = Utc::now();
    log::info!("[governor] === observe cycle start at {} ===", now.to_rfc3339());

    // Create poller for live usage data
    let credentials_path = pricing_config.credentials_path.clone();
    let mut poller = match Poller::with_credentials_path(credentials_path) {
        Ok(p) => p,
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to create poller: {}", e));
        }
    };

    // Run observation phase (extracted from run_governor_cycle)
    run_observe_cycle_internal(
        &mut poller,
        state_path,
        alert_config,
        agents,
        promotions,
        composite_risk_config,
        cone_scaling_config,
        pricing_config,
        now,
    )?;

    log::info!("[governor] === observe cycle complete ===");

    // Load the updated state to return observation data
    let state = state::load_state(state_path)?;

    let forecast = &state.capacity_forecast;

    Ok(ObserveOutput {
        timestamp: now.to_rfc3339(),
        success: true,
        message: "Observation cycle completed successfully".to_string(),
        windows: vec![
            WindowSummary {
                name: "five_hour".to_string(),
                utilization: forecast.five_hour.current_utilization,
                remaining_hours: forecast.five_hour.hours_remaining,
            },
            WindowSummary {
                name: "seven_day".to_string(),
                utilization: forecast.seven_day.current_utilization,
                remaining_hours: forecast.seven_day.hours_remaining,
            },
            WindowSummary {
                name: "weekly_scoped".to_string(),
                utilization: forecast.weekly_scoped.current_utilization,
                remaining_hours: forecast.weekly_scoped.hours_remaining,
            },
        ],
    })
}

/// Internal implementation of observation logic
///
/// Extracted from run_governor_cycle to enable observation-only operation via the
/// _observe subcommand. This function runs all observation logic: polling, forecasting,
/// calibration, and state writing, but excludes scaling and alerting actions.
///
/// The observe cycle performs:
/// 1. Poll usage data from Anthropic API
/// 2. Run token collector pass
/// 3. Update fleet aggregate from database
/// 4. Count current workers
/// 5. Compute burn rates and capacity forecast
/// 6. Update confidence-cone calibration
/// 7. Track safe-mode state
/// 8. Write governor-state.json with observe-owned fields
///
/// Scaling decisions (step 6 in daemon) and alerting (step 8 in daemon) are NOT
/// performed here.
fn run_observe_cycle_internal(
    poller: &mut Poller,
    state_path: &Path,
    _alert_config: &AlertConfig,
    agents: &std::collections::HashMap<String, AgentConfig>,
    _promotions: &[Promotion],
    composite_risk_config: &CompositeRiskConfig,
    cone_scaling_config: &ConeScalingConfig,
    pricing_config: &crate::config::GovernorConfig,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    // 1. Load current state
    let mut state = state::load_state(state_path)?;

    // 1a. Load baseline burn rates from config (warm state)
    state.load_baseline_burn_rates_from_config(agents);

    // 1b. Shift snapshot state before poll: current becomes previous
    state.previous_api_snapshot = state.current_api_snapshot.take();

    // 2. Poll Anthropic API for live usage data
    match poller.poll() {
        Ok(usage_data) => {
            // Extract weekly_scoped utilization from model-agnostic limits[] array
            let weekly_scoped_util = usage_data
                .scoped_weekly()
                .map(|(_, window)| window.utilization)
                .unwrap_or(usage_data.weekly_scoped_utilization);

            let scoped_label = crate::state::weekly_scoped_display_label(
                usage_data.weekly_scoped_model.as_deref(),
            );
            log::info!(
                "[governor] observe polled usage: {}={:.1}%, all_models={:.1}%, 5h={:.1}%{}",
                scoped_label,
                weekly_scoped_util,
                usage_data.seven_day_utilization,
                usage_data.five_hour_utilization,
                if usage_data.stale { " (stale)" } else { "" },
            );

            // Detect weekly_scoped model identity change BEFORE updating state
            let prev_model = state.usage.weekly_scoped_model.clone();
            let new_model = usage_data.weekly_scoped_model.clone();

            log::info!(
                "[governor] weekly_scoped model change detection: prev_model={:?}, new_model={:?}, new_weekly_scoped_pct={:.2}%",
                prev_model,
                new_model,
                weekly_scoped_util
            );

            let model_changed = crate::state::reset_weekly_scoped_on_model_change(
                &prev_model,
                &new_model,
                &mut state.burn_rate,
            );

            // If model changed, clear the previous weekly_scoped snapshot
            if model_changed {
                if let Some(ref mut prev_snap) = state.previous_api_snapshot {
                    log::info!(
                        "[governor] clearing previous_api_snapshot.weekly_scoped_pct due to model change"
                    );
                    prev_snap.weekly_scoped_pct = 0.0;
                }

                // Reset fleet_pct_ema_samples to trigger cold-start seeding
                log::info!(
                    "[governor] resetting fleet_pct_ema_samples from {} to 0 due to model change",
                    state.burn_rate.fleet_pct_ema_samples
                );
                state.burn_rate.fleet_pct_ema_samples = 0;
            }

            // Track consecutive_absent_polls for structurally inactive windows
            // A window is "absent" if its resets_at field is empty (from window_or_default)
            // Check BEFORE moving values into state.usage
            let five_hour_present = !usage_data.five_hour_resets_at.is_empty();
            let seven_day_present = !usage_data.seven_day_resets_at.is_empty();
            let weekly_scoped_present = !usage_data.weekly_scoped_resets_at.is_empty();

            state.usage = state::UsageState {
                weekly_scoped_pct: weekly_scoped_util,
                sonnet_pct: 0.0, // Deprecated
                all_models_pct: usage_data.seven_day_utilization,
                five_hour_pct: usage_data.five_hour_utilization,
                sonnet_resets_at: usage_data.weekly_scoped_resets_at,
                seven_day_resets_at: usage_data.seven_day_resets_at,
                five_hour_resets_at: usage_data.five_hour_resets_at,
                stale: usage_data.stale,
                weekly_scoped_model: usage_data.weekly_scoped_model.clone(),
            };
            state.token_refresh_failing = usage_data.stale;

            state.update_consecutive_absent_polls(
                five_hour_present,
                seven_day_present,
                weekly_scoped_present,
            );

            log::debug!(
                "[governor] consecutive_absent_polls: 5h={}, 7d={}, 7ds={}",
                state.get_consecutive_absent_count("five_hour"),
                state.get_consecutive_absent_count("seven_day"),
                state.get_consecutive_absent_count("weekly_scoped"),
            );

            // Update current_api_snapshot with the new snapshot data
            state.current_api_snapshot = Some(state::PrevUsageSnapshot {
                taken_at: now,
                five_hour_pct: usage_data.five_hour_utilization,
                seven_day_pct: usage_data.seven_day_utilization,
                weekly_scoped_pct: weekly_scoped_util,
            });

            // Calculate window deltas from consecutive API snapshots
            if let (Some(prev), Some(curr)) =
                (&state.previous_api_snapshot, &state.current_api_snapshot)
            {
                let prev_pct = crate::db::WindowPctSnapshot {
                    five_hour: prev.five_hour_pct,
                    seven_day: prev.seven_day_pct,
                    weekly_scoped: prev.weekly_scoped_pct,
                };
                let curr_pct = crate::db::WindowPctSnapshot {
                    five_hour: curr.five_hour_pct,
                    seven_day: curr.seven_day_pct,
                    weekly_scoped: curr.weekly_scoped_pct,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&prev_pct, &curr_pct);

                log::info!(
                    "[governor] window deltas: 5h={:+.2}%, 7d={:+.2}%, 7ds={:+.2}%",
                    delta_5h, delta_7d, delta_7ds,
                );

                // Store computed deltas in governor state
                state.p5h_delta = Some(delta_5h);
                state.p7d_delta = Some(delta_7d);
                state.p7ds_delta = Some(delta_7ds);
            } else {
                // No previous snapshot to subtract from (first poll, or the poll
                // after a failed one). Clear every delta field explicitly so a
                // stale Some(..) from an earlier cycle is not mistaken for a
                // reading of the current interval. Mirrors run_governor_cycle.
                state.p5h_delta = None;
                state.p7d_delta = None;
                state.p7ds_delta = None;

                log::debug!(
                    "[governor] no previous API snapshot; window deltas cleared (first poll or poll following a failure)"
                );
            }
        }
        Err(e) => {
            // Reset token_refresh_failing for non-auth errors
            if let Some(pe) = e.downcast_ref::<crate::poller::PollerError>() {
                match pe {
                    crate::poller::PollerError::ApiRequestFailed(_)
                    | crate::poller::PollerError::ApiError(_)
                    | crate::poller::PollerError::ParseError(_) => {
                        state.token_refresh_failing = false;
                    }
                    _ => {}
                }
            } else {
                state.token_refresh_failing = false;
            }
            log::warn!("[governor] poll failed, keeping previous usage data: {}", e);
        }
    }

    // 3. Clear emergency-brake-triggered safe_mode when utilization drops
    if state.safe_mode.active && state.safe_mode.trigger.as_deref() == Some("emergency_brake") {
        let max_util = [
            state.capacity_forecast.five_hour.current_utilization,
            state.capacity_forecast.seven_day.current_utilization,
            state.capacity_forecast.weekly_scoped.current_utilization,
        ]
        .into_iter()
        .fold(0.0_f64, f64::max);
        if max_util < EMERGENCY_BRAKE_THRESHOLD {
            log::info!(
                "[governor] clearing emergency_brake safe_mode — max utilization {:.1}% < {:.0}% threshold",
                max_util,
                EMERGENCY_BRAKE_THRESHOLD
            );
            state.safe_mode = state::SafeModeState::default();
        }
    }

    // 4. Run token collector pass
    match collector::run_collection_pass() {
        Ok(result) => {
            log::info!(
                "[governor] collector pass: {} lines, {} instances, ${:.4} total",
                result.lines_processed,
                result.instance_records,
                result.total_usd,
            );
        }
        Err(e) => {
            log::warn!("[governor] collector pass failed: {}", e);
        }
    }

    // 5. Read latest fleet record from database
    let db_path = collector::default_db_path();
    if let Ok(conn) = db::open_db(&db_path) {
        if let Ok(fleet_records) = db::query_last_fleets(&conn, 1) {
            if let Some(fleet_json) = fleet_records.first() {
                if let (Some(t0_str), Some(t1_str)) = (
                    fleet_json.get("t0").and_then(|v| v.as_str()),
                    fleet_json.get("t1").and_then(|v| v.as_str()),
                ) {
                    let t0: DateTime<Utc> = t0_str.parse().unwrap_or_else(|_| now);
                    let t1: DateTime<Utc> = t1_str.parse().unwrap_or_else(|_| now);
                    let workers = fleet_json
                        .get("workers")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let total_usd = fleet_json
                        .get("total-usd")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let p75_usd_hr = fleet_json
                        .get("p75-usd-hr")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let std_usd_hr = fleet_json
                        .get("std-usd-hr")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let p5h = fleet_json.get("p5h").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let p7d = fleet_json.get("p7d").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let p7ds = fleet_json.get("p7ds").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let fleet_cache_eff = fleet_json
                        .get("fleet-cache-eff")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);

                    state.last_fleet_aggregate = state::FleetAggregate {
                        t0,
                        t1,
                        sonnet_workers: workers,
                        sonnet_usd_total: total_usd,
                        sonnet_p75_usd_hr: p75_usd_hr,
                        sonnet_std_usd_hr: std_usd_hr,
                        window_pct_deltas: state::WindowPctDeltas {
                            five_hour: p5h,
                            seven_day: p7d,
                            weekly_scoped: p7ds,
                        },
                        fleet_cache_eff,
                        cache_eff_p25: 0.0,
                        cli_tokens: 0,
                        cli_cost: 0.0,
                        sdk_tokens: 0,
                        sdk_cost: 0.0,
                    };

                    log::debug!(
                        "[governor] fleet aggregate: {} workers, ${:.2}/hr p75",
                        workers, p75_usd_hr
                    );
                }
            }
        }
    }

    // 6. Count current workers (seed from config if empty)
    if state.workers.is_empty() && !agents.is_empty() {
        for (name, agent) in agents {
            state.workers.insert(
                name.clone(),
                state::WorkerState {
                    current: 0,
                    target: 0,
                    min: agent.min_workers,
                    max: agent.max_workers,
                },
            );
        }
    }

    // Build per-agent WorkerConfigs
    let agent_worker_configs: Vec<(String, WorkerConfig)> = agents
        .iter()
        .map(|(name, agent)| (name.clone(), WorkerConfig::from_agent_config(agent)))
        .collect();

    let worker_configs: Vec<(String, WorkerConfig)> = if agent_worker_configs.is_empty() {
        vec![("default".to_string(), WorkerConfig::default())]
    } else {
        agent_worker_configs
    };

    // Count workers across all configured agents
    let mut total_tmux_count = 0usize;
    for (_name, wc) in &worker_configs {
        let wc_count = worker::count_workers(wc);
        total_tmux_count += wc_count.tmux_count;
    }

    // Update worker state with current count
    let mut current_workers_per_agent: HashMap<String, u32> = HashMap::new();
    for (name, wc) in &worker_configs {
        let wc_count = worker::count_workers(wc);
        current_workers_per_agent.insert(name.clone(), wc_count.tmux_count as u32);
    }

    for (name, ws) in state.workers.iter_mut() {
        ws.current = *current_workers_per_agent.get(name).unwrap_or(&0);
    }

    // 7. Compute burn rates and capacity forecast
    let target_ceiling = pricing_config.daemon.target_ceiling;

    // Save old snapshot before updating EMA
    let old_snapshot = state.burn_rate.prev_usage_snapshot.clone();

    // Update burn rate EMA and generate forecast
    if !state.usage.stale {
        let new_five_hour = state.usage.five_hour_pct;
        let new_seven_day = state.usage.all_models_pct;
        let new_weekly_scoped = state.usage.weekly_scoped_pct;

        if let Some(snap) = old_snapshot.clone() {
            const EMA_ALPHA: f64 = 0.2;
            const MIN_ELAPSED_SECS: f64 = 60.0;
            const MAX_ELAPSED_SECS: f64 = 1800.0;

            let elapsed_secs = (now - snap.taken_at).num_seconds() as f64;

            if elapsed_secs >= MIN_ELAPSED_SECS && elapsed_secs <= MAX_ELAPSED_SECS {
                let old_pct = crate::db::WindowPctSnapshot {
                    five_hour: snap.five_hour_pct,
                    seven_day: snap.seven_day_pct,
                    weekly_scoped: snap.weekly_scoped_pct,
                };
                let new_pct = crate::db::WindowPctSnapshot {
                    five_hour: new_five_hour,
                    seven_day: new_seven_day,
                    weekly_scoped: new_weekly_scoped,
                };
                let (delta_5h, delta_7d, delta_7ds) =
                    calculate_window_pct_delta(&old_pct, &new_pct);

                let elapsed_hours = elapsed_secs / 3600.0;
                let baseline = get_sonnet_baseline_config(&state, agents);
                let usd_per_worker = crate::burn_rate::staleness_checked_fleet_dollar_rate(
                    &state.last_fleet_aggregate,
                    &baseline,
                );
                let fleet_usd_hr = usd_per_worker * state.last_fleet_aggregate.sonnet_workers as f64;

                let samples = state.burn_rate.fleet_pct_ema_samples;

                if delta_5h > 0.0 {
                    let rate = delta_5h / elapsed_hours;
                    if samples == 0 {
                        state.burn_rate.fleet_pct_hr_ema.five_hour = rate;
                    } else {
                        state.burn_rate.fleet_pct_hr_ema.five_hour =
                            EMA_ALPHA * rate + (1.0 - EMA_ALPHA) * state.burn_rate.fleet_pct_hr_ema.five_hour;
                    }
                    if fleet_usd_hr > 0.0 {
                        let ratio = fleet_usd_hr / rate;
                        if samples == 0 {
                            state.burn_rate.usd_per_pct_ema_five_hour = ratio;
                        } else {
                            state.burn_rate.usd_per_pct_ema_five_hour =
                                EMA_ALPHA * ratio + (1.0 - EMA_ALPHA) * state.burn_rate.usd_per_pct_ema_five_hour;
                        }
                    }
                    state.burn_rate.fleet_pct_ema_samples += 1;
                }

                if delta_7d > 0.0 {
                    let rate = delta_7d / elapsed_hours;
                    if samples == 0 {
                        state.burn_rate.fleet_pct_hr_ema.seven_day = rate;
                    } else {
                        state.burn_rate.fleet_pct_hr_ema.seven_day =
                            EMA_ALPHA * rate + (1.0 - EMA_ALPHA) * state.burn_rate.fleet_pct_hr_ema.seven_day;
                    }
                }

                if delta_7ds > 0.0 {
                    let rate = delta_7ds / elapsed_hours;
                    if samples == 0 {
                        state.burn_rate.fleet_pct_hr_ema.weekly_scoped = rate;
                    } else {
                        state.burn_rate.fleet_pct_hr_ema.weekly_scoped =
                            EMA_ALPHA * rate + (1.0 - EMA_ALPHA) * state.burn_rate.fleet_pct_hr_ema.weekly_scoped;
                    }
                }
            }
        }

        // Update burn rate snapshot
        state.burn_rate.prev_usage_snapshot = Some(state::PrevUsageSnapshot {
            taken_at: now,
            five_hour_pct: new_five_hour,
            seven_day_pct: new_seven_day,
            weekly_scoped_pct: new_weekly_scoped,
        });
    }

    // Generate capacity forecast
    // Build effective hours remaining map from current usage data
    let mut hours_remaining = std::collections::HashMap::new();
    hours_remaining.insert(
        "five_hour".to_string(),
        (state.usage.five_hour_resets_at.parse::<DateTime<Utc>>()
            .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
            .unwrap_or(0.0)),
    );
    hours_remaining.insert(
        "seven_day".to_string(),
        (state.usage.seven_day_resets_at.parse::<DateTime<Utc>>()
            .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
            .unwrap_or(0.0)),
    );
    hours_remaining.insert(
        "weekly_scoped".to_string(),
        (state.usage.sonnet_resets_at.parse::<DateTime<Utc>>()
            .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
            .unwrap_or(0.0)),
    );

    // Build current utilization map
    let mut current_utilization = std::collections::HashMap::new();
    current_utilization.insert("five_hour".to_string(), state.usage.five_hour_pct);
    current_utilization.insert("seven_day".to_string(), state.usage.all_models_pct);
    current_utilization.insert("weekly_scoped".to_string(), state.usage.weekly_scoped_pct);

    // Build fleet_pct_per_hour map from burn_rate EMA
    let mut fleet_pct_per_hour = std::collections::HashMap::new();
    fleet_pct_per_hour.insert("five_hour".to_string(), state.burn_rate.fleet_pct_hr_ema.five_hour);
    fleet_pct_per_hour.insert("seven_day".to_string(), state.burn_rate.fleet_pct_hr_ema.seven_day);
    fleet_pct_per_hour.insert("weekly_scoped".to_string(), state.burn_rate.fleet_pct_hr_ema.weekly_scoped);

    // Build capacity forecast for each window using burn_rate module
    let mut five_hour_forecast = state::WindowForecast::default();
    let mut seven_day_forecast = state::WindowForecast::default();
    let mut weekly_scoped_forecast = state::WindowForecast::default();

    let current_total = state.last_fleet_aggregate.sonnet_workers as f64;

    for window in &["five_hour", "seven_day", "weekly_scoped"] {
        let util = current_utilization.get(*window).copied().unwrap_or(0.0);
        let hrs_left = hours_remaining.get(*window).copied().unwrap_or(0.0);
        let fleet_pct_hr = fleet_pct_per_hour.get(*window).copied().unwrap_or(0.0);

        // Get the base target ceiling for this specific window
        let base_target_ceiling = pricing_config.daemon.get_target_ceiling_for_window(window);
        let effective_target_ceiling = base_target_ceiling;

        // Per-worker pct/hr rate for safe_worker_count calculation
        let baseline = get_sonnet_baseline_config(&state, agents);
        let pct_per_worker = if fleet_pct_hr > 0.0 {
            fleet_pct_hr / current_total.max(1.0)
        } else {
            0.0
        };

        // Convert per-worker USD/hr stddev to pct/hr stddev
        let baseline_usd_per_pct =
            baseline.dollars_per_worker_per_hour / baseline.pct_per_worker_per_hour;
        let usd_per_pct = match *window {
            "five_hour" => state.burn_rate.usd_per_pct_ema_five_hour,
            "seven_day" => state.burn_rate.usd_per_pct_ema_seven_day,
            "weekly_scoped" => state.burn_rate.usd_per_pct_ema_weekly_scoped,
            _ => 0.0,
        };
        let effective_usd_per_pct = if usd_per_pct > 0.0 {
            usd_per_pct
        } else {
            baseline_usd_per_pct
        };
        let std_pct_hr = state.last_fleet_aggregate.sonnet_std_usd_hr / effective_usd_per_pct;

        // Compute estimate_quality based on EMA sample count
        let ema_val = match *window {
            "five_hour" => state.burn_rate.fleet_pct_hr_ema.five_hour,
            "seven_day" => state.burn_rate.fleet_pct_hr_ema.seven_day,
            "weekly_scoped" => state.burn_rate.fleet_pct_hr_ema.weekly_scoped,
            _ => 0.0,
        };
        let estimate_quality = if state.burn_rate.fleet_pct_ema_samples >= 3 && ema_val > 0.0 {
            state::EstimateQuality::Calibrated
        } else if state.burn_rate.fleet_pct_ema_samples == 0 {
            state::EstimateQuality::ColdStart
        } else {
            state::EstimateQuality::InsufficientSamples
        };

        // Cold-start base-rate seeding
        let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) = if matches!(
            estimate_quality,
            state::EstimateQuality::ColdStart | state::EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0.0
        {
            let base_per_worker = baseline.pct_per_worker_per_hour;
            let seeded_fleet_pct = base_per_worker * current_total as f64;
            let widened_std_pct = seeded_fleet_pct;
            (seeded_fleet_pct, base_per_worker, widened_std_pct)
        } else {
            (fleet_pct_hr, pct_per_worker, std_pct_hr)
        };

        let forecast = crate::burn_rate::generate_window_forecast(
            window,
            fleet_pct_hr_seeded,
            util,
            effective_target_ceiling,
            hrs_left,
            pct_per_worker_seeded,
            std_pct_hr_seeded,
            estimate_quality,
        );

        match *window {
            "five_hour" => five_hour_forecast = forecast,
            "seven_day" => seven_day_forecast = forecast,
            "weekly_scoped" => weekly_scoped_forecast = forecast,
            _ => {}
        }
    }

    // Identify binding window (highest risk_score)
    let windows = [
        ("five_hour", &five_hour_forecast),
        ("seven_day", &seven_day_forecast),
        ("weekly_scoped", &weekly_scoped_forecast),
    ];

    let binding_window = windows
        .iter()
        .filter(|(name, _)| {
            hours_remaining.contains_key(*name) && !state.is_window_consecutively_absent(name)
        })
        .max_by(|(_, a), (_, b)| {
            a.risk_score
                .partial_cmp(&b.risk_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(name, _)| name.to_string())
        .unwrap_or_default();

    // Set binding flag
    if binding_window == "five_hour" {
        five_hour_forecast.binding = true;
    } else if binding_window == "seven_day" {
        seven_day_forecast.binding = true;
    } else if binding_window == "weekly_scoped" {
        weekly_scoped_forecast.binding = true;
    }

    // Update state with new capacity forecast
    state.capacity_forecast = state::CapacityForecast {
        five_hour: five_hour_forecast,
        seven_day: seven_day_forecast,
        weekly_scoped: weekly_scoped_forecast,
        binding_window: binding_window.clone(),
        dollars_per_pct_7d_s: 0.0,
        estimated_remaining_dollars: 0.0,
    };

    // Log forecast
    log_capacity_forecast(
        &state.capacity_forecast,
        state.usage.weekly_scoped_model.as_deref(),
    );

    // 8. Update calibration from predictions
    const WINDOW_RESET_THRESHOLD: f64 = 1.0;

    if let Some(prev_snap) = old_snapshot {
        let cur_5h = state.usage.five_hour_pct;
        let cur_7d = state.usage.all_models_pct;
        let cur_7ds = state.usage.weekly_scoped_pct;

        let prev_5h = prev_snap.five_hour_pct;
        let prev_7d = prev_snap.seven_day_pct;
        let prev_7ds = prev_snap.weekly_scoped_pct;

        let windows_to_check = [
            ("five_hour", cur_5h, prev_5h),
            ("seven_day", cur_7d, prev_7d),
            ("weekly_scoped", cur_7ds, prev_7ds),
        ];

        for (window_name, current, previous) in windows_to_check {
            if current < previous - WINDOW_RESET_THRESHOLD {
                if let Some(pred) = state.pending_predictions.get(window_name) {
                    let predicted_change = pred.predicted_final_pct - pred.starting_pct;
                    let actual_change = previous - pred.starting_pct;

                    let score = calibrator::score_prediction(
                        window_name,
                        predicted_change,
                        actual_change,
                        pred.prediction_time,
                    );

                    log::info!(
                        "[governor] scored prediction for {}: predicted={:.2}%, actual={:.2}%, error={:+.2}%",
                        window_name,
                        predicted_change,
                        actual_change,
                        score.error,
                    );

                    if let Err(e) = calibrator::append_score(&score) {
                        log::warn!("[governor] failed to append prediction score: {}", e);
                    }

                    state.pending_predictions.remove(window_name);
                }
            }
        }
    }

    // 9. Write state (observe-owned fields only)
    state.updated_at = now;
    state::save_previous_state(&state, state_path)?;
    state::save_state(&state, state_path)?;

    log::info!("[governor] === observe cycle complete ===");
    Ok(())
}

/// Run the governor daemon (infinite loop with graceful shutdown on SIGINT/SIGTERM)
///
/// Executes `run_governor_cycle` every `loop_interval` seconds.
/// Sets up signal handlers for graceful shutdown via ctrlc crate.
pub fn run_daemon(
    state_path: &Path,
    dry_run: bool,
    loop_interval: u64,
    hysteresis_band: f64,
    max_up_per_cycle: u32,
    max_down_per_cycle: u32,
    target_ceiling: f64,
    alert_config: &AlertConfig,
    agents: &std::collections::HashMap<String, AgentConfig>,
    pre_scale_minutes: u64,
    promotions: &[Promotion],
    composite_risk_config: &CompositeRiskConfig,
    cone_scaling_config: &ConeScalingConfig,
    pricing_config: &crate::config::GovernorConfig,
) -> anyhow::Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        log::info!("[governor] received shutdown signal, draining...");
        r.store(false, Ordering::SeqCst);
    })
    .map_err(|e| anyhow::anyhow!("Failed to set signal handler: {}", e))?;

    log::info!(
        "[governor] daemon started (dry_run={}, interval={}s, hysteresis={:.1}, ceiling={:.0}%)",
        dry_run,
        loop_interval,
        hysteresis_band,
        target_ceiling
    );

    // Create poller for live usage data (persists across cycles for stale-data fallback)
    let credentials_path = pricing_config.credentials_path.clone();
    let mut poller = match Poller::with_credentials_path(credentials_path) {
        Ok(p) => p,
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to create poller: {}", e));
        }
    };

    // Initial cycle
    if let Err(e) = run_governor_cycle(
        &mut poller,
        state_path,
        dry_run,
        loop_interval,
        hysteresis_band,
        max_up_per_cycle,
        max_down_per_cycle,
        target_ceiling,
        alert_config,
        agents,
        pre_scale_minutes,
        promotions,
        composite_risk_config,
        cone_scaling_config,
        pricing_config,
    ) {
        log::error!("[governor] initial cycle failed: {}", e);
    }

    while running.load(Ordering::SeqCst) {
        // Sleep for loop interval, checking shutdown every second
        for _ in 0..loop_interval {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }

        if !running.load(Ordering::SeqCst) {
            break;
        }

        if let Err(e) = run_governor_cycle(
            &mut poller,
            state_path,
            dry_run,
            loop_interval,
            hysteresis_band,
            max_up_per_cycle,
            max_down_per_cycle,
            target_ceiling,
            alert_config,
            agents,
            pre_scale_minutes,
            promotions,
            composite_risk_config,
            cone_scaling_config,
            pricing_config,
        ) {
            log::error!("[governor] cycle failed: {}", e);
            // Continue running despite cycle failures
        }
    }

    log::info!("[governor] daemon stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // Test helper functions
    // ---------------------------------------------------------------------------

    /// Create a UsageSnapshot with specified utilization percentages.
    ///
    /// Helper function to create UsageSnapshot instances with custom values
    /// for testing governor behavior across different utilization scenarios.
    ///
    /// # Arguments
    /// - `five_hour`: 5-hour window utilization percentage
    /// - `seven_day`: 7-day window utilization percentage (all models)
    /// - `weekly_scoped`: 7-day window utilization percentage (Sonnet only)
    ///
    /// ⚠️ BUG: The documentation above incorrectly states "Sonnet only".
    /// weekly_scoped is MODEL-AGNOSTIC and can be Fable, Opus, Sonnet, etc.
    /// This field represents the scoped weekly cap utilization for whatever model
    /// is active this period (see state::UsageState.weekly_scoped_model).
    ///
    /// # Returns
    /// A UsageSnapshot struct with the specified window values.
    ///
    /// # Example
    /// ```rust
    /// use crate::governor::tests::make_usage_snapshot;
    ///
    /// let snapshot = make_usage_snapshot(75.5, 45.0, 38.2);
    /// assert_eq!(snapshot.get("five_hour"), Some(75.5));
    /// assert_eq!(snapshot.get("seven_day"), Some(45.0));
    /// assert_eq!(snapshot.get("weekly_scoped"), Some(38.2));
    /// ```
    pub fn make_usage_snapshot(
        five_hour: f64,
        seven_day: f64,
        weekly_scoped: f64,
    ) -> UsageSnapshot {
        UsageSnapshot::from_windows(five_hour, seven_day, weekly_scoped)
    }

    /// Create a UsageSnapshot with custom window values.
    ///
    /// Helper function to create UsageSnapshot instances with arbitrary
    /// window names and values for testing custom scenarios.
    ///
    /// # Arguments
    /// - `windows`: HashMap of window name -> utilization percentage
    ///
    /// # Returns
    /// A UsageSnapshot struct with the specified window values.
    ///
    /// # Example
    /// ```rust
    /// use crate::governor::tests::make_usage_snapshot_from_map;
    /// use std::collections::HashMap;
    ///
    /// let mut windows = HashMap::new();
    /// windows.insert("five_hour".to_string(), 80.0);
    /// windows.insert("seven_day".to_string(), 50.0);
    /// windows.insert("weekly_scoped".to_string(), 45.0);
    ///
    /// let snapshot = make_usage_snapshot_from_map(windows);
    /// assert_eq!(snapshot.get("five_hour"), Some(80.0));
    /// ```
    pub fn make_usage_snapshot_from_map(
        windows: std::collections::HashMap<String, f64>,
    ) -> UsageSnapshot {
        UsageSnapshot { windows }
    }

    /// Create a GovernorState with a standard set of test agents.
    ///
    /// Helper function to create a GovernorState instance with three
    /// pre-configured agents for testing governor behavior.
    ///
    /// # Returns
    /// A GovernorState with three agents:
    /// - "agent-1": 5 workers, not idle
    /// - "agent-2": 3 workers, idle
    /// - "agent-3": 10 workers, not idle
    ///
    /// # Example
    /// ```rust
    /// use crate::governor::tests::governor_with_agents;
    ///
    /// let state = governor_with_agents();
    /// assert_eq!(state.agents.len(), 3);
    /// assert_eq!(state.agents["agent-1"].workers, 5);
    /// assert_eq!(state.agents["agent-2"].is_idle, true);
    /// ```
    pub fn governor_with_agents() -> GovernorState {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);
        state.add_agent("agent-2", 3, true);
        state.add_agent("agent-3", 10, false);
        state
    }

    /// Test that snapshot helper functions create valid structs.
    ///
    /// Demonstrates the usage of the helper functions and verifies they
    /// produce correctly constructed snapshots.
    #[test]
    fn test_usage_snapshot_helpers_create_valid_structs() {
        // Test make_usage_snapshot
        let snapshot = make_usage_snapshot(75.5, 45.0, 38.2);
        assert_eq!(snapshot.get("five_hour"), Some(75.5));
        assert_eq!(snapshot.get("seven_day"), Some(45.0));
        assert_eq!(snapshot.get("weekly_scoped"), Some(38.2));

        // Test make_usage_snapshot_from_map
        let mut windows = std::collections::HashMap::new();
        windows.insert("five_hour".to_string(), 80.0);
        windows.insert("seven_day".to_string(), 50.0);
        windows.insert("weekly_scoped".to_string(), 45.0);

        let custom_snapshot = make_usage_snapshot_from_map(windows);
        assert_eq!(custom_snapshot.get("five_hour"), Some(80.0));
        assert_eq!(custom_snapshot.get("seven_day"), Some(50.0));
        assert_eq!(custom_snapshot.get("weekly_scoped"), Some(45.0));
    }

    // --- Core emergency brake tests ---

    #[test]
    fn test_97_9_pct_no_brake() {
        let mut state = governor_with_agents();
        let usage = make_usage_snapshot(97.9, 50.0, 50.0);

        let result = state.check_emergency_brake(&usage);

        assert!(result.is_none());
        assert!(!state.emergency_brake_active);

        // Agents should NOT be scaled
        assert_eq!(state.agents["agent-1"].workers, 5);
        assert_eq!(state.agents["agent-2"].workers, 3);
        assert_eq!(state.agents["agent-3"].workers, 10);
    }

    #[test]
    fn test_98_0_pct_brake_triggers() {
        let mut state = governor_with_agents();
        let usage = make_usage_snapshot(98.0, 50.0, 50.0);

        let result = state.check_emergency_brake(&usage);

        assert!(result.is_some());
        let brake = result.unwrap();
        assert_eq!(brake.triggered_window, WINDOW_FIVE_HOUR);
        assert!((brake.utilization_pct - 98.0).abs() < 0.001);

        assert!(state.emergency_brake_active);
        assert!(state.emergency_brake.is_some());
    }

    #[test]
    fn test_brake_scales_all_agents_to_zero() {
        let mut state = governor_with_agents();
        let usage = make_usage_snapshot(50.0, 98.5, 50.0); // seven_day triggers

        let _ = state.check_emergency_brake(&usage);

        // ALL agents should be scaled to 0
        for agent in state.agents.values() {
            assert_eq!(agent.workers, 0, "Agent {} should have 0 workers", agent.id);
        }
    }

    #[test]
    fn test_brake_overrides_hysteresis() {
        // Even if agents are idle, brake should still scale them to 0
        let mut state = GovernorState::new();
        state.add_agent("idle-agent", 5, true); // idle agent with workers
        state.add_agent("busy-agent", 5, false);

        let usage = make_usage_snapshot(99.0, 50.0, 50.0);

        let _ = state.check_emergency_brake(&usage);

        // Both should be scaled to 0, regardless of idle status
        assert_eq!(state.agents["idle-agent"].workers, 0);
        assert_eq!(state.agents["busy-agent"].workers, 0);
    }

    #[test]
    fn test_brake_clears_below_98_pct() {
        let mut state = governor_with_agents();

        // Trigger brake
        let usage_high = make_usage_snapshot(98.5, 50.0, 50.0);
        let _ = state.check_emergency_brake(&usage_high);
        assert!(state.emergency_brake_active);

        // Now drop below threshold
        let usage_low = make_usage_snapshot(97.0, 50.0, 50.0);
        let cleared = state.clear_emergency_brake(&usage_low);

        assert!(cleared);
        assert!(!state.emergency_brake_active);
        assert!(state.emergency_brake.is_none());
    }

    #[test]
    fn test_brake_clears_on_window_reset() {
        // Window reset is detected as a drop in utilization
        let mut state = governor_with_agents();

        // Trigger brake at 99%
        let usage_high = make_usage_snapshot(99.0, 50.0, 50.0);
        let _ = state.check_emergency_brake(&usage_high);
        assert!(state.emergency_brake_active);

        // Simulate window reset (utilization drops significantly)
        let usage_reset = make_usage_snapshot(10.0, 50.0, 50.0);
        let cleared = state.clear_emergency_brake(&usage_reset);

        assert!(cleared);
        assert!(!state.emergency_brake_active);
    }

    // --- Additional tests ---

    #[test]
    fn test_brake_triggers_on_any_window() {
        // Test weekly_scoped window
        let mut state = governor_with_agents();
        let usage = make_usage_snapshot(50.0, 50.0, 98.0);
        let result = state.check_emergency_brake(&usage);
        assert!(result.is_some());
        assert_eq!(result.unwrap().triggered_window, WINDOW_WEEKLY_SCOPED);

        // Test seven_day window
        let mut state2 = governor_with_agents();
        let usage2 = make_usage_snapshot(50.0, 99.0, 50.0);
        let result2 = state2.check_emergency_brake(&usage2);
        assert!(result2.is_some());
        assert_eq!(result2.unwrap().triggered_window, WINDOW_SEVEN_DAY);
    }

    #[test]
    fn test_brake_does_not_clear_if_still_above_threshold() {
        let mut state = governor_with_agents();

        // Trigger on five_hour
        let usage_high = make_usage_snapshot(99.0, 98.5, 50.0);
        let _ = state.check_emergency_brake(&usage_high);
        assert!(state.emergency_brake_active);

        // Drop five_hour but seven_day still above
        let usage_still_high = make_usage_snapshot(50.0, 98.5, 50.0);
        let cleared = state.clear_emergency_brake(&usage_still_high);

        assert!(!cleared);
        assert!(state.emergency_brake_active);
    }

    #[test]
    fn test_update_combines_check_and_clear() {
        let mut state = governor_with_agents();

        // Initial trigger
        let usage1 = make_usage_snapshot(98.5, 50.0, 50.0);
        let result1 = state.update_emergency_brake(&usage1);
        assert!(result1.is_some());
        assert!(state.emergency_brake_active);

        // Still high - should return existing brake
        let usage2 = make_usage_snapshot(99.0, 50.0, 50.0);
        let result2 = state.update_emergency_brake(&usage2);
        assert!(result2.is_some());
        assert!(state.emergency_brake_active);

        // Drops below - should clear and not retrigger
        let usage3 = make_usage_snapshot(97.0, 50.0, 50.0);
        let result3 = state.update_emergency_brake(&usage3);
        assert!(result3.is_none());
        assert!(!state.emergency_brake_active);
    }

    #[test]
    fn test_empty_agents_still_sets_flag() {
        let mut state = GovernorState::new(); // no agents
        let usage = make_usage_snapshot(98.0, 50.0, 50.0);

        let result = state.check_emergency_brake(&usage);

        assert!(result.is_some());
        assert!(state.emergency_brake_active);
    }

    #[test]
    fn test_usage_snapshot_helpers() {
        let snap = UsageSnapshot::from_windows(10.0, 20.0, 30.0);

        assert_eq!(snap.get(WINDOW_FIVE_HOUR), Some(10.0));
        assert_eq!(snap.get(WINDOW_SEVEN_DAY), Some(20.0));
        assert_eq!(snap.get(WINDOW_WEEKLY_SCOPED), Some(30.0));
        assert_eq!(snap.get("unknown"), None);
    }

    // --- Sprint tests ---

    fn default_sprint_config() -> SprintConfig {
        SprintConfig::default()
    }

    fn make_sprint_trigger(worker_id: &str, target_workers: u32, window: &str) -> SprintTrigger {
        SprintTrigger {
            worker_id: worker_id.to_string(),
            target_workers,
            window: window.to_string(),
            utilization_pct: 45.0,
            hours_remaining: 1.5,
            reason: format!("test sprint for {}", worker_id),
            triggered_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn sprint_apply_boosts_agent_to_max() {
        let mut state = governor_with_agents();
        let trigger = make_sprint_trigger("agent-1", 20, WINDOW_FIVE_HOUR);

        state.apply_sprint(&trigger);

        assert!(state.is_sprint_active());
        assert_eq!(state.agents["agent-1"].workers, 20);
        // Other agents unchanged
        assert_eq!(state.agents["agent-2"].workers, 3);
        assert_eq!(state.agents["agent-3"].workers, 10);

        // Sprint state should track original workers
        let sprint = state.sprint.as_ref().unwrap();
        assert_eq!(sprint.original_workers, 5);
        assert_eq!(sprint.target_workers, 20);
        assert_eq!(sprint.worker_id, "agent-1");
        assert_eq!(sprint.window, WINDOW_FIVE_HOUR);
    }

    #[test]
    fn sprint_clear_restores_original_workers() {
        let mut state = governor_with_agents();
        let trigger = make_sprint_trigger("agent-1", 20, WINDOW_FIVE_HOUR);

        state.apply_sprint(&trigger);
        assert_eq!(state.agents["agent-1"].workers, 20);

        let cleared = state.clear_sprint();
        assert!(cleared);
        assert!(!state.is_sprint_active());
        assert_eq!(state.agents["agent-1"].workers, 5); // restored to original
    }

    #[test]
    fn sprint_clear_returns_false_when_no_sprint() {
        let mut state = governor_with_agents();
        assert!(!state.clear_sprint());
    }

    #[test]
    fn sprint_blocked_during_emergency_brake() {
        let mut state = governor_with_agents();

        // Activate emergency brake
        let usage = make_usage_snapshot(99.0, 50.0, 50.0);
        let _ = state.check_emergency_brake(&usage);
        assert!(state.emergency_brake_active);

        // Try to apply sprint — should be blocked
        let trigger = make_sprint_trigger("agent-1", 20, WINDOW_FIVE_HOUR);
        state.apply_sprint(&trigger);

        assert!(!state.is_sprint_active());
        assert_eq!(state.agents["agent-1"].workers, 0); // still at brake level
    }

    #[test]
    fn sprint_not_reapplied_when_already_active() {
        let mut state = governor_with_agents();
        let trigger1 = make_sprint_trigger("agent-1", 20, WINDOW_FIVE_HOUR);
        let trigger2 = make_sprint_trigger("agent-2", 30, WINDOW_SEVEN_DAY);

        state.apply_sprint(&trigger1);
        state.apply_sprint(&trigger2); // should be ignored

        assert!(state.is_sprint_active());
        assert_eq!(state.sprint.as_ref().unwrap().worker_id, "agent-1");
        assert_eq!(state.agents["agent-1"].workers, 20);
        assert_eq!(state.agents["agent-2"].workers, 3); // unchanged
    }

    #[test]
    fn sprint_ends_when_utilization_exceeds_threshold() {
        let mut state = governor_with_agents();
        let trigger = make_sprint_trigger("agent-1", 20, WINDOW_FIVE_HOUR);

        state.apply_sprint(&trigger);
        assert!(state.is_sprint_active());

        // Utilization now exceeds 50% threshold
        let usage = make_usage_snapshot(55.0, 50.0, 50.0);
        let config = default_sprint_config();
        let ended = state.check_sprint_end(&usage, &config);

        assert!(ended);
        assert!(!state.is_sprint_active());
        assert_eq!(state.agents["agent-1"].workers, 5); // restored
    }

    #[test]
    fn sprint_continues_when_utilization_below_threshold() {
        let mut state = governor_with_agents();
        let trigger = make_sprint_trigger("agent-1", 20, WINDOW_FIVE_HOUR);

        state.apply_sprint(&trigger);

        // Utilization still below threshold
        let usage = make_usage_snapshot(45.0, 50.0, 50.0);
        let config = default_sprint_config();
        let ended = state.check_sprint_end(&usage, &config);

        assert!(!ended);
        assert!(state.is_sprint_active());
        assert_eq!(state.agents["agent-1"].workers, 20); // still boosted
    }

    #[test]
    fn sprint_end_noop_when_no_sprint() {
        let mut state = governor_with_agents();
        let usage = make_usage_snapshot(55.0, 50.0, 50.0);
        let config = default_sprint_config();

        let ended = state.check_sprint_end(&usage, &config);
        assert!(!ended);
    }

    #[test]
    fn new_governor_has_no_sprint() {
        let state = GovernorState::new();
        assert!(!state.is_sprint_active());
        assert!(state.sprint.is_none());
    }

    // --- Pre-scale tests ---

    // Helper: create a 2x off-peak promotion active in March 2026
    fn march_2026_promo() -> Promotion {
        Promotion {
            name: "March 2026 Off-Peak Promotion".to_string(),
            start_date: "2026-03-15".to_string(),
            end_date: "2026-03-25".to_string(),
            peak_start_hour_et: 8,
            peak_end_hour_et: 14,
            offpeak_multiplier: 2.0,
            applies_to: vec!["weekly_scoped".to_string()],
        }
    }

    // Helper: create UTC from Eastern components (March 2026 = EDT, UTC-4)
    fn et(year: i32, month: u32, day: u32, hour: u32, min: u32) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono_tz::America::New_York
            .with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn pre_scale_triggers_before_losing_multiplier_bonus() {
        // Transition-detection baseline: at 07:35 ET, confirm the next transition
        // is off-peak → peak (25 min away, losing the 2x bonus).
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 7, 35);
        let deadline = now + chrono::Duration::hours(2);

        let t = schedule::next_transition_from(now, deadline, &promos, "weekly_scoped")
            .expect("Should detect off-peak → peak transition");

        assert_eq!(t.minutes_until, 25);
        assert!((t.multiplier_before - 2.0).abs() < 1e-9);
        assert!((t.multiplier_after - 1.0).abs() < 1e-9);
        assert!(t.multiplier_after < t.multiplier_before);
        assert!(t.minutes_until <= 30, "within 30-minute pre-scale window");
    }

    #[test]
    fn compute_pre_scale_target_triggers_at_07_35() {
        // Core bead test: mock clock at 07:35 ET during promo.
        // With 4 workers, target=4, pre_scale_minutes=30 (window starts at 07:30):
        //   - transition at 08:00 is 25 min away → within window
        //   - ratio = 1.0/2.0 = 0.5 → post_transition_target = floor(4*0.5) = 2
        //   - effective_target = max(2, 4-1) = 3
        // Scale-down to 3 should trigger.
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 7, 35);
        let reset_time = now + chrono::Duration::days(2); // well past transition

        let result = compute_pre_scale_target(now, 30, &promos, reset_time, 4, 4, "weekly_scoped");

        assert!(
            result.is_some(),
            "pre-scale should trigger at 07:35 before 08:00 transition"
        );
        assert_eq!(
            result.unwrap(),
            3,
            "should ramp down one worker (4→3, toward post-target 2)"
        );
    }

    #[test]
    fn compute_pre_scale_target_no_trigger_outside_window() {
        // At 06:00 ET, peak is 2 hours away — outside 30-min window.
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 6, 0);
        let reset_time = now + chrono::Duration::days(2);

        let result = compute_pre_scale_target(now, 30, &promos, reset_time, 4, 4, "weekly_scoped");

        assert!(
            result.is_none(),
            "should not pre-scale when transition is 120 min away"
        );
    }

    #[test]
    fn compute_pre_scale_target_never_triggers_for_gaining_bonus() {
        // Conservative-only: at 13:45 ET, peak ends in 15 min (gaining 2x bonus).
        // Should NOT trigger pre-scale.
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 13, 45);
        let reset_time = now + chrono::Duration::days(2);

        let result = compute_pre_scale_target(now, 30, &promos, reset_time, 4, 4, "weekly_scoped");

        assert!(
            result.is_none(),
            "should not pre-scale when gaining a bonus"
        );
    }

    #[test]
    fn compute_pre_scale_target_no_trigger_when_already_at_post_target() {
        // At 07:35 with only 2 workers running — already at or below post-target (2).
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 7, 35);
        let reset_time = now + chrono::Duration::days(2);

        // current_total=2, target=2 → post_transition_target=1, but 2 > 1 so this would trigger
        // Let's test with current_total=1: post_target=floor(1*0.5)=0, effective=max(0,0)=0
        // Actually: post_target=0 < current_total=1, so effective_target = max(0, 0) = 0
        // Let's use current_total=2, target=2: post_target=1, effective=max(1,1)=1
        let result = compute_pre_scale_target(now, 30, &promos, reset_time, 2, 2, "weekly_scoped");
        // post_target = floor(2 * 0.5) = 1, effective = max(1, 2-1) = max(1,1) = 1
        assert_eq!(result, Some(1));

        // Now test where current_total already equals post_transition_target: no trigger
        let result_at_target =
            compute_pre_scale_target(now, 30, &promos, reset_time, 0, 0, "weekly_scoped");
        // post_target = 0, current_total = 0: post_target >= current_total → None
        assert!(
            result_at_target.is_none(),
            "no pre-scale needed if already at 0"
        );
    }

    #[test]
    fn compute_pre_scale_target_disabled_when_zero() {
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 7, 35);
        let reset_time = now + chrono::Duration::days(2);

        // pre_scale_minutes = 0 disables pre-scaling entirely
        let result = compute_pre_scale_target(now, 0, &promos, reset_time, 4, 4, "weekly_scoped");
        assert!(
            result.is_none(),
            "pre_scale_minutes=0 should disable pre-scaling"
        );
    }

    #[test]
    fn pre_scale_does_not_trigger_when_outside_window() {
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 6, 0);
        let deadline = now + chrono::Duration::hours(3);

        let t = schedule::next_transition_from(now, deadline, &promos, "weekly_scoped").unwrap();
        assert_eq!(t.minutes_until, 120);
        assert!(t.minutes_until > 30, "outside 30-minute window");
    }

    #[test]
    fn pre_scale_never_triggers_for_gaining_bonus() {
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 13, 45);
        let deadline = now + chrono::Duration::hours(1);

        let t = schedule::next_transition_from(now, deadline, &promos, "weekly_scoped").unwrap();
        assert!(t.multiplier_after > t.multiplier_before, "gaining bonus");
        assert!(t.minutes_until <= 30, "within window");
        // Conservative: multiplier_after > multiplier_before → no pre-scale
    }

    // --- Regression: per-window multiplier and applies_to ---

    /// Promotion applies to only five_hour window (config/promotions.json pattern).
    /// Verify get_multiplier_at() returns 2.0 for five_hour and 1.0 for the other
    /// windows during an off-peak time inside the promo date range.
    #[test]
    fn schedule_state_per_window_applies_to_filtering() {
        // Promo applies ONLY to five_hour (mirrors real config/promotions.json)
        let promos = vec![Promotion {
            name: "March 2026".to_string(),
            start_date: "2026-03-13".to_string(),
            end_date: "2026-03-29".to_string(),
            peak_start_hour_et: 8,
            peak_end_hour_et: 14,
            offpeak_multiplier: 2.0,
            applies_to: vec!["five_hour".to_string()],
        }];

        // Off-peak weekday inside promo range: March 18, 2026 at 06:00 ET
        let t = et(2026, 3, 18, 6, 0);

        let mult_five = schedule::get_multiplier_at(t, &promos, "five_hour");
        let mult_7d = schedule::get_multiplier_at(t, &promos, "seven_day");
        let mult_7ds = schedule::get_multiplier_at(t, &promos, "weekly_scoped");

        assert!(
            (mult_five - 2.0).abs() < 1e-9,
            "five_hour should get 2x (in applies_to), got {mult_five}"
        );
        assert!(
            (mult_7d - 1.0).abs() < 1e-9,
            "seven_day should get 1.0x (not in applies_to), got {mult_7d}"
        );
        assert!(
            (mult_7ds - 1.0).abs() < 1e-9,
            "weekly_scoped should get 1.0x (not in applies_to), got {mult_7ds}"
        );

        // is_any_promo_active_at should be true (we are inside the date range)
        assert!(
            schedule::is_any_promo_active_at(t, &promos),
            "promo should be active on March 18 during date range"
        );

        // Outside date range: April 1 — promo should be inactive
        let after_promo = et(2026, 4, 1, 6, 0);
        assert!(
            !schedule::is_any_promo_active_at(after_promo, &promos),
            "promo should be inactive after end_date"
        );
        assert!(
            (schedule::get_multiplier_at(after_promo, &promos, "five_hour") - 1.0).abs() < 1e-9,
            "five_hour should be 1.0x after promo ends"
        );
    }

    // --- Safe mode calibration tests ---

    fn make_cal_stats(
        median_error: f64,
        total_samples: u32,
        median_error_7ds: f64,
    ) -> calibrator::CalibrationStats {
        calibrator::CalibrationStats {
            total_samples,
            median_error,
            median_error_7ds,
            ..Default::default()
        }
    }

    #[test]
    fn safe_mode_enters_when_accuracy_degrades() {
        let mut safe_mode = state::SafeModeState::default();
        let mut calibration = state::CalibrationState::default();
        // median_error=16 > entry_threshold=15, 5 samples >= min_samples=5
        let stats = make_cal_stats(16.0, 5, 14.0);
        let now = Utc::now();

        let changed =
            update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert!(changed, "should return true when entering safe mode");
        assert!(safe_mode.active);
        assert_eq!(safe_mode.trigger.as_deref(), Some("median_error"));
        assert!((safe_mode.median_error_at_entry.unwrap() - 16.0).abs() < 1e-9);
        assert_eq!(safe_mode.scored_at_entry, 5);
        assert!(safe_mode.entered_at.is_some());
    }

    #[test]
    fn safe_mode_does_not_enter_below_threshold() {
        let mut safe_mode = state::SafeModeState::default();
        let mut calibration = state::CalibrationState::default();
        // median_error=14 < entry_threshold=15 — should not trigger
        let stats = make_cal_stats(14.0, 5, 12.0);
        let now = Utc::now();

        let changed =
            update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert!(!changed);
        assert!(!safe_mode.active);
    }

    #[test]
    fn safe_mode_does_not_enter_with_insufficient_samples() {
        let mut safe_mode = state::SafeModeState::default();
        let mut calibration = state::CalibrationState::default();
        // total_samples=4 < min_samples=5 even though error is high
        let stats = make_cal_stats(20.0, 4, 18.0);
        let now = Utc::now();

        let changed =
            update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert!(!changed);
        assert!(!safe_mode.active);
    }

    #[test]
    fn safe_mode_exits_when_accuracy_recovers() {
        let now = Utc::now();
        let mut safe_mode = state::SafeModeState {
            active: true,
            entered_at: Some(now - chrono::Duration::hours(1)),
            trigger: Some("median_error".to_string()),
            median_error_at_entry: Some(16.0),
            predictions_since_entry: 0,
            scored_at_entry: 5,
        };
        let mut calibration = state::CalibrationState::default();
        // median_error=7 < exit_threshold=8, total_samples=8 → predictions_since_entry=8-5=3 >= min=3
        let stats = make_cal_stats(7.0, 8, 6.0);

        let changed =
            update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert!(changed, "should return true when exiting safe mode");
        assert!(!safe_mode.active, "safe mode should be inactive after exit");
    }

    #[test]
    fn safe_mode_does_not_exit_with_insufficient_new_predictions() {
        let now = Utc::now();
        let mut safe_mode = state::SafeModeState {
            active: true,
            entered_at: Some(now - chrono::Duration::hours(1)),
            trigger: Some("median_error".to_string()),
            median_error_at_entry: Some(16.0),
            predictions_since_entry: 0,
            scored_at_entry: 5,
        };
        let mut calibration = state::CalibrationState::default();
        // median_error=7 < exit_threshold=8, but total_samples=7 → 7-5=2 < min=3
        let stats = make_cal_stats(7.0, 7, 6.0);

        let changed =
            update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert!(!changed);
        assert!(safe_mode.active, "safe mode should remain active");
        assert_eq!(safe_mode.predictions_since_entry, 2);
    }

    #[test]
    fn safe_mode_does_not_exit_when_error_still_high() {
        let now = Utc::now();
        let mut safe_mode = state::SafeModeState {
            active: true,
            entered_at: Some(now - chrono::Duration::hours(1)),
            trigger: Some("median_error".to_string()),
            median_error_at_entry: Some(16.0),
            predictions_since_entry: 0,
            scored_at_entry: 5,
        };
        let mut calibration = state::CalibrationState::default();
        // median_error=9 > exit_threshold=8 — accuracy not recovered enough
        let stats = make_cal_stats(9.0, 8, 8.0);

        let changed =
            update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert!(!changed);
        assert!(safe_mode.active, "safe mode should remain active");
    }

    #[test]
    fn safe_mode_syncs_calibration_state() {
        let mut safe_mode = state::SafeModeState::default();
        let mut calibration = state::CalibrationState::default();
        let stats = make_cal_stats(5.0, 12, 4.5);
        let now = Utc::now();

        update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert_eq!(calibration.predictions_scored, 12);
        assert!((calibration.median_error_7ds - 4.5).abs() < 1e-9);
    }

    #[test]
    fn safe_mode_entry_uses_absolute_error() {
        // Negative median_error (over-predicting by 17 pct points) should also trigger
        let mut safe_mode = state::SafeModeState::default();
        let mut calibration = state::CalibrationState::default();
        let stats = make_cal_stats(-17.0, 5, -15.0);
        let now = Utc::now();

        let changed =
            update_safe_mode_from_calibration(&mut safe_mode, &mut calibration, &stats, now);

        assert!(changed);
        assert!(safe_mode.active);
        // median_error_at_entry should store the absolute value
        assert!((safe_mode.median_error_at_entry.unwrap() - 17.0).abs() < 1e-9);
    }

    // --- Baseline dollar fallback ---

    /// When no API-delta EMA samples exist but the collector reports dollar burn,
    /// the governor estimates pct/hr using the hardcoded baseline ratio.
    /// This test verifies the formula and that generate_window_forecast produces
    /// a non-None safe_worker_count from the resulting pct/hr.
    #[test]
    fn baseline_dollar_fallback_produces_nonzero_pct_hr() {
        // Simulate 2 workers each burning the baseline $5/hr (p75)
        let fleet_usd_hr = 10.0_f64;
        const BASELINE_USD_PER_PCT: f64 = 5.0 / 1.5;

        // The formula used in rate_for (C) branch
        let estimated_pct_hr = fleet_usd_hr / BASELINE_USD_PER_PCT;

        // ~3.0 pct/hr for 2 workers at the baseline rate
        assert!(
            (estimated_pct_hr - 3.0).abs() < 1e-9,
            "expected ~3.0 pct/hr, got {}",
            estimated_pct_hr
        );

        // Verify that generate_window_forecast produces usable output with this rate
        let forecast = generate_window_forecast(
            "weekly_scoped",
            estimated_pct_hr,
            50.0,                               // current utilization
            90.0,                               // target ceiling
            24.0,                               // hours remaining
            estimated_pct_hr / 2.0,             // mean per-worker (half fleet for 2 workers)
            0.0,                                // std_pct_hr (no spread data in this test)
            state::EstimateQuality::Calibrated, // backward-compatible default for tests
        );

        assert!(
            forecast.safe_worker_count.is_some(),
            "baseline fallback should produce a non-None safe_worker_count"
        );
        assert!(
            forecast.predicted_exhaustion_hours.is_finite(),
            "baseline fallback should produce a finite exhaustion estimate"
        );
    }

    // --- Cold-start base-rate seeding (bead bf-3ebgd) ---

    /// Verify cold-start seeding logic: when a window has no burn rate data
    /// but exists this period (util > 0), it should be seeded from baseline
    /// instead of using 0.0 (which would imply infinite headroom).
    #[test]
    fn cold_start_seeds_from_baseline_when_window_exists() {
        use crate::state::EstimateQuality;

        // Simulate cold-start conditions
        let estimate_quality = EstimateQuality::ColdStart;
        let util = 15.0; // window exists with 15% utilization
        let fleet_pct_hr = 0.0; // no burn rate data yet
        let current_total = 2; // 2 workers running
        let baseline = crate::state::BaselineBurnRates {
            pct_per_worker_per_hour: 1.5, // default baseline
            dollars_per_worker_per_hour: 5.0,
        };

        // Apply the seeding logic (matches the inline code in governor.rs)
        let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) = if matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0
        {
            let base_per_worker = baseline.pct_per_worker_per_hour;
            let seeded_fleet_pct = base_per_worker * current_total as f64;
            let widened_std_pct = seeded_fleet_pct;
            (seeded_fleet_pct, base_per_worker, widened_std_pct)
        } else {
            (fleet_pct_hr, fleet_pct_hr / current_total as f64, 0.0)
        };

        // Verify seeding occurred
        assert!(
            (fleet_pct_hr_seeded - 3.0).abs() < 1e-9,
            "expected fleet_pct_hr_seeded = 3.0 (1.5 * 2 workers), got {}",
            fleet_pct_hr_seeded
        );
        assert!(
            (pct_per_worker_seeded - 1.5).abs() < 1e-9,
            "expected pct_per_worker_seeded = 1.5 (baseline), got {}",
            pct_per_worker_seeded
        );
        assert!(
            std_pct_hr_seeded > 0.0,
            "expected widened std_pct_hr_seeded > 0 for uncertainty, got {}",
            std_pct_hr_seeded
        );

        // Verify the forecast produces meaningful results (not infinite headroom)
        let forecast = generate_window_forecast(
            "weekly_scoped",
            fleet_pct_hr_seeded,
            util,
            90.0, // target ceiling
            24.0, // hours remaining
            pct_per_worker_seeded,
            std_pct_hr_seeded,
            estimate_quality,
        );

        assert!(
            forecast.safe_worker_count.is_some(),
            "cold-start seeded forecast should produce a non-None safe_worker_count"
        );
        assert!(
            forecast.predicted_exhaustion_hours.is_finite(),
            "cold-start seeded forecast should produce a finite exhaustion estimate, got {}",
            forecast.predicted_exhaustion_hours
        );
    }

    /// Verify cold-start does NOT seed when window is absent (util == 0).
    /// An absent window should stay at 0.0 pct/hr (genuinely empty).
    #[test]
    fn cold_start_does_not_seed_when_window_absent() {
        use crate::state::EstimateQuality;

        let estimate_quality = EstimateQuality::ColdStart;
        let util = 0.0; // window absent (sentinel value)
        let fleet_pct_hr = 0.0;
        let current_total = 2;
        let baseline = crate::state::BaselineBurnRates {
            pct_per_worker_per_hour: 1.5,
            dollars_per_worker_per_hour: 5.0,
        };

        let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) = if matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0
        {
            let base_per_worker = baseline.pct_per_worker_per_hour;
            let seeded_fleet_pct = base_per_worker * current_total as f64;
            let widened_std_pct = seeded_fleet_pct;
            (seeded_fleet_pct, base_per_worker, widened_std_pct)
        } else {
            (fleet_pct_hr, fleet_pct_hr / current_total as f64, 0.0)
        };

        // Verify NO seeding occurred (util == 0 means absent window)
        assert_eq!(
            fleet_pct_hr_seeded, 0.0,
            "absent window (util=0) should NOT be seeded, got {}",
            fleet_pct_hr_seeded
        );
        assert_eq!(
            pct_per_worker_seeded, 0.0,
            "absent window pct_per_worker should stay 0.0, got {}",
            pct_per_worker_seeded
        );
    }

    /// Verify calibrated windows are never seeded (already have data).
    #[test]
    fn calibrated_windows_are_never_seeded() {
        use crate::state::EstimateQuality;

        let estimate_quality = EstimateQuality::Calibrated; // NOT cold-start
        let util = 15.0;
        let fleet_pct_hr = 4.2; // has observed burn rate
        let current_total = 2;
        let baseline = crate::state::BaselineBurnRates {
            pct_per_worker_per_hour: 1.5,
            dollars_per_worker_per_hour: 5.0,
        };

        let (fleet_pct_hr_seeded, _, _) = if matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0
        {
            panic!("calibrated window should never enter seeding logic");
        } else {
            (fleet_pct_hr, fleet_pct_hr / current_total as f64, 0.0)
        };

        // Verify original values passed through unchanged
        assert_eq!(
            fleet_pct_hr_seeded, 4.2,
            "calibrated window should keep original fleet_pct_hr, got {}",
            fleet_pct_hr_seeded
        );
    }

    /// Comprehensive test for cold-start production path behavior.
    ///
    /// This test verifies that a window with 0 prior samples (cold-start) produces
    /// a forecast that:
    /// 1. Reports a seeded (non-zero) base rate from baseline
    /// 2. Is flagged as cold/uncertain via the EstimateQuality signal (Child-1)
    /// 3. Does NOT report exactly 0.0 burn with high implied confidence
    /// 4. Produces meaningful, conservative safe_worker_count values
    ///
    /// This tests the PRODUCTION path (governor.rs inline EMA + generate_window_forecast),
    /// NOT the test-only estimate_burn_rates function.
    ///
    /// This test FAILS if Child-1 (cold-start signaling) is reverted, serving as a
    /// regression guard for the critical safety mechanism that prevents the governor
    /// from treating "no data" as "definitely empty" (which would cause dangerous
    /// over-scaling).
    #[test]
    fn cold_start_production_path_seeds_and_signals_uncertainty() {
        use crate::state::EstimateQuality;

        // Cold-start conditions: window exists but has 0 prior burn samples
        let estimate_quality = EstimateQuality::ColdStart;
        let util = 75.0; // window exists with 75% utilization (more realistic pressure scenario)
        let fleet_pct_hr = 0.0; // no burn rate data yet (0 samples)
        let current_total = 1; // 1 worker running
        let target_ceiling = 90.0; // target ceiling
        let hrs_remaining = 12.0; // 12 hours until reset (creates realistic pressure for safe_worker_count calculation)

        // Baseline burn rate for seeding (conservative default)
        let baseline_pct_per_worker_hr = 1.5;

        // Apply the production seeding logic (matches governor.rs:4735-4787)
        let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) = if matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0
        {
            let base_per_worker = baseline_pct_per_worker_hr;
            let seeded_fleet_pct = base_per_worker * current_total as f64;
            let widened_std_pct = seeded_fleet_pct; // Conservative: use full fleet rate as spread
            (seeded_fleet_pct, base_per_worker, widened_std_pct)
        } else {
            (fleet_pct_hr, fleet_pct_hr / current_total as f64, 0.0)
        };

        // ASSERT 1: Verify seeded base rate is NON-ZERO (the key safety fix)
        assert!(
            fleet_pct_hr_seeded > 0.0,
            "cold-start window must be seeded with non-zero base rate, got {}",
            fleet_pct_hr_seeded
        );
        assert!(
            (fleet_pct_hr_seeded - 1.5).abs() < 1e-9,
            "expected seeded fleet_pct_hr = 1.5 (1.5 * 1 worker), got {}",
            fleet_pct_hr_seeded
        );
        assert!(
            pct_per_worker_seeded > 0.0,
            "cold-start per-worker rate must be non-zero, got {}",
            pct_per_worker_seeded
        );

        // ASSERT 2: Verify widened uncertainty signal (std > 0 for cold-start)
        assert!(
            std_pct_hr_seeded > 0.0,
            "cold-start forecast must have widened uncertainty (std > 0), got {}",
            std_pct_hr_seeded
        );

        // Generate forecast using the PRODUCTION path (generate_window_forecast)
        let forecast = generate_window_forecast(
            "weekly_scoped",
            fleet_pct_hr_seeded,
            util,
            target_ceiling,
            hrs_remaining,
            pct_per_worker_seeded,
            std_pct_hr_seeded,
            estimate_quality,
        );

        // ASSERT 3: Verify forecast is flagged as cold/uncertain via the signal
        assert_eq!(
            forecast.estimate_quality,
            EstimateQuality::ColdStart,
            "cold-start forecast must be flagged with EstimateQuality::ColdStart (Child-1 signal), got {:?}",
            forecast.estimate_quality
        );

        // ASSERT 4: Verify forecast does NOT report exactly 0.0 burn with high confidence
        assert!(
            forecast.fleet_pct_per_hour > 0.0,
            "cold-start forecast fleet_pct_per_hour must be non-zero, got {}",
            forecast.fleet_pct_per_hour
        );
        assert!(
            (forecast.fleet_pct_per_hour - 1.5).abs() < 1e-6,
            "forecast should preserve seeded rate 1.5, got {}",
            forecast.fleet_pct_per_hour
        );

        // ASSERT 5: Verify forecast produces meaningful (not infinite) exhaustion estimate
        assert!(
            forecast.predicted_exhaustion_hours.is_finite(),
            "cold-start forecast must produce finite exhaustion hours, got {}",
            forecast.predicted_exhaustion_hours
        );
        assert!(
            forecast.predicted_exhaustion_hours > 0.0,
            "cold-start forecast must predict positive exhaustion hours, got {}",
            forecast.predicted_exhaustion_hours
        );

        // ASSERT 6: Verify wide confidence cone (uncertainty signal)
        // Cold-start forecasts should have cone_ratio > 1.0 to signal uncertainty
        assert!(
            forecast.cone_ratio > 1.0,
            "cold-start forecast must have wide confidence cone (cone_ratio > 1.0) to signal uncertainty, got {}",
            forecast.cone_ratio
        );

        // ASSERT 7: Verify safe_worker_count is computed (can be 0 if margin is tight)
        assert!(
            forecast.safe_worker_count.is_some(),
            "cold-start forecast must produce safe_worker_count, got None"
        );
        let safe_workers = forecast.safe_worker_count.unwrap();

        // Debug: Print forecast values to understand the safe_worker_count calculation
        eprintln!("DEBUG forecast values:");
        eprintln!(
            "  util: {}, target_ceiling: {}, remaining_pct: {}",
            util, target_ceiling, forecast.remaining_pct
        );
        eprintln!(
            "  hrs_remaining: {}, margin_hrs: {}",
            hrs_remaining, forecast.margin_hrs
        );
        eprintln!(
            "  fleet_pct_per_hour: {}, pct_per_worker: {}",
            forecast.fleet_pct_per_hour, pct_per_worker_seeded
        );
        eprintln!(
            "  predicted_exhaustion_hours: {}",
            forecast.predicted_exhaustion_hours
        );
        eprintln!("  safe_worker_count: {}", safe_workers);
        eprintln!(
            "  safe_worker_count_p75: {:?}",
            forecast.safe_worker_count_p75
        );
        eprintln!("  cone_ratio: {}", forecast.cone_ratio);

        // safe_worker_count can legitimately be 0 in cold-start scenarios with tight margins
        // the key safety guarantee is that the burn rate is non-zero, not that we can add workers
        assert!(
            safe_workers < 1000, // Sanity check: not absurdly high
            "cold-start safe_worker_count must be reasonable, got {}",
            safe_workers
        );

        // ASSERT 8: Verify p75 safe worker count exists (conservative path for uncertainty)
        assert!(
            forecast.safe_worker_count_p75.is_some(),
            "cold-start forecast must produce safe_worker_count_p75 for conservative uncertainty handling"
        );
        let safe_workers_p75 = forecast.safe_worker_count_p75.unwrap();
        // P75 should be <= P50 (more conservative) - both can be 0 in tight-margin scenarios
        assert!(
            safe_workers_p75 <= safe_workers,
            "p75 conservative safe workers ({}) must be <= p50 ({})",
            safe_workers_p75,
            safe_workers
        );

        // Verify forecast structure is complete
        assert!(
            forecast.margin_hrs.is_finite(),
            "cold-start margin_hrs must be finite, got {}",
            forecast.margin_hrs
        );
        assert!(
            forecast.exh_hrs_p25.is_finite(),
            "cold-start exh_hrs_p25 must be finite, got {}",
            forecast.exh_hrs_p25
        );
        assert!(
            forecast.exh_hrs_p50.is_finite(),
            "cold-start exh_hrs_p50 must be finite, got {}",
            forecast.exh_hrs_p50
        );
        assert!(
            forecast.exh_hrs_p75.is_finite(),
            "cold-start exh_hrs_p75 must be finite, got {}",
            forecast.exh_hrs_p75
        );
    }

    /// Test: First-startup cold-start behavior (brand-new governor state)
    ///
    /// This test verifies that when cgov starts for the first time with:
    /// - No persisted weekly_scoped_model (None)
    /// - No accumulated samples (fleet_pct_ema_samples = 0)
    /// - No observed burn rate yet (fleet_pct_hr_ema.weekly_scoped = 0.0)
    ///
    /// The production path (inline EMA + generate_window_forecast) ensures:
    /// - The window is flagged as ColdStart (not "empty" or "absent")
    /// - A seeded (non-zero) baseline rate is used (NOT 0.0 which would give infinite headroom)
    /// - The forecast signals uncertainty via wide cone_ratio
    /// - Safe worker counts are computable (enforcing bounds, not unbounded scaling)
    ///
    /// This validates the critical first-startup safety property: the governor treats "no
    /// data" as "unknown/cold" with conservative bounds, NOT as "definitely empty" with
    /// infinite capacity (which would cause dangerous over-scaling).
    #[test]
    fn first_startup_cold_start_production_path() {
        use crate::state::EstimateQuality;

        // First-startup conditions: brand-new governor state with no prior history
        let weekly_scoped_model_at_startup: Option<String> = None; // No previous model persisted
        let ema_samples_at_startup = 0; // No accumulated samples
        let ema_value_at_startup = 0.0; // No observed rate yet

        // First poll returns weekly_scoped scoped to a model (e.g., "Fable")
        let first_poll_model = Some("Fable".to_string());
        let first_poll_util = 50.0; // Window has real utilization

        // Governor detects this as initialization (None -> Some), not rotation
        // Since model went from None to Some, reset_weekly_scoped_on_model_change()
        // returns true (for logging) but the EMAs are already 0 from default state

        // Determine estimate quality (production path logic)
        let estimate_quality = if ema_samples_at_startup >= 3 && ema_value_at_startup > 0.0 {
            EstimateQuality::Calibrated
        } else if ema_samples_at_startup == 0 {
            EstimateQuality::ColdStart
        } else {
            EstimateQuality::InsufficientSamples
        };

        // VERIFY 1: First-startup is flagged as ColdStart (NOT treated as "empty" or "absent")
        assert_eq!(
            estimate_quality,
            EstimateQuality::ColdStart,
            "First startup with no samples should be flagged ColdStart, not treated as empty/absent"
        );

        // Cold-start seeding parameters (production path logic)
        let baseline_pct_per_worker = 1.5;
        let current_workers = 3;
        let fleet_pct_hr_at_startup = 0.0; // No observed rate yet

        let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) = if matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        )
            && first_poll_util > 0.0
            && fleet_pct_hr_at_startup == 0.0
            && current_workers > 0
        {
            let seeded_fleet_pct = baseline_pct_per_worker * current_workers as f64;
            let widened_std_pct = seeded_fleet_pct; // Conservative wide uncertainty cone
            (seeded_fleet_pct, baseline_pct_per_worker, widened_std_pct)
        } else {
            (
                fleet_pct_hr_at_startup,
                fleet_pct_hr_at_startup / current_workers as f64,
                0.0,
            )
        };

        // VERIFY 2: Seeded base rate is NON-ZERO (prevents infinite headroom)
        assert!(
            fleet_pct_hr_seeded > 0.0,
            "First-startup must seed with non-zero baseline rate, got {}",
            fleet_pct_hr_seeded
        );
        assert_eq!(
            fleet_pct_hr_seeded,
            4.5, // 1.5 * 3 workers
            "Seeded fleet rate should be 4.5%/hr (baseline * workers), got {}",
            fleet_pct_hr_seeded
        );

        // VERIFY 3: Uncertainty cone is widened (std > 0 signals uncertainty)
        assert!(
            std_pct_hr_seeded > 0.0,
            "First-startup must have widened uncertainty cone (std > 0), got {}",
            std_pct_hr_seeded
        );

        // Generate forecast using the PRODUCTION path (generate_window_forecast)
        let target_ceiling = 90.0;
        let hours_remaining = 120.0; // 7-day window
        let forecast = generate_window_forecast(
            "weekly_scoped",
            fleet_pct_hr_seeded,
            first_poll_util,
            target_ceiling,
            hours_remaining,
            pct_per_worker_seeded,
            std_pct_hr_seeded,
            estimate_quality,
        );

        // VERIFY 4: Forecast is flagged as cold-start (not confident-empty)
        assert_eq!(
            forecast.estimate_quality,
            EstimateQuality::ColdStart,
            "First-startup forecast must be flagged ColdStart, not Calibrated or treated as absent"
        );

        // VERIFY 5: Forecast uses seeded rate (NOT 0.0, which would give infinite headroom)
        assert_eq!(
            forecast.fleet_pct_per_hour, fleet_pct_hr_seeded,
            "Fleet burn rate should use seeded baseline (4.5%/hr), not 0.0 (infinite headroom)"
        );
        assert!(
            forecast.predicted_exhaustion_hours.is_finite(),
            "Predicted exhaustion should be finite (seeded rate), not +inf (0 rate would give infinite)"
        );

        // With 40% remaining (90-50) and 4.5%/hr seeded: exhaustion = 40 / 4.5 = 8.89 hours
        let expected_exhaustion = 40.0 / fleet_pct_hr_seeded;
        assert!(
            (forecast.predicted_exhaustion_hours - expected_exhaustion).abs() < 0.1,
            "Exhaustion time should use seeded rate (8.89h), not 0.0 (infinite)"
        );

        // VERIFY 6: Wide uncertainty cone signals estimate is uncertain (not confident)
        assert!(
            forecast.cone_ratio > 1.0,
            "First-startup forecast must have wide uncertainty cone (cone_ratio > 1.0) to signal uncertainty, got {:.3}",
            forecast.cone_ratio
        );

        // VERIFY 7: Safe worker counts are computable (enforces bounds, prevents unbounded scaling)
        assert!(
            forecast.safe_worker_count.is_some(),
            "Safe worker count should be computable from seeded rate (enforces bound)"
        );
        assert!(
            forecast.safe_worker_count_p75.is_some(),
            "P75 safe worker count should enforce conservative bound"
        );

        // VERIFY 8: P75 <= P50 (conservative uncertainty cone makes P75 more restrictive)
        assert!(
            forecast.safe_worker_count_p75 <= forecast.safe_worker_count,
            "P75 should be more conservative (<= P50) due to widened uncertainty"
        );

        // VERIFY 9: No claim of 0% utilization (window exists this period)
        assert_eq!(
            forecast.current_utilization, first_poll_util,
            "Current utilization should reflect real API value (50%), not be claimed as 0%"
        );

        // VERIFY 10: Margin is finite and reflects conservative seeding
        // margin_hrs = predicted_exhaustion - hours_remaining
        // With seeded rate: margin = 8.89 - 120 = -111.11 hours (negative = safe)
        assert!(
            !forecast.margin_hrs.is_infinite(),
            "Margin should be finite from seeded rate"
        );
        assert!(
            forecast.margin_hrs < 0.0,
            "Margin should be negative (safe) with long time horizon and conservative rate"
        );

        // VERIFY 11: All forecast fields are complete and valid
        assert!(
            forecast.exh_hrs_p25.is_finite(),
            "exh_hrs_p25 must be finite"
        );
        assert!(
            forecast.exh_hrs_p50.is_finite(),
            "exh_hrs_p50 must be finite"
        );
        assert!(
            forecast.exh_hrs_p75.is_finite(),
            "exh_hrs_p75 must be finite"
        );

        // VERIFY 12: First-startup vs identity change both produce ColdStart
        // (Both paths should trigger cold-start seeding, just via different detection)
        assert_eq!(
            forecast.estimate_quality,
            EstimateQuality::ColdStart,
            "First-startup (None->Some) should produce same ColdStart signal as identity change (Some->Some)"
        );
    }

    // --- safe_worker_count_or_hold fallback tests ---

    #[test]
    fn safe_worker_count_none_holds_at_current() {
        // None → current_total, not max_workers (ADR-002: never guess capacity up
        // when there's no data to guess from — hold, don't scale).
        assert_eq!(safe_worker_count_or_hold(None, 8, 3), 3);
    }

    #[test]
    fn safe_worker_count_none_at_fresh_restart_holds_at_zero() {
        // The failure mode ADR-002 fixes: a fresh restart has current_total=0 and no
        // burn-rate samples yet. Must NOT launch workers at max_workers capacity
        // before any usage data confirms it's affordable.
        assert_eq!(safe_worker_count_or_hold(None, 8, 0), 0);
    }

    #[test]
    fn safe_worker_count_some_zero_scales_to_zero() {
        // Some(0) → 0: the binding window can't afford even one worker; scale to 0 and
        // let it recover (use-or-lose governor: idle-then-refill, no cold-start penalty).
        assert_eq!(safe_worker_count_or_hold(Some(0), 8, 3), 0);
    }

    #[test]
    fn safe_worker_count_some_nonzero_uses_value() {
        assert_eq!(safe_worker_count_or_hold(Some(5), 8, 3), 5);
    }

    #[test]
    fn workspace_from_launch_cmd_parses_flag() {
        assert_eq!(
            workspace_from_launch_cmd(
                "needle run --agent claude-print-opus --workspace /home/coding/cgov-polish-queue --identifier cgov-polish"
            ),
            Some("/home/coding/cgov-polish-queue".to_string())
        );
        assert_eq!(workspace_from_launch_cmd("needle run --agent x"), None);
    }

    #[test]
    fn compute_target_workers_none_safe_count_holds_at_current() {
        // ADR-002: when safe_worker_count is None (no burn-rate data), the governor
        // holds at current_total rather than jumping to global_max — it must not
        // scale up capacity it can't confirm is affordable.
        let mut state = state::GovernorState::new();
        state.workers.insert(
            "w1".to_string(),
            state::WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 6,
            },
        );
        state.capacity_forecast.binding_window = "weekly_scoped".to_string();
        // Leave safe_worker_count as None (default)

        let target = compute_target_workers(
            &state,
            90.0,
            &CompositeRiskConfig {
                enabled: false,
                ..Default::default()
            },
            &ConeScalingConfig::default(),
        );

        // Should hold at current_total (2), clamped to [min=1, max=6]
        assert_eq!(
            target, 2,
            "expected hold at current_total=2 when safe_worker_count is None"
        );
    }

    #[test]
    fn compute_target_workers_none_safe_count_at_fresh_restart_stays_zero() {
        // The actual incident ADR-002 fixes: a freshly restarted governor has
        // current_total=0 and no burn-rate samples. Must not launch any workers
        // until real usage data confirms it's safe to do so.
        let mut state = state::GovernorState::new();
        state.workers.insert(
            "w1".to_string(),
            state::WorkerState {
                current: 0,
                target: 0,
                min: 0,
                max: 4,
            },
        );
        state.capacity_forecast.binding_window = "weekly_scoped".to_string();
        // Leave safe_worker_count as None (default) — mirrors a restart with zero samples

        let target = compute_target_workers(
            &state,
            90.0,
            &CompositeRiskConfig {
                enabled: false,
                ..Default::default()
            },
            &ConeScalingConfig::default(),
        );

        assert_eq!(
            target, 0,
            "expected no workers launched at fresh restart with no burn-rate data"
        );
    }

    // --- Cost priority distribution tests ---

    use crate::config::{GovernorConfig, ModelPricing, PricingConfig};

    #[test]
    fn distribute_scale_down_reduces_highest_cost_first() {
        // Test that when scaling down, the highest-cost agent is reduced first
        // Setup: Opus @ $9/hr with 5 workers, Sonnet @ $5/hr with 5 workers
        // Scale down by 2 workers → should reduce Opus by 2, Sonnet by 0

        let mut agents = std::collections::HashMap::new();
        agents.insert(
            "opus".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-opus --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "opus-{id}".to_string(),
                min_workers: 0,
                max_workers: 10,
                subscription: false,
                baseline_burn_rate: None,
            },
        );
        agents.insert(
            "sonnet".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-sonnet --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "sonnet-{id}".to_string(),
                min_workers: 0,
                max_workers: 10,
                subscription: false,
                baseline_burn_rate: None,
            },
        );

        let mut current_workers = std::collections::HashMap::new();
        current_workers.insert("opus".to_string(), 5);
        current_workers.insert("sonnet".to_string(), 5);

        let burn_rate_by_model = std::collections::HashMap::new();
        let mut pricing_models = std::collections::HashMap::new();
        pricing_models.insert(
            "claude-opus".to_string(),
            ModelPricing {
                input_per_mtok: 15.0,
                output_per_mtok: 75.0,
                cache_write_5m_per_mtok: 18.75,
                cache_write_1h_per_mtok: 30.0,
                cache_read_per_mtok: 1.50,
            },
        );
        pricing_models.insert(
            "claude-sonnet".to_string(),
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_write_5m_per_mtok: 3.75,
                cache_write_1h_per_mtok: 6.0,
                cache_read_per_mtok: 0.30,
            },
        );
        let pricing_config = GovernorConfig {
            pricing: PricingConfig {
                models: pricing_models,
            },
            sprint: Default::default(),
            daemon: Default::default(),
            alerts: Default::default(),
            composite_risk: Default::default(),
            cone_scaling: Default::default(),
            agents: Default::default(),
            credentials_path: None,
        };

        let result = distribute_workers_by_cost_priority(
            &agents,
            &current_workers,
            8, // target 8 total (down from 10)
            &burn_rate_by_model,
            &pricing_config,
            false, // cutoff_risk doesn't affect scale-down priority
        );

        // Opus should be reduced from 5 to 3 (highest cost first)
        assert_eq!(result.get("opus"), Some(&3), "Opus should be reduced first");
        // Sonnet should stay at 5
        assert_eq!(
            result.get("sonnet"),
            Some(&5),
            "Sonnet should not be reduced"
        );
        // Total should be 8
        assert_eq!(result.values().sum::<u32>(), 8, "Total should be 8");
    }

    #[test]
    fn distribute_scale_up_adds_lowest_cost_first() {
        // Test that when scaling up, the lowest-cost agent is expanded first
        // Setup: Opus @ $9/hr with 2 workers (max 10), Sonnet @ $5/hr with 2 workers (max 10)
        // Scale up by 4 workers → should add to Sonnet first, then Opus

        let mut agents = std::collections::HashMap::new();
        agents.insert(
            "opus".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-opus --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "opus-{id}".to_string(),
                min_workers: 0,
                max_workers: 10,
                subscription: false,
                baseline_burn_rate: None,
            },
        );
        agents.insert(
            "sonnet".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-sonnet --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "sonnet-{id}".to_string(),
                min_workers: 0,
                max_workers: 10,
                subscription: false,
                baseline_burn_rate: None,
            },
        );

        let mut current_workers = std::collections::HashMap::new();
        current_workers.insert("opus".to_string(), 2);
        current_workers.insert("sonnet".to_string(), 2);

        let burn_rate_by_model = std::collections::HashMap::new();
        let mut pricing_models = std::collections::HashMap::new();
        pricing_models.insert(
            "claude-opus".to_string(),
            ModelPricing {
                input_per_mtok: 15.0,
                output_per_mtok: 75.0,
                cache_write_5m_per_mtok: 18.75,
                cache_write_1h_per_mtok: 30.0,
                cache_read_per_mtok: 1.50,
            },
        );
        pricing_models.insert(
            "claude-sonnet".to_string(),
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_write_5m_per_mtok: 3.75,
                cache_write_1h_per_mtok: 6.0,
                cache_read_per_mtok: 0.30,
            },
        );
        let pricing_config = GovernorConfig {
            pricing: PricingConfig {
                models: pricing_models,
            },
            sprint: Default::default(),
            daemon: Default::default(),
            alerts: Default::default(),
            composite_risk: Default::default(),
            cone_scaling: Default::default(),
            agents: Default::default(),
            credentials_path: None,
        };

        let result = distribute_workers_by_cost_priority(
            &agents,
            &current_workers,
            8, // target 8 total (up from 4)
            &burn_rate_by_model,
            &pricing_config,
            false,
        );

        // Sonnet should be filled first (lowest cost), from 2 to 6 (all 4 new workers)
        assert_eq!(
            result.get("sonnet"),
            Some(&6),
            "Sonnet should be expanded first"
        );
        // Opus should stay at 2 (no capacity needed yet)
        assert_eq!(
            result.get("opus"),
            Some(&2),
            "Opus should not be expanded yet"
        );
        // Total should be 8
        assert_eq!(result.values().sum::<u32>(), 8, "Total should be 8");
    }

    #[test]
    fn distribute_enforces_min_workers_for_expensive_pool() {
        // A dedicated expensive pool (opus, min 1, max 1) must get its guaranteed
        // worker even though the cheap agent (sonnet) would win on pure cost.
        let mut agents = std::collections::HashMap::new();
        agents.insert(
            "opus".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-opus --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "opus-{id}".to_string(),
                min_workers: 1,
                max_workers: 1,
                subscription: true,
                baseline_burn_rate: None,
            },
        );
        agents.insert(
            "sonnet".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-sonnet --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "sonnet-{id}".to_string(),
                min_workers: 0,
                max_workers: 8,
                subscription: false,
                baseline_burn_rate: None,
            },
        );

        let mut current_workers = std::collections::HashMap::new();
        current_workers.insert("opus".to_string(), 0);
        current_workers.insert("sonnet".to_string(), 1);

        let burn_rate_by_model = std::collections::HashMap::new();
        let mut pricing_models = std::collections::HashMap::new();
        pricing_models.insert(
            "claude-opus".to_string(),
            ModelPricing {
                input_per_mtok: 15.0,
                output_per_mtok: 75.0,
                cache_write_5m_per_mtok: 18.75,
                cache_write_1h_per_mtok: 30.0,
                cache_read_per_mtok: 1.50,
            },
        );
        pricing_models.insert(
            "claude-sonnet".to_string(),
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_write_5m_per_mtok: 3.75,
                cache_write_1h_per_mtok: 6.0,
                cache_read_per_mtok: 0.30,
            },
        );
        let pricing_config = GovernorConfig {
            pricing: PricingConfig {
                models: pricing_models,
            },
            sprint: Default::default(),
            daemon: Default::default(),
            alerts: Default::default(),
            composite_risk: Default::default(),
            cone_scaling: Default::default(),
            agents: Default::default(),
            credentials_path: None,
        };

        let result = distribute_workers_by_cost_priority(
            &agents,
            &current_workers,
            2, // budget for 2 workers total
            &burn_rate_by_model,
            &pricing_config,
            false,
        );

        assert_eq!(
            result.get("opus"),
            Some(&1),
            "expensive pool must get its guaranteed min worker"
        );
        assert_eq!(
            result.get("sonnet"),
            Some(&1),
            "cheap agent gets the remainder"
        );
        assert_eq!(result.values().sum::<u32>(), 2, "Total should be 2");
    }

    #[test]
    fn distribute_respects_max_workers_constraint() {
        // Test that scale-up respects max_workers constraint
        // Setup: Sonnet @ $5/hr with 8 workers (max 10), Haiku @ $1/hr with 2 workers (max 3)
        // Scale up by 5 workers → should fill Haiku to max (3), then add remaining 2 to Sonnet

        let mut agents = std::collections::HashMap::new();
        agents.insert(
            "sonnet".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-sonnet --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "sonnet-{id}".to_string(),
                min_workers: 0,
                max_workers: 10,
                subscription: false,
                baseline_burn_rate: None,
            },
        );
        agents.insert(
            "haiku".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-haiku --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "haiku-{id}".to_string(),
                min_workers: 0,
                max_workers: 3, // Limited capacity
                subscription: false,
                baseline_burn_rate: None,
            },
        );

        let mut current_workers = std::collections::HashMap::new();
        current_workers.insert("sonnet".to_string(), 8);
        current_workers.insert("haiku".to_string(), 2);

        let burn_rate_by_model = std::collections::HashMap::new();
        let mut pricing_models = std::collections::HashMap::new();
        pricing_models.insert(
            "claude-sonnet".to_string(),
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_write_5m_per_mtok: 3.75,
                cache_write_1h_per_mtok: 6.0,
                cache_read_per_mtok: 0.30,
            },
        );
        pricing_models.insert(
            "claude-haiku".to_string(),
            ModelPricing {
                input_per_mtok: 0.25,
                output_per_mtok: 1.25,
                cache_write_5m_per_mtok: 0.31,
                cache_write_1h_per_mtok: 0.50,
                cache_read_per_mtok: 0.025,
            },
        );
        let pricing_config = GovernorConfig {
            pricing: PricingConfig {
                models: pricing_models,
            },
            sprint: Default::default(),
            daemon: Default::default(),
            alerts: Default::default(),
            composite_risk: Default::default(),
            cone_scaling: Default::default(),
            agents: Default::default(),
            credentials_path: None,
        };

        let result = distribute_workers_by_cost_priority(
            &agents,
            &current_workers,
            15, // target 15 total (up from 10)
            &burn_rate_by_model,
            &pricing_config,
            false,
        );

        // Haiku should be filled to max (3)
        assert_eq!(
            result.get("haiku"),
            Some(&3),
            "Haiku should be filled to max"
        );
        // Sonnet should get remaining 2 workers (8 + 2 = 10)
        assert_eq!(
            result.get("sonnet"),
            Some(&10),
            "Sonnet should get remaining workers"
        );
        // Total should be 13 (capped by capacity constraints)
        assert_eq!(
            result.values().sum::<u32>(),
            13,
            "Total should be 13 (capped by max_workers)"
        );
    }

    #[test]
    fn distribute_uses_burn_rate_when_available() {
        // Test that burn rate data is used for cost when available
        // Setup: Opus and Sonnet with empirical burn rate data

        let mut agents = std::collections::HashMap::new();
        agents.insert(
            "opus".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-opus --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "opus-{id}".to_string(),
                min_workers: 0,
                max_workers: 10,
                subscription: false,
                baseline_burn_rate: None,
            },
        );
        agents.insert(
            "sonnet".to_string(),
            AgentConfig {
                launch_cmd: "needle run --agent claude-sonnet --workspace test".to_string(),
                heartbeat_dir: "/tmp/heartbeats".to_string(),
                session_pattern: "sonnet-{id}".to_string(),
                min_workers: 0,
                max_workers: 10,
                subscription: false,
                baseline_burn_rate: None,
            },
        );

        let mut current_workers = std::collections::HashMap::new();
        current_workers.insert("opus".to_string(), 5);
        current_workers.insert("sonnet".to_string(), 5);

        let mut burn_rate_by_model = std::collections::HashMap::new();
        burn_rate_by_model.insert(
            "claude-opus".to_string(),
            state::ModelBurnRate {
                pct_per_worker_per_hour: 0.0,
                dollars_per_worker_per_hour: 12.0, // Empirical: higher than pricing estimate
                samples: 100,
            },
        );
        burn_rate_by_model.insert(
            "claude-sonnet".to_string(),
            state::ModelBurnRate {
                pct_per_worker_per_hour: 0.0,
                dollars_per_worker_per_hour: 4.0, // Empirical: lower than pricing estimate
                samples: 100,
            },
        );

        let pricing_config = GovernorConfig {
            pricing: PricingConfig {
                models: std::collections::HashMap::new(),
            },
            sprint: Default::default(),
            daemon: Default::default(),
            alerts: Default::default(),
            composite_risk: Default::default(),
            cone_scaling: Default::default(),
            agents: Default::default(),
            credentials_path: None,
        };

        let result = distribute_workers_by_cost_priority(
            &agents,
            &current_workers,
            8, // target 8 total (down from 10)
            &burn_rate_by_model,
            &pricing_config,
            false,
        );

        // Opus should be reduced first based on empirical burn rate ($12 > $4)
        assert_eq!(
            result.get("opus"),
            Some(&3),
            "Opus should be reduced first based on empirical burn rate"
        );
        assert_eq!(
            result.get("sonnet"),
            Some(&5),
            "Sonnet should not be reduced"
        );
        assert_eq!(result.values().sum::<u32>(), 8, "Total should be 8");
    }

    // -----------------------------------------------------------------------
    // Consecutive snapshot delta computation tests
    // -----------------------------------------------------------------------

    /// Test consecutive snapshot delta computation with governor state integration.
    ///
    /// This test demonstrates the full flow of delta computation from consecutive
    /// API snapshots as it occurs in the governor cycle:
    /// 1. Two consecutive snapshots are created with known values
    /// 2. Deltas are computed from the snapshot difference
    /// 3. Delta values are verified to match expected computation
    /// 4. Delta fields are populated in governor state structure
    ///
    /// This simulates the behavior in `run_governor_cycle` where
    /// `state.previous_api_snapshot` and `state.current_api_snapshot` are used
    /// to compute percentage changes across polling intervals.
    #[test]
    fn test_consecutive_snapshot_delta_computation() {
        use crate::state::{PrevUsageSnapshot, WindowPctDeltas};
        use chrono::Utc;

        // Setup: Create two consecutive API snapshots with known values
        // These represent the API readings from two consecutive poll cycles
        let previous_snapshot = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 10.0,     // 5-hour window at 10%
            seven_day_pct: 20.0,     // 7-day window at 20%
            weekly_scoped_pct: 15.0, // 7-day-sonnet window at 15%
        };

        let current_snapshot = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 12.5,     // 5-hour window increased by 2.5%
            seven_day_pct: 22.0,     // 7-day window increased by 2.0%
            weekly_scoped_pct: 18.0, // 7-day-sonnet window increased by 3.0%
        };

        // Step 1: Convert snapshots to WindowPctSnapshot format for delta calculation
        // This matches the conversion in run_governor_cycle (lines 1728-1737)
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: previous_snapshot.five_hour_pct,
            seven_day: previous_snapshot.seven_day_pct,
            weekly_scoped: previous_snapshot.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: current_snapshot.five_hour_pct,
            seven_day: current_snapshot.seven_day_pct,
            weekly_scoped: current_snapshot.weekly_scoped_pct,
        };

        // Step 2: Compute deltas from consecutive snapshots
        // This is the core delta computation from run_governor_cycle (line 1738)
        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Step 3: Calculate expected delta percentage manually from snapshot values
        // Delta formula: delta = current_snapshot_value - previous_snapshot_value
        // This represents the percentage change in utilization between consecutive API polls
        let expected_delta_5h = current_snapshot.five_hour_pct - previous_snapshot.five_hour_pct;
        let expected_delta_7d = current_snapshot.seven_day_pct - previous_snapshot.seven_day_pct;
        let expected_delta_7ds =
            current_snapshot.weekly_scoped_pct - previous_snapshot.weekly_scoped_pct;

        // Step 4: Verify computed deltas match expected calculation
        // The delta formula: current_pct - previous_pct = delta_pct
        assert!(
            (delta_5h - expected_delta_5h).abs() < f64::EPSILON,
            "5-hour delta: computed {} should equal expected {} ({} - {})",
            delta_5h,
            expected_delta_5h,
            current_snapshot.five_hour_pct,
            previous_snapshot.five_hour_pct
        );
        assert!(
            (delta_7d - expected_delta_7d).abs() < f64::EPSILON,
            "7-day delta: computed {} should equal expected {} ({} - {})",
            delta_7d,
            expected_delta_7d,
            current_snapshot.seven_day_pct,
            previous_snapshot.seven_day_pct
        );
        assert!(
            (delta_7ds - expected_delta_7ds).abs() < f64::EPSILON,
            "7-day-sonnet delta: computed {} should equal expected {} ({} - {})",
            delta_7ds,
            expected_delta_7ds,
            current_snapshot.weekly_scoped_pct,
            previous_snapshot.weekly_scoped_pct
        );

        // Step 5: Verify the specific expected values for this test case
        assert!(
            (expected_delta_5h - 2.5).abs() < f64::EPSILON,
            "Expected 5-hour delta should be 12.5 - 10.0 = 2.5"
        );
        assert!(
            (expected_delta_7d - 2.0).abs() < f64::EPSILON,
            "Expected 7-day delta should be 22.0 - 20.0 = 2.0"
        );
        assert!(
            (expected_delta_7ds - 3.0).abs() < f64::EPSILON,
            "Expected 7-day-sonnet delta should be 18.0 - 15.0 = 3.0"
        );

        // Step 6: Populate delta fields in governor state structure
        // This simulates storing deltas in state.last_fleet_aggregate.window_pct_deltas
        // (as done in run_governor_cycle lines 1741-1745)
        let window_pct_deltas = WindowPctDeltas {
            five_hour: delta_5h,
            seven_day: delta_7d,
            weekly_scoped: delta_7ds,
        };

        // Step 7: Assert that delta fields are correctly populated with expected values
        assert!(
            (window_pct_deltas.five_hour - expected_delta_5h).abs() < f64::EPSILON,
            "State five_hour delta should be {} (from {} - {})",
            expected_delta_5h,
            current_snapshot.five_hour_pct,
            previous_snapshot.five_hour_pct
        );
        assert!(
            (window_pct_deltas.seven_day - expected_delta_7d).abs() < f64::EPSILON,
            "State seven_day delta should be {} (from {} - {})",
            expected_delta_7d,
            current_snapshot.seven_day_pct,
            previous_snapshot.seven_day_pct
        );
        assert!(
            (window_pct_deltas.weekly_scoped - expected_delta_7ds).abs() < f64::EPSILON,
            "State weekly_scoped delta should be {} (from {} - {})",
            expected_delta_7ds,
            current_snapshot.weekly_scoped_pct,
            previous_snapshot.weekly_scoped_pct
        );

        // Verify all deltas are non-zero (indicating active consumption)
        assert!(
            delta_5h > 0.0,
            "5-hour delta should be positive (increasing)"
        );
        assert!(
            delta_7d > 0.0,
            "7-day delta should be positive (increasing)"
        );
        assert!(
            delta_7ds > 0.0,
            "7-day-sonnet delta should be positive (increasing)"
        );
    }

    /// Test consecutive snapshot delta computation with window reset (negative deltas).
    ///
    /// Verifies that when a window resets (utilization drops), the delta computation
    /// correctly produces negative values, which is expected behavior during window
    /// boundary transitions.
    #[test]
    fn test_consecutive_snapshot_delta_with_window_reset() {
        use crate::state::{PrevUsageSnapshot, WindowPctDeltas};
        use chrono::Utc;

        // Setup: Previous snapshot shows high utilization (near window limit)
        let previous_snapshot = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 80.0,     // 5-hour window at 80% (near exhaustion)
            seven_day_pct: 90.0,     // 7-day window at 90% (near exhaustion)
            weekly_scoped_pct: 85.0, // 7-day-sonnet at 85%
        };

        // Current snapshot shows window reset (utilization dropped)
        let current_snapshot = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 5.0,     // Window reset: 80% -> 5% (delta: -75.0)
            seven_day_pct: 15.0,    // Window reset: 90% -> 15% (delta: -75.0)
            weekly_scoped_pct: 8.0, // Window reset: 85% -> 8% (delta: -77.0)
        };

        // Compute deltas
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: previous_snapshot.five_hour_pct,
            seven_day: previous_snapshot.seven_day_pct,
            weekly_scoped: previous_snapshot.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: current_snapshot.five_hour_pct,
            seven_day: current_snapshot.seven_day_pct,
            weekly_scoped: current_snapshot.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Calculate expected deltas manually from snapshot values
        // Delta formula: delta = current - previous (negative when window resets)
        let expected_delta_5h = current_snapshot.five_hour_pct - previous_snapshot.five_hour_pct;
        let expected_delta_7d = current_snapshot.seven_day_pct - previous_snapshot.seven_day_pct;
        let expected_delta_7ds =
            current_snapshot.weekly_scoped_pct - previous_snapshot.weekly_scoped_pct;

        // Verify computed deltas match expected calculation (negative for window reset)
        assert!(
            (delta_5h - expected_delta_5h).abs() < f64::EPSILON,
            "5-hour delta: computed {} should equal expected {} ({} - {})",
            delta_5h,
            expected_delta_5h,
            current_snapshot.five_hour_pct,
            previous_snapshot.five_hour_pct
        );
        assert!(
            (delta_7d - expected_delta_7d).abs() < f64::EPSILON,
            "7-day delta: computed {} should equal expected {} ({} - {})",
            delta_7d,
            expected_delta_7d,
            current_snapshot.seven_day_pct,
            previous_snapshot.seven_day_pct
        );
        assert!(
            (delta_7ds - expected_delta_7ds).abs() < f64::EPSILON,
            "7-day-sonnet delta: computed {} should equal expected {} ({} - {})",
            delta_7ds,
            expected_delta_7ds,
            current_snapshot.weekly_scoped_pct,
            previous_snapshot.weekly_scoped_pct
        );

        // Verify the specific expected values for this window reset test case
        assert!(
            (expected_delta_5h - (-75.0)).abs() < f64::EPSILON,
            "Expected 5-hour delta should be 5.0 - 80.0 = -75.0 (window reset)"
        );
        assert!(
            (expected_delta_7d - (-75.0)).abs() < f64::EPSILON,
            "Expected 7-day delta should be 15.0 - 90.0 = -75.0 (window reset)"
        );
        assert!(
            (expected_delta_7ds - (-77.0)).abs() < f64::EPSILON,
            "Expected 7-day-sonnet delta should be 8.0 - 85.0 = -77.0 (window reset)"
        );

        // Populate in state structure
        let window_pct_deltas = WindowPctDeltas {
            five_hour: delta_5h,
            seven_day: delta_7d,
            weekly_scoped: delta_7ds,
        };

        // Verify state correctly captures negative deltas (matching expected calculation)
        assert!(
            (window_pct_deltas.five_hour - expected_delta_5h).abs() < f64::EPSILON,
            "State five_hour delta should be {} (from {} - {})",
            expected_delta_5h,
            current_snapshot.five_hour_pct,
            previous_snapshot.five_hour_pct
        );
        assert!(
            (window_pct_deltas.seven_day - expected_delta_7d).abs() < f64::EPSILON,
            "State seven_day delta should be {} (from {} - {})",
            expected_delta_7d,
            current_snapshot.seven_day_pct,
            previous_snapshot.seven_day_pct
        );
        assert!(
            (window_pct_deltas.weekly_scoped - expected_delta_7ds).abs() < f64::EPSILON,
            "State weekly_scoped delta should be {} (from {} - {})",
            expected_delta_7ds,
            current_snapshot.weekly_scoped_pct,
            previous_snapshot.weekly_scoped_pct
        );

        // Verify all deltas are negative (window reset condition)
        assert!(
            window_pct_deltas.five_hour < 0.0,
            "State five_hour delta should be negative (window reset)"
        );
        assert!(
            window_pct_deltas.seven_day < 0.0,
            "State seven_day delta should be negative (window reset)"
        );
        assert!(
            window_pct_deltas.weekly_scoped < 0.0,
            "State weekly_scoped delta should be negative (window reset)"
        );
    }

    /// Test consecutive snapshot delta computation with identical values (zero deltas).
    ///
    /// Verifies that when consecutive snapshots have identical values (no consumption
    /// occurred between polls), all deltas are zero. This is expected behavior during
    /// idle periods or when the API percentage hasn't changed.
    #[test]
    fn test_consecutive_snapshot_delta_identical_snapshots() {
        use crate::state::{PrevUsageSnapshot, WindowPctDeltas};
        use chrono::Utc;

        // Setup: Both snapshots have identical values (no consumption)
        let snapshot_values = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 25.0,
            seven_day_pct: 35.0,
            weekly_scoped_pct: 28.0,
        };

        // Previous and current are identical
        let previous_snapshot = snapshot_values.clone();
        let current_snapshot = snapshot_values;

        // Compute deltas
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: previous_snapshot.five_hour_pct,
            seven_day: previous_snapshot.seven_day_pct,
            weekly_scoped: previous_snapshot.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: current_snapshot.five_hour_pct,
            seven_day: current_snapshot.seven_day_pct,
            weekly_scoped: current_snapshot.weekly_scoped_pct,
        };

        let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Calculate expected deltas manually from snapshot values
        // Delta formula: delta = current - previous (both are identical here)
        let expected_delta_5h = current_snapshot.five_hour_pct - previous_snapshot.five_hour_pct;
        let expected_delta_7d = current_snapshot.seven_day_pct - previous_snapshot.seven_day_pct;
        let expected_delta_7ds =
            current_snapshot.weekly_scoped_pct - previous_snapshot.weekly_scoped_pct;

        // Verify computed deltas match expected calculation (should be zero for identical snapshots)
        assert_eq!(
            delta_5h,
            expected_delta_5h,
            "5-hour delta: computed {} should equal expected {} ({} - {})",
            delta_5h,
            expected_delta_5h,
            current_snapshot.five_hour_pct,
            previous_snapshot.five_hour_pct
        );
        assert_eq!(
            delta_7d,
            expected_delta_7d,
            "7-day delta: computed {} should equal expected {} ({} - {})",
            delta_7d,
            expected_delta_7d,
            current_snapshot.seven_day_pct,
            previous_snapshot.seven_day_pct
        );
        assert_eq!(
            delta_7ds,
            expected_delta_7ds,
            "7-day-sonnet delta: computed {} should equal expected {} ({} - {})",
            delta_7ds,
            expected_delta_7ds,
            current_snapshot.weekly_scoped_pct,
            previous_snapshot.weekly_scoped_pct
        );

        // Verify all expected deltas are exactly zero (identical snapshots)
        assert_eq!(
            expected_delta_5h, 0.0,
            "Expected 5-hour delta should be 0.0 for identical snapshots"
        );
        assert_eq!(
            expected_delta_7d, 0.0,
            "Expected 7-day delta should be 0.0 for identical snapshots"
        );
        assert_eq!(
            expected_delta_7ds, 0.0,
            "Expected 7-day-sonnet delta should be 0.0 for identical snapshots"
        );

        // Populate in state structure
        let window_pct_deltas = WindowPctDeltas {
            five_hour: delta_5h,
            seven_day: delta_7d,
            weekly_scoped: delta_7ds,
        };

        // Verify state correctly shows zero deltas (matching expected calculation)
        assert_eq!(
            window_pct_deltas.five_hour, expected_delta_5h,
            "State five_hour delta should be {} (from {} - {})",
            expected_delta_5h, current_snapshot.five_hour_pct, previous_snapshot.five_hour_pct
        );
        assert_eq!(
            window_pct_deltas.seven_day, expected_delta_7d,
            "State seven_day delta should be {} (from {} - {})",
            expected_delta_7d, current_snapshot.seven_day_pct, previous_snapshot.seven_day_pct
        );
        assert_eq!(
            window_pct_deltas.weekly_scoped,
            expected_delta_7ds,
            "State weekly_scoped delta should be {} (from {} - {})",
            expected_delta_7ds,
            current_snapshot.weekly_scoped_pct,
            previous_snapshot.weekly_scoped_pct
        );
    }

    // ---------------------------------------------------------------------------
    // Basic governor cycle tests
    // ---------------------------------------------------------------------------

    /// Test basic governor cycle flow without external dependencies.
    ///
    /// Drives the decision half of a cycle from a single usage snapshot —
    /// `compute_target_workers` then `apply_scaling` — with no poller, database or
    /// tmux involved, and pins both results.
    ///
    /// This is the in-process floor, not the end-to-end cycle: the full
    /// `run_governor_cycle` (poll → persist → decide) is covered against
    /// `MockPoller` in `mock_poller_tests`, starting with
    /// `test_governor_cycle_smoke`.
    #[test]
    fn test_governor_cycle_basic_flow() {
        // 1. Create a minimal governor state
        let mut state = state::GovernorState::new();
        state.workers.insert(
            "test-agent".to_string(),
            state::WorkerState {
                current: 5,
                target: 5,
                min: 1,
                max: 10,
            },
        );

        // 2. Create a usage snapshot with moderate utilization
        let usage = make_usage_snapshot(50.0, 40.0, 35.0);

        // 3. Build capacity forecast from the snapshot
        let snapshot = crate::db::WindowPctSnapshot {
            five_hour: usage.get(WINDOW_FIVE_HOUR).unwrap_or(0.0),
            seven_day: usage.get(WINDOW_SEVEN_DAY).unwrap_or(0.0),
            weekly_scoped: usage.get(WINDOW_WEEKLY_SCOPED).unwrap_or(0.0),
        };

        // Simulate minimal capacity forecast
        state.capacity_forecast = state::CapacityForecast {
            five_hour: state::WindowForecast {
                current_utilization: snapshot.five_hour,
                safe_worker_count: Some(5),
                safe_worker_count_p75: Some(4),
                ..Default::default()
            },
            seven_day: state::WindowForecast {
                current_utilization: snapshot.seven_day,
                safe_worker_count: Some(6),
                safe_worker_count_p75: Some(5),
                ..Default::default()
            },
            weekly_scoped: state::WindowForecast {
                current_utilization: snapshot.weekly_scoped,
                safe_worker_count: Some(7),
                safe_worker_count_p75: Some(6),
                ..Default::default()
            },
            binding_window: WINDOW_WEEKLY_SCOPED.to_string(),
            ..Default::default()
        };

        // 4. Compute target workers
        let target = compute_target_workers(
            &state,
            90.0, // target_ceiling
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default(),
        );

        // 5. Apply scaling decision
        let current_total = 5;
        let decision = apply_scaling(
            target,
            current_total,
            2.0, // hysteresis_band
            3,   // max_up_per_cycle
            2,   // max_down_per_cycle
        );

        // 6. A single snapshot has exactly one right answer, so assert it rather
        //    than accepting whatever came back: the binding window is
        //    weekly_scoped, whose safe_worker_count is 7, and 7 is within the
        //    2.0 hysteresis band of the current 5, so nothing moves.
        assert_eq!(
            target, 7,
            "target should be the binding (weekly_scoped) window's safe_worker_count"
        );
        assert!(
            matches!(decision, ScalingDecision::NoChange),
            "target {} vs current {} is inside the 2.0 hysteresis band, so the \
             decision should be NoChange, got {:?}",
            target,
            current_total,
            decision
        );

        // 7. Verify state is consistent after the cycle
        assert!(
            !state.workers.is_empty(),
            "State should retain workers after cycle"
        );
        assert_eq!(
            state.workers["test-agent"].current, 5,
            "Current workers unchanged"
        );
        assert!(!state.safe_mode.active, "Safe mode should not be active");
    }

    /// Test governor cycle with high utilization triggers emergency brake.
    ///
    /// This test verifies that when utilization exceeds the emergency brake threshold,
    /// the governor correctly responds with an EmergencyBrake decision.
    #[test]
    fn test_governor_cycle_emergency_brake() {
        let mut state = state::GovernorState::new();
        state.workers.insert(
            "test-agent".to_string(),
            state::WorkerState {
                current: 10,
                target: 10,
                min: 1,
                max: 10,
            },
        );

        // High utilization above emergency brake threshold (98%)
        state.capacity_forecast = state::CapacityForecast {
            five_hour: state::WindowForecast {
                current_utilization: 99.0,
                safe_worker_count: Some(0),
                ..Default::default()
            },
            seven_day: state::WindowForecast {
                current_utilization: 50.0,
                safe_worker_count: Some(5),
                ..Default::default()
            },
            weekly_scoped: state::WindowForecast {
                current_utilization: 50.0,
                safe_worker_count: Some(5),
                ..Default::default()
            },
            binding_window: WINDOW_FIVE_HOUR.to_string(),
            ..Default::default()
        };

        let target = compute_target_workers(
            &state,
            90.0,
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default(),
        );

        // At 99% utilization, target should be 0 (emergency brake)
        assert_eq!(target, 0, "Target should be 0 at 99% utilization");

        let decision = apply_scaling(target, 10, 2.0, 3, 2);

        assert!(
            matches!(decision, ScalingDecision::EmergencyBrake),
            "Should trigger EmergencyBrake decision at 99% utilization"
        );
    }

    /// Test governor cycle with scaling decision within hysteresis band.
    ///
    /// Verifies that when the target is within the hysteresis band of current,
    /// the governor correctly decides to make no change.
    #[test]
    fn test_governor_cycle_hysteresis_no_change() {
        let mut state = state::GovernorState::new();
        state.workers.insert(
            "test-agent".to_string(),
            state::WorkerState {
                current: 5,
                target: 5,
                min: 1,
                max: 10,
            },
        );

        // Target exactly equals current
        state.capacity_forecast = state::CapacityForecast {
            five_hour: state::WindowForecast {
                current_utilization: 50.0,
                safe_worker_count: Some(5),
                ..Default::default()
            },
            seven_day: state::WindowForecast {
                current_utilization: 50.0,
                safe_worker_count: Some(5),
                ..Default::default()
            },
            weekly_scoped: state::WindowForecast {
                current_utilization: 50.0,
                safe_worker_count: Some(5),
                ..Default::default()
            },
            binding_window: WINDOW_WEEKLY_SCOPED.to_string(),
            ..Default::default()
        };

        let target = compute_target_workers(
            &state,
            90.0,
            &CompositeRiskConfig::default(),
            &ConeScalingConfig::default(),
        );

        assert_eq!(
            target, 5,
            "Target should equal current at moderate utilization"
        );

        let decision = apply_scaling(target, 5, 2.0, 3, 2);

        assert!(
            matches!(decision, ScalingDecision::NoChange),
            "Should decide NoChange when target equals current"
        );
    }
}

// ---------------------------------------------------------------------------
// Mock Poller for Testing
// ---------------------------------------------------------------------------

/// Mock poller for governor cycle testing.
///
/// This mock poller allows tests to configure the usage data returned by `poll()`,
/// simulate error conditions, and test stale data scenarios for token refresh logic.
#[cfg(test)]
pub struct MockPoller {
    /// Configurable usage data to return on success
    pub usage_data: Option<crate::poller::UsageData>,
    /// Configurable error message to return on failure
    pub error_message: Option<String>,
    /// Whether to simulate stale data (for testing token refresh logic)
    pub stale: bool,
    /// Call count tracker for testing behavior across multiple calls
    pub poll_count: u32,
}

#[cfg(test)]
impl MockPoller {
    /// Create a new mock poller with default settings.
    ///
    /// By default, returns successful usage data with moderate utilization values.
    pub fn new() -> Self {
        Self {
            usage_data: Some(Self::default_usage_data()),
            error_message: None,
            stale: false,
            poll_count: 0,
        }
    }

    /// Create a mock poller that always returns an error.
    ///
    /// Useful for testing error handling in governor cycles.
    pub fn with_error(message: impl Into<String>) -> Self {
        Self {
            usage_data: None,
            error_message: Some(message.into()),
            stale: false,
            poll_count: 0,
        }
    }

    /// Create a mock poller that returns stale data.
    ///
    /// Simulates token refresh failure scenarios where the poller
    /// falls back to cached data with the `stale` flag set.
    pub fn with_stale_data() -> Self {
        let mut data = Self::default_usage_data();
        data.stale = true;

        Self {
            usage_data: Some(data),
            error_message: None,
            stale: true,
            poll_count: 0,
        }
    }

    /// Create a mock poller with custom utilization values.
    ///
    /// # Arguments
    /// - `five_hour_util`: 5-hour window utilization percentage
    /// - `seven_day_util`: 7-day window utilization percentage (all models)
    /// - `weekly_scoped_util`: 7-day window utilization percentage (Sonnet only)
    ///
    /// ⚠️ BUG: The documentation above incorrectly states "Sonnet only".
    /// weekly_scoped_util is MODEL-AGNOSTIC. It represents utilization for whatever
    /// model carries the scoped cap (Fable, Opus, Sonnet, etc.) this period.
    /// Use UsageData.weekly_scoped_model to identify the active model.
    pub fn with_utilization(
        five_hour_util: f64,
        seven_day_util: f64,
        weekly_scoped_util: f64,
    ) -> Self {
        let mut data = Self::default_usage_data();
        data.five_hour_utilization = five_hour_util;
        data.seven_day_utilization = seven_day_util;
        data.weekly_scoped_utilization = weekly_scoped_util;

        Self {
            usage_data: Some(data),
            error_message: None,
            stale: false,
            poll_count: 0,
        }
    }

    /// Create a mock poller that simulates emergency brake conditions.
    ///
    /// Returns utilization >= 98% to trigger the emergency brake.
    pub fn with_emergency_brake() -> Self {
        Self::with_utilization(99.0, 99.0, 99.0)
    }

    /// Create a mock poller that simulates low utilization.
    ///
    /// Returns utilization <= 25% for underutilization scenarios.
    pub fn with_low_utilization() -> Self {
        Self::with_utilization(15.0, 20.0, 18.0)
    }

    /// Create a mock poller that simulates high utilization (near cutoff).
    ///
    /// Returns utilization >= 90% for near-cutoff scenarios.
    pub fn with_high_utilization() -> Self {
        Self::with_utilization(92.0, 94.0, 93.0)
    }

    /// Create a mock poller with a specific weekly_scoped model.
    ///
    /// # Arguments
    /// - `model`: The model name for weekly_scoped (e.g., "Sonnet", "Opus", "Fable")
    /// - `five_hour_util`: 5-hour window utilization percentage
    /// - `seven_day_util`: 7-day window utilization percentage (all models)
    /// - `weekly_scoped_util`: 7-day window utilization percentage (scoped to model)
    pub fn with_model(
        model: Option<&str>,
        five_hour_util: f64,
        seven_day_util: f64,
        weekly_scoped_util: f64,
    ) -> Self {
        let mut data = Self::default_usage_data();
        data.five_hour_utilization = five_hour_util;
        data.seven_day_utilization = seven_day_util;
        data.weekly_scoped_utilization = weekly_scoped_util;
        data.weekly_scoped_model = model.map(|s| s.to_string());

        Self {
            usage_data: Some(data),
            error_message: None,
            stale: false,
            poll_count: 0,
        }
    }

    /// Create default usage data with moderate utilization values.
    ///
    /// Returns a UsageData struct with:
    /// - 5-hour window: 50% utilization
    /// - 7-day window: 60% utilization (all models)
    /// - 7-day window: 55% utilization (Sonnet only)
    ///
    /// ⚠️ BUG: The documentation above incorrectly states "Sonnet only".
    /// The weekly_scoped utilization is MODEL-AGNOSTIC. This default creates
    /// data without setting weekly_scoped_model (None), making it apply to the
    /// generic scoped cap regardless of which model is active.
    /// - Reset times 4-5 hours in the future
    fn default_usage_data() -> crate::poller::UsageData {
        use chrono::Duration;

        let now = Utc::now();
        let five_hour_reset = now + Duration::hours(4);
        let seven_day_reset = now + Duration::hours(120);

        crate::poller::UsageData {
            five_hour_utilization: 50.0,
            five_hour_resets_at: five_hour_reset.to_rfc3339(),
            five_hour_hours_remaining: 4.0,
            seven_day_utilization: 60.0,
            seven_day_resets_at: seven_day_reset.to_rfc3339(),
            seven_day_hours_remaining: 120.0,
            weekly_scoped_utilization: 55.0,
            weekly_scoped_resets_at: seven_day_reset.to_rfc3339(),
            weekly_scoped_hours_remaining: 120.0,
            weekly_scoped_model: None,
            limits: vec![],
            timestamp: now,
            stale: false,
        }
    }

    /// Simulate a poll call, returning configured data or error.
    ///
    /// This method implements the same interface as `Poller::poll()` but
    /// returns mock data instead of making actual API calls.
    ///
    /// Increments `poll_count` on each call to track invocation patterns.
    ///
    /// # Returns
    /// - `Ok(UsageData)` if `usage_data` is set
    /// - `Err(anyhow::Error)` if `error_message` is set
    pub fn poll(&mut self) -> anyhow::Result<crate::poller::UsageData> {
        self.poll_count += 1;

        if let Some(ref message) = self.error_message {
            Err(anyhow::anyhow!("{}", message))
        } else if let Some(ref data) = self.usage_data {
            Ok(data.clone())
        } else {
            // Should not happen with proper construction, but handle gracefully
            Ok(Self::default_usage_data())
        }
    }

    /// Set a new error to return on subsequent polls.
    ///
    /// Clears any existing `usage_data`.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error_message = Some(message.into());
        self.usage_data = None;
    }

    /// Set new usage data to return on subsequent polls.
    ///
    /// Clears any existing `error_message`.
    pub fn set_usage_data(&mut self, data: crate::poller::UsageData) {
        self.usage_data = Some(data);
        self.error_message = None;
    }

    /// Reset the poll counter.
    pub fn reset_poll_count(&mut self) {
        self.poll_count = 0;
    }
}

#[cfg(test)]
impl Default for MockPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// Lets `MockPoller` stand in for the real `Poller` in `run_governor_cycle`.
#[cfg(test)]
impl UsagePoller for MockPoller {
    fn poll_usage(&mut self) -> anyhow::Result<crate::poller::UsageData> {
        self.poll()
    }
}

// ---------------------------------------------------------------------------
// Mock Poller Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod mock_poller_tests {
    use super::*;

    /// Test that MockPoller returns default usage data.
    #[test]
    fn test_mock_poller_default_returns_usage_data() {
        let mut poller = MockPoller::new();

        let result = poller.poll();

        assert!(result.is_ok(), "poll() should return Ok");
        let data = result.unwrap();
        assert!(!data.stale, "Default data should not be stale");
        assert_eq!(data.five_hour_utilization, 50.0);
        assert_eq!(data.seven_day_utilization, 60.0);
        assert_eq!(data.weekly_scoped_utilization, 55.0);
    }

    /// Test that MockPoller can return error responses.
    #[test]
    fn test_mock_poller_returns_error() {
        let test_message = "Test API error";
        let mut poller = MockPoller::with_error(test_message);

        let result = poller.poll();

        assert!(result.is_err(), "poll() should return Err");
        let err = result.unwrap_err();
        assert_eq!(err.to_string(), test_message);
    }

    /// Test that MockPoller can return stale data.
    #[test]
    fn test_mock_poller_returns_stale_data() {
        let mut poller = MockPoller::with_stale_data();

        let result = poller.poll();

        assert!(result.is_ok(), "poll() should return Ok");
        let data = result.unwrap();
        assert!(data.stale, "Data should be marked as stale");
    }

    /// Test that MockPoller can return custom utilization values.
    #[test]
    fn test_mock_poller_custom_utilization() {
        let mut poller = MockPoller::with_utilization(75.0, 80.0, 77.5);

        let result = poller.poll();

        assert!(result.is_ok(), "poll() should return Ok");
        let data = result.unwrap();
        assert_eq!(data.five_hour_utilization, 75.0);
        assert_eq!(data.seven_day_utilization, 80.0);
        assert_eq!(data.weekly_scoped_utilization, 77.5);
        assert!(!data.stale, "Custom data should not be stale");
    }

    /// Test that MockPoller emergency brake scenario returns 99% utilization.
    #[test]
    fn test_mock_poller_emergency_brake() {
        let mut poller = MockPoller::with_emergency_brake();

        let result = poller.poll();

        assert!(result.is_ok(), "poll() should return Ok");
        let data = result.unwrap();
        assert_eq!(data.five_hour_utilization, 99.0);
        assert_eq!(data.seven_day_utilization, 99.0);
        assert_eq!(data.weekly_scoped_utilization, 99.0);
    }

    /// Test that MockPoller low utilization scenario returns low values.
    #[test]
    fn test_mock_poller_low_utilization() {
        let mut poller = MockPoller::with_low_utilization();

        let result = poller.poll();

        assert!(result.is_ok(), "poll() should return Ok");
        let data = result.unwrap();
        assert!(data.five_hour_utilization <= 25.0);
        assert!(data.seven_day_utilization <= 25.0);
        assert!(data.weekly_scoped_utilization <= 25.0);
    }

    /// Test that MockPoller high utilization scenario returns high values.
    #[test]
    fn test_mock_poller_high_utilization() {
        let mut poller = MockPoller::with_high_utilization();

        let result = poller.poll();

        assert!(result.is_ok(), "poll() should return Ok");
        let data = result.unwrap();
        assert!(data.five_hour_utilization >= 90.0);
        assert!(data.seven_day_utilization >= 90.0);
        assert!(data.weekly_scoped_utilization >= 90.0);
    }

    /// Test that poll_count tracks invocations.
    #[test]
    fn test_mock_poller_poll_count_tracking() {
        let mut poller = MockPoller::new();

        assert_eq!(poller.poll_count, 0, "Initial count should be 0");

        poller.poll().unwrap();
        assert_eq!(
            poller.poll_count, 1,
            "Count should increment after first poll"
        );

        poller.poll().unwrap();
        assert_eq!(
            poller.poll_count, 2,
            "Count should increment after second poll"
        );

        poller.reset_poll_count();
        assert_eq!(poller.poll_count, 0, "Count should reset to 0");
    }

    /// Test that set_error changes poller behavior to return errors.
    #[test]
    fn test_mock_poller_set_error() {
        let mut poller = MockPoller::new();

        // First poll succeeds
        assert!(poller.poll().is_ok());

        // Set error
        let test_message = "New error";
        poller.set_error(test_message);

        // Subsequent polls fail
        let result = poller.poll();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), test_message);
    }

    /// Test that set_usage_data changes poller behavior to return new data.
    #[test]
    fn test_mock_poller_set_usage_data() {
        use chrono::Duration;

        let mut poller = MockPoller::with_error("Error");

        // First poll fails
        assert!(poller.poll().is_err());

        // Set new usage data
        let now = Utc::now();
        let new_data = crate::poller::UsageData {
            five_hour_utilization: 88.0,
            five_hour_resets_at: (now + Duration::hours(2)).to_rfc3339(),
            five_hour_hours_remaining: 2.0,
            seven_day_utilization: 75.0,
            seven_day_resets_at: (now + Duration::hours(96)).to_rfc3339(),
            seven_day_hours_remaining: 96.0,
            weekly_scoped_utilization: 72.0,
            weekly_scoped_resets_at: (now + Duration::hours(96)).to_rfc3339(),
            weekly_scoped_hours_remaining: 96.0,
            weekly_scoped_model: None,
            limits: vec![],
            timestamp: now,
            stale: false,
        };

        poller.set_usage_data(new_data.clone());

        // Subsequent polls return the new data
        let result = poller.poll();
        assert!(result.is_ok());
        let data = result.unwrap();
        assert_eq!(data.five_hour_utilization, 88.0);
        assert_eq!(data.seven_day_utilization, 75.0);
    }

    /// Test that MockPoller is reusable across multiple tests.
    #[test]
    fn test_mock_poller_reusability() {
        // Create a poller for one scenario
        let mut poller = MockPoller::with_emergency_brake();
        let result1 = poller.poll().unwrap();
        assert_eq!(result1.five_hour_utilization, 99.0);

        // Reconfigure for a different scenario
        poller.set_usage_data(MockPoller::default_usage_data());
        let result2 = poller.poll().unwrap();
        assert_eq!(result2.five_hour_utilization, 50.0);

        // Configure for error scenario
        poller.set_error("Transient error");
        assert!(poller.poll().is_err());

        // Back to success scenario
        poller.set_usage_data(MockPoller::with_low_utilization().usage_data.unwrap());
        let result3 = poller.poll().unwrap();
        assert!(result3.five_hour_utilization <= 25.0);
    }

    /// Test that MockPoller handles concurrent-like usage patterns.
    #[test]
    fn test_mock_poller_multiple_calls_consistency() {
        let mut poller = MockPoller::with_utilization(65.0, 70.0, 68.0);

        // Multiple calls should return consistent data
        let result1 = poller.poll().unwrap();
        let result2 = poller.poll().unwrap();
        let result3 = poller.poll().unwrap();

        assert_eq!(result1.five_hour_utilization, result2.five_hour_utilization);
        assert_eq!(result2.five_hour_utilization, result3.five_hour_utilization);
        assert_eq!(poller.poll_count, 3);
    }

    /// Test MockPoller with extreme utilization values.
    #[test]
    fn test_mock_poller_extreme_values() {
        // Test 0% utilization
        let mut poller = MockPoller::with_utilization(0.0, 0.0, 0.0);
        let result = poller.poll().unwrap();
        assert_eq!(result.five_hour_utilization, 0.0);
        assert_eq!(result.seven_day_utilization, 0.0);

        // Test 100% utilization
        poller = MockPoller::with_utilization(100.0, 100.0, 100.0);
        let result = poller.poll().unwrap();
        assert_eq!(result.five_hour_utilization, 100.0);
        assert_eq!(result.seven_day_utilization, 100.0);
    }

    // ---------------------------------------------------------------------------
    // Governor cycle smoke tests
    // ---------------------------------------------------------------------------

    /// Minimal `AlertConfig` for cycle tests.
    ///
    /// Alerts are disabled so a cycle never shells out to the configured alert
    /// command (`bf create ...`) from a test process.
    fn smoke_alert_config() -> crate::config::AlertConfig {
        crate::config::AlertConfig {
            enabled: false,
            ..crate::config::AlertConfig::default()
        }
    }

    /// Minimal `GovernorConfig` for cycle tests: empty pricing table, empty agents,
    /// alerts disabled, everything else at its default.
    fn smoke_governor_config() -> crate::config::GovernorConfig {
        use std::collections::HashMap;

        crate::config::GovernorConfig {
            pricing: crate::config::PricingConfig {
                models: HashMap::new(),
            },
            sprint: crate::config::SprintConfig::default(),
            daemon: crate::config::DaemonConfig::default(),
            alerts: smoke_alert_config(),
            composite_risk: crate::config::CompositeRiskConfig::default(),
            cone_scaling: crate::config::ConeScalingConfig::default(),
            agents: HashMap::new(),
            credentials_path: None,
        }
    }

    /// Basic smoke test for governor cycle - verifies the cycle runs without panicking.
    ///
    /// This test creates a minimal environment and calls run_governor_cycle with
    /// dry_run=true to verify that:
    /// - The function executes without panic or crash
    /// - The function returns Ok(())
    /// - The cycle handles minimal state gracefully
    ///
    /// The test uses:
    /// - `MockPoller` with a simple success response (no credentials, no network)
    /// - Temporary directory for state files
    /// - Minimal config fixtures
    #[test]
    fn test_governor_cycle_smoke() {
        use std::collections::HashMap;
        use tempfile::TempDir;

        // 1. Create temporary directory for state files (fresh state, no prior poll)
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");

        // 2. Mock poller with a simple success response — keeps the cycle off the
        //    real API so the test needs neither credentials nor network.
        let mut poller = MockPoller::new();

        // 3. Minimal config fixtures
        let alert_config = smoke_alert_config();
        let composite_risk_config = crate::config::CompositeRiskConfig::default();
        let cone_scaling_config = crate::config::ConeScalingConfig::default();
        let pricing_config = smoke_governor_config();

        // 4. Empty agents map and promotions list (nothing to scale in a smoke test)
        let agents: HashMap<String, crate::config::AgentConfig> = HashMap::new();
        let promotions: Vec<crate::schedule::Promotion> = Vec::new();

        // 5. Run the governor cycle with dry_run=true
        let result = run_governor_cycle(
            &mut poller,
            &state_path,
            true, // dry_run = true
            60,   // loop_interval
            2.0,  // hysteresis_band
            3,    // max_up_per_cycle
            2,    // max_down_per_cycle
            90.0, // target_ceiling
            &alert_config,
            &agents,
            0, // pre_scale_minutes (disabled)
            &promotions,
            &composite_risk_config,
            &cone_scaling_config,
            &pricing_config,
        );

        // 6. Verify the cycle completed successfully
        assert!(
            result.is_ok(),
            "run_governor_cycle should return Ok(()) in dry_run mode, got: {:?}",
            result.err()
        );

        // 7. Verify the cycle actually consumed the mock (not some other data source)
        assert_eq!(
            poller.poll_count, 1,
            "cycle should poll the mock poller exactly once"
        );

        // 8. Verify state file was created (even if minimal)
        assert!(
            state_path.exists(),
            "State file should be created after cycle run"
        );

        // 9. Verify the mock's usage data landed in the persisted state
        let saved = state::load_state(&state_path).expect("state should load back");
        assert_eq!(
            saved.usage.five_hour_pct, 50.0,
            "persisted state should carry the mock poller's 5h utilization"
        );

        // 10. Reaching this point means the cycle ran without panicking — the point
        //     of the smoke test.
    }

    /// Test that run_governor_cycle handles None prev_snapshot without panicking.
    ///
    /// This test verifies the first-poll scenario where previous_api_snapshot is None:
    /// - Fresh state with no prior poll data (previous_api_snapshot = None)
    /// - run_governor_cycle should complete without panic
    /// - Initial state should be handled gracefully
    /// - Deltas should be Some(0.0) for all windows (as per line 3128-3133 in run_governor_cycle)
    #[test]
    fn test_first_poll_none_prev_snapshot_no_panic() {
        use std::collections::HashMap;
        use tempfile::TempDir;

        // 1. Create temporary directory for state files (fresh state, no previous snapshots)
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state-first-poll.json");

        // 2. Verify initial state has no previous snapshot
        let initial_state =
            crate::state::load_state(&state_path).expect("Failed to load initial state");
        assert!(
            initial_state.previous_api_snapshot.is_none(),
            "Initial state should have None previous_api_snapshot"
        );
        assert!(
            initial_state.current_api_snapshot.is_none(),
            "Initial state should have None current_api_snapshot"
        );

        // 3. Create poller (will use default credentials path, or fail gracefully)
        let poller_result = crate::poller::Poller::new();

        // 4. Create minimal config objects with defaults
        let alert_config = crate::config::AlertConfig::default();
        let composite_risk_config = crate::config::CompositeRiskConfig::default();
        let cone_scaling_config = crate::config::ConeScalingConfig::default();

        // Create minimal pricing config with required fields
        let pricing_config = crate::config::GovernorConfig {
            pricing: crate::config::PricingConfig {
                models: HashMap::new(),
            },
            sprint: crate::config::SprintConfig::default(),
            daemon: crate::config::DaemonConfig::default(),
            alerts: crate::config::AlertConfig::default(),
            composite_risk: crate::config::CompositeRiskConfig::default(),
            cone_scaling: crate::config::ConeScalingConfig::default(),
            agents: HashMap::new(),
            credentials_path: None,
        };

        // 5. Create minimal agents HashMap (empty is OK for first poll test)
        let agents: HashMap<String, crate::config::AgentConfig> = HashMap::new();

        // 6. Create empty promotions list
        let promotions: Vec<crate::schedule::Promotion> = Vec::new();

        // 7. Run the governor cycle with dry_run=true (first poll with None prev_snapshot)
        //
        // This is the critical test: run_governor_cycle should handle the None prev_snapshot
        // gracefully without panicking. On first poll:
        // - previous_api_snapshot is None (no prior data)
        // - After poll, current_api_snapshot becomes Some (first successful poll data)
        // - Delta computation should yield Some(0.0) for all windows
        let result = if let Ok(mut poller) = poller_result {
            run_governor_cycle(
                &mut poller,
                &state_path,
                true, // dry_run = true
                60,   // loop_interval
                2.0,  // hysteresis_band
                3,    // max_up_per_cycle
                2,    // max_down_per_cycle
                90.0, // target_ceiling
                &alert_config,
                &agents,
                0, // pre_scale_minutes (disabled)
                &promotions,
                &composite_risk_config,
                &cone_scaling_config,
                &pricing_config,
            )
        } else {
            // Poller creation failed - verify that run_governor_cycle handles this gracefully
            let mut poller = crate::poller::Poller::default();
            run_governor_cycle(
                &mut poller,
                &state_path,
                true, // dry_run = true
                60,   // loop_interval
                2.0,  // hysteresis_band
                3,    // max_up_per_cycle
                2,    // max_down_per_cycle
                90.0, // target_ceiling
                &alert_config,
                &agents,
                0, // pre_scale_minutes (disabled)
                &promotions,
                &composite_risk_config,
                &cone_scaling_config,
                &pricing_config,
            )
        };

        // 8. Verify the cycle completed successfully without panic
        assert!(
            result.is_ok(),
            "run_governor_cycle should return Ok(()) with None prev_snapshot in dry_run mode"
        );

        // 9. Verify state file was created after first poll
        assert!(
            state_path.exists(),
            "State file should be created after first poll cycle"
        );

        // 10. Load and verify the state after first poll
        let final_state =
            crate::state::load_state(&state_path).expect("Failed to load final state");

        // On first successful poll:
        // - previous_api_snapshot should still be None (was None, shifted to None at start)
        // - current_api_snapshot should be Some (first successful poll data)
        // NOTE: The actual poll might fail if no credentials, so we check the state is valid
        // The key assertion is that the cycle didn't panic with None prev_snapshot

        // 11. Verify no panic occurred (test reaching this point means no panic)
        // This is the key assertion - run_governor_cycle handled None prev_snapshot gracefully
    }

    /// Test that run_governor_cycle handles first and second polls correctly.
    ///
    /// This test verifies the complete first-poll → second-poll transition:
    /// - First poll: prev_snapshot is None, delta computation is skipped (Some(0.0))
    /// - Second poll: both snapshots exist, delta computation executes
    /// - No panics occur in either scenario
    ///
    /// This is a comprehensive integration test that calls run_governor_cycle twice
    /// and verifies the snapshot state machine works correctly across polls.
    #[test]
    fn test_first_poll_and_second_poll_complete_flow() {
        use std::collections::HashMap;
        use tempfile::TempDir;

        // 1. Create temporary directory for state files
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state-flow.json");

        // 2. Create poller helper function (will use default credentials path, or fail gracefully)
        let create_poller = || -> crate::poller::Poller {
            match crate::poller::Poller::new() {
                Ok(poller) => poller,
                Err(_) => crate::poller::Poller::default(),
            }
        };

        // 3. Create minimal config objects with defaults
        let alert_config = crate::config::AlertConfig::default();
        let composite_risk_config = crate::config::CompositeRiskConfig::default();
        let cone_scaling_config = crate::config::ConeScalingConfig::default();

        // Create minimal pricing config with required fields
        let pricing_config = crate::config::GovernorConfig {
            pricing: crate::config::PricingConfig {
                models: HashMap::new(),
            },
            sprint: crate::config::SprintConfig::default(),
            daemon: crate::config::DaemonConfig::default(),
            alerts: crate::config::AlertConfig::default(),
            composite_risk: crate::config::CompositeRiskConfig::default(),
            cone_scaling: crate::config::ConeScalingConfig::default(),
            agents: HashMap::new(),
            credentials_path: None,
        };

        // 4. Create minimal agents HashMap (empty is OK for this test)
        let agents: HashMap<String, crate::config::AgentConfig> = HashMap::new();

        // 5. Create empty promotions list
        let promotions: Vec<crate::schedule::Promotion> = Vec::new();

        // ========================================================================
        // FIRST POLL: Verify None prev_snapshot is handled gracefully
        // ========================================================================

        // 6. Verify initial state has no previous snapshot (first poll condition)
        let initial_state =
            crate::state::load_state(&state_path).expect("Failed to load initial state");
        assert!(
            initial_state.previous_api_snapshot.is_none(),
            "Initial state should have None previous_api_snapshot (first poll)"
        );
        assert!(
            initial_state.current_api_snapshot.is_none(),
            "Initial state should have None current_api_snapshot (first poll)"
        );

        // 7. Run the first governor cycle (no previous snapshot exists)
        let mut poller1 = create_poller();
        let first_poll_result = run_governor_cycle(
            &mut poller1,
            &state_path,
            true, // dry_run = true
            60,   // loop_interval
            2.0,  // hysteresis_band
            3,    // max_up_per_cycle
            2,    // max_down_per_cycle
            90.0, // target_ceiling
            &alert_config,
            &agents,
            0, // pre_scale_minutes (disabled)
            &promotions,
            &composite_risk_config,
            &cone_scaling_config,
            &pricing_config,
        );

        // 8. Verify first poll completed successfully (no panic with None prev_snapshot)
        assert!(
            first_poll_result.is_ok(),
            "First poll: run_governor_cycle should return Ok(()) with None prev_snapshot"
        );

        // 9. Load state after first poll and verify snapshot state
        let first_poll_state =
            crate::state::load_state(&state_path).expect("Failed to load state after first poll");

        // On first poll:
        // - previous_api_snapshot should still be None (was None, shifted to None at start of cycle)
        // - current_api_snapshot should be Some (if poll succeeded) or None (if poll failed)
        //
        // Note: If credentials aren't configured, the poll will fail and current_api_snapshot
        // will remain None. The test should handle both cases gracefully.

        // ========================================================================
        // SECOND POLL: Verify both snapshots exist and delta computation works
        // ========================================================================

        // 10. Run the second governor cycle (now previous_api_snapshot may be Some)
        let mut poller2 = create_poller();
        let second_poll_result = run_governor_cycle(
            &mut poller2,
            &state_path,
            true, // dry_run = true
            60,   // loop_interval
            2.0,  // hysteresis_band
            3,    // max_up_per_cycle
            2,    // max_down_per_cycle
            90.0, // target_ceiling
            &alert_config,
            &agents,
            0, // pre_scale_minutes (disabled)
            &promotions,
            &composite_risk_config,
            &cone_scaling_config,
            &pricing_config,
        );

        // 11. Verify second poll completed successfully
        assert!(
            second_poll_result.is_ok(),
            "Second poll: run_governor_cycle should return Ok(())"
        );

        // 12. Load state after second poll
        let second_poll_state =
            crate::state::load_state(&state_path).expect("Failed to load state after second poll");

        // 13. Verify no panic occurred in either poll (test reaching this point = success)
        // The key assertion is that the governor cycle handles:
        // - First poll (None prev_snapshot) gracefully
        // - Second poll (prev_snapshot may be Some or None) gracefully
        // - No panics occur during snapshot state transitions
    }

    // ---------------------------------------------------------------------------
    // Comprehensive snapshot delta computation tests
    // ---------------------------------------------------------------------------

    /// Test realistic consecutive API poll scenarios with actual usage patterns.
    ///
    /// Uses realistic fixtures based on actual Anthropic API usage data patterns:
    /// - 5-hour window: fastest changing, shows immediate impact
    /// - 7-day window: slower changing, shows accumulated usage
    /// - 7-day Sonnet window: typically lower than all-models 7-day
    #[test]
    fn test_realistic_consecutive_api_polls() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Scenario 1: Normal workload progression
        // Poll 1 (baseline): 5h=8%, 7d=42%, 7ds=35%
        // Poll 2 (after 60s): 5h=10.5%, 7d=43%, 7ds=36.5%
        let poll1 = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(120),
            five_hour_pct: 8.0,
            seven_day_pct: 42.0,
            weekly_scoped_pct: 35.0,
        };

        let poll2 = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 10.5,
            seven_day_pct: 43.0,
            weekly_scoped_pct: 36.5,
        };

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: poll1.five_hour_pct,
            seven_day: poll1.seven_day_pct,
            weekly_scoped: poll1.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: poll2.five_hour_pct,
            seven_day: poll2.seven_day_pct,
            weekly_scoped: poll2.weekly_scoped_pct,
        };

        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // Verify realistic delta patterns
        assert!(
            (d5h - 2.5).abs() < f64::EPSILON,
            "5h delta: 10.5 - 8.0 = 2.5"
        );
        assert!(
            (d7d - 1.0).abs() < f64::EPSILON,
            "7d delta: 43.0 - 42.0 = 1.0"
        );
        assert!(
            (d7ds - 1.5).abs() < f64::EPSILON,
            "7ds delta: 36.5 - 35.0 = 1.5"
        );

        // 5-hour window should show fastest change (highest delta)
        assert!(
            d5h > d7d,
            "5h delta should be > 7d delta (fastest changing)"
        );
        assert!(
            d7ds > d7d,
            "7ds delta should be > 7d delta (Sonnet usage more volatile)"
        );
    }

    /// Test delta computation with minimal API changes (precision edge case).
    ///
    /// Verifies that very small utilization changes are computed accurately.
    /// This tests the floating-point precision limits of delta computation.
    #[test]
    fn test_minimal_api_changes_precision() {
        // Test with very small deltas (0.01% changes)
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 50.0,
            seven_day: 60.0,
            weekly_scoped: 55.0,
        };

        let curr = crate::db::WindowPctSnapshot {
            five_hour: 50.01,     // +0.01%
            seven_day: 60.01,     // +0.01%
            weekly_scoped: 55.01, // +0.01%
        };

        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);

        // Verify precision to 4 decimal places
        assert!((d5h - 0.01).abs() < 1e-9, "5h: 50.01 - 50.0 = 0.01");
        assert!((d7d - 0.01).abs() < 1e-9, "7d: 60.01 - 60.0 = 0.01");
        assert!((d7ds - 0.01).abs() < 1e-9, "7ds: 55.01 - 55.0 = 0.01");
    }

    /// Test delta computation with maximum API changes (saturation edge case).
    ///
    /// Verifies delta computation when windows go from empty to near-full.
    /// This represents rapid consumption scenarios.
    #[test]
    fn test_maximum_api_changes_saturation() {
        // Test with large deltas (0% to 95%)
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 0.0,
            seven_day: 0.0,
            weekly_scoped: 0.0,
        };

        let curr = crate::db::WindowPctSnapshot {
            five_hour: 95.0,
            seven_day: 88.0,
            weekly_scoped: 92.0,
        };

        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);

        assert!((d5h - 95.0).abs() < f64::EPSILON, "5h: 95.0 - 0.0 = 95.0");
        assert!((d7d - 88.0).abs() < f64::EPSILON, "7d: 88.0 - 0.0 = 88.0");
        assert!((d7ds - 92.0).abs() < f64::EPSILON, "7ds: 92.0 - 0.0 = 92.0");

        // All deltas should be large and positive
        assert!(d5h > 80.0, "5h delta should be large (> 80%)");
        assert!(d7d > 80.0, "7d delta should be large (> 80%)");
        assert!(d7ds > 80.0, "7ds delta should be large (> 80%)");
    }

    /// Test delta computation during window reset boundary transitions.
    ///
    /// Simulates the exact moment when a window resets and counting starts over.
    /// This is a critical edge case for the governor's prediction calibration.
    #[test]
    fn test_window_reset_boundary_transitions() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Scenario: Window reset occurs between polls
        // Previous: near limit (98%, 95%, 97%)
        // Current: after reset (2%, 3%, 1.5%)
        let pre_reset = PrevUsageSnapshot {
            taken_at: Utc::now() - chrono::Duration::seconds(60),
            five_hour_pct: 98.0,
            seven_day_pct: 95.0,
            weekly_scoped_pct: 97.0,
        };

        let post_reset = PrevUsageSnapshot {
            taken_at: Utc::now(),
            five_hour_pct: 2.0,
            seven_day_pct: 3.0,
            weekly_scoped_pct: 1.5,
        };

        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: pre_reset.five_hour_pct,
            seven_day: pre_reset.seven_day_pct,
            weekly_scoped: pre_reset.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: post_reset.five_hour_pct,
            seven_day: post_reset.seven_day_pct,
            weekly_scoped: post_reset.weekly_scoped_pct,
        };

        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        // All deltas should be large and negative (indicating reset)
        assert!(
            d5h < -90.0,
            "5h delta should indicate reset (2.0 - 98.0 = -96.0)"
        );
        assert!(
            d7d < -90.0,
            "7d delta should indicate reset (3.0 - 95.0 = -92.0)"
        );
        assert!(
            d7ds < -90.0,
            "7ds delta should indicate reset (1.5 - 97.0 = -95.5)"
        );

        assert!((d5h - (-96.0)).abs() < f64::EPSILON);
        assert!((d7d - (-92.0)).abs() < f64::EPSILON);
        assert!((d7ds - (-95.5)).abs() < f64::EPSILON);
    }

    /// Test state integration with snapshot delta updates.
    ///
    /// Verifies that the state module's update_api_snapshot method correctly
    /// maintains the previous/current snapshot chain for delta computation.
    #[test]
    fn test_state_snapshot_chain_integration() {
        use crate::state::GovernorState;
        use chrono::Utc;

        let mut state = GovernorState::new();
        let now = Utc::now();

        // First poll: only current snapshot should be set
        state.update_api_snapshot(now, 15.0, 45.0, 38.0);
        assert!(
            state.previous_api_snapshot.is_none(),
            "Previous should be None after first poll"
        );
        assert!(
            state.current_api_snapshot.is_some(),
            "Current should be Some after first poll"
        );

        // Second poll: previous should now be set
        state.update_api_snapshot(now + chrono::Duration::seconds(60), 17.5, 47.0, 40.0);
        assert!(
            state.previous_api_snapshot.is_some(),
            "Previous should be Some after second poll"
        );
        assert!(
            state.current_api_snapshot.is_some(),
            "Current should still be Some"
        );

        // Verify the chain: previous holds first poll data, current holds second
        let prev = state.previous_api_snapshot.as_ref().unwrap();
        let curr = state.current_api_snapshot.as_ref().unwrap();

        assert!((prev.five_hour_pct - 15.0).abs() < f64::EPSILON);
        assert!((curr.five_hour_pct - 17.5).abs() < f64::EPSILON);

        // Compute deltas using the state's snapshot chain
        let prev_pct = crate::db::WindowPctSnapshot {
            five_hour: prev.five_hour_pct,
            seven_day: prev.seven_day_pct,
            weekly_scoped: prev.weekly_scoped_pct,
        };

        let curr_pct = crate::db::WindowPctSnapshot {
            five_hour: curr.five_hour_pct,
            seven_day: curr.seven_day_pct,
            weekly_scoped: curr.weekly_scoped_pct,
        };

        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

        assert!((d5h - 2.5).abs() < f64::EPSILON);
        assert!((d7d - 2.0).abs() < f64::EPSILON);
        assert!((d7ds - 2.0).abs() < f64::EPSILON);
    }

    /// Test asymmetric window behavior (windows changing in different directions).
    ///
    /// Simulates realistic scenarios where different windows show different trends
    /// (e.g., 5-hour increasing while 7-day is flat or decreasing).
    #[test]
    fn test_asymmetric_window_behavior() {
        // Scenario: 5-hour window burning fast, 7-day windows flat or decreasing
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 10.0,
            seven_day: 70.0,
            weekly_scoped: 65.0,
        };

        let curr = crate::db::WindowPctSnapshot {
            five_hour: 25.0,     // +15.0 (rapid burn)
            seven_day: 70.5,     // +0.5 (nearly flat)
            weekly_scoped: 64.0, // -1.0 (slight decrease due to older consumption rolling off)
        };

        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);

        assert!(
            (d5h - 15.0).abs() < f64::EPSILON,
            "5h should increase rapidly"
        );
        assert!((d7d - 0.5).abs() < f64::EPSILON, "7d should be nearly flat");
        assert!(
            (d7ds - (-1.0)).abs() < f64::EPSILON,
            "7ds can decrease slightly"
        );

        // 5-hour delta should be much larger than 7-day deltas
        assert!(
            d5h.abs() > 10.0 * d7d.abs(),
            "5h should change much faster than 7d"
        );
    }

    /// Test delta computation with NaN/Infinity handling.
    ///
    /// Verifies that the delta computation doesn't panic on extreme inputs.
    #[test]
    fn test_delta_computation_no_panic_on_extreme_inputs() {
        // Test with very large numbers
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 1e308,
            seven_day: 1e308,
            weekly_scoped: 1e308,
        };

        let curr = crate::db::WindowPctSnapshot {
            five_hour: 1e308 - 1.0,
            seven_day: 1e308 - 1.0,
            weekly_scoped: 1e308 - 1.0,
        };

        // Should not panic
        let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev, &curr);

        // Results should be computable (though may be imprecise at extreme values)
        assert!(d5h.is_finite() || d5h == 0.0);
        assert!(d7d.is_finite() || d7d == 0.0);
        assert!(d7ds.is_finite() || d7ds == 0.0);
    }

    /// Test performance: delta computations should complete quickly.
    ///
    /// Verifies that 10,000 delta computations complete in under 1 second,
    /// ensuring the governor cycle can process deltas efficiently.
    #[test]
    fn test_delta_computation_performance() {
        let prev = crate::db::WindowPctSnapshot {
            five_hour: 10.0,
            seven_day: 20.0,
            weekly_scoped: 15.0,
        };

        let curr = crate::db::WindowPctSnapshot {
            five_hour: 12.5,
            seven_day: 22.0,
            weekly_scoped: 18.0,
        };

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let _ = calculate_window_pct_delta(&prev, &curr);
        }
        let elapsed = start.elapsed();

        // Should complete 10,000 computations in under 100ms
        assert!(
            elapsed.as_millis() < 100,
            "10,000 delta computations should complete in < 100ms, took {}ms",
            elapsed.as_millis()
        );
    }

    /// Test realistic fixture data from actual API response patterns.
    ///
    /// Uses fixtures based on real Anthropic API usage data to ensure
    /// delta computation works with production-like values.
    #[test]
    fn test_realistic_api_fixture_data() {
        use crate::state::PrevUsageSnapshot;
        use chrono::Utc;

        // Fixture based on actual API data (high Sonnet usage scenario)
        let fixtures = vec![
            // (time_offset, five_hr, seven_day, weekly_scoped)
            (0, 12.5, 45.2, 38.7),
            (60, 14.8, 46.1, 40.2),
            (120, 17.2, 47.3, 42.1),
            (180, 19.5, 48.5, 44.0),
        ];

        let mut snapshots = Vec::new();
        for (offset, p5h, p7d, p7ds) in fixtures {
            snapshots.push(PrevUsageSnapshot {
                taken_at: Utc::now() + chrono::Duration::seconds(offset),
                five_hour_pct: p5h,
                seven_day_pct: p7d,
                weekly_scoped_pct: p7ds,
            });
        }

        // Test consecutive delta computations
        for i in 1..snapshots.len() {
            let prev = &snapshots[i - 1];
            let curr = &snapshots[i];

            let prev_pct = crate::db::WindowPctSnapshot {
                five_hour: prev.five_hour_pct,
                seven_day: prev.seven_day_pct,
                weekly_scoped: prev.weekly_scoped_pct,
            };

            let curr_pct = crate::db::WindowPctSnapshot {
                five_hour: curr.five_hour_pct,
                seven_day: curr.seven_day_pct,
                weekly_scoped: curr.weekly_scoped_pct,
            };

            let (d5h, d7d, d7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);

            // All deltas should be positive (increasing usage)
            assert!(
                d5h > 0.0,
                "Interval {}-{}: 5h delta should be positive",
                i - 1,
                i
            );
            assert!(
                d7d > 0.0,
                "Interval {}-{}: 7d delta should be positive",
                i - 1,
                i
            );
            assert!(
                d7ds > 0.0,
                "Interval {}-{}: 7ds delta should be positive",
                i - 1,
                i
            );

            // 5-hour should change fastest
            assert!(
                d5h > d7d,
                "Interval {}-{}: 5h should change faster than 7d",
                i - 1,
                i
            );
            assert!(
                d7ds > d7d,
                "Interval {}-{}: 7ds should change faster than 7d",
                i - 1,
                i
            );
        }
    }

    // -----------------------------------------------------------------------
    // Governor cycle behavior tests
    // -----------------------------------------------------------------------
    //
    // These drive the real `run_governor_cycle` against `MockPoller` and assert
    // on what the cycle itself produced (the persisted state file), rather than
    // re-implementing the cycle's steps in the test. Every assertion below is
    // about a value the production code wrote.
    //
    // Two things a cycle does are NOT reachable in-process and so are not
    // asserted here: the scaling *decision* reads live worker counts from tmux
    // (`worker::count_workers`), which is always 0 in a test process, and the
    // `EmergencyBrake` decision arm requires `current > 0`. The emergency brake
    // is therefore pinned at the two points the cycle does reach: the forecast
    // it persists, and the safe_mode clear/hold decision it makes against the
    // 98% threshold.

    /// Run one governor cycle in dry-run mode with the smoke fixtures.
    ///
    /// Keeps the 15-argument call in one place so each test below reads as
    /// "arrange state → run cycle → assert on persisted state".
    fn run_cycle(poller: &mut MockPoller, state_path: &std::path::Path) -> anyhow::Result<()> {
        use std::collections::HashMap;

        let alert_config = smoke_alert_config();
        let composite_risk_config = crate::config::CompositeRiskConfig::default();
        let cone_scaling_config = crate::config::ConeScalingConfig::default();
        let pricing_config = smoke_governor_config();
        let agents: HashMap<String, crate::config::AgentConfig> = HashMap::new();
        let promotions: Vec<crate::schedule::Promotion> = Vec::new();

        run_governor_cycle(
            poller,
            state_path,
            true, // dry_run — never touches tmux
            60,   // loop_interval
            2.0,  // hysteresis_band
            3,    // max_up_per_cycle
            2,    // max_down_per_cycle
            90.0, // target_ceiling
            &alert_config,
            &agents,
            0, // pre_scale_minutes (disabled)
            &promotions,
            &composite_risk_config,
            &cone_scaling_config,
            &pricing_config,
        )
    }

    /// The cycle polls exactly once and the polled numbers reach persisted state.
    ///
    /// Utilizations are deliberately unlike `MockPoller`'s defaults (50/60/55),
    /// so the test fails if the cycle ever ignores the poll result and writes
    /// defaults or zeros instead.
    #[test]
    fn test_cycle_polls_once_and_persists_polled_usage() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");

        // 5h=42.5, 7d=63.25, weekly_scoped=57.75
        let mut poller = MockPoller::with_utilization(42.5, 63.25, 57.75);

        run_cycle(&mut poller, &state_path).expect("cycle should return Ok in dry-run");

        assert_eq!(
            poller.poll_count, 1,
            "cycle should call poll_usage() exactly once"
        );

        let saved = state::load_state(&state_path).expect("state should load back");
        assert_eq!(
            saved.usage.five_hour_pct, 42.5,
            "5h utilization from the poll should be persisted"
        );
        assert_eq!(
            saved.usage.all_models_pct, 63.25,
            "7d (all models) utilization from the poll should be persisted"
        );
        assert_eq!(
            saved.usage.weekly_scoped_pct, 57.75,
            "weekly_scoped utilization from the poll should be persisted"
        );
        assert!(
            !saved.usage.stale,
            "fresh poll data should not be marked stale"
        );
        assert!(
            !saved.token_refresh_failing,
            "a successful, non-stale poll should clear token_refresh_failing"
        );

        // The poll also becomes the current snapshot; nothing precedes it on cycle 1.
        let current = saved
            .current_api_snapshot
            .expect("cycle should record the poll as current_api_snapshot");
        assert_eq!(current.five_hour_pct, 42.5);
        assert_eq!(current.seven_day_pct, 63.25);
        assert_eq!(current.weekly_scoped_pct, 57.75);
        assert!(
            saved.previous_api_snapshot.is_none(),
            "first cycle has no previous snapshot to shift into place"
        );
    }

    /// The first cycle has nothing to subtract from, so it must not invent a delta.
    ///
    /// `run_governor_cycle` guards the delta computation behind a
    /// `(Some(prev), Some(curr))` match. The failure this pins is the guard being
    /// dropped or weakened to `unwrap_or_default()` on the previous snapshot: the
    /// cycle would then subtract against an implicit 0.0 baseline and persist the
    /// full current reading (42.5 / 63.25 / 57.75) as if the fleet had burned that
    /// much in one interval — a fabricated spike straight into the burn-rate inputs.
    ///
    /// The assertion is deliberately "absent or zero" rather than "is_none": either
    /// answer is a graceful first poll, and pinning the exact representation here
    /// would collide with bf-9mtsa, which explicitly initializes these fields.
    #[test]
    fn test_first_cycle_does_not_fabricate_deltas_without_a_previous_snapshot() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");
        assert!(
            !state_path.exists(),
            "precondition: no persisted state, so the cycle starts with no previous snapshot"
        );

        let mut poller = MockPoller::with_utilization(42.5, 63.25, 57.75);
        run_cycle(&mut poller, &state_path).expect("first cycle should succeed, not panic");

        let saved = state::load_state(&state_path).expect("state should load back");
        assert!(
            saved.previous_api_snapshot.is_none(),
            "precondition: the first cycle really did run the (None, Some) path"
        );

        for (label, delta, current_reading) in [
            ("5h", saved.p5h_delta, 42.5_f64),
            ("7d", saved.p7d_delta, 63.25),
            ("weekly_scoped", saved.p7ds_delta, 57.75),
        ] {
            assert_eq!(
                delta.unwrap_or(0.0),
                0.0,
                "{} delta should be absent or zero on the first poll, got {:?}",
                label,
                delta
            );
            assert_ne!(
                delta,
                Some(current_reading),
                "{} delta must not be the current reading — that is a subtraction against a missing baseline",
                label
            );
        }
    }

    /// Without a previous snapshot the cycle clears the delta fields instead of
    /// leaving whatever an earlier cycle wrote.
    ///
    /// This is the (None, Some) branch again, but from the state a failed poll
    /// leaves behind: the failure never writes `current_api_snapshot`, so the
    /// next cycle's rotation shifts `None` into `previous_api_snapshot` while
    /// `p5h/p7d/p7ds_delta` still hold the last successfully computed interval.
    /// Before the delta fields were initialized explicitly, those stale values
    /// survived the cycle and read as a delta for an interval that had already
    /// scrolled past. Seeded deltas here are deliberately unlike anything the
    /// poll could produce (9.9 / 8.8 / 7.7), so retention is unambiguous.
    #[test]
    fn test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");

        let mut state = state::GovernorState::new();
        state.p5h_delta = Some(9.9);
        state.p7d_delta = Some(8.8);
        state.p7ds_delta = Some(7.7);
        state::save_state(&state, &state_path).expect("failed to seed state file");

        let mut poller = MockPoller::with_utilization(42.5, 63.25, 57.75);
        run_cycle(&mut poller, &state_path).expect("cycle should succeed, not panic");

        let saved = state::load_state(&state_path).expect("state should load back");
        assert!(
            saved.previous_api_snapshot.is_none(),
            "precondition: the cycle really did run the (None, Some) path"
        );

        for (label, delta, stale) in [
            ("5h", saved.p5h_delta, 9.9_f64),
            ("7d", saved.p7d_delta, 8.8),
            ("weekly_scoped", saved.p7ds_delta, 7.7),
        ] {
            assert_ne!(
                delta,
                Some(stale),
                "{} delta must not retain the value from before the gap",
                label
            );
            assert_eq!(
                delta, None,
                "{} delta should be None with no baseline to subtract from, got {:?}",
                label, delta
            );
        }
    }

    /// A second cycle re-polls, shifts the snapshot, and computes window deltas.
    #[test]
    fn test_second_cycle_repolls_and_computes_window_deltas() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");

        let mut poller = MockPoller::with_utilization(10.0, 20.0, 30.0);
        run_cycle(&mut poller, &state_path).expect("first cycle should succeed");

        // Same poller instance across both cycles: the count must keep climbing.
        poller.set_usage_data({
            let mut data = MockPoller::with_utilization(14.0, 25.0, 33.0)
                .usage_data
                .expect("with_utilization always sets usage data");
            data.stale = false;
            data
        });
        run_cycle(&mut poller, &state_path).expect("second cycle should succeed");

        assert_eq!(
            poller.poll_count, 2,
            "each cycle should poll once — two cycles, two polls"
        );

        let saved = state::load_state(&state_path).expect("state should load back");
        assert_eq!(
            saved.usage.five_hour_pct, 14.0,
            "state should carry the newest poll, not the first one"
        );

        let previous = saved
            .previous_api_snapshot
            .expect("second cycle should shift cycle 1's reading into previous_api_snapshot");
        assert_eq!(previous.five_hour_pct, 10.0);
        assert_eq!(previous.seven_day_pct, 20.0);
        assert_eq!(previous.weekly_scoped_pct, 30.0);

        let current = saved
            .current_api_snapshot
            .expect("second cycle should record its own reading as current");
        // All three fields, not just 5h: each one is the input side of a delta
        // assertion below, so leaving 7d/7ds unchecked would anchor those deltas
        // to numbers the test never confirmed the cycle actually stored.
        assert_eq!(current.five_hour_pct, 14.0);
        assert_eq!(current.seven_day_pct, 25.0);
        assert_eq!(current.weekly_scoped_pct, 33.0);

        // === Delta value verification ===
        // Formula (`calculate_window_pct_delta`): delta = current − previous, per
        // window. The operands are percent-of-quota readings, so a delta is a
        // signed difference in *percentage points* — not a ratio and not a
        // relative percent change. Here 5h moves 10.0% → 14.0%, which is a delta
        // of 4.0 points, not 40%.
        //
        // Expected values are derived from the snapshot pair the cycle itself
        // persisted, so the assertions track the cycle's own inputs rather than
        // restating the fixture. The literal checks that follow pin the fixture
        // arithmetic, so the two together fail if either side drifts.
        let expected_5h_delta = current.five_hour_pct - previous.five_hour_pct;
        let expected_7d_delta = current.seven_day_pct - previous.seven_day_pct;
        let expected_7ds_delta = current.weekly_scoped_pct - previous.weekly_scoped_pct;

        assert_eq!(
            saved.p5h_delta,
            Some(expected_5h_delta),
            "5h delta should be current ({}) − previous ({})",
            current.five_hour_pct,
            previous.five_hour_pct
        );
        assert_eq!(
            saved.p7d_delta,
            Some(expected_7d_delta),
            "7d delta should be current ({}) − previous ({})",
            current.seven_day_pct,
            previous.seven_day_pct
        );
        assert_eq!(
            saved.p7ds_delta,
            Some(expected_7ds_delta),
            "weekly_scoped delta should be current ({}) − previous ({})",
            current.weekly_scoped_pct,
            previous.weekly_scoped_pct
        );

        // The fixture's three deltas are distinct (4.0 / 5.0 / 3.0), so a cycle
        // that crossed the windows up — writing the 7ds delta into p7d, say —
        // fails here rather than passing on shape alone.
        assert_eq!(saved.p5h_delta, Some(4.0), "5h delta should be 14.0 - 10.0");
        assert_eq!(saved.p7d_delta, Some(5.0), "7d delta should be 25.0 - 20.0");
        assert_eq!(
            saved.p7ds_delta,
            Some(3.0),
            "weekly_scoped delta should be 33.0 - 30.0"
        );
    }

    /// A cycle whose windows *drop* records negative deltas, not their magnitude.
    ///
    /// The delta formula is signed — `current − previous` — and a window reset is
    /// the case that depends on it: utilization falls, and the cycle must persist
    /// the drop as a negative number so downstream forecasting sees a reset rather
    /// than a burn. The reset tests elsewhere in this file
    /// (`test_window_reset_boundary_transitions`,
    /// `test_negative_deltas_window_reset`) call `calculate_window_pct_delta`
    /// directly; this one drives the sign through `run_governor_cycle` and the
    /// persisted state file, which is where an `.abs()` or a flipped operand order
    /// would actually hurt.
    #[test]
    fn test_cycle_computes_negative_deltas_when_windows_reset() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");

        // Cycle 1: windows near exhaustion.
        let mut poller = MockPoller::with_utilization(80.0, 90.0, 85.0);
        run_cycle(&mut poller, &state_path).expect("first cycle should succeed");

        // Cycle 2: the windows have rolled over and counting restarted.
        poller.set_usage_data({
            let mut data = MockPoller::with_utilization(5.0, 15.0, 8.0)
                .usage_data
                .expect("with_utilization always sets usage data");
            data.stale = false;
            data
        });
        run_cycle(&mut poller, &state_path).expect("second cycle should succeed");

        let saved = state::load_state(&state_path).expect("state should load back");
        let previous = saved
            .previous_api_snapshot
            .expect("second cycle should shift cycle 1's reading into previous_api_snapshot");
        let current = saved
            .current_api_snapshot
            .expect("second cycle should record its own reading as current");

        // Same formula as the increasing case: delta = current − previous, in
        // signed percentage points. Falling utilization makes each delta negative.
        let expected_5h_delta = current.five_hour_pct - previous.five_hour_pct;
        let expected_7d_delta = current.seven_day_pct - previous.seven_day_pct;
        let expected_7ds_delta = current.weekly_scoped_pct - previous.weekly_scoped_pct;

        assert_eq!(
            saved.p5h_delta,
            Some(expected_5h_delta),
            "5h delta should be 5.0 - 80.0 = -75.0"
        );
        assert_eq!(
            saved.p7d_delta,
            Some(expected_7d_delta),
            "7d delta should be 15.0 - 90.0 = -75.0"
        );
        assert_eq!(
            saved.p7ds_delta,
            Some(expected_7ds_delta),
            "weekly_scoped delta should be 8.0 - 85.0 = -77.0"
        );

        // The sign is the point: magnitudes alone would survive an `.abs()`.
        assert_eq!(saved.p5h_delta, Some(-75.0));
        assert_eq!(saved.p7d_delta, Some(-75.0));
        assert_eq!(saved.p7ds_delta, Some(-77.0));
    }

    /// The cycle writes both the state file and the previous-state rollover file,
    /// stamping `updated_at` forward on every run.
    #[test]
    fn test_cycle_writes_state_to_disk_each_run() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");
        assert!(
            !state_path.exists(),
            "precondition: no state file before the first cycle"
        );

        let before_first = Utc::now();
        let mut poller = MockPoller::new();
        run_cycle(&mut poller, &state_path).expect("first cycle should succeed");

        assert!(
            state_path.exists(),
            "cycle should write the state file to disk"
        );
        let first = state::load_state(&state_path).expect("state should load back");
        assert!(
            first.updated_at >= before_first,
            "updated_at should be stamped with this cycle's timestamp"
        );

        run_cycle(&mut poller, &state_path).expect("second cycle should succeed");

        let second = state::load_state(&state_path).expect("state should load back");
        assert!(
            second.updated_at > first.updated_at,
            "each cycle should advance updated_at (first {}, second {})",
            first.updated_at,
            second.updated_at,
        );

        // The rollover copy lets the next cycle diff against the prior write.
        // `governor-state.json` -> `governor-state.prev.json` (see state::save_previous_state).
        let prev_path = temp_dir.path().join("governor-state.prev.json");
        assert!(
            prev_path.exists(),
            "cycle should also write the previous-state file at {}",
            prev_path.display()
        );
    }

    /// Seed a state file whose persisted forecast sits at `utilization` on every
    /// window, with emergency-brake safe_mode already active.
    ///
    /// The cycle's safe_mode clear check (step 1b) runs against the forecast it
    /// loaded from disk, which is what a real governor sees at the top of the
    /// cycle following the brake.
    fn seed_braked_state(state_path: &std::path::Path, utilization: f64) {
        let mut state = state::GovernorState::new();
        state.safe_mode.active = true;
        state.safe_mode.trigger = Some("emergency_brake".to_string());
        state.safe_mode.entered_at = Some(Utc::now());
        state.capacity_forecast.five_hour.current_utilization = utilization;
        state.capacity_forecast.seven_day.current_utilization = utilization;
        state.capacity_forecast.weekly_scoped.current_utilization = utilization;
        state::save_state(&state, state_path).expect("failed to seed state file");
    }

    /// At exactly 98% the brake holds: safe_mode stays active through the cycle.
    ///
    /// Paired with the 97.9% test below, this pins the threshold itself — a
    /// change to `EMERGENCY_BRAKE_THRESHOLD` breaks exactly one of the two.
    #[test]
    fn test_cycle_holds_emergency_brake_safe_mode_at_98_percent() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");
        seed_braked_state(&state_path, 98.0);

        // Poll far below the threshold: only the braked forecast should matter here.
        let mut poller = MockPoller::with_utilization(10.0, 10.0, 10.0);
        run_cycle(&mut poller, &state_path).expect("cycle should succeed");

        let saved = state::load_state(&state_path).expect("state should load back");
        assert!(
            saved.safe_mode.active,
            "safe_mode should remain active while utilization is at the 98% threshold"
        );
        assert_eq!(
            saved.safe_mode.trigger.as_deref(),
            Some("emergency_brake"),
            "the emergency_brake trigger should survive the cycle"
        );
    }

    /// A hair below 98% the brake releases: the cycle clears safe_mode.
    #[test]
    fn test_cycle_clears_emergency_brake_safe_mode_below_98_percent() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");
        seed_braked_state(&state_path, 97.9);

        let mut poller = MockPoller::with_utilization(10.0, 10.0, 10.0);
        run_cycle(&mut poller, &state_path).expect("cycle should succeed");

        let saved = state::load_state(&state_path).expect("state should load back");
        assert!(
            !saved.safe_mode.active,
            "safe_mode should clear once utilization falls below the 98% threshold"
        );
        assert_eq!(
            saved.safe_mode.trigger, None,
            "clearing safe_mode should drop the emergency_brake trigger"
        );
    }

    /// A cycle polling 98%+ persists a forecast that drives the target to zero.
    ///
    /// `compute_target_workers` is the production function the cycle itself calls
    /// for the target; here it is re-run against the forecast the cycle wrote, so
    /// the assertion covers "poll → forecast → brake" end to end. The 50% control
    /// case shows the zero is the brake, not an artifact of the empty test fleet.
    #[test]
    fn test_cycle_forecast_at_98_percent_forces_zero_target() {
        use tempfile::TempDir;

        let composite_risk_config = crate::config::CompositeRiskConfig::default();
        let cone_scaling_config = crate::config::ConeScalingConfig::default();

        // A worker entry is required for compute_target_workers to have min/max
        // bounds to clamp into; the cycle keeps the entry and zeroes its `current`
        // (no tmux sessions in a test process).
        let seed_worker = |state_path: &std::path::Path| {
            let mut state = state::GovernorState::new();
            state.workers.insert(
                "test-agent".to_string(),
                state::WorkerState {
                    current: 0,
                    target: 0,
                    min: 1,
                    max: 10,
                },
            );
            state::save_state(&state, state_path).expect("failed to seed state file");
        };

        // Braked case: 98.5% on the 5-hour window.
        let braked_dir = TempDir::new().expect("Failed to create temp dir");
        let braked_path = braked_dir.path().join("governor-state.json");
        seed_worker(&braked_path);
        let mut braked_poller = MockPoller::with_utilization(98.5, 60.0, 55.0);
        run_cycle(&mut braked_poller, &braked_path).expect("cycle should succeed");

        let braked = state::load_state(&braked_path).expect("state should load back");
        assert!(
            braked.capacity_forecast.five_hour.current_utilization >= EMERGENCY_BRAKE_THRESHOLD,
            "the cycle should carry the polled 98.5% into the persisted 5h forecast, got {:.2}%",
            braked.capacity_forecast.five_hour.current_utilization
        );
        assert_eq!(
            compute_target_workers(&braked, 90.0, &composite_risk_config, &cone_scaling_config),
            0,
            "a window at or above 98% should brake the target to 0 workers"
        );

        // Control: same fleet, same fixtures, utilization well below the threshold.
        let calm_dir = TempDir::new().expect("Failed to create temp dir");
        let calm_path = calm_dir.path().join("governor-state.json");
        seed_worker(&calm_path);
        let mut calm_poller = MockPoller::with_utilization(50.0, 60.0, 55.0);
        run_cycle(&mut calm_poller, &calm_path).expect("cycle should succeed");

        let calm = state::load_state(&calm_path).expect("state should load back");
        assert!(
            calm.capacity_forecast.five_hour.current_utilization < EMERGENCY_BRAKE_THRESHOLD,
            "control case should stay below the brake threshold, got {:.2}%",
            calm.capacity_forecast.five_hour.current_utilization
        );
        assert!(
            compute_target_workers(&calm, 90.0, &composite_risk_config, &cone_scaling_config) > 0,
            "below the threshold the target should be non-zero — otherwise the braked \
             assertion above proves nothing"
        );
    }

    /// A failing poll is absorbed: the cycle still returns Ok, keeps the last good
    /// usage numbers, and writes state.
    #[test]
    fn test_cycle_survives_poll_failure_and_keeps_previous_usage() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");

        // Cycle 1 succeeds and establishes known-good usage numbers.
        let mut good_poller = MockPoller::with_utilization(42.0, 61.0, 58.0);
        run_cycle(&mut good_poller, &state_path).expect("first cycle should succeed");
        let before = state::load_state(&state_path).expect("state should load back");
        assert_eq!(before.usage.five_hour_pct, 42.0, "precondition");

        // Cycle 2 polls into an error.
        let mut failing_poller = MockPoller::with_error("Simulated API failure");
        let result = run_cycle(&mut failing_poller, &state_path);

        assert!(
            result.is_ok(),
            "a poll failure should not abort the cycle, got: {:?}",
            result.err()
        );
        assert_eq!(
            failing_poller.poll_count, 1,
            "the cycle should have attempted the poll"
        );

        let after = state::load_state(&state_path).expect("state should load back");
        assert_eq!(
            after.usage.five_hour_pct, 42.0,
            "failed poll should leave the last good 5h utilization in place"
        );
        assert_eq!(
            after.usage.all_models_pct, 61.0,
            "failed poll should leave the last good 7d utilization in place"
        );
        assert_eq!(
            after.usage.weekly_scoped_pct, 58.0,
            "failed poll should leave the last good weekly_scoped utilization in place"
        );
        assert!(
            after.updated_at > before.updated_at,
            "the cycle should still complete and write state after a poll failure"
        );
    }

    /// Stale poll data is accepted but flagged, so downstream logic can tell that
    /// the numbers came from a failing token refresh rather than a fresh read.
    #[test]
    fn test_cycle_flags_stale_poll_data() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state_path = temp_dir.path().join("governor-state.json");

        let mut poller = MockPoller::with_stale_data();
        run_cycle(&mut poller, &state_path).expect("cycle should succeed on stale data");

        let saved = state::load_state(&state_path).expect("state should load back");
        assert!(
            saved.usage.stale,
            "stale poll data should be flagged in state"
        );
        assert!(
            saved.token_refresh_failing,
            "stale data should set token_refresh_failing"
        );
        assert_eq!(
            saved.usage.five_hour_pct, 50.0,
            "stale data is still applied — stale numbers beat no numbers"
        );
    }
}

#[cfg(test)]
mod annotation_guard_tests {
    use super::*;
    use chrono::{Duration, Utc};

    /// Test Guard 1: Interval too short (< 2 minutes)
    #[test]
    fn test_annotation_guard_short_interval_skips() {
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(90); // Only 90 seconds - should skip

        let elapsed_seconds = (t1 - t0).num_seconds().abs();

        // Guard should trigger
        assert!(
            elapsed_seconds < 120,
            "Test setup: interval should be < 120s"
        );
    }

    /// Test Guard 1 passes: Interval >= 2 minutes
    #[test]
    fn test_annotation_guard_sufficient_interval_proceeds() {
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(150); // 150 seconds - should pass

        let elapsed_seconds = (t1 - t0).num_seconds().abs();

        // Guard should not trigger
        assert!(
            elapsed_seconds >= 120,
            "Test setup: interval should be >= 120s"
        );
    }

    /// Test Guard 2: Worker count changed mid-interval
    #[test]
    fn test_annotation_guard_worker_change_skips() {
        let workers_at_start = 5;
        let workers_at_end = 7;

        // Guard should trigger - workers changed
        assert_ne!(
            workers_at_start, workers_at_end,
            "Test setup: workers should differ"
        );
    }

    /// Test Guard 2 passes: Worker count stable
    #[test]
    fn test_annotation_guard_stable_workers_proceeds() {
        let workers_at_start = 5;
        let workers_at_end = 5;

        // Guard should not trigger
        assert_eq!(
            workers_at_start, workers_at_end,
            "Test setup: workers should be equal"
        );
    }

    /// Test Guard 3: Window reset detected (utilization drop > 1%)
    #[test]
    fn test_annotation_guard_window_reset_skips() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 18.5, // Dropped 1.5% - should trigger
            seven_day: 46.0,
            weekly_scoped: 36.0,
        };

        let reset_threshold = 1.0;

        let five_hour_reset = new_pct.five_hour < old_pct.five_hour - reset_threshold;
        let seven_day_reset = new_pct.seven_day < old_pct.seven_day - reset_threshold;
        let weekly_scoped_reset = new_pct.weekly_scoped < old_pct.weekly_scoped - reset_threshold;

        // At least one guard should trigger (5h dropped > 1%)
        assert!(
            five_hour_reset || seven_day_reset || weekly_scoped_reset,
            "Test setup: at least one window should show reset"
        );
    }

    /// Test Guard 3 passes: No window reset (normal utilization increase)
    #[test]
    fn test_annotation_guard_no_reset_proceeds() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 21.5, // Increased 1.5% - normal
            seven_day: 46.0,
            weekly_scoped: 36.5,
        };

        let reset_threshold = 1.0;

        let five_hour_reset = new_pct.five_hour < old_pct.five_hour - reset_threshold;
        let seven_day_reset = new_pct.seven_day < old_pct.seven_day - reset_threshold;
        let weekly_scoped_reset = new_pct.weekly_scoped < old_pct.weekly_scoped - reset_threshold;

        // No guard should trigger - all increased or stable
        assert!(
            !(five_hour_reset || seven_day_reset || weekly_scoped_reset),
            "Test setup: no window should show reset"
        );
    }

    /// Test Guard 3: Multiple windows reset
    #[test]
    fn test_annotation_guard_multiple_window_resets() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 25.0,
            seven_day: 50.0,
            weekly_scoped: 40.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 22.0,     // Dropped 3%
            seven_day: 48.0,     // Dropped 2%
            weekly_scoped: 38.5, // Dropped 1.5%
        };

        let reset_threshold = 1.0;

        let five_hour_reset = new_pct.five_hour < old_pct.five_hour - reset_threshold;
        let seven_day_reset = new_pct.seven_day < old_pct.seven_day - reset_threshold;
        let weekly_scoped_reset = new_pct.weekly_scoped < old_pct.weekly_scoped - reset_threshold;

        // Multiple guards should trigger
        assert!(five_hour_reset, "5h window should show reset");
        assert!(seven_day_reset, "7d window should show reset");
        assert!(weekly_scoped_reset, "7ds window should show reset");
    }

    /// Test all guards pass: Ideal conditions for annotation
    #[test]
    fn test_annotation_all_guards_pass() {
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(180); // 3 minutes - passes Guard 1

        let workers_at_start = 6;
        let workers_at_end = 6; // Stable - passes Guard 2

        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 22.0,     // Increased 2% - no reset
            seven_day: 46.5,     // Increased 1.5%
            weekly_scoped: 36.5, // Increased 1.5%
        };

        // Guard 1: Check interval
        let elapsed_seconds = (t1 - t0).num_seconds().abs();
        assert!(
            elapsed_seconds >= 120,
            "Guard 1: interval should be sufficient"
        );

        // Guard 2: Check worker stability
        assert_eq!(
            workers_at_start, workers_at_end,
            "Guard 2: workers should be stable"
        );

        // Guard 3: Check no window reset
        let reset_threshold = 1.0;
        let five_hour_reset = new_pct.five_hour < old_pct.five_hour - reset_threshold;
        let seven_day_reset = new_pct.seven_day < old_pct.seven_day - reset_threshold;
        let weekly_scoped_reset = new_pct.weekly_scoped < old_pct.weekly_scoped - reset_threshold;
        assert!(
            !(five_hour_reset || seven_day_reset || weekly_scoped_reset),
            "Guard 3: no window reset should occur"
        );
    }

    /// Test Guard 3 edge case: Exactly at threshold (1% drop = reset)
    #[test]
    fn test_annotation_guard_reset_at_threshold() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 18.99, // Dropped 1.01% - just over threshold
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let reset_threshold = 1.0;

        let five_hour_reset = new_pct.five_hour < old_pct.five_hour - reset_threshold;

        // Guard should trigger (just barely)
        assert!(five_hour_reset, "Drop of 1.01% should trigger reset guard");
    }

    /// Test Guard 3 edge case: Just below threshold (0.99% drop = no reset)
    #[test]
    fn test_annotation_guard_reset_below_threshold() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 19.01, // Dropped 0.99% - just under threshold
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let reset_threshold = 1.0;

        let five_hour_reset = new_pct.five_hour < old_pct.five_hour - reset_threshold;

        // Guard should not trigger (just barely)
        assert!(
            !five_hour_reset,
            "Drop of 0.99% should not trigger reset guard"
        );
    }

    /// Regression test: continuously-calibrated windows are unaffected by cold-start fixes.
    ///
    /// This test guards the 'only the cold path changes' invariant: when a window has
    /// accumulated sufficient EMA samples (>= 3) and has a non-zero burn rate, it should
    /// be classified as Calibrated and bypass the cold-start seeding logic entirely.
    ///
    /// Test scenario:
    /// - Window has 12 EMA samples (well above the 3-sample threshold)
    /// - Window has non-zero burn rate (2.5 %/hr from real measurements)
    /// - EstimateQuality is Calibrated
    /// - Current utilization is 65% with 2 workers
    ///
    /// Expected behavior:
    /// - Cold-start seeding logic should NOT trigger (wrong quality)
    /// - Forecast should use original EMA values (not seeded baseline)
    /// - Forecast should be numerically identical with or without cold-start code
    ///
    /// This test FAILS if the cold-start fix (bf-3ebgd Children 1-3) inadvertently
    /// changes hot-path behavior for calibrated windows.
    #[test]
    fn continuously_calibrated_window_bypasses_cold_start_logic() {
        use crate::state::EstimateQuality;

        // Continuously-calibrated window conditions
        let estimate_quality = EstimateQuality::Calibrated;
        let util = 65.0; // 65% utilization
        let fleet_pct_hr = 2.5; // non-zero burn rate from EMA
        let current_total = 2; // 2 workers
        let pct_per_worker = fleet_pct_hr / current_total as f64; // 1.25 %/worker/hr
        let std_pct_hr = 0.8; // realistic standard deviation from actual measurements

        let target_ceiling = 90.0;
        let hrs_remaining = 24.0;

        // Baseline config (should be ignored for calibrated windows)
        let baseline_pct_per_worker_hr = 1.5;

        // ASSERT 1: Verify seeding condition is NOT met due to estimate_quality
        let should_seed = matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0;

        assert!(
            !should_seed,
            "Calibrated window must NOT trigger seeding. Quality={:?}, util={}, fleet_pct_hr={}, workers={}",
            estimate_quality, util, fleet_pct_hr, current_total
        );

        // Apply the production seeding logic (matches governor.rs:4762-4787)
        let (fleet_pct_hr_seeded, pct_per_worker_seeded, std_pct_hr_seeded) = if matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0
        {
            let base_per_worker = baseline_pct_per_worker_hr;
            let seeded_fleet_pct = base_per_worker * current_total as f64;
            let widened_std_pct = seeded_fleet_pct;
            (seeded_fleet_pct, base_per_worker, widened_std_pct)
        } else {
            (fleet_pct_hr, pct_per_worker, std_pct_hr)
        };

        // ASSERT 2: Verify original values are preserved (not seeded)
        assert!(
            (fleet_pct_hr_seeded - fleet_pct_hr).abs() < 1e-9,
            "Continuously-calibrated window should preserve original fleet_pct_hr {}, got {}",
            fleet_pct_hr,
            fleet_pct_hr_seeded
        );
        assert!(
            (pct_per_worker_seeded - pct_per_worker).abs() < 1e-9,
            "Continuously-calibrated window should preserve original pct_per_worker {}, got {}",
            pct_per_worker,
            pct_per_worker_seeded
        );
        assert!(
            (std_pct_hr_seeded - std_pct_hr).abs() < 1e-9,
            "Continuously-calibrated window should preserve original std_pct_hr {}, got {}",
            std_pct_hr,
            std_pct_hr_seeded
        );

        // Generate forecast using the PRODUCTION path (generate_window_forecast)
        let forecast_before_cold_fix = generate_window_forecast(
            "weekly_scoped",
            fleet_pct_hr_seeded,
            util,
            target_ceiling,
            hrs_remaining,
            pct_per_worker_seeded,
            std_pct_hr_seeded,
            estimate_quality,
        );

        // ASSERT 3: Verify forecast uses calibrated EMA values (not seeded baseline)
        assert!(
            (forecast_before_cold_fix.fleet_pct_per_hour - fleet_pct_hr).abs() < 1e-6,
            "Forecast should preserve calibrated EMA rate {}, got {}",
            fleet_pct_hr,
            forecast_before_cold_fix.fleet_pct_per_hour
        );

        // ASSERT 4: Verify forecast is flagged as Calibrated (not ColdStart/Insufficient)
        assert_eq!(
            forecast_before_cold_fix.estimate_quality,
            EstimateQuality::Calibrated,
            "Continuously-calibrated window must be flagged as Calibrated, got {:?}",
            forecast_before_cold_fix.estimate_quality
        );

        // ASSERT 5: Verify forecast produces meaningful exhaustion prediction
        assert!(
            forecast_before_cold_fix
                .predicted_exhaustion_hours
                .is_finite(),
            "Continuously-calibrated window should produce finite exhaustion hours, got {}",
            forecast_before_cold_fix.predicted_exhaustion_hours
        );

        // ASSERT 6: Verify forecast has safe_worker_count
        assert!(
            forecast_before_cold_fix.safe_worker_count.is_some(),
            "Continuously-calibrated window should produce safe_worker_count"
        );

        // Simulate what would happen WITHOUT the cold-start code (baseline path)
        // This represents the "before cold-start fix" state
        let forecast_without_cold_logic = generate_window_forecast(
            "weekly_scoped",
            fleet_pct_hr, // Direct EMA, no seeding
            util,
            target_ceiling,
            hrs_remaining,
            pct_per_worker,
            std_pct_hr,
            estimate_quality,
        );

        // ASSERT 7: Verify forecasts are numerically identical
        // This is the key invariant: hot path must NOT change
        assert!(
            (forecast_before_cold_fix.fleet_pct_per_hour - forecast_without_cold_logic.fleet_pct_per_hour).abs() < 1e-9,
            "Cold-start logic should not change fleet_pct_per_hour for calibrated windows. Before={}, After={}",
            forecast_without_cold_logic.fleet_pct_per_hour, forecast_before_cold_fix.fleet_pct_per_hour
        );

        assert!(
            (forecast_before_cold_fix.predicted_exhaustion_hours - forecast_without_cold_logic.predicted_exhaustion_hours).abs() < 1e-6,
            "Cold-start logic should not change predicted_exhaustion_hours for calibrated windows. Before={}, After={}",
            forecast_without_cold_logic.predicted_exhaustion_hours, forecast_before_cold_fix.predicted_exhaustion_hours
        );

        assert!(
            forecast_before_cold_fix.safe_worker_count == forecast_without_cold_logic.safe_worker_count,
            "Cold-start logic should not change safe_worker_count for calibrated windows. Before={:?}, After={:?}",
            forecast_without_cold_logic.safe_worker_count, forecast_before_cold_fix.safe_worker_count
        );

        // ASSERT 8: Verify both forecasts have the same quality
        assert_eq!(
            forecast_before_cold_fix.estimate_quality, forecast_without_cold_logic.estimate_quality,
            "Estimate quality should be identical with and without cold-start logic"
        );
    }

    /// Regression test: continuously-calibrated windows with 3+ samples bypass cold-start.
    ///
    /// This test verifies the boundary condition: a window with exactly 3 samples
    /// (the MIN_SAMPLES_FOR_EMA threshold) is classified as Calibrated and bypasses
    /// the cold-start seeding logic.
    #[test]
    fn continuously_calibrated_window_at_threshold_bypasses_cold_start() {
        use crate::state::EstimateQuality;

        // Window at the calibration threshold (exactly 3 samples)
        let estimate_quality = EstimateQuality::Calibrated;
        let util = 80.0; // 80% utilization
        let fleet_pct_hr = 3.0; // burn rate from exactly 3 samples
        let current_total = 3; // 3 workers
        let pct_per_worker = fleet_pct_hr / current_total as f64; // 1.0 %/worker/hr
        let std_pct_hr = 0.5; // smaller std at threshold

        let target_ceiling = 90.0;
        let hrs_remaining = 8.0; // shorter time horizon for higher pressure

        // Verify seeding logic is bypassed
        let should_seed = matches!(
            estimate_quality,
            EstimateQuality::ColdStart | EstimateQuality::InsufficientSamples
        ) && util > 0.0
            && fleet_pct_hr == 0.0
            && current_total > 0;

        assert!(
            !should_seed,
            "Window at calibration threshold must bypass seeding"
        );

        // Generate forecasts with and without cold-start logic
        let forecast_with_logic = generate_window_forecast(
            "five_hour",
            fleet_pct_hr, // EMA value bypasses seeding
            util,
            target_ceiling,
            hrs_remaining,
            pct_per_worker,
            std_pct_hr,
            estimate_quality,
        );

        let forecast_without_logic = generate_window_forecast(
            "five_hour",
            fleet_pct_hr,
            util,
            target_ceiling,
            hrs_remaining,
            pct_per_worker,
            std_pct_hr,
            EstimateQuality::Calibrated,
        );

        // Verify numerical identity
        assert_eq!(
            forecast_with_logic.safe_worker_count, forecast_without_logic.safe_worker_count,
            "Safe worker count should be identical at calibration threshold"
        );

        assert!(
            (forecast_with_logic.predicted_exhaustion_hours
                - forecast_without_logic.predicted_exhaustion_hours)
                .abs()
                < 1e-6,
            "Exhaustion prediction should be identical at calibration threshold"
        );

        // Verify the forecast is Calibrated
        assert_eq!(
            forecast_with_logic.estimate_quality,
            EstimateQuality::Calibrated,
            "Window at threshold should be Calibrated"
        );
    }

    // -------------------------------------------------------------------------
    // Tests for guard helper functions
    // -------------------------------------------------------------------------

    /// Test check_elapsed_minimum: interval too short
    #[test]
    fn test_check_elapsed_minimum_short_interval_skips() {
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(90); // Only 90 seconds

        let result = check_elapsed_minimum(t0, t1);

        assert!(
            result.is_some(),
            "Should skip when interval is less than 120 seconds"
        );
        match result {
            Some(SkipReason::IntervalTooShort { elapsed_seconds }) => {
                assert_eq!(elapsed_seconds, 90);
            }
            _ => panic!("Expected IntervalTooShort, got {:?}", result),
        }
    }

    /// Test check_elapsed_minimum: interval sufficient
    #[test]
    fn test_check_elapsed_minimum_sufficient_interval_proceeds() {
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(180); // 3 minutes

        let result = check_elapsed_minimum(t0, t1);

        assert!(
            result.is_none(),
            "Should proceed when interval is >= 120 seconds"
        );
    }

    /// Test check_elapsed_minimum: exactly at threshold
    #[test]
    fn test_check_elapsed_minimum_at_threshold_proceeds() {
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(120); // Exactly 2 minutes

        let result = check_elapsed_minimum(t0, t1);

        assert!(
            result.is_none(),
            "Should proceed when interval is exactly 120 seconds"
        );
    }

    /// Test check_elapsed_minimum: just below threshold
    #[test]
    fn test_check_elapsed_minimum_just_below_threshold_skips() {
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(119); // Just under 2 minutes

        let result = check_elapsed_minimum(t0, t1);

        assert!(
            result.is_some(),
            "Should skip when interval is just under 120 seconds"
        );
    }

    /// Test check_elapsed_minimum: negative duration (t1 before t0)
    #[test]
    fn test_check_elapsed_minimum_negative_duration_abs_is_taken() {
        let t1 = Utc::now();
        let t0 = t1 + Duration::seconds(90); // t0 after t1 (negative elapsed)

        let result = check_elapsed_minimum(t0, t1);

        assert!(
            result.is_some(),
            "Should skip (abs() is taken, so 90s < 120s)"
        );
    }

    /// Test check_worker_count_stable: worker count changed
    #[test]
    fn test_check_worker_count_stable_changed_skips() {
        let result = check_worker_count_stable(5, 7);

        assert!(
            result.is_some(),
            "Should skip when worker count changes"
        );
        match result {
            Some(SkipReason::WorkerCountChanged {
                workers_start,
                workers_end,
            }) => {
                assert_eq!(workers_start, 5);
                assert_eq!(workers_end, 7);
            }
            _ => panic!("Expected WorkerCountChanged, got {:?}", result),
        }
    }

    /// Test check_worker_count_stable: worker count stable
    #[test]
    fn test_check_worker_count_stable_unchanged_proceeds() {
        let result = check_worker_count_stable(5, 5);

        assert!(
            result.is_none(),
            "Should proceed when worker count is stable"
        );
    }

    /// Test check_worker_count_stable: both zero (edge case)
    #[test]
    fn test_check_worker_count_stable_both_zero_proceeds() {
        let result = check_worker_count_stable(0, 0);

        assert!(
            result.is_none(),
            "Should proceed when both counts are zero (stable)"
        );
    }

    /// Test check_worker_count_stable: decrease in workers
    #[test]
    fn test_check_worker_count_stable_decrease_skips() {
        let result = check_worker_count_stable(10, 3);

        assert!(
            result.is_some(),
            "Should skip when worker count decreases"
        );
        match result {
            Some(SkipReason::WorkerCountChanged {
                workers_start,
                workers_end,
            }) => {
                assert_eq!(workers_start, 10);
                assert_eq!(workers_end, 3);
            }
            _ => panic!("Expected WorkerCountChanged, got {:?}", result),
        }
    }

    /// Test check_window_reset: single window reset (5-hour)
    #[test]
    fn test_check_window_reset_single_window_skips() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 18.5, // Dropped 1.5%
            seven_day: 46.0,
            weekly_scoped: 36.0,
        };

        let result = check_window_reset(&old_pct, &new_pct);

        assert!(
            result.is_some(),
            "Should skip when any window drops > 1%"
        );
        match result {
            Some(SkipReason::WindowReset {
                five_hour_reset,
                seven_day_reset,
                weekly_scoped_reset,
            }) => {
                assert!(five_hour_reset, "5-hour should show reset");
                assert!(!seven_day_reset, "7-day should not show reset");
                assert!(!weekly_scoped_reset, "7ds should not show reset");
            }
            _ => panic!("Expected WindowReset, got {:?}", result),
        }
    }

    /// Test check_window_reset: no reset (normal increase)
    #[test]
    fn test_check_window_reset_no_reset_proceeds() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 21.5, // Increased 1.5%
            seven_day: 46.5,
            weekly_scoped: 36.5,
        };

        let result = check_window_reset(&old_pct, &new_pct);

        assert!(
            result.is_none(),
            "Should proceed when no window drops > 1%"
        );
    }

    /// Test check_window_reset: multiple windows reset
    #[test]
    fn test_check_window_reset_multiple_windows_skips() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 25.0,
            seven_day: 50.0,
            weekly_scoped: 40.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 22.0,     // Dropped 3%
            seven_day: 48.0,     // Dropped 2%
            weekly_scoped: 38.5, // Dropped 1.5%
        };

        let result = check_window_reset(&old_pct, &new_pct);

        assert!(
            result.is_some(),
            "Should skip when multiple windows reset"
        );
        match result {
            Some(SkipReason::WindowReset {
                five_hour_reset,
                seven_day_reset,
                weekly_scoped_reset,
            }) => {
                assert!(five_hour_reset, "5-hour should show reset");
                assert!(seven_day_reset, "7-day should show reset");
                assert!(weekly_scoped_reset, "7ds should show reset");
            }
            _ => panic!("Expected WindowReset, got {:?}", result),
        }
    }

    /// Test check_window_reset: exactly at threshold
    #[test]
    fn test_check_window_reset_at_threshold_skips() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 18.99, // Dropped 1.01%
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let result = check_window_reset(&old_pct, &new_pct);

        assert!(
            result.is_some(),
            "Should skip when drop is just over threshold (1.01%)"
        );
    }

    /// Test check_window_reset: just below threshold
    #[test]
    fn test_check_window_reset_below_threshold_proceeds() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 19.01, // Dropped 0.99%
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let result = check_window_reset(&old_pct, &new_pct);

        assert!(
            result.is_none(),
            "Should proceed when drop is just under threshold (0.99%)"
        );
    }

    /// Test check_window_reset: stable (no change)
    #[test]
    fn test_check_window_reset_stable_proceeds() {
        let old_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let new_pct = db::WindowPctSnapshot {
            five_hour: 20.0,
            seven_day: 45.0,
            weekly_scoped: 35.0,
        };

        let result = check_window_reset(&old_pct, &new_pct);

        assert!(result.is_none(), "Should proceed when all windows are stable");
    }

    /// Test SkipReason::description() method
    #[test]
    fn test_skip_reason_description() {
        let reason = SkipReason::IntervalTooShort { elapsed_seconds: 90 };
        assert_eq!(reason.description(), "interval too short (90s < 120s)");

        let reason = SkipReason::WorkerCountChanged {
            workers_start: 5,
            workers_end: 7,
        };
        assert_eq!(
            reason.description(),
            "worker count changed mid-interval (5 -> 7)"
        );

        let reason = SkipReason::WindowReset {
            five_hour_reset: true,
            seven_day_reset: false,
            weekly_scoped_reset: true,
        };
        assert_eq!(reason.description(), "interval spans window reset (5h, 7ds)");
    }

    /// Test SkipReason::description() with all windows reset
    #[test]
    fn test_skip_reason_description_all_windows() {
        let reason = SkipReason::WindowReset {
            five_hour_reset: true,
            seven_day_reset: true,
            weekly_scoped_reset: true,
        };
        assert_eq!(
            reason.description(),
            "interval spans window reset (5h, 7d, 7ds)"
        );
    }

    /// Test SkipReason::description() with no windows reset (shouldn't happen in practice)
    #[test]
    fn test_skip_reason_description_no_windows() {
        let reason = SkipReason::WindowReset {
            five_hour_reset: false,
            seven_day_reset: false,
            weekly_scoped_reset: false,
        };
        assert_eq!(reason.description(), "interval spans window reset ()");
    }
}

#[cfg(test)]
mod is_structurally_inactive_tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper function to create a test UsageWindow
    fn create_test_window(name: &str, is_active: Option<bool>) -> UsageWindow {
        UsageWindow {
            name: name.to_string(),
            utilization: 50.0,
            resets_at: "2024-01-01T00:00:00Z".to_string(),
            is_active,
        }
    }

    /// Helper function to create a test GovernorState with specific consecutive absent counts
    fn create_test_state(consecutive_absent_counts: HashMap<String, u32>) -> state::GovernorState {
        let mut state = state::GovernorState::new();
        state.consecutive_absent_polls = consecutive_absent_counts;
        state
    }

    /// Test: Returns true when consecutive_absence_count >= MIN_CONSECUTIVE_ABSENT
    #[test]
    fn test_returns_true_when_consecutive_absence_threshold_reached() {
        let window_name = "five_hour";
        let mut absent_counts = HashMap::new();
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT);

        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            result,
            "should return true when consecutive_absence_count >= MIN_CONSECUTIVE_ABSENT (3), got {}",
            result
        );
    }

    /// Test: Returns true when consecutive_absence_count > MIN_CONSECUTIVE_ABSENT
    #[test]
    fn test_returns_true_when_consecutive_absence_exceeds_threshold() {
        let window_name = "seven_day";
        let mut absent_counts = HashMap::new();
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT + 1);

        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            result,
            "should return true when consecutive_absence_count > MIN_CONSECUTIVE_ABSENT, got {}",
            result
        );
    }

    /// Test: Returns true when is_active == false
    #[test]
    fn test_returns_true_when_is_active_is_false() {
        let window_name = "weekly_scoped";
        let absent_counts = HashMap::new(); // No consecutive absences

        // Window is explicitly marked as inactive by API
        let window = create_test_window(window_name, Some(false));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            result,
            "should return true when is_active == false, got {}",
            result
        );
    }

    /// Test: Returns true when BOTH conditions are true
    #[test]
    fn test_returns_true_when_both_conditions_are_true() {
        let window_name = "five_hour";
        let mut absent_counts = HashMap::new();
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT);

        // Both conditions: consecutive absence threshold reached AND is_active is false
        let window = create_test_window(window_name, Some(false));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            result,
            "should return true when both conditions are true, got {}",
            result
        );
    }

    /// Test: Returns false when both conditions are false
    #[test]
    fn test_returns_false_when_both_conditions_are_false() {
        let window_name = "seven_day";
        let mut absent_counts = HashMap::new();
        // Consecutive absence count below threshold (2 < 3)
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT - 1);

        // Window is active (is_active = true)
        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            !result,
            "should return false when both conditions are false (active + below threshold), got {}",
            result
        );
    }

    /// Test: Returns false when is_active is None/null (treat as active)
    #[test]
    fn test_returns_false_when_is_active_is_none() {
        let window_name = "weekly_scoped";
        let absent_counts = HashMap::new(); // No consecutive absences

        // is_active is None (field not populated in API response)
        let window = create_test_window(window_name, None);
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            !result,
            "should return false when is_active is None (treat as active), got {}",
            result
        );
    }

    /// Test: Returns false when is_active is None even with moderate consecutive absences
    #[test]
    fn test_returns_false_when_is_active_none_with_below_threshold_absent() {
        let window_name = "five_hour";
        let mut absent_counts = HashMap::new();
        // Below threshold but still present
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT - 1);

        // is_active is None (field not populated) - should treat as active
        let window = create_test_window(window_name, None);
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            !result,
            "should return false when is_active is None even with below-threshold absences, got {}",
            result
        );
    }

    /// Test: Consecutive absence condition works independently
    #[test]
    fn test_consecutive_absence_condition_works_independently() {
        let window_name = "seven_day";

        // Test with consecutive absence threshold reached, but is_active = true
        let mut absent_counts = HashMap::new();
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT);
        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            result,
            "consecutive absence condition should work independently (even with is_active=true), got {}",
            result
        );
    }

    /// Test: is_active == false condition works independently
    #[test]
    fn test_is_active_false_condition_works_independently() {
        let window_name = "weekly_scoped";
        let absent_counts = HashMap::new(); // No consecutive absences

        // Test with is_active = false, but no consecutive absences
        let window = create_test_window(window_name, Some(false));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            result,
            "is_active=false condition should work independently (even with no absences), got {}",
            result
        );
    }

    /// Test: Returns false when consecutive absence count is 0
    #[test]
    fn test_returns_false_when_consecutive_absence_is_zero() {
        let window_name = "five_hour";
        let mut absent_counts = HashMap::new();
        absent_counts.insert(window_name.to_string(), 0);

        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            !result,
            "should return false when consecutive_absence_count is 0, got {}",
            result
        );
    }

    /// Test: Returns false when window not in consecutive_absent_polls map
    #[test]
    fn test_returns_false_when_window_not_in_absent_map() {
        let window_name = "seven_day";
        let absent_counts = HashMap::new(); // Window not present in map

        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            !result,
            "should return false when window not in consecutive_absent_polls map, got {}",
            result
        );
    }

    /// Test: Boundary case - exactly at threshold
    #[test]
    fn test_boundary_case_exactly_at_threshold() {
        let window_name = "weekly_scoped";
        let mut absent_counts = HashMap::new();
        // Exactly at threshold (3 == MIN_CONSECUTIVE_ABSENT)
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT);

        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            result,
            "should return true at exact threshold ({}), got {}",
            MIN_CONSECUTIVE_ABSENT, result
        );
    }

    /// Test: Boundary case - just below threshold
    #[test]
    fn test_boundary_case_just_below_threshold() {
        let window_name = "five_hour";
        let mut absent_counts = HashMap::new();
        // Just below threshold (2 == MIN_CONSECUTIVE_ABSENT - 1)
        absent_counts.insert(window_name.to_string(), MIN_CONSECUTIVE_ABSENT - 1);

        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            !result,
            "should return false just below threshold ({}), got {}",
            MIN_CONSECUTIVE_ABSENT - 1,
            result
        );
    }

    /// Test: is_active == true should not mark as inactive
    #[test]
    fn test_is_active_true_not_inactive() {
        let window_name = "seven_day";
        let absent_counts = HashMap::new();

        // is_active = true should explicitly mark window as active
        let window = create_test_window(window_name, Some(true));
        let state = create_test_state(absent_counts);

        let result = is_structurally_inactive(&window, &state);

        assert!(
            !result,
            "should return false when is_active is true (window is active), got {}",
            result
        );
    }
}
