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
    pub fn from_windows(five_hour: f64, seven_day: f64, seven_day_sonnet: f64) -> Self {
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
/// - `None` → `max_workers`: no burn rate data yet; use the configured ceiling so the
///   fleet stays at full capacity rather than freezing at current_total.
/// - `Some(0)` → `current_total`: formula computed 0 (near-exhaustion) but we don't
///   drop below current to avoid a sudden cold-start penalty.
/// - `Some(w)` → `w`: normal case.
fn safe_worker_count_or_max(safe: Option<u32>, max_workers: u32, current_total: u32) -> u32 {
    match safe {
        None => {
            log::info!("[governor] insufficient burn rate data, using max_workers as ceiling");
            max_workers
        }
        Some(0) => current_total,
        Some(w) => w,
    }
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
        (&WINDOW_SEVEN_DAY_SONNET, &forecast.seven_day_sonnet),
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
        _ => &forecast.seven_day_sonnet,
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
            forecast.seven_day_sonnet.clone(),
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
                safe_worker_count_or_max(selected_safe, global_max, current_total)
            }
        }
    } else {
        safe_worker_count_or_max(selected_safe, global_max, current_total)
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
// Governor daemon loop
// ---------------------------------------------------------------------------

/// Run one governor cycle: poll -> schedule -> burn_rate -> target -> scale -> alert -> write_state
///
/// This is the core loop body executed every `loop_interval` seconds.
pub fn run_governor_cycle(
    poller: &mut Poller,
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
) -> anyhow::Result<()> {
    let now = Utc::now();
    log::info!("[governor] === cycle start at {} ===", now.to_rfc3339());

    // 1. Load current state
    let mut state = state::load_state(state_path)?;

    // 1a. Poll Anthropic API for live usage data
    match poller.poll() {
        Ok(usage_data) => {
            log::info!(
                "[governor] polled usage: sonnet={:.1}%, all_models={:.1}%, 5h={:.1}%{}",
                usage_data.seven_day_sonnet_utilization,
                usage_data.seven_day_utilization,
                usage_data.five_hour_utilization,
                if usage_data.stale { " (stale)" } else { "" },
            );
            state.usage = state::UsageState {
                sonnet_pct: usage_data.seven_day_sonnet_utilization,
                all_models_pct: usage_data.seven_day_utilization,
                five_hour_pct: usage_data.five_hour_utilization,
                sonnet_resets_at: usage_data.seven_day_sonnet_resets_at,
                five_hour_resets_at: usage_data.five_hour_resets_at,
                stale: usage_data.stale,
            };
            state.token_refresh_failing = usage_data.stale;
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
                            seven_day_sonnet: p7ds,
                        },
                        fleet_cache_eff,
                        cache_eff_p25,
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
    // Distribute evenly across configured agents
    let agent_count = state.workers.len().max(1);
    let per_agent = current_total / agent_count as u32;
    let remainder = current_total % agent_count as u32;

    for (i, ws) in state.workers.values_mut().enumerate() {
        let extra = if (i as u32) < remainder { 1 } else { 0 };
        ws.current = per_agent + extra;
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
    {
        const EMA_ALPHA: f64 = 0.2;
        // Require at least 60 s between delta samples to avoid noise from very short windows
        const MIN_ELAPSED_SECS: f64 = 60.0;
        // If the governor was paused for > 30 min, the snapshot is too stale to use
        const MAX_ELAPSED_SECS: f64 = 1800.0;

        let new_five_hour = state.usage.five_hour_pct;
        let new_seven_day = state.usage.all_models_pct;
        let new_seven_day_sonnet = state.usage.sonnet_pct;

        if !state.usage.stale {
            if let Some(snap) = state.burn_rate.prev_usage_snapshot.clone() {
                let elapsed_secs = (now - snap.taken_at).num_seconds() as f64;
                let elapsed_hours_snap = elapsed_secs / 3600.0;

                if elapsed_secs >= MIN_ELAPSED_SECS && elapsed_secs <= MAX_ELAPSED_SECS {
                    // Per-window deltas — positive only; negatives indicate a quota reset
                    let delta_5h = new_five_hour - snap.five_hour_pct;
                    let delta_7d = new_seven_day - snap.seven_day_pct;
                    let delta_7ds = new_seven_day_sonnet - snap.seven_day_sonnet_pct;

                    // Fleet total USD/hr from the most recent fleet aggregate
                    let fleet_usd_hr = state.last_fleet_aggregate.sonnet_p75_usd_hr
                        * state.last_fleet_aggregate.sonnet_workers as f64;

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
                        if samples == 0 {
                            state.burn_rate.fleet_pct_hr_ema.seven_day_sonnet = rate;
                        } else {
                            state.burn_rate.fleet_pct_hr_ema.seven_day_sonnet = EMA_ALPHA * rate
                                + (1.0 - EMA_ALPHA)
                                    * state.burn_rate.fleet_pct_hr_ema.seven_day_sonnet;
                        }
                        if fleet_usd_hr > 0.0 {
                            let ratio = fleet_usd_hr / rate;
                            if samples == 0 {
                                state.burn_rate.usd_per_pct_ema_seven_day_sonnet = ratio;
                            } else {
                                state.burn_rate.usd_per_pct_ema_seven_day_sonnet = EMA_ALPHA
                                    * ratio
                                    + (1.0 - EMA_ALPHA)
                                        * state.burn_rate.usd_per_pct_ema_seven_day_sonnet;
                            }
                        }
                        updated_any = true;
                    }

                    if updated_any {
                        state.burn_rate.fleet_pct_ema_samples =
                            state.burn_rate.fleet_pct_ema_samples.saturating_add(1);
                    }

                    log::debug!(
                        "[governor] API delta in {:.0}s: 5h={:+.3}% 7d={:+.3}% 7ds={:+.3}% \
                         → EMA pct/hr: 5h={:.4} 7d={:.4} 7ds={:.4} (samples={})",
                        elapsed_secs,
                        delta_5h,
                        delta_7d,
                        delta_7ds,
                        state.burn_rate.fleet_pct_hr_ema.five_hour,
                        state.burn_rate.fleet_pct_hr_ema.seven_day,
                        state.burn_rate.fleet_pct_hr_ema.seven_day_sonnet,
                        state.burn_rate.fleet_pct_ema_samples,
                    );
                }
            }

            // Update the snapshot for use in the next cycle
            state.burn_rate.prev_usage_snapshot = Some(state::PrevUsageSnapshot {
                taken_at: now,
                five_hour_pct: new_five_hour,
                seven_day_pct: new_seven_day,
                seven_day_sonnet_pct: new_seven_day_sonnet,
            });
        }
    }

    let elapsed_hours = if state.last_fleet_aggregate.t0 != state.last_fleet_aggregate.t1 {
        (state.last_fleet_aggregate.t1 - state.last_fleet_aggregate.t0).num_seconds() as f64
            / 3600.0
    } else {
        0.0
    };

    // Build current utilization map from polled usage
    let mut current_utilization = HashMap::new();
    current_utilization.insert("five_hour".to_string(), state.usage.five_hour_pct);
    current_utilization.insert("seven_day".to_string(), state.usage.all_models_pct);
    current_utilization.insert("seven_day_sonnet".to_string(), state.usage.sonnet_pct);

    // Build effective hours remaining map from poller data
    // Uses effective_hours_remaining_from so only windows in applies_to get the promo boost.
    let mut hours_remaining = HashMap::new();
    if let Ok(reset_time) = state.usage.five_hour_resets_at.parse::<DateTime<Utc>>() {
        hours_remaining.insert(
            "five_hour".to_string(),
            schedule::effective_hours_remaining_from(now, reset_time, promotions, "five_hour"),
        );
    }
    if let Ok(reset_time) = state.usage.sonnet_resets_at.parse::<DateTime<Utc>>() {
        hours_remaining.insert(
            "seven_day_sonnet".to_string(),
            schedule::effective_hours_remaining_from(
                now,
                reset_time,
                promotions,
                "seven_day_sonnet",
            ),
        );
        // Approximate seven_day reset time as same as seven_day_sonnet
        hours_remaining.insert(
            "seven_day".to_string(),
            schedule::effective_hours_remaining_from(now, reset_time, promotions, "seven_day"),
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
        // Fleet total USD/hr (p75 per-worker × active workers)
        let fleet_usd_hr = state.last_fleet_aggregate.sonnet_p75_usd_hr
            * state.last_fleet_aggregate.sonnet_workers as f64;
        // Baseline dollars-per-pct ratio from default burn rate assumptions:
        // ~$5/hr/worker ÷ ~1.5%/hr/worker ≈ 3.33 $/pct
        const BASELINE_USD_PER_PCT: f64 = 5.0 / 1.5;

        let rate_for = |ema_val: f64, usd_per_pct: f64| -> f64 {
            if samples >= 1 && ema_val > 0.0 {
                ema_val // (A) API delta EMA
            } else if fleet_usd_hr > 0.0 && usd_per_pct > 0.0 {
                fleet_usd_hr / usd_per_pct // (B) learned ratio
            } else if fleet_usd_hr > 0.0 {
                fleet_usd_hr / BASELINE_USD_PER_PCT // (C) baseline ratio fallback
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
            "seven_day_sonnet".to_string(),
            rate_for(
                ema.seven_day_sonnet,
                state.burn_rate.usd_per_pct_ema_seven_day_sonnet,
            ),
        );

        if samples == 0 && fleet_usd_hr > 0.0 {
            log::debug!(
                "[governor] fleet_pct_hr: baseline dollar fallback active \
                 (fleet_usd_hr={:.4}/hr, usd_per_pct={:.3}) → \
                 5h={:.4} 7d={:.4} 7ds={:.4} pct/hr",
                fleet_usd_hr,
                BASELINE_USD_PER_PCT,
                map["five_hour"],
                map["seven_day"],
                map["seven_day_sonnet"],
            );
        }

        map
    };

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
    // - target_ceiling: reduced by SAFE_MODE_CEILING_REDUCTION pct points
    // - hysteresis_band: widened by SAFE_MODE_HYSTERESIS_MULTIPLIER
    // - composite risk: disabled (cross-window optimisation is too uncertain)
    // - sprint: sprint eligibility is also blocked (checked in check_underutilization_sprint)
    let effective_target_ceiling = if state.safe_mode.active {
        let reduced = target_ceiling - SAFE_MODE_CEILING_REDUCTION;
        log::info!(
            "[governor] safe_mode active: target_ceiling {:.0}% → {:.0}%",
            target_ceiling,
            reduced
        );
        reduced.max(50.0) // never below 50%
    } else {
        target_ceiling
    };

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
    let mut seven_day_sonnet_forecast = state::WindowForecast::default();

    for window in &["five_hour", "seven_day", "seven_day_sonnet"] {
        let util = current_utilization.get(*window).copied().unwrap_or(0.0);
        let hrs_left = hours_remaining.get(*window).copied().unwrap_or(0.0);
        let fleet_pct_hr = fleet_pct_per_hour.get(*window).copied().unwrap_or(0.0);

        // Per-worker pct/hr rate for safe_worker_count calculation
        let pct_per_worker = if current_total > 0 && fleet_pct_hr > 0.0 {
            fleet_pct_hr / current_total as f64
        } else {
            0.0
        };

        // Convert per-worker USD/hr stddev to pct/hr stddev using per-window USD-per-pct ratio.
        // Falls back to baseline ratio (~3.33 $/pct) when the learned ratio is unavailable.
        const BASELINE_USD_PER_PCT: f64 = 5.0 / 1.5;
        let usd_per_pct = match *window {
            "five_hour" => state.burn_rate.usd_per_pct_ema_five_hour,
            "seven_day" => state.burn_rate.usd_per_pct_ema_seven_day,
            "seven_day_sonnet" => state.burn_rate.usd_per_pct_ema_seven_day_sonnet,
            _ => 0.0,
        };
        let effective_usd_per_pct = if usd_per_pct > 0.0 {
            usd_per_pct
        } else {
            BASELINE_USD_PER_PCT
        };
        let std_pct_hr = state.last_fleet_aggregate.sonnet_std_usd_hr / effective_usd_per_pct;

        let forecast = generate_window_forecast(
            window,
            fleet_pct_hr,
            util,
            effective_target_ceiling,
            hrs_left,
            pct_per_worker,
            std_pct_hr,
        );

        match *window {
            "five_hour" => five_hour_forecast = forecast,
            "seven_day" => seven_day_forecast = forecast,
            "seven_day_sonnet" => seven_day_sonnet_forecast = forecast,
            _ => {}
        }
    }

    // Identify binding window (highest risk_score)
    // The risk_score combines margin urgency, duration weight, and volatility (cone_ratio).
    // Higher risk_score = more urgent window that should drive scaling decisions.
    let windows = [
        ("five_hour", &five_hour_forecast),
        ("seven_day", &seven_day_forecast),
        ("seven_day_sonnet", &seven_day_sonnet_forecast),
    ];

    let binding_window = windows
        .iter()
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
    } else if binding_window == "seven_day_sonnet" {
        seven_day_sonnet_forecast.binding = true;
    }

    // Update state with new capacity forecast
    state.capacity_forecast = state::CapacityForecast {
        five_hour: five_hour_forecast,
        seven_day: seven_day_forecast,
        seven_day_sonnet: seven_day_sonnet_forecast,
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
    let mult_seven_day_sonnet = if is_peak {
        1.0
    } else {
        let applies = promotions.iter().any(|p| {
            p.applies_to.iter().any(|w| w == "seven_day_sonnet")
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
    let eff_seven_day_sonnet = hours_remaining
        .get("seven_day_sonnet")
        .copied()
        .unwrap_or(0.0);
    // Effective hours for display: use the binding window's value
    let eff_display = match binding_window.as_str() {
        "five_hour" => eff_five_hour,
        "seven_day" => eff_seven_day,
        _ => eff_seven_day_sonnet,
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
        promo_multiplier_seven_day_sonnet: mult_seven_day_sonnet,
        // max across windows for backward-compatible display
        promo_multiplier: [mult_five_hour, mult_seven_day, mult_seven_day_sonnet]
            .iter()
            .cloned()
            .fold(1.0_f64, f64::max),
        effective_hours_remaining_five_hour: eff_five_hour,
        effective_hours_remaining_seven_day: eff_seven_day,
        effective_hours_remaining_seven_day_sonnet: eff_seven_day_sonnet,
        effective_hours_remaining: eff_display,
        raw_hours_remaining: raw_hours,
    };

    // Update burn_rate from fleet aggregate if we have valid data
    if elapsed_hours > 0.0 && current_total > 0 {
        let deltas = &state.last_fleet_aggregate.window_pct_deltas;
        let total_pct_delta = deltas.five_hour + deltas.seven_day + deltas.seven_day_sonnet;
        let avg_pct_per_hour = total_pct_delta / (elapsed_hours * 3.0); // Average across windows

        let entry = state
            .burn_rate
            .by_model
            .entry("claude-sonnet-4-20250514".to_string())
            .or_insert(state::ModelBurnRate {
                pct_per_worker_per_hour: 0.0,
                dollars_per_worker_per_hour: 0.0,
                samples: 0,
            });

        // Compute per-worker rates
        let pct_per_worker = avg_pct_per_hour / current_total as f64;
        let usd_per_worker = state.last_fleet_aggregate.sonnet_p75_usd_hr / current_total as f64;

        entry.pct_per_worker_per_hour = pct_per_worker;
        entry.dollars_per_worker_per_hour = usd_per_worker;
        entry.samples = entry.samples.saturating_add(1);
        state.burn_rate.last_sample_at = Some(now);
    }

    // 6. Log capacity forecast
    log_capacity_forecast(&state.capacity_forecast);

    // 4. Compute target workers
    let target = compute_target_workers(
        &state,
        effective_target_ceiling,
        effective_composite_risk,
        effective_cone_scaling,
    );
    log::info!(
        "[governor] target workers: {} (ceiling: {:.0}%{})",
        target,
        effective_target_ceiling,
        if state.safe_mode.active {
            ", safe_mode"
        } else {
            ""
        }
    );

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
                "seven_day_sonnet",
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
    // Use the first configured agent's WorkerConfig for scaling operations
    let primary_worker_config = &worker_configs[0].1;
    match &decision {
        ScalingDecision::NoChange => {
            log::info!("[governor] no scaling action this cycle");
        }
        ScalingDecision::ScaleUp(n) => {
            log::info!("[governor] scaling up by {} workers", n);
            if !dry_run {
                let launched = worker::scale_up(*n, primary_worker_config, false);
                log::info!("[governor] launched {} workers", launched);
            } else {
                log::info!("[governor] DRY RUN: would scale up by {}", n);
            }
        }
        ScalingDecision::ScaleDown(n) => {
            log::info!("[governor] gracefully scaling down by {} workers", n);
            if !dry_run {
                let result = worker::scale_down_graceful(*n, primary_worker_config, false);
                log::info!(
                    "[governor] scaled down: {} graceful, {} force-killed",
                    result.graceful,
                    result.force_killed
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

    // 7. Update target in state
    match &decision {
        ScalingDecision::EmergencyBrake => {
            for ws in state.workers.values_mut() {
                ws.target = 0;
            }
        }
        ScalingDecision::ScaleUp(_n) | ScalingDecision::ScaleDown(_n) => {
            let new_total = match &decision {
                ScalingDecision::ScaleUp(n) => current_total.saturating_add(*n),
                ScalingDecision::ScaleDown(n) => current_total.saturating_sub(*n),
                _ => current_total,
            };
            let per_agent = new_total / agent_count as u32;
            let remainder = new_total % agent_count as u32;
            for (i, ws) in state.workers.values_mut().enumerate() {
                let extra = if (i as u32) < remainder { 1 } else { 0 };
                ws.target = per_agent + extra;
            }
        }
        ScalingDecision::NoChange => {
            // Still update target to reflect current desired state
            let per_agent = effective_target / agent_count as u32;
            let remainder = effective_target % agent_count as u32;
            for (i, ws) in state.workers.values_mut().enumerate() {
                let extra = if (i as u32) < remainder { 1 } else { 0 };
                ws.target = per_agent + extra;
            }
        }
    }

    // 8. Check alerts and fire via configured command
    let mut alert_conditions = check_alert_conditions(&state, now);
    alert_conditions.extend(check_low_cache_efficiency(&state, alert_config, now));
    for alert in &alert_conditions {
        if should_fire(
            alert.alert_type,
            &state.alert_cooldown,
            now,
            alert_config.cooldown_minutes,
        ) {
            // Fire the alert: execute configured command (e.g. br create --type human)
            // and log to governor.log
            if let Err(e) = fire_alert(alert, alert_config) {
                log::warn!("[governor] alert fire failed: {}", e);
            }
            update_cooldown(&mut state.alert_cooldown, alert.alert_type, now);
            state.alerts.push(serde_json::json!({
                "type": alert.alert_type.to_string(),
                "message": alert.message,
                "severity": format!("{:?}", alert.severity),
                "detected_at": alert.detected_at.to_rfc3339(),
            }));
        }
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
    let mut poller = match Poller::new() {
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
            applies_to: vec!["seven_day_sonnet".to_string()],
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

        let t = schedule::next_transition_from(now, deadline, &promos, "seven_day_sonnet")
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

        let result =
            compute_pre_scale_target(now, 30, &promos, reset_time, 4, 4, "seven_day_sonnet");

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

        let result =
            compute_pre_scale_target(now, 30, &promos, reset_time, 4, 4, "seven_day_sonnet");

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

        let result =
            compute_pre_scale_target(now, 30, &promos, reset_time, 4, 4, "seven_day_sonnet");

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
        let result =
            compute_pre_scale_target(now, 30, &promos, reset_time, 2, 2, "seven_day_sonnet");
        // post_target = floor(2 * 0.5) = 1, effective = max(1, 2-1) = max(1,1) = 1
        assert_eq!(result, Some(1));

        // Now test where current_total already equals post_transition_target: no trigger
        let result_at_target =
            compute_pre_scale_target(now, 30, &promos, reset_time, 0, 0, "seven_day_sonnet");
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
        let result =
            compute_pre_scale_target(now, 0, &promos, reset_time, 4, 4, "seven_day_sonnet");
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

        let t = schedule::next_transition_from(now, deadline, &promos, "seven_day_sonnet").unwrap();
        assert_eq!(t.minutes_until, 120);
        assert!(t.minutes_until > 30, "outside 30-minute window");
    }

    #[test]
    fn pre_scale_never_triggers_for_gaining_bonus() {
        let promos = vec![march_2026_promo()];
        let now = et(2026, 3, 16, 13, 45);
        let deadline = now + chrono::Duration::hours(1);

        let t = schedule::next_transition_from(now, deadline, &promos, "seven_day_sonnet").unwrap();
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
        let mult_7ds = schedule::get_multiplier_at(t, &promos, "seven_day_sonnet");

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
            "seven_day_sonnet should get 1.0x (not in applies_to), got {mult_7ds}"
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
            "seven_day_sonnet",
            estimated_pct_hr,
            50.0,                   // current utilization
            90.0,                   // target ceiling
            24.0,                   // hours remaining
            estimated_pct_hr / 2.0, // mean per-worker (half fleet for 2 workers)
            0.0,                    // std_pct_hr (no spread data in this test)
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

    // --- safe_worker_count_or_max fallback tests ---

    #[test]
    fn safe_worker_count_none_uses_max_workers() {
        // None → max_workers, not current_total
        assert_eq!(safe_worker_count_or_max(None, 8, 3), 8);
    }

    #[test]
    fn safe_worker_count_some_zero_uses_current_total() {
        // Some(0) → current_total (near-exhaustion, avoid cold-start drop)
        assert_eq!(safe_worker_count_or_max(Some(0), 8, 3), 3);
    }

    #[test]
    fn safe_worker_count_some_nonzero_uses_value() {
        assert_eq!(safe_worker_count_or_max(Some(5), 8, 3), 5);
    }

    #[test]
    fn compute_target_workers_none_safe_count_falls_back_to_max() {
        // When safe_worker_count is None (zero burn rate, no data yet), the governor
        // must fall back to global_max rather than current_total to keep the fleet
        // running at full capacity.
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
        state.capacity_forecast.binding_window = "seven_day_sonnet".to_string();
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

        // Should be global_max (6), clamped to [min=1, max=6]
        assert_eq!(
            target, 6,
            "expected fallback to max_workers=6 when safe_worker_count is None"
        );
    }
}
