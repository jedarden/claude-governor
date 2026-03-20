//! Governor - Capacity management and emergency brake
//!
//! This module handles:
//! - Emergency brake detection (98% hard stop)
//! - Governor state management
//! - Agent scaling decisions

use std::collections::HashMap;

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

/// Governor state
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorState {
    /// Whether emergency brake is currently active
    pub emergency_brake_active: bool,

    /// Tracked agents
    pub agents: HashMap<String, Agent>,

    /// The emergency brake event if active
    pub emergency_brake: Option<EmergencyBrake>,
}

impl GovernorState {
    /// Create a new governor state
    pub fn new() -> Self {
        Self {
            emergency_brake_active: false,
            agents: HashMap::new(),
            emergency_brake: None,
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
}
