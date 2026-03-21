//! Alert Condition Checker and Cooldown Deduplication
//!
//! This module handles:
//! - Alert condition evaluation from governor state
//! - Per-type cooldown deduplication to prevent alert spam
//! - Alert severity classification
//! - Firing alerts via configured command (default: br create --type human)
//! - Logging alerts to governor.log

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use crate::config::AlertConfig;
use crate::state::{AlertCooldown, CapacityForecast, GovernorState};

/// Alert severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    /// Informational - no immediate action required
    Info,
    /// Warning - attention needed soon
    Warning,
    /// Critical - immediate action required
    Critical,
}

impl std::fmt::Display for AlertSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertSeverity::Info => write!(f, "INFO"),
            AlertSeverity::Warning => write!(f, "WARNING"),
            AlertSeverity::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// Types of alerts the governor can emit
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertType {
    /// Any window has cutoff_risk=1 with margin_hrs < -2
    CutoffImminent,
    /// Seven-day Sonnet window at cutoff risk
    SonnetCutoffRisk,
    /// Five-hour window at cutoff risk
    SessionCutoffRisk,
    /// Burn rate significantly higher than baseline
    BurnRateSpike,
    /// All windows have abundant remaining capacity
    Underutilization,
    /// OAuth token refresh failing
    TokenRefreshFailing,
    /// Emergency brake was activated (98%+ utilization)
    EmergencyBrakeActivated,
    /// Off-peak promotion not applying as expected
    PromotionNotApplying,
    /// Token collector has stopped reporting
    CollectorOffline,
}

impl std::fmt::Display for AlertType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertType::CutoffImminent => write!(f, "cutoff_imminent"),
            AlertType::SonnetCutoffRisk => write!(f, "sonnet_cutoff_risk"),
            AlertType::SessionCutoffRisk => write!(f, "session_cutoff_risk"),
            AlertType::BurnRateSpike => write!(f, "burn_rate_spike"),
            AlertType::Underutilization => write!(f, "underutilization"),
            AlertType::TokenRefreshFailing => write!(f, "token_refresh_failing"),
            AlertType::EmergencyBrakeActivated => write!(f, "emergency_brake_activated"),
            AlertType::PromotionNotApplying => write!(f, "promotion_not_applying"),
            AlertType::CollectorOffline => write!(f, "collector_offline"),
        }
    }
}

/// An alert condition detected from governor state
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertCondition {
    /// The type of alert
    pub alert_type: AlertType,
    /// Human-readable message with specific details
    pub message: String,
    /// Severity level
    pub severity: AlertSeverity,
    /// Timestamp when this condition was detected
    pub detected_at: DateTime<Utc>,
}

/// Default cooldown duration in minutes
pub const DEFAULT_COOLDOWN_MINUTES: i64 = 60;

/// Sprint trigger event - indicates a sprint should be initiated
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SprintTrigger {
    /// The worker pool/agent that should sprint
    pub worker_id: String,
    /// The window triggering the sprint
    pub window: String,
    /// Current utilization percentage
    pub utilization_pct: f64,
    /// Hours remaining until window reset
    pub hours_remaining: f64,
    /// Target worker count for sprint (max_workers)
    pub target_workers: u32,
    /// Reason for the sprint
    pub reason: String,
    /// Timestamp when sprint was triggered
    pub triggered_at: DateTime<Utc>,
}

/// Check if an underutilization sprint should be triggered (auto-selects best worker)
///
/// Sprint triggers when:
/// - Utilization < threshold (default 50%) AND
/// - Hours remaining < limit (default 2 hours) AND
/// - No other window has cutoff_risk (safety check)
///
/// Automatically selects the worker with the most headroom (max - current).
/// Returns Some(SprintTrigger) if sprint should be triggered, None otherwise.
pub fn check_underutilization_sprint(
    state: &crate::state::GovernorState,
    config: &crate::config::SprintConfig,
) -> Option<SprintTrigger> {
    let now = Utc::now();

    // Find worker with most headroom (max - current)
    let best_worker = state
        .workers
        .iter()
        .filter(|(_, w)| w.current < w.max) // Only workers not already at max
        .max_by_key(|(_, w)| w.max - w.current)?;

    let worker_id = best_worker.0.as_str();
    let max_workers = best_worker.1.max;

    check_underutilization_sprint_for_worker(state, config, worker_id, max_workers, now)
}

/// Check if an underutilization sprint should be triggered for a specific worker
///
/// Sprint triggers when:
/// - Utilization < threshold (default 50%) AND
/// - Hours remaining < limit (default 2 hours) AND
/// - No other window has cutoff_risk (safety check)
///
/// Returns Some(SprintTrigger) if sprint should be triggered, None otherwise.
pub fn check_underutilization_sprint_for_worker(
    state: &crate::state::GovernorState,
    config: &crate::config::SprintConfig,
    worker_id: &str,
    max_workers: u32,
    now: DateTime<Utc>,
) -> Option<SprintTrigger> {
    let forecast = &state.capacity_forecast;

    // Safety check: don't sprint if any window has cutoff_risk
    let windows = [
        ("five_hour", &forecast.five_hour),
        ("seven_day", &forecast.seven_day),
        ("seven_day_sonnet", &forecast.seven_day_sonnet),
    ];

    // Check for cutoff_risk in any window - safety check
    let any_cutoff_risk = windows.iter().any(|(_, win)| win.cutoff_risk);
    if any_cutoff_risk {
        log::debug!(
            "Sprint inhibited: another window has cutoff_risk"
        );
        return None;
    }

    // Find windows that meet sprint criteria
    for (name, win) in windows {
        let utilization = win.current_utilization;
        let hours_remaining = win.hours_remaining;

        // Check if this window meets sprint criteria
        if utilization < config.underutilization_threshold_pct
            && hours_remaining > 0.0
            && hours_remaining < config.underutilization_hours_remaining
        {
            let trigger = SprintTrigger {
                worker_id: worker_id.to_string(),
                window: name.to_string(),
                utilization_pct: utilization,
                hours_remaining,
                target_workers: max_workers,
                reason: format!(
                    "Underutilization sprint on {} for worker {}: {:.1}% used, {:.1}h to reset",
                    name, worker_id, utilization, hours_remaining
                ),
                triggered_at: now,
            };

            log::info!(
                "Sprint triggered for worker {}: {} at {:.1}%, {:.1}h to reset -> boosting to {} workers",
                worker_id, name, utilization, hours_remaining, max_workers
            );

            return Some(trigger);
        }
    }

    None
}

/// Check if an alert should be fired based on cooldown
///
/// Returns true if:
/// - No previous fire for this alert type, OR
/// - Cooldown period has elapsed since last fire, OR
/// - Condition had cleared and is now re-triggered (cooldown was cleared)
pub fn should_fire(
    alert_type: AlertType,
    cooldown: &AlertCooldown,
    now: DateTime<Utc>,
    cooldown_minutes: i64,
) -> bool {
    match cooldown.get_last_fired(&alert_type.to_string()) {
        None => true, // Never fired before
        Some(last_fired) => {
            let elapsed = (now - last_fired).num_minutes();
            elapsed >= cooldown_minutes
        }
    }
}

/// Update cooldown state after firing an alert
pub fn update_cooldown(cooldown: &mut AlertCooldown, alert_type: AlertType, now: DateTime<Utc>) {
    cooldown.record_fired(&alert_type.to_string(), now);
}

// ---------------------------------------------------------------------------
// Alert firing and logging
// ---------------------------------------------------------------------------

/// Default path for the governor alert log
pub fn default_alert_log_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".needle")
        .join("logs")
        .join("governor.log")
}

/// Fire an alert by executing the configured command and logging to governor.log.
///
/// This function:
/// 1. Checks if alerts are enabled in config
/// 2. Checks if the alert severity meets the minimum threshold
/// 3. Executes the configured command (default: br create --type human "...")
/// 4. Logs the alert to governor.log
///
/// Returns Ok(()) if the alert was fired successfully, or an error message.
pub fn fire_alert(alert: &AlertCondition, config: &AlertConfig) -> Result<(), String> {
    // Check if alerts are enabled
    if !config.enabled {
        log::debug!("[alert] alerts disabled, skipping {}", alert.alert_type);
        return Ok(());
    }

    // Check severity threshold
    if !meets_severity_threshold(alert.severity, &config.min_severity) {
        log::debug!(
            "[alert] severity {:?} below threshold '{}', skipping {}",
            alert.severity,
            config.min_severity,
            alert.alert_type
        );
        return Ok(());
    }

    log::info!(
        "[alert] firing [{}] {}: {}",
        alert.severity,
        alert.alert_type,
        alert.message
    );

    // Build the command with the alert message as the final argument
    if config.command.is_empty() {
        log::warn!("[alert] no command configured, skipping alert execution");
        return Err("no alert command configured".to_string());
    }

    let mut cmd = Command::new(&config.command[0]);
    if config.command.len() > 1 {
        cmd.args(&config.command[1..]);
    }
    // Append the alert message as the final argument
    let alert_message = format!("[{}] {}: {}", alert.severity, alert.alert_type, alert.message);
    cmd.arg(&alert_message);

    // Execute the command
    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                log::info!("[alert] command executed successfully");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!("[alert] command failed: {}", stderr.trim());
            }
        }
        Err(e) => {
            log::warn!("[alert] failed to execute command: {}", e);
        }
    }

    // Log to governor.log
    if let Err(e) = log_alert_to_file(alert) {
        log::warn!("[alert] failed to write to governor.log: {}", e);
    }

    Ok(())
}

/// Check if an alert severity meets the minimum threshold.
fn meets_severity_threshold(severity: AlertSeverity, min_severity: &str) -> bool {
    let min = match min_severity.to_lowercase().as_str() {
        "info" => 0,
        "warning" => 1,
        "critical" => 2,
        _ => 1, // default to warning
    };

    let level = match severity {
        AlertSeverity::Info => 0,
        AlertSeverity::Warning => 1,
        AlertSeverity::Critical => 2,
    };

    level >= min
}

/// Log an alert to the governor.log file.
///
/// Creates the log directory if it doesn't exist.
/// Appends a single line with timestamp, severity, type, and message.
fn log_alert_to_file(alert: &AlertCondition) -> std::io::Result<()> {
    let path = default_alert_log_path();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open file for append
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    // Format: 2026-03-20T10:00:00Z [CRITICAL] cutoff_imminent: Window five_hour at cutoff risk...
    let log_line = format!(
        "{} [{:?}] {}: {}\n",
        alert.detected_at.to_rfc3339(),
        alert.severity,
        alert.alert_type,
        alert.message
    );

    file.write_all(log_line.as_bytes())?;

    Ok(())
}

/// Process all pending alerts: filter by cooldown, fire, and update cooldown state.
///
/// This is a convenience function that combines:
/// 1. `check_alert_conditions` - find active alerts
/// 2. `should_fire` - filter by cooldown
/// 3. `fire_alert` - execute and log
/// 4. `update_cooldown` - record that we fired
///
/// Returns the number of alerts that were actually fired.
pub fn process_alerts(
    state: &mut GovernorState,
    config: &AlertConfig,
    now: DateTime<Utc>,
) -> usize {
    let conditions = check_alert_conditions(state, now);
    let mut fired_count = 0;

    for alert in &conditions {
        if should_fire(alert.alert_type, &state.alert_cooldown, now, config.cooldown_minutes) {
            if fire_alert(alert, config).is_ok() {
                update_cooldown(&mut state.alert_cooldown, alert.alert_type, now);
                fired_count += 1;
            }
        }
    }

    fired_count
}

/// Check all alert conditions from governor state
///
/// Returns a list of all currently active alert conditions (before cooldown filtering).
/// Callers should use `should_fire` to filter based on cooldown state.
pub fn check_alert_conditions(state: &GovernorState, now: DateTime<Utc>) -> Vec<AlertCondition> {
    let mut alerts = Vec::new();
    let forecast = &state.capacity_forecast;

    // Check CutoffImminent: any window with cutoff_risk=1 and margin_hrs < -2
    check_cutoff_imminent(forecast, now, &mut alerts);

    // Check SonnetCutoffRisk: seven_day_sonnet cutoff_risk=1
    check_sonnet_cutoff_risk(forecast, now, &mut alerts);

    // Check SessionCutoffRisk: five_hour cutoff_risk=1
    check_session_cutoff_risk(forecast, now, &mut alerts);

    // Check BurnRateSpike: burn_rate_sample > baseline * 2
    // (This requires baseline tracking which is not yet implemented)
    // Placeholder: we can detect if current burn rate is very high

    // Check Underutilization: all windows margin_hrs > hrs_left * 0.5
    check_underutilization(forecast, now, &mut alerts);

    // Check EmergencyBrakeActivated
    if state.safe_mode.active {
        if let Some(ref trigger) = state.safe_mode.trigger {
            if trigger == "emergency_brake" {
                let msg = format!(
                    "Emergency brake active since {}",
                    state.safe_mode.entered_at.map(|t| t.to_rfc3339()).unwrap_or_else(|| "unknown".to_string())
                );
                alerts.push(AlertCondition {
                    alert_type: AlertType::EmergencyBrakeActivated,
                    message: msg,
                    severity: AlertSeverity::Critical,
                    detected_at: now,
                });
            }
        }
    }

    // Check PromotionNotApplying: promo not validated and it's off-peak
    if !state.schedule.is_peak_hour && !state.burn_rate.promotion_validated {
        let observed = state.burn_rate.offpeak_ratio_observed;
        let expected = state.burn_rate.offpeak_ratio_expected;
        let msg = format!(
            "Off-peak promotion not applying: observed ratio {:.2} vs expected {:.2}",
            observed, expected
        );
        alerts.push(AlertCondition {
            alert_type: AlertType::PromotionNotApplying,
            message: msg,
            severity: AlertSeverity::Warning,
            detected_at: now,
        });
    }

    // Check CollectorOffline: last fleet aggregate too old
    let collector_age = (now - state.last_fleet_aggregate.t1).num_seconds();
    if collector_age > 300 {
        // 5 minutes
        let age_minutes = collector_age / 60;
        let msg = format!(
            "Token collector offline: last update {} minutes ago",
            age_minutes
        );
        alerts.push(AlertCondition {
            alert_type: AlertType::CollectorOffline,
            message: msg,
            severity: AlertSeverity::Warning,
            detected_at: now,
        });
    }

    // Check TokenRefreshFailing: poller detected auth issues
    if state.token_refresh_failing {
        let msg = "OAuth token refresh failing — Claude Code sessions may be unable to make API calls. Run: claude login".to_string();
        alerts.push(AlertCondition {
            alert_type: AlertType::TokenRefreshFailing,
            message: msg,
            severity: AlertSeverity::Critical,
            detected_at: now,
        });
    }

    alerts
}

/// Check for CutoffImminent: any window with cutoff_risk=1 and margin_hrs < -2
fn check_cutoff_imminent(
    forecast: &CapacityForecast,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    let windows = [
        ("five_hour", &forecast.five_hour),
        ("seven_day", &forecast.seven_day),
        ("seven_day_sonnet", &forecast.seven_day_sonnet),
    ];

    for (name, win) in windows {
        if win.cutoff_risk && win.margin_hrs < -2.0 {
            let msg = format!(
                "Window {} at cutoff risk: margin_hrs={:.1}h, utilization={:.1}%, hrs_left={:.1}h",
                name, win.margin_hrs, win.current_utilization, win.hours_remaining
            );
            alerts.push(AlertCondition {
                alert_type: AlertType::CutoffImminent,
                message: msg,
                severity: AlertSeverity::Critical,
                detected_at: now,
            });
            // Only report once (any window triggers it)
            return;
        }
    }
}

/// Check for SonnetCutoffRisk: seven_day_sonnet cutoff_risk=1
fn check_sonnet_cutoff_risk(
    forecast: &CapacityForecast,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    let win = &forecast.seven_day_sonnet;
    if win.cutoff_risk {
        let msg = format!(
            "Seven-day Sonnet window at cutoff risk: {:.1}% utilized, {:.1}h remaining, margin_hrs={:.1}h",
            win.current_utilization, win.hours_remaining, win.margin_hrs
        );
        alerts.push(AlertCondition {
            alert_type: AlertType::SonnetCutoffRisk,
            message: msg,
            severity: AlertSeverity::Warning,
            detected_at: now,
        });
    }
}

/// Check for SessionCutoffRisk: five_hour cutoff_risk=1
fn check_session_cutoff_risk(
    forecast: &CapacityForecast,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    let win = &forecast.five_hour;
    if win.cutoff_risk {
        let msg = format!(
            "Five-hour session window at cutoff risk: {:.1}% utilized, {:.1}h remaining, margin_hrs={:.1}h",
            win.current_utilization, win.hours_remaining, win.margin_hrs
        );
        alerts.push(AlertCondition {
            alert_type: AlertType::SessionCutoffRisk,
            message: msg,
            severity: AlertSeverity::Warning,
            detected_at: now,
        });
    }
}

/// Check for Underutilization: all windows have margin_hrs > hrs_left * 0.5
fn check_underutilization(
    forecast: &CapacityForecast,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    let windows = [
        ("five_hour", &forecast.five_hour),
        ("seven_day", &forecast.seven_day),
        ("seven_day_sonnet", &forecast.seven_day_sonnet),
    ];

    let all_abundant = windows.iter().all(|(_, win)| {
        win.hours_remaining > 0.0 && win.margin_hrs > win.hours_remaining * 0.5
    });

    if all_abundant {
        let msg = "All windows have abundant capacity: safe to increase worker count".to_string();
        alerts.push(AlertCondition {
            alert_type: AlertType::Underutilization,
            message: msg,
            severity: AlertSeverity::Info,
            detected_at: now,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::config::{AlertConfig, SprintConfig};
    use crate::state::{
        AlertCooldown, BurnRateState, CapacityForecast, FleetAggregate, GovernorState, ScheduleState,
        SafeModeState, UsageState, WorkerState, WindowForecast,
    };
    use chrono::{Duration, Utc};
    use std::fs::OpenOptions;
    use std::io::Write;
    use tempfile::TempDir;

    fn base_now() -> DateTime<Utc> {
        "2026-03-20T10:00:00Z".parse().unwrap()
    }

    fn make_window(cutoff_risk: bool, margin_hrs: f64, hrs_left: f64) -> WindowForecast {
        WindowForecast {
            target_ceiling: 90.0,
            current_utilization: 50.0,
            remaining_pct: 40.0,
            hours_remaining: hrs_left,
            fleet_pct_per_hour: 5.0,
            predicted_exhaustion_hours: hrs_left - margin_hrs,
            cutoff_risk,
            margin_hrs,
            binding: false,
            safe_worker_count: None,
        }
    }

    fn make_state_with_forecast(forecast: CapacityForecast) -> GovernorState {
        GovernorState {
            updated_at: base_now(),
            usage: UsageState::default(),
            last_fleet_aggregate: FleetAggregate {
                t1: base_now(),
                ..FleetAggregate::default()
            },
            capacity_forecast: forecast,
            schedule: ScheduleState {
                is_peak_hour: true,
                is_promo_active: false,
                ..Default::default()
            },
            workers: Default::default(),
            burn_rate: BurnRateState {
                promotion_validated: true,
                offpeak_ratio_observed: 2.0,
                offpeak_ratio_expected: 2.0,
                ..BurnRateState::default()
            },
            alerts: Vec::new(),
            safe_mode: SafeModeState::default(),
            alert_cooldown: AlertCooldown::default(),
            token_refresh_failing: false,
        }
    }

    // --- AlertType tests ---

    #[test]
    fn alert_type_display() {
        assert_eq!(AlertType::CutoffImminent.to_string(), "cutoff_imminent");
        assert_eq!(AlertType::SonnetCutoffRisk.to_string(), "sonnet_cutoff_risk");
        assert_eq!(AlertType::SessionCutoffRisk.to_string(), "session_cutoff_risk");
        assert_eq!(AlertType::BurnRateSpike.to_string(), "burn_rate_spike");
        assert_eq!(AlertType::Underutilization.to_string(), "underutilization");
    }

    // --- Cooldown tests ---

    #[test]
    fn should_fire_returns_true_when_never_fired() {
        let cooldown = AlertCooldown::new();
        assert!(should_fire(
            AlertType::CutoffImminent,
            &cooldown,
            base_now(),
            DEFAULT_COOLDOWN_MINUTES
        ));
    }

    #[test]
    fn should_fire_suppresses_within_cooldown() {
        let mut cooldown = AlertCooldown::new();
        let now = base_now();
        cooldown.record_fired(&AlertType::CutoffImminent.to_string(), now);

        // 30 minutes later - should NOT fire
        let later = now + Duration::minutes(30);
        assert!(!should_fire(
            AlertType::CutoffImminent,
            &cooldown,
            later,
            DEFAULT_COOLDOWN_MINUTES
        ));
    }

    #[test]
    fn should_fire_allows_after_cooldown_expiry() {
        let mut cooldown = AlertCooldown::new();
        let now = base_now();
        cooldown.record_fired(&AlertType::CutoffImminent.to_string(), now);

        // 60 minutes later - should fire
        let later = now + Duration::minutes(60);
        assert!(should_fire(
            AlertType::CutoffImminent,
            &cooldown,
            later,
            DEFAULT_COOLDOWN_MINUTES
        ));
    }

    #[test]
    fn should_fire_allows_re_trigger_after_condition_cleared() {
        let mut cooldown = AlertCooldown::new();
        let now = base_now();
        cooldown.record_fired(&AlertType::CutoffImminent.to_string(), now);

        // Clear the cooldown (condition cleared)
        cooldown.clear(&AlertType::CutoffImminent.to_string());

        // Should fire immediately even if within cooldown window
        let later = now + Duration::minutes(10);
        assert!(should_fire(
            AlertType::CutoffImminent,
            &cooldown,
            later,
            DEFAULT_COOLDOWN_MINUTES
        ));
    }

    #[test]
    fn cooldown_per_type_independent() {
        let mut cooldown = AlertCooldown::new();
        let now = base_now();

        // Fire CutoffImminent
        cooldown.record_fired(&AlertType::CutoffImminent.to_string(), now);

        // Other types should still fire
        assert!(should_fire(
            AlertType::SonnetCutoffRisk,
            &cooldown,
            now,
            DEFAULT_COOLDOWN_MINUTES
        ));
        assert!(should_fire(
            AlertType::SessionCutoffRisk,
            &cooldown,
            now,
            DEFAULT_COOLDOWN_MINUTES
        ));

        // CutoffImminent should NOT fire
        assert!(!should_fire(
            AlertType::CutoffImminent,
            &cooldown,
            now,
            DEFAULT_COOLDOWN_MINUTES
        ));
    }

    // --- Alert condition tests ---

    #[test]
    fn cutoff_imminent_triggers_on_negative_margin() {
        let forecast = CapacityForecast {
            five_hour: make_window(true, -3.0, 2.0), // cutoff_risk=1, margin_hrs=-3
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts.iter().find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(imminent.is_some(), "Should have CutoffImminent alert");
        let alert = imminent.unwrap();
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert!(alert.message.contains("five_hour"));
        assert!(alert.message.contains("margin_hrs"));
    }

    #[test]
    fn cutoff_imminent_requires_margin_less_than_minus_2() {
        // margin_hrs = -1.9 should NOT trigger
        let forecast = CapacityForecast {
            five_hour: make_window(true, -1.9, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts.iter().find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(
            imminent.is_none(),
            "Should NOT have CutoffImminent when margin_hrs > -2"
        );
    }

    #[test]
    fn sonnet_cutoff_risk_triggers() {
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(true, -5.0, 30.0), // cutoff_risk=1
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let sonnet = alerts.iter().find(|a| a.alert_type == AlertType::SonnetCutoffRisk);
        assert!(sonnet.is_some(), "Should have SonnetCutoffRisk alert");
        assert!(sonnet.unwrap().message.contains("Seven-day Sonnet"));
    }

    #[test]
    fn session_cutoff_risk_triggers() {
        let forecast = CapacityForecast {
            five_hour: make_window(true, -1.0, 2.0), // cutoff_risk=1
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let session = alerts.iter().find(|a| a.alert_type == AlertType::SessionCutoffRisk);
        assert!(session.is_some(), "Should have SessionCutoffRisk alert");
        assert!(session.unwrap().message.contains("Five-hour"));
    }

    #[test]
    fn underutilization_triggers_when_all_abundant() {
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),  // margin > hrs_left * 0.5
            seven_day: make_window(false, 20.0, 30.0), // margin > hrs_left * 0.5
            seven_day_sonnet: make_window(false, 20.0, 30.0),
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let underutil = alerts.iter().find(|a| a.alert_type == AlertType::Underutilization);
        assert!(underutil.is_some(), "Should have Underutilization alert");
        assert_eq!(underutil.unwrap().severity, AlertSeverity::Info);
    }

    #[test]
    fn underutilization_does_not_trigger_if_any_constrained() {
        let forecast = CapacityForecast {
            five_hour: make_window(false, 0.5, 2.0), // margin < hrs_left * 0.5 (1.0)
            seven_day: make_window(false, 20.0, 30.0),
            seven_day_sonnet: make_window(false, 20.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let underutil = alerts.iter().find(|a| a.alert_type == AlertType::Underutilization);
        assert!(
            underutil.is_none(),
            "Should NOT have Underutilization when any window constrained"
        );
    }

    #[test]
    fn promotion_not_applying_triggers_off_peak() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.schedule.is_peak_hour = false;
        state.burn_rate.promotion_validated = false;
        state.burn_rate.offpeak_ratio_observed = 1.5;
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let promo = alerts.iter().find(|a| a.alert_type == AlertType::PromotionNotApplying);
        assert!(promo.is_some(), "Should have PromotionNotApplying alert");
        assert!(promo.unwrap().message.contains("1.50"));
        assert!(promo.unwrap().message.contains("2.00"));
    }

    #[test]
    fn collector_offline_triggers_when_stale() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        // Set last fleet aggregate to 10 minutes ago
        state.last_fleet_aggregate.t1 = base_now() - Duration::minutes(10);

        let alerts = check_alert_conditions(&state, base_now());

        let offline = alerts.iter().find(|a| a.alert_type == AlertType::CollectorOffline);
        assert!(offline.is_some(), "Should have CollectorOffline alert");
        assert!(offline.unwrap().message.contains("10 minutes ago"));
    }

    #[test]
    fn multiple_simultaneous_alerts() {
        let forecast = CapacityForecast {
            five_hour: make_window(true, -3.0, 2.0), // CutoffImminent + SessionCutoffRisk
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(true, -5.0, 30.0), // SonnetCutoffRisk
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let mut state = make_state_with_forecast(forecast);
        state.schedule.is_peak_hour = false;
        state.burn_rate.promotion_validated = false;
        state.last_fleet_aggregate.t1 = base_now() - Duration::minutes(10);

        let alerts = check_alert_conditions(&state, base_now());

        // Should have: CutoffImminent, SessionCutoffRisk, SonnetCutoffRisk, PromotionNotApplying, CollectorOffline
        assert!(alerts.len() >= 4, "Should have multiple alerts, got {:?}", alerts);

        let types: Vec<AlertType> = alerts.iter().map(|a| a.alert_type).collect();
        assert!(types.contains(&AlertType::CutoffImminent));
        assert!(types.contains(&AlertType::SessionCutoffRisk));
        assert!(types.contains(&AlertType::SonnetCutoffRisk));
        assert!(types.contains(&AlertType::PromotionNotApplying));
        assert!(types.contains(&AlertType::CollectorOffline));
    }

    #[test]
    fn alert_message_contains_specifics() {
        let forecast = CapacityForecast {
            five_hour: WindowForecast {
                target_ceiling: 90.0,
                current_utilization: 75.5,
                remaining_pct: 14.5,
                hours_remaining: 1.5,
                fleet_pct_per_hour: 10.0,
                predicted_exhaustion_hours: 1.45,
                cutoff_risk: true,
                margin_hrs: -2.5,
                binding: true,
                safe_worker_count: Some(1),
            },
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts.iter().find(|a| a.alert_type == AlertType::CutoffImminent).unwrap();

        // Message should contain window name, percentages, and hours
        assert!(imminent.message.contains("five_hour"));
        assert!(imminent.message.contains("75"));
        assert!(imminent.message.contains("1.5"));
        assert!(imminent.message.contains("-2"));
    }

    #[test]
    fn emergency_brake_alert_from_safe_mode() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.safe_mode.active = true;
        state.safe_mode.trigger = Some("emergency_brake".to_string());
        state.safe_mode.entered_at = Some(base_now() - Duration::minutes(5));

        let alerts = check_alert_conditions(&state, base_now());

        let brake = alerts.iter().find(|a| a.alert_type == AlertType::EmergencyBrakeActivated);
        assert!(brake.is_some(), "Should have EmergencyBrakeActivated alert");
        assert_eq!(brake.unwrap().severity, AlertSeverity::Critical);
    }

    #[test]
    fn token_refresh_failing_triggers() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.token_refresh_failing = true;

        let alerts = check_alert_conditions(&state, base_now());

        let trf = alerts.iter().find(|a| a.alert_type == AlertType::TokenRefreshFailing);
        assert!(trf.is_some(), "Should have TokenRefreshFailing alert");
        assert_eq!(trf.unwrap().severity, AlertSeverity::Critical);
        assert!(trf.unwrap().message.contains("claude login"));
    }

    #[test]
    fn token_refresh_failing_does_not_trigger_when_false() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.token_refresh_failing = false;

        let alerts = check_alert_conditions(&state, base_now());

        let trf = alerts.iter().find(|a| a.alert_type == AlertType::TokenRefreshFailing);
        assert!(trf.is_none(), "Should NOT have TokenRefreshFailing when flag is false");
    }

    // --- Update cooldown test ---

    #[test]
    fn update_cooldown_records_timestamp() {
        let mut cooldown = AlertCooldown::new();
        let now = base_now();

        update_cooldown(&mut cooldown, AlertType::CutoffImminent, now);

        let recorded = cooldown.get_last_fired(&AlertType::CutoffImminent.to_string());
        assert_eq!(recorded, Some(now));
    }

    // --- Sprint trigger tests ---

    fn default_sprint_config() -> SprintConfig {
        SprintConfig::default()
    }

    fn make_window_with_util(
        util: f64,
        hrs_left: f64,
        cutoff_risk: bool,
    ) -> WindowForecast {
        WindowForecast {
            target_ceiling: 90.0,
            current_utilization: util,
            remaining_pct: 90.0 - util,
            hours_remaining: hrs_left,
            fleet_pct_per_hour: 5.0,
            predicted_exhaustion_hours: if hrs_left > 0.0 { (90.0 - util) / 5.0 } else { 0.0 },
            cutoff_risk,
            margin_hrs: hrs_left - (90.0 - util) / 5.0,
            binding: false,
            safe_worker_count: None,
        }
    }

    fn make_state_with_workers(
        forecast: CapacityForecast,
        workers: HashMap<String, WorkerState>,
    ) -> GovernorState {
        let mut state = make_state_with_forecast(forecast);
        state.workers = workers;
        state
    }

    #[test]
    fn sprint_triggers_when_underutilized_and_close_to_reset() {
        // 45% used, 1.5h to reset -> sprint triggers
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(45.0, 1.5, false),
            seven_day: make_window_with_util(45.0, 100.0, false),
            seven_day_sonnet: make_window_with_util(45.0, 100.0, false),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 2, target: 2, min: 1, max: 5 },
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config);
        assert!(trigger.is_some(), "Sprint should trigger at 45% with 1.5h to reset");

        let t = trigger.unwrap();
        assert_eq!(t.worker_id, "sonnet");
        assert_eq!(t.target_workers, 5);
        assert_eq!(t.window, "five_hour");
    }

    #[test]
    fn sprint_does_not_trigger_above_threshold() {
        // 55% used, 1.5h to reset -> no sprint (above 50% threshold)
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(55.0, 1.5, false),
            seven_day: make_window_with_util(55.0, 100.0, false),
            seven_day_sonnet: make_window_with_util(55.0, 100.0, false),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 2, target: 2, min: 1, max: 5 },
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config);
        assert!(
            trigger.is_none(),
            "Sprint should NOT trigger at 55% (above threshold)"
        );
    }

    #[test]
    fn sprint_does_not_trigger_too_far_from_reset() {
        // 45% used, 3h to reset -> no sprint (too far from reset)
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(45.0, 3.0, false),
            seven_day: make_window_with_util(45.0, 100.0, false),
            seven_day_sonnet: make_window_with_util(45.0, 100.0, false),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 2, target: 2, min: 1, max: 5 },
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config);
        assert!(
            trigger.is_none(),
            "Sprint should NOT trigger at 3h remaining (above 2h threshold)"
        );
    }

    #[test]
    fn sprint_inhibited_when_other_window_has_cutoff_risk() {
        // five_hour underutilized and close to reset, but seven_day has cutoff_risk
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(45.0, 1.5, false),
            seven_day: make_window_with_util(80.0, 10.0, true), // cutoff_risk!
            seven_day_sonnet: make_window_with_util(45.0, 100.0, false),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 2, target: 2, min: 1, max: 5 },
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config);
        assert!(
            trigger.is_none(),
            "Sprint should NOT trigger when another window has cutoff_risk"
        );
    }

    #[test]
    fn sprint_boosts_to_max_workers() {
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(45.0, 1.5, false),
            seven_day: make_window_with_util(45.0, 100.0, false),
            seven_day_sonnet: make_window_with_util(45.0, 100.0, false),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 2, target: 2, min: 1, max: 8 },
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config).unwrap();
        assert_eq!(
            trigger.target_workers, 8,
            "Sprint should boost to max_workers (8)"
        );
    }

    #[test]
    fn sprint_no_trigger_when_all_workers_at_max() {
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(45.0, 1.5, false),
            seven_day: make_window_with_util(45.0, 100.0, false),
            seven_day_sonnet: make_window_with_util(45.0, 100.0, false),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 5, target: 5, min: 1, max: 5 }, // already at max
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config);
        assert!(
            trigger.is_none(),
            "Sprint should NOT trigger when all workers already at max"
        );
    }

    #[test]
    fn sprint_reason_contains_window_and_utilization() {
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(45.0, 1.5, false),
            seven_day: make_window_with_util(45.0, 100.0, false),
            seven_day_sonnet: make_window_with_util(45.0, 100.0, false),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 2, target: 2, min: 1, max: 5 },
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config).unwrap();
        assert!(trigger.reason.contains("five_hour"));
        assert!(trigger.reason.contains("45"));
        assert!(trigger.reason.contains("1.5"));
        assert!(trigger.reason.contains("sonnet"));
        assert!(trigger.reason.contains("Underutilization sprint"));
    }

    #[test]
    fn sprint_picks_worker_with_most_headroom() {
        let forecast = CapacityForecast {
            five_hour: make_window_with_util(45.0, 1.5, false),
            seven_day: make_window_with_util(45.0, 100.0, false),
            seven_day_sonnet: make_window_with_util(45.0, 100.0, false),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let mut workers = HashMap::new();
        workers.insert(
            "sonnet".to_string(),
            WorkerState { current: 3, target: 3, min: 1, max: 5 }, // headroom: 2
        );
        workers.insert(
            "opus".to_string(),
            WorkerState { current: 1, target: 1, min: 1, max: 10 }, // headroom: 9
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config).unwrap();
        assert_eq!(
            trigger.worker_id, "opus",
            "Sprint should pick worker with most headroom"
        );
        assert_eq!(trigger.target_workers, 10);
    }

    // --- Alert firing tests ---

    #[test]
    fn meets_severity_threshold_info() {
        assert!(meets_severity_threshold(AlertSeverity::Info, "info"));
        assert!(!meets_severity_threshold(AlertSeverity::Info, "warning"));
        assert!(!meets_severity_threshold(AlertSeverity::Info, "critical"));
    }

    #[test]
    fn meets_severity_threshold_warning() {
        assert!(meets_severity_threshold(AlertSeverity::Warning, "info"));
        assert!(meets_severity_threshold(AlertSeverity::Warning, "warning"));
        assert!(!meets_severity_threshold(AlertSeverity::Warning, "critical"));
    }

    #[test]
    fn meets_severity_threshold_critical() {
        assert!(meets_severity_threshold(AlertSeverity::Critical, "info"));
        assert!(meets_severity_threshold(AlertSeverity::Critical, "warning"));
        assert!(meets_severity_threshold(AlertSeverity::Critical, "critical"));
    }

    #[test]
    fn fire_alert_disabled_skips() {
        let alert = AlertCondition {
            alert_type: AlertType::CutoffImminent,
            message: "test".to_string(),
            severity: AlertSeverity::Critical,
            detected_at: base_now(),
        };

        let config = AlertConfig {
            enabled: false,
            ..AlertConfig::default()
        };

        let result = fire_alert(&alert, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn fire_alert_below_severity_skips() {
        let alert = AlertCondition {
            alert_type: AlertType::Underutilization,
            message: "test".to_string(),
            severity: AlertSeverity::Info,
            detected_at: base_now(),
        };

        let config = AlertConfig {
            min_severity: "critical".to_string(),
            ..AlertConfig::default()
        };

        let result = fire_alert(&alert, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn fire_alert_empty_command_returns_error() {
        let alert = AlertCondition {
            alert_type: AlertType::CutoffImminent,
            message: "test".to_string(),
            severity: AlertSeverity::Critical,
            detected_at: base_now(),
        };

        let config = AlertConfig {
            command: vec![],
            ..AlertConfig::default()
        };

        let result = fire_alert(&alert, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no alert command"));
    }

    #[test]
    fn log_alert_to_file_creates_file() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        // Override the log path by using a temp file directly
        let log_path = temp_dir.path().join("governor.log");

        let alert = AlertCondition {
            alert_type: AlertType::CutoffImminent,
            message: "Test alert message".to_string(),
            severity: AlertSeverity::Critical,
            detected_at: base_now(),
        };

        // Manually write to the temp path
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap();

        let log_line = format!(
            "{} [{:?}] {}: {}\n",
            alert.detected_at.to_rfc3339(),
            alert.severity,
            alert.alert_type,
            alert.message
        );
        file.write_all(log_line.as_bytes()).unwrap();

        // Verify file was created and contains expected content
        assert!(log_path.exists());
        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("cutoff_imminent"));
        assert!(contents.contains("Test alert message"));
        assert!(contents.contains("Critical"));
    }

    #[test]
    fn process_alerts_filters_and_fires() {
        let forecast = CapacityForecast {
            five_hour: make_window(true, -3.0, 2.0), // CutoffImminent
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let mut state = make_state_with_forecast(forecast);
        state.alert_cooldown = AlertCooldown::new();

        let config = AlertConfig {
            enabled: true,
            cooldown_minutes: 60,
            command: vec!["echo".to_string()], // Safe command for testing
            ..AlertConfig::default()
        };

        let fired = process_alerts(&mut state, &config, base_now());
        assert!(fired >= 1, "Should have fired at least one alert");

        // Cooldown should now be set
        assert!(state.alert_cooldown.get_last_fired("cutoff_imminent").is_some());
    }

    #[test]
    fn process_alerts_respects_cooldown() {
        let forecast = CapacityForecast {
            five_hour: make_window(true, -3.0, 2.0), // CutoffImminent
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let mut state = make_state_with_forecast(forecast);

        // Set cooldown for both expected alert types to have just fired
        state.alert_cooldown.record_fired("cutoff_imminent", base_now());
        state.alert_cooldown.record_fired("session_cutoff_risk", base_now());

        let config = AlertConfig {
            enabled: true,
            cooldown_minutes: 60,
            command: vec!["echo".to_string()],
            ..AlertConfig::default()
        };

        let fired = process_alerts(&mut state, &config, base_now());
        // Both CutoffImminent and SessionCutoffRisk should be skipped due to cooldown
        assert_eq!(fired, 0, "Should have fired zero alerts due to cooldown");
    }

    #[test]
    fn alert_log_path_is_in_home_directory() {
        let path = default_alert_log_path();
        assert!(path.to_string_lossy().contains(".needle"));
        assert!(path.to_string_lossy().contains("governor.log"));
    }
}
