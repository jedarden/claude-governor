//! Governor - Capacity management and emergency brake
//!
//! This module handles:
//! - Emergency brake detection (98% hard stop)
//! - Underutilization sprint triggering and management
//! - End-of-window capacity sprint
//! - Governor state management
//! - Agent scaling decisions

use chrono::{DateTime, Utc};
use std::collections::HashMap;

use crate::alerts::SprintTrigger;
use crate::config::SprintConfig;

/// Emergency brake threshold (98%)
const EMERGENCY_BRAKE_THRESHOLD: f64 = 98.0;

/// Window names for utilization tracking
pub const WINDOW_FIVE_HOUR: &str = "five_hour";
pub const WINDOW_SEVEN_DAY: &str = "seven_day";
pub const WINDOW_SEVEN_DAY_SONNET: &str = "seven_day_sonnet";

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
    pub fn from_windows(
        five_hour: f64,
        seven_day: f64,
        seven_day_sonnet: f64,
    ) -> Self {
        let mut windows = HashMap::new();
        windows.insert(WINDOW_FIVE_HOUR.to_string(), five_hour);
        windows.insert(WINDOW_SEVEN_DAY.to_string(), seven_day);
        windows.insert(WINDOW_SEVEN_DAY_SONNET.to_string(), seven_day_sonnet);
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
    /// Window name (five_hour, seven_day, seven_day_sonnet)
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
            trigger.worker_id, original_workers, trigger.target_workers, trigger.window
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
                    sprint.worker_id, sprint.original_workers
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
    pub fn check_sprint_end(&mut self, usage: &UsageSnapshot, sprint_config: &SprintConfig) -> bool {
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
                    sprint.window, util, sprint_config.underutilization_threshold_pct
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
                window_ctx.name, window_ctx.hours_remaining, horizon_hours
            );
            return false;
        }

        // Check minimum headroom
        if window_ctx.headroom_pct <= config.min_headroom_pct {
            log::debug!(
                "[sprint] Blocked: window {} headroom {:.1}% <= min {:.1}%",
                window_ctx.name, window_ctx.headroom_pct, config.min_headroom_pct
            );
            return false;
        }

        // Check for backlog
        if !window_ctx.has_backlog {
            log::debug!("[sprint] Blocked: no backlog for window {}", window_ctx.name);
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
                    cone_ratio, config.max_cone_ratio
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
                window_ctx.headroom_pct, config.sprint_end_headroom_pct
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
                    effective, boosted, cap
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
mod tests {
    use super::*;

    /// Helper: create a usage snapshot with given utilizations
    fn snap(five_hour: f64, seven_day: f64, seven_day_sonnet: f64) -> UsageSnapshot {
        UsageSnapshot::from_windows(five_hour, seven_day, seven_day_sonnet)
    }

    /// Helper: create a governor with some agents
    fn governor_with_agents() -> GovernorState {
        let mut state = GovernorState::new();
        state.add_agent("agent-1", 5, false);
        state.add_agent("agent-2", 3, true);
        state.add_agent("agent-3", 10, false);
        state
    }

    // --- Core emergency brake tests ---

    #[test]
    fn test_97_9_pct_no_brake() {
        let mut state = governor_with_agents();
        let usage = snap(97.9, 50.0, 50.0);

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
        let usage = snap(98.0, 50.0, 50.0);

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
        let usage = snap(50.0, 98.5, 50.0); // seven_day triggers

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

        let usage = snap(99.0, 50.0, 50.0);

        let _ = state.check_emergency_brake(&usage);

        // Both should be scaled to 0, regardless of idle status
        assert_eq!(state.agents["idle-agent"].workers, 0);
        assert_eq!(state.agents["busy-agent"].workers, 0);
    }

    #[test]
    fn test_brake_clears_below_98_pct() {
        let mut state = governor_with_agents();

        // Trigger brake
        let usage_high = snap(98.5, 50.0, 50.0);
        let _ = state.check_emergency_brake(&usage_high);
        assert!(state.emergency_brake_active);

        // Now drop below threshold
        let usage_low = snap(97.0, 50.0, 50.0);
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
        let usage_high = snap(99.0, 50.0, 50.0);
        let _ = state.check_emergency_brake(&usage_high);
        assert!(state.emergency_brake_active);

        // Simulate window reset (utilization drops significantly)
        let usage_reset = snap(10.0, 50.0, 50.0);
        let cleared = state.clear_emergency_brake(&usage_reset);

        assert!(cleared);
        assert!(!state.emergency_brake_active);
    }

    // --- Additional tests ---

    #[test]
    fn test_brake_triggers_on_any_window() {
        // Test seven_day_sonnet window
        let mut state = governor_with_agents();
        let usage = snap(50.0, 50.0, 98.0);
        let result = state.check_emergency_brake(&usage);
        assert!(result.is_some());
        assert_eq!(result.unwrap().triggered_window, WINDOW_SEVEN_DAY_SONNET);

        // Test seven_day window
        let mut state2 = governor_with_agents();
        let usage2 = snap(50.0, 99.0, 50.0);
        let result2 = state2.check_emergency_brake(&usage2);
        assert!(result2.is_some());
        assert_eq!(result2.unwrap().triggered_window, WINDOW_SEVEN_DAY);
    }

    #[test]
    fn test_brake_does_not_clear_if_still_above_threshold() {
        let mut state = governor_with_agents();

        // Trigger on five_hour
        let usage_high = snap(99.0, 98.5, 50.0);
        let _ = state.check_emergency_brake(&usage_high);
        assert!(state.emergency_brake_active);

        // Drop five_hour but seven_day still above
        let usage_still_high = snap(50.0, 98.5, 50.0);
        let cleared = state.clear_emergency_brake(&usage_still_high);

        assert!(!cleared);
        assert!(state.emergency_brake_active);
    }

    #[test]
    fn test_update_combines_check_and_clear() {
        let mut state = governor_with_agents();

        // Initial trigger
        let usage1 = snap(98.5, 50.0, 50.0);
        let result1 = state.update_emergency_brake(&usage1);
        assert!(result1.is_some());
        assert!(state.emergency_brake_active);

        // Still high - should return existing brake
        let usage2 = snap(99.0, 50.0, 50.0);
        let result2 = state.update_emergency_brake(&usage2);
        assert!(result2.is_some());
        assert!(state.emergency_brake_active);

        // Drops below - should clear and not retrigger
        let usage3 = snap(97.0, 50.0, 50.0);
        let result3 = state.update_emergency_brake(&usage3);
        assert!(result3.is_none());
        assert!(!state.emergency_brake_active);
    }

    #[test]
    fn test_empty_agents_still_sets_flag() {
        let mut state = GovernorState::new(); // no agents
        let usage = snap(98.0, 50.0, 50.0);

        let result = state.check_emergency_brake(&usage);

        assert!(result.is_some());
        assert!(state.emergency_brake_active);
    }

    #[test]
    fn test_usage_snapshot_helpers() {
        let snap = UsageSnapshot::from_windows(10.0, 20.0, 30.0);

        assert_eq!(snap.get(WINDOW_FIVE_HOUR), Some(10.0));
        assert_eq!(snap.get(WINDOW_SEVEN_DAY), Some(20.0));
        assert_eq!(snap.get(WINDOW_SEVEN_DAY_SONNET), Some(30.0));
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
        let usage = snap(99.0, 50.0, 50.0);
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
        let usage = snap(55.0, 50.0, 50.0);
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
        let usage = snap(45.0, 50.0, 50.0);
        let config = default_sprint_config();
        let ended = state.check_sprint_end(&usage, &config);

        assert!(!ended);
        assert!(state.is_sprint_active());
        assert_eq!(state.agents["agent-1"].workers, 20); // still boosted
    }

    #[test]
    fn sprint_end_noop_when_no_sprint() {
        let mut state = governor_with_agents();
        let usage = snap(55.0, 50.0, 50.0);
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
}
