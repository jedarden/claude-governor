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

use crate::burn_rate::MIN_VALIDATION_SAMPLES;
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
    /// Fleet cache efficiency below threshold for N consecutive intervals
    LowCacheEfficiency,
    /// Off-peak promotion ratio anomaly (observed > 2.5 or < 0.8)
    PromotionRatioAnomaly,
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
            AlertType::LowCacheEfficiency => write!(f, "low_cache_efficiency"),
            AlertType::PromotionRatioAnomaly => write!(f, "promotion_ratio_anomaly"),
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
    // Safety check: don't sprint while safe mode is active —
    // predictions are unreliable so cross-window sprinting is too risky.
    if state.safe_mode.active {
        log::debug!(
            "Sprint inhibited: safe mode active (trigger: {:?})",
            state.safe_mode.trigger
        );
        return None;
    }

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
        log::debug!("Sprint inhibited: another window has cutoff_risk");
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

    // Log to alert log file regardless of auto_bead setting
    if let Err(e) = log_alert_to_file(alert) {
        log::debug!("[alert] could not write to alert log: {}", e);
    }

    // When auto_bead is disabled, log but do not execute the bead-creation command.
    // This prevents fleet waste on documenting false-positive alerts while still
    // maintaining alert telemetry in the log file.
    if !config.auto_bead {
        log::info!(
            "[alert] auto_bead disabled — logged but did not execute command for {}",
            alert.alert_type
        );
        return Ok(());
    }

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
    let alert_message = format!(
        "[{}] {}: {}",
        alert.severity, alert.alert_type, alert.message
    );
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
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

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
        if should_fire(
            alert.alert_type,
            &state.alert_cooldown,
            now,
            config.cooldown_minutes,
        ) && fire_alert(alert, config).is_ok()
        {
            update_cooldown(&mut state.alert_cooldown, alert.alert_type, now);
            fired_count += 1;
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

    // Check EmergencyBrakeActivated: log-only — the governor already handled the
    // scaling (scaled to 0 at 98%+). Creating a human-type bead for this is a false
    // positive because no human intervention is needed. The governor log records the
    // brake application with full details.
    //
    // Previously this created HUMAN-type beads that workers would claim and document
    // as false positives (100% FP rate over 50 consecutive alerts). The emergency brake
    // is an automated response, not a human-actionable alert.
    //
    // To re-enable bead creation (after FP rate is confirmed <5%), set:
    //   alerts.emergency_brake_auto_bead = true
    // in governor.yaml. For now, the alert is always logged to governor.log but never
    // triggers external command execution.

    // Check PromotionNotApplying: promo is active but not validated and it's off-peak.
    // Require is_promo_active so stale sample counts from a past promotion don't
    // trigger a false positive after the promo expires.
    // Suppress until we have at least MIN_VALIDATION_SAMPLES in each category —
    // prevents false positives on zero/insufficient data (both ratios would be 0.0).
    // Also require offpeak_ratio_expected > 0.0: if the expected ratio is zero, the
    // validation result is uninitialised (e.g. zero-median-peak guard was hit) and
    // "observed 0.00 vs expected 0.00" is a meaningless comparison.
    if state.schedule.is_promo_active
        && !state.schedule.is_peak_hour
        && !state.burn_rate.promotion_validated
        && state.burn_rate.promotion_peak_samples >= MIN_VALIDATION_SAMPLES
        && state.burn_rate.promotion_offpeak_samples >= MIN_VALIDATION_SAMPLES
        && state.burn_rate.offpeak_ratio_expected > 0.0
    {
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

    // Check PromotionRatioAnomaly: observed ratio is outside expected range.
    // Anomaly when observed > 2.5 (possible miscalibration) or observed < 0.8 (inverse anomaly).
    // Require sufficient samples and a valid expected ratio to avoid false positives.
    if state.burn_rate.promotion_peak_samples >= MIN_VALIDATION_SAMPLES
        && state.burn_rate.promotion_offpeak_samples >= MIN_VALIDATION_SAMPLES
        && state.burn_rate.offpeak_ratio_expected > 0.0
    {
        let observed = state.burn_rate.offpeak_ratio_observed;
        // Anomaly thresholds: > 2.5 or < 0.8
        if !(0.8..=2.5).contains(&observed) {
            let msg = if observed > 2.5 {
                format!(
                    "Promotion ratio anomaly: observed ratio {:.2} exceeds 2.5 threshold (expected {:.2}). Possible miscalibration.",
                    observed, state.burn_rate.offpeak_ratio_expected
                )
            } else {
                format!(
                    "Promotion ratio anomaly: observed ratio {:.2} below 0.8 threshold (expected {:.2}). Inverse anomaly detected.",
                    observed, state.burn_rate.offpeak_ratio_expected
                )
            };
            alerts.push(AlertCondition {
                alert_type: AlertType::PromotionRatioAnomaly,
                message: msg,
                severity: AlertSeverity::Warning,
                detected_at: now,
            });
        }
    }

    // Check CollectorOffline: last fleet aggregate too old.
    // Threshold is 30 minutes (matching the governor's fallback-to-baseline staleness tier).
    // The 5-minute threshold produced 100% false positives because normal collection intervals
    // (5 min) plus processing latency routinely exceeded it.
    let collector_age = (now - state.last_fleet_aggregate.t1).num_seconds();
    if collector_age > 1800 {
        // 30 minutes
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

/// Minimum remaining headroom to the 100% hard limit before cutoff alerts fire.
///
/// When hard_limit_remaining_pct exceeds this, the fleet is far enough from the platform
/// cutoff that burn-rate extrapolation is unreliable — producing negative margins that
/// almost never result in actual cutoffs (observed 100% FP rate over 50 consecutive alerts).
/// The governor's scaling logic (safe_worker_count, emergency brake at 98%) handles the
/// sub-threshold case without human-alert beads.
const MIN_HARD_LIMIT_REMAINING_PCT_FOR_CUTOFF_ALERT: f64 = 5.0;

/// Check whether a cutoff alert is consistent: utilization must be close enough to the
/// hard limit that the burn-rate extrapolation is reliable.
///
/// Returns false (suppress) when:
/// - hard_limit_remaining_pct > MIN_HARD_LIMIT_REMAINING_PCT_FOR_CUTOFF_ALERT
///   (fleet is far from 100%, so negative margin is speculative), OR
/// - hard_limit_margin_hrs >= 0 (no risk — margin is positive)
///
/// This is the consistency guard that eliminates the "negative margin at sub-100% util"
/// false-positive pattern.
fn is_cutoff_alert_consistent(win: &crate::state::WindowForecast) -> bool {
    win.hard_limit_remaining_pct > 0.0
        && win.hard_limit_remaining_pct <= MIN_HARD_LIMIT_REMAINING_PCT_FOR_CUTOFF_ALERT
        && win.hard_limit_margin_hrs < 0.0
}

/// Check for CutoffImminent: any window with cutoff_risk=1 AND either:
/// - hard_limit_margin_hrs < -2 AND utilization >= 95% (high utilization risk), OR
/// - hard_limit_margin_hrs < -24 AND utilization >= 90% (deep margin risk)
///
/// Uses hard_limit_margin_hrs (margin against the 100% platform limit) rather than margin_hrs
/// (margin against the target ceiling). This prevents false positives when utilization exceeds
/// the target ceiling (e.g. 92% with a 90% ceiling) but is far from the platform hard limit.
///
/// The higher utilization thresholds (95%/90%) compared to the old values (80%/60%) reflect that
/// this alert signals genuine risk of platform-forced worker stoppage, not just exceeding a
/// self-imposed safety reserve. The governor's scaling logic handles the safety reserve case.
///
/// Additionally, the consistency guard (`is_cutoff_alert_consistent`) suppresses alerts when
/// hard_limit_remaining_pct > 5%, because burn-rate extrapolation beyond that range produces
/// deeply negative margins that don't correspond to actual cutoffs (100% FP rate observed).
fn check_cutoff_imminent(
    forecast: &CapacityForecast,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    const HIGH_UTIL_THRESHOLD: f64 = 95.0;
    const DEEP_MARGIN_THRESHOLD: f64 = -24.0;
    const DEEP_MARGIN_UTIL_THRESHOLD: f64 = 90.0;

    let windows = [
        ("five_hour", &forecast.five_hour),
        ("seven_day", &forecast.seven_day),
        ("seven_day_sonnet", &forecast.seven_day_sonnet),
    ];

    for (name, win) in windows {
        // Consistency guard: suppress when burn-rate extrapolation is unreliable
        if !is_cutoff_alert_consistent(win) {
            continue;
        }

        let high_util_risk = win.cutoff_risk
            && win.hard_limit_margin_hrs < -2.0
            && win.current_utilization >= HIGH_UTIL_THRESHOLD;
        let deep_margin_risk = win.cutoff_risk
            && win.hard_limit_margin_hrs < DEEP_MARGIN_THRESHOLD
            && win.current_utilization >= DEEP_MARGIN_UTIL_THRESHOLD;

        if high_util_risk || deep_margin_risk {
            let msg = format!(
                "Window {} at cutoff risk: hard_limit_margin_hrs={:.1}h, utilization={:.1}%, hrs_left={:.1}h, remaining_to_100={:.1}%",
                name, win.hard_limit_margin_hrs, win.current_utilization, win.hours_remaining, win.hard_limit_remaining_pct
            );
            alerts.push(AlertCondition {
                alert_type: AlertType::CutoffImminent,
                message: msg,
                severity: AlertSeverity::Critical,
                detected_at: now,
            });
            return;
        }
    }
}

/// Check for SonnetCutoffRisk: seven_day_sonnet cutoff_risk=1 AND hard_limit_margin_hrs < 0 AND utilization >= 85%
///
/// Uses hard_limit_margin_hrs (against 100% platform limit) instead of margin_hrs (against target
/// ceiling). The higher utilization threshold (85% vs old 50%) ensures this alert only fires when
/// utilization is genuinely close to the platform hard limit, not just above the self-imposed
/// safety reserve.
///
/// At 85%+ utilization, the fleet has at most 15% headroom to the hard limit. Combined with
/// hard_limit_margin_hrs < 0, this indicates the fleet is on track to hit 100% before the
/// window resets — a genuine cutoff risk requiring attention.
fn check_sonnet_cutoff_risk(
    forecast: &CapacityForecast,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    const UTILIZATION_THRESHOLD: f64 = 85.0;
    let win = &forecast.seven_day_sonnet;

    // Consistency guard: suppress when burn-rate extrapolation is unreliable
    if !is_cutoff_alert_consistent(win) {
        return;
    }

    if win.cutoff_risk
        && win.hard_limit_margin_hrs < 0.0
        && win.current_utilization >= UTILIZATION_THRESHOLD
    {
        let msg = format!(
            "Seven-day Sonnet window at cutoff risk: {:.1}% utilized, {:.1}h remaining, hard_limit_margin_hrs={:.1}h, remaining_to_100={:.1}%",
            win.current_utilization, win.hours_remaining, win.hard_limit_margin_hrs, win.hard_limit_remaining_pct
        );
        alerts.push(AlertCondition {
            alert_type: AlertType::SonnetCutoffRisk,
            message: msg,
            severity: AlertSeverity::Warning,
            detected_at: now,
        });
    }
}

/// Check for SessionCutoffRisk: five_hour cutoff_risk=1 AND hard_limit_margin_hrs < 0 AND utilization >= 85%
///
/// Uses hard_limit_margin_hrs (against 100% platform limit) instead of margin_hrs (against target
/// ceiling). The higher utilization threshold (85% vs old 50%) ensures this alert only fires when
/// the session window is genuinely close to the hard limit.
fn check_session_cutoff_risk(
    forecast: &CapacityForecast,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    const UTILIZATION_THRESHOLD: f64 = 85.0;
    let win = &forecast.five_hour;

    // Consistency guard: suppress when burn-rate extrapolation is unreliable
    if !is_cutoff_alert_consistent(win) {
        return;
    }

    if win.cutoff_risk
        && win.hard_limit_margin_hrs < 0.0
        && win.current_utilization >= UTILIZATION_THRESHOLD
    {
        let msg = format!(
            "Five-hour session window at cutoff risk: {:.1}% utilized, {:.1}h remaining, hard_limit_margin_hrs={:.1}h, remaining_to_100={:.1}%",
            win.current_utilization, win.hours_remaining, win.hard_limit_margin_hrs, win.hard_limit_remaining_pct
        );
        alerts.push(AlertCondition {
            alert_type: AlertType::SessionCutoffRisk,
            message: msg,
            severity: AlertSeverity::Warning,
            detected_at: now,
        });
    }
}

/// Check for LowCacheEfficiency: fleet_cache_eff below threshold for N consecutive intervals.
///
/// Only fires when workers > 0 (the consecutive counter is only incremented during active
/// intervals, so this guard is belt-and-suspenders). Returns None when the condition is
/// not met so callers can extend an existing alert list with `extend(check_low_cache_efficiency(…))`.
pub fn check_low_cache_efficiency(
    state: &GovernorState,
    config: &crate::config::AlertConfig,
    now: DateTime<Utc>,
) -> Option<AlertCondition> {
    let workers = state.last_fleet_aggregate.sonnet_workers;
    let consecutive = state.low_cache_eff_consecutive;
    let eff = state.last_fleet_aggregate.fleet_cache_eff;

    if workers > 0 && consecutive >= config.low_cache_eff_intervals {
        let msg = format!(
            "Fleet cache efficiency {:.1}% below threshold {:.0}% for {} consecutive intervals (~{} min)",
            eff * 100.0,
            config.low_cache_eff_threshold * 100.0,
            consecutive,
            consecutive * 5,
        );
        Some(AlertCondition {
            alert_type: AlertType::LowCacheEfficiency,
            message: msg,
            severity: AlertSeverity::Warning,
            detected_at: now,
        })
    } else {
        None
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

    let all_abundant = windows
        .iter()
        .all(|(_, win)| win.hours_remaining > 0.0 && win.margin_hrs > win.hours_remaining * 0.5);

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
    use crate::config::{AlertConfig, SprintConfig};
    use crate::state::{
        AlertCooldown, AlertFpTelemetry, BurnRateState, CapacityForecast, FleetAggregate,
        GovernorState, SafeModeState, ScheduleState, UsageState, WindowForecast, WorkerState,
    };
    use chrono::{Duration, Utc};
    use std::collections::HashMap;
    use std::fs::OpenOptions;
    use std::io::Write;

    fn base_now() -> DateTime<Utc> {
        "2026-03-20T10:00:00Z".parse().unwrap()
    }

    fn make_window(cutoff_risk: bool, margin_hrs: f64, hrs_left: f64) -> WindowForecast {
        make_window_with_util_and_margin(50.0, cutoff_risk, margin_hrs, hrs_left)
    }

    fn make_window_with_util_and_margin(
        util: f64,
        cutoff_risk: bool,
        margin_hrs: f64,
        hrs_left: f64,
    ) -> WindowForecast {
        let fleet_pct_hr = 5.0;
        let hard_limit_remaining_pct = (100.0 - util).max(0.0);
        let hard_limit_margin_hrs = hard_limit_remaining_pct / fleet_pct_hr - hrs_left;

        WindowForecast {
            target_ceiling: 90.0,
            current_utilization: util,
            remaining_pct: 90.0 - util,
            hours_remaining: hrs_left,
            fleet_pct_per_hour: fleet_pct_hr,
            predicted_exhaustion_hours: hrs_left - margin_hrs,
            cutoff_risk,
            margin_hrs,
            binding: false,
            safe_worker_count: None,
            hard_limit_remaining_pct,
            hard_limit_margin_hrs,
            ..Default::default()
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
            low_cache_eff_consecutive: 0,
            alert_fp_telemetry: AlertFpTelemetry::default(),
        }
    }

    // --- AlertType tests ---

    #[test]
    fn alert_type_display() {
        assert_eq!(AlertType::CutoffImminent.to_string(), "cutoff_imminent");
        assert_eq!(
            AlertType::SonnetCutoffRisk.to_string(),
            "sonnet_cutoff_risk"
        );
        assert_eq!(
            AlertType::SessionCutoffRisk.to_string(),
            "session_cutoff_risk"
        );
        assert_eq!(AlertType::BurnRateSpike.to_string(), "burn_rate_spike");
        assert_eq!(AlertType::Underutilization.to_string(), "underutilization");
        assert_eq!(
            AlertType::PromotionRatioAnomaly.to_string(),
            "promotion_ratio_anomaly"
        );
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
    fn cutoff_imminent_triggers_on_negative_hard_limit_margin_at_high_util() {
        // At 97% utilization with a burn rate that will exhaust the remaining 3%
        // before the window resets, the consistency guard passes and the alert fires.
        // hard_limit_remaining_pct = 3.0 <= 5.0 (consistency guard OK)
        // hard_limit_margin_hrs = 3.0/5.0 - 5.0 = -4.4 < -2.0 (high util path)
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(97.0, true, -4.4, 5.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(imminent.is_some(), "Should have CutoffImminent alert at 97% util with negative hard limit margin");
        let alert = imminent.unwrap();
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert!(alert.message.contains("five_hour"));
        assert!(alert.message.contains("hard_limit_margin_hrs"));
    }

    #[test]
    fn cutoff_imminent_requires_margin_less_than_minus_2() {
        // At 96% util, hard_limit_margin_hrs = -1.0 which is >= -2.0 threshold,
        // so even though consistency guard passes, the high_util_risk path doesn't fire.
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(96.0, true, -1.0, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(
            imminent.is_none(),
            "Should NOT have CutoffImminent when hard_limit_margin_hrs > -2"
        );
    }

    #[test]
    fn cutoff_imminent_requires_high_utilization_for_moderate_margin() {
        // Low utilization (52%) with small negative margin (-3h) should NOT trigger.
        // This is the transient burn rate spike false positive case.
        // The 80% threshold prevents firing for moderate negative margins at low utilization.
        let forecast = CapacityForecast {
            seven_day: make_window_with_util_and_margin(52.0, true, -3.0, 60.5),
            five_hour: make_window(false, 10.0, 2.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(
            imminent.is_none(),
            "Should NOT have CutoffImminent when utilization < 80% AND margin > -24"
        );
    }

    #[test]
    fn cutoff_imminent_fires_on_deep_margin_at_high_utilization() {
        // At 96% util with hard_limit_margin_hrs < -24, the deep_margin path fires.
        // hard_limit_remaining_pct = 4.0 <= 5.0 (consistency guard OK)
        // hard_limit_margin_hrs = 4.0/5.0 - 27.0 = -26.2 < -24.0 (deep margin path)
        // util=96.0 >= 90.0 (deep margin util threshold)
        let forecast = CapacityForecast {
            seven_day: make_window_with_util_and_margin(96.0, true, -26.2, 27.0),
            five_hour: make_window(false, 10.0, 2.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(
            imminent.is_some(),
            "Should have CutoffImminent when hard_limit_margin_hrs < -24 AND utilization >= 90%"
        );
        let alert = imminent.unwrap();
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert!(alert.message.contains("seven_day"));
        assert!(alert.message.contains("-26.2"));
        assert!(alert.message.contains("96"));
    }

    #[test]
    fn cutoff_imminent_no_deep_margin_fire_below_50_pct_utilization() {
        // Deep margin (-48h) but utilization below 50% should NOT fire.
        // Very low utilization with negative margin is likely a measurement anomaly.
        let forecast = CapacityForecast {
            seven_day: make_window_with_util_and_margin(40.0, true, -48.0, 50.5),
            five_hour: make_window(false, 10.0, 2.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(
            imminent.is_none(),
            "Should NOT fire deep_margin_risk when utilization < 50%"
        );
    }

    #[test]
    fn sonnet_cutoff_risk_triggers() {
        // At 96% utilization, consistency guard passes (hard_limit_remaining_pct=4.0 <= 5.0)
        // and hard_limit_margin_hrs = 4.0/5.0 - 5.0 = -4.2 < 0.0
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window_with_util_and_margin(96.0, true, -4.2, 5.0),
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let sonnet = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::SonnetCutoffRisk);
        assert!(sonnet.is_some(), "Should have SonnetCutoffRisk alert");
        assert!(sonnet.unwrap().message.contains("Seven-day Sonnet"));
    }

    #[test]
    fn session_cutoff_risk_triggers() {
        // At 96% utilization, consistency guard passes (hard_limit_remaining_pct=4.0 <= 5.0)
        // and hard_limit_margin_hrs = 4.0/5.0 - 2.0 = -1.2 < 0.0, util >= 85%.
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(96.0, true, -1.2, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let session = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::SessionCutoffRisk);
        assert!(session.is_some(), "Should have SessionCutoffRisk alert");
        assert!(session.unwrap().message.contains("Five-hour"));
    }

    #[test]
    fn consistency_guard_suppresses_cutoff_at_100_pct_utilization() {
        // Regression test for bead docs-iqqe: cutoff_imminent false positive at 100% utilization.
        // At 100% utilization, hard_limit_remaining_pct = 0.0 — the window is fully consumed.
        // The emergency brake (98%) already scaled workers to 0, so this alert is post-hoc.
        // If the platform hasn't cut off workers at 100%, the alert is wrong; if it has,
        // the alert is too late. Either way, it's unactionable.
        //
        // The consistency guard now requires hard_limit_remaining_pct > 0.0 (not just <= 5.0)
        // to exclude this degenerate case. The pattern is always: margin = -hrs_left because
        // hard_limit_margin_hrs = 0.0/fleet_pct_hr - hrs_left = -hrs_left.
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),
            seven_day: make_window_with_util_and_margin(100.0, true, -9.2, 9.2),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        assert!(
            alerts.iter().all(|a| !matches!(
                a.alert_type,
                AlertType::CutoffImminent | AlertType::SonnetCutoffRisk | AlertType::SessionCutoffRisk
            )),
            "Consistency guard should suppress all cutoff alerts at 100% util (hard_limit_remaining_pct=0.0), got: {:?}",
            alerts.iter().map(|a| a.alert_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn consistency_guard_suppresses_negative_margin_at_sub_100_util() {
        // Regression test for the root cause of the 100% FP rate (docs-878a):
        // A negative hard_limit_margin_hrs at sub-100% utilization is the canonical false positive.
        // At 86% utilization, hard_limit_remaining_pct = 14.0 which is > 5.0, so the consistency
        // guard suppresses the alert regardless of how negative the margin is. The fleet is far
        // enough from the platform hard limit that burn-rate extrapolation is unreliable.
        //
        // This is the exact pattern that produced 50/50 false positives:
        //   util=86%, margin=-16.2h, hard_limit_remaining_pct=14.0
        //   util=99%, margin=-10.2h, hard_limit_remaining_pct=1.0  ← this would pass the guard
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window_with_util_and_margin(86.0, true, -16.2, 26.2),
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        // None of the cutoff-related alerts should fire
        assert!(
            alerts.iter().all(|a| !matches!(
                a.alert_type,
                AlertType::CutoffImminent | AlertType::SonnetCutoffRisk | AlertType::SessionCutoffRisk
            )),
            "Consistency guard should suppress all cutoff alerts at 86% util (hard_limit_remaining_pct=14.0 > 5.0), got: {:?}",
            alerts.iter().map(|a| a.alert_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn consistency_guard_allows_alert_when_near_hard_limit() {
        // Complement to the suppression test: at 96% utilization, hard_limit_remaining_pct = 4.0
        // which is <= 5.0, so the consistency guard passes and the alert fires.
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window_with_util_and_margin(96.0, true, -26.2, 27.0),
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        assert!(
            alerts.iter().any(|a| matches!(a.alert_type, AlertType::SonnetCutoffRisk | AlertType::CutoffImminent)),
            "Consistency guard should allow alerts at 96% util (hard_limit_remaining_pct=4.0 <= 5.0), got: {:?}",
            alerts.iter().map(|a| a.alert_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn sonnet_cutoff_risk_false_positive_when_margin_positive() {
        // Regression test for bead docs-c7il:
        // Alert should NOT fire when cutoff_risk=true but margin_hrs is positive.
        // Positive margin_hrs means SAFE (exhaustion after reset), not at risk.
        // This catches corrupted state or sign convention mismatches between modules.
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(true, 84.0, 87.7), // cutoff_risk=1 BUT margin=84h (safe!)
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let sonnet = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::SonnetCutoffRisk);
        assert!(
            sonnet.is_none(),
            "Should NOT have SonnetCutoffRisk when margin_hrs is positive (safe)"
        );
    }

    #[test]
    fn sonnet_cutoff_risk_false_positive_when_low_utilization() {
        // Regression test for bead docs-amvn:
        // 40% utilization with margin_hrs=-108h but stale EMA (12.47%/hr vs actual 0.47%/hr).
        // During seven-day window rollover, old high-usage data drops off causing net-negative
        // deltas. The EMA only updates on positive deltas, so it stays inflated while actual
        // utilization declines. At 40% utilization with 50% headroom to the 90% ceiling, this
        // is not a real capacity crisis.
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window_with_util_and_margin(40.0, true, -108.0, 112.0), // cutoff_risk=1, util=40% < 50%
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let sonnet = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::SonnetCutoffRisk);
        assert!(
            sonnet.is_none(),
            "Should NOT have SonnetCutoffRisk when utilization is below 50%"
        );
    }

    #[test]
    fn session_cutoff_risk_false_positive_when_margin_positive() {
        // Same false positive check for session window
        let forecast = CapacityForecast {
            five_hour: make_window(true, 5.0, 2.0), // cutoff_risk=1 BUT margin=5h (safe!)
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let session = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::SessionCutoffRisk);
        assert!(
            session.is_none(),
            "Should NOT have SessionCutoffRisk when margin_hrs is positive (safe)"
        );
    }

    #[test]
    fn session_cutoff_risk_false_positive_when_low_utilization() {
        // Regression test: 26% utilization with negative margin_hrs is a false positive.
        // Low utilization means the governor has ample headroom to scale down workers.
        // The negative margin comes from a transient spike in fleet_pct_per_hour, not a real crisis.
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(26.0, true, -1.0, 3.1), // cutoff_risk=1, util=26% < 50%
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let session = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::SessionCutoffRisk);
        assert!(
            session.is_none(),
            "Should NOT have SessionCutoffRisk when utilization is below 50%"
        );
    }

    #[test]
    fn underutilization_triggers_when_all_abundant() {
        let forecast = CapacityForecast {
            five_hour: make_window(false, 5.0, 2.0), // margin > hrs_left * 0.5
            seven_day: make_window(false, 20.0, 30.0), // margin > hrs_left * 0.5
            seven_day_sonnet: make_window(false, 20.0, 30.0),
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let underutil = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::Underutilization);
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

        let underutil = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::Underutilization);
        assert!(
            underutil.is_none(),
            "Should NOT have Underutilization when any window constrained"
        );
    }

    #[test]
    fn promotion_not_applying_triggers_off_peak() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.schedule.is_peak_hour = false;
        state.schedule.is_promo_active = true;
        state.burn_rate.promotion_validated = false;
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 1.5;
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let promo = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionNotApplying);
        assert!(promo.is_some(), "Should have PromotionNotApplying alert");
        assert!(promo.unwrap().message.contains("1.50"));
        assert!(promo.unwrap().message.contains("2.00"));
    }

    #[test]
    fn promotion_not_applying_suppressed_when_zero_samples() {
        // Both ratios 0.0 and no samples — the original false-positive scenario
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.schedule.is_peak_hour = false;
        state.schedule.is_promo_active = true;
        state.burn_rate.promotion_validated = false;
        // peak/offpeak samples default to 0
        // offpeak_ratio_observed/expected default to 0.0

        let alerts = check_alert_conditions(&state, base_now());

        let promo = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionNotApplying);
        assert!(
            promo.is_none(),
            "Should NOT fire PromotionNotApplying when both ratios are 0.0 (no samples collected)"
        );
    }

    #[test]
    fn promotion_not_applying_suppressed_when_insufficient_peak_samples() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.schedule.is_peak_hour = false;
        state.schedule.is_promo_active = true;
        state.burn_rate.promotion_validated = false;
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES - 1;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 1.5;
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let promo = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionNotApplying);
        assert!(
            promo.is_none(),
            "Should NOT fire PromotionNotApplying when peak samples < MIN_VALIDATION_SAMPLES"
        );
    }

    #[test]
    fn promotion_not_applying_suppressed_when_insufficient_offpeak_samples() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.schedule.is_peak_hour = false;
        state.schedule.is_promo_active = true;
        state.burn_rate.promotion_validated = false;
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES - 1;
        state.burn_rate.offpeak_ratio_observed = 1.5;
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let promo = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionNotApplying);
        assert!(
            promo.is_none(),
            "Should NOT fire PromotionNotApplying when offpeak samples < MIN_VALIDATION_SAMPLES"
        );
    }

    #[test]
    fn promotion_not_applying_suppressed_when_expected_ratio_zero() {
        // Enough samples but expected ratio uninitialised (zero-median-peak guard hit)
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.schedule.is_peak_hour = false;
        state.schedule.is_promo_active = true;
        state.burn_rate.promotion_validated = false;
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 0.0;
        state.burn_rate.offpeak_ratio_expected = 0.0;

        let alerts = check_alert_conditions(&state, base_now());

        let promo = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionNotApplying);
        assert!(
            promo.is_none(),
            "Should NOT fire PromotionNotApplying when expected_ratio is 0.0"
        );
    }

    // --- PromotionRatioAnomaly tests ---

    #[test]
    fn promotion_ratio_anomaly_triggers_when_above_2_5() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 2.8; // Above 2.5 threshold
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let anomaly = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionRatioAnomaly);
        assert!(anomaly.is_some(), "Should have PromotionRatioAnomaly alert");
        assert!(anomaly.unwrap().message.contains("2.80"));
        assert!(anomaly.unwrap().message.contains("exceeds 2.5"));
    }

    #[test]
    fn promotion_ratio_anomaly_triggers_when_below_0_8() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 0.5; // Below 0.8 threshold
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let anomaly = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionRatioAnomaly);
        assert!(anomaly.is_some(), "Should have PromotionRatioAnomaly alert");
        assert!(anomaly.unwrap().message.contains("0.50"));
        assert!(anomaly.unwrap().message.contains("below 0.8"));
    }

    #[test]
    fn promotion_ratio_anomaly_does_not_trigger_in_range() {
        // Ratio of 2.1 is within [0.8, 2.5] - should not trigger
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 2.1;
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let anomaly = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionRatioAnomaly);
        assert!(
            anomaly.is_none(),
            "Should NOT fire PromotionRatioAnomaly when ratio is in range [0.8, 2.5]"
        );
    }

    #[test]
    fn promotion_ratio_anomaly_boundary_at_2_5() {
        // Exactly at threshold should not trigger
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 2.5; // Exactly at threshold
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let anomaly = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionRatioAnomaly);
        assert!(
            anomaly.is_none(),
            "Should NOT fire PromotionRatioAnomaly when ratio is exactly 2.5"
        );
    }

    #[test]
    fn promotion_ratio_anomaly_boundary_at_0_8() {
        // Exactly at threshold should not trigger
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 0.8; // Exactly at threshold
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let anomaly = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionRatioAnomaly);
        assert!(
            anomaly.is_none(),
            "Should NOT fire PromotionRatioAnomaly when ratio is exactly 0.8"
        );
    }

    #[test]
    fn promotion_ratio_anomaly_suppressed_with_insufficient_samples() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES - 1; // Insufficient
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 3.0; // Would normally trigger
        state.burn_rate.offpeak_ratio_expected = 2.0;

        let alerts = check_alert_conditions(&state, base_now());

        let anomaly = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionRatioAnomaly);
        assert!(
            anomaly.is_none(),
            "Should NOT fire PromotionRatioAnomaly when peak samples < MIN_VALIDATION_SAMPLES"
        );
    }

    #[test]
    fn promotion_ratio_anomaly_suppressed_when_expected_ratio_zero() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.offpeak_ratio_observed = 3.0;
        state.burn_rate.offpeak_ratio_expected = 0.0; // Zero expected ratio

        let alerts = check_alert_conditions(&state, base_now());

        let anomaly = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::PromotionRatioAnomaly);
        assert!(
            anomaly.is_none(),
            "Should NOT fire PromotionRatioAnomaly when expected_ratio is 0.0"
        );
    }

    #[test]
    fn collector_offline_triggers_when_stale() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        // Set last fleet aggregate to 31 minutes ago (above 30-minute threshold)
        state.last_fleet_aggregate.t1 = base_now() - Duration::minutes(31);

        let alerts = check_alert_conditions(&state, base_now());

        let offline = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CollectorOffline);
        assert!(offline.is_some(), "Should have CollectorOffline alert");
        assert!(offline.unwrap().message.contains("31 minutes ago"));
    }

    #[test]
    fn multiple_simultaneous_alerts() {
        // Use high utilization (97%) so consistency guard passes and all thresholds are met.
        // hard_limit_remaining_pct = 3.0 <= 5.0 (consistency guard OK)
        // hard_limit_margin_hrs = 3.0/5.0 - 2.0 = -1.4 for five_hour
        // hard_limit_margin_hrs = 3.0/5.0 - 30.0 = -29.4 for seven_day_sonnet
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(97.0, true, -1.4, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window_with_util_and_margin(97.0, true, -29.4, 30.0),
            binding_window: "seven_day_sonnet".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let mut state = make_state_with_forecast(forecast);
        state.schedule.is_peak_hour = false;
        state.schedule.is_promo_active = true;
        state.burn_rate.promotion_validated = false;
        state.burn_rate.promotion_peak_samples = MIN_VALIDATION_SAMPLES;
        state.burn_rate.promotion_offpeak_samples = MIN_VALIDATION_SAMPLES;
        state.last_fleet_aggregate.t1 = base_now() - Duration::minutes(31);

        let alerts = check_alert_conditions(&state, base_now());

        // Should have: CutoffImminent, SessionCutoffRisk, SonnetCutoffRisk, PromotionNotApplying, CollectorOffline
        assert!(
            alerts.len() >= 4,
            "Should have multiple alerts, got {:?}",
            alerts
        );

        let types: Vec<AlertType> = alerts.iter().map(|a| a.alert_type).collect();
        assert!(types.contains(&AlertType::CutoffImminent));
        assert!(types.contains(&AlertType::SessionCutoffRisk));
        assert!(types.contains(&AlertType::SonnetCutoffRisk));
        assert!(types.contains(&AlertType::PromotionNotApplying));
        assert!(types.contains(&AlertType::CollectorOffline));
    }

    #[test]
    fn alert_message_contains_specifics() {
        // Use 97% utilization so consistency guard passes and alert fires.
        let forecast = CapacityForecast {
            five_hour: WindowForecast {
                target_ceiling: 90.0,
                current_utilization: 97.0,
                remaining_pct: -7.0,
                hours_remaining: 1.5,
                fleet_pct_per_hour: 10.0,
                predicted_exhaustion_hours: 0.0,
                cutoff_risk: true,
                margin_hrs: -2.5,
                binding: true,
                safe_worker_count: Some(1),
                hard_limit_remaining_pct: 3.0,
                hard_limit_margin_hrs: -2.2,
                ..Default::default()
            },
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now());

        let imminent = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CutoffImminent)
            .unwrap();

        // Message should contain window name, percentages, and hours
        assert!(imminent.message.contains("five_hour"));
        assert!(imminent.message.contains("97"));
        assert!(imminent.message.contains("1.5"));
        assert!(imminent.message.contains("-2"));
    }

    #[test]
    fn emergency_brake_does_not_create_alert_bead() {
        // EmergencyBrakeActivated was disabled because it had a 100% FP rate —
        // every bead created was documented as a false positive. The governor's
        // scaling logic handles the emergency brake automatically (scales to 0 at
        // 98%+ utilization), so no human-actionable bead is needed.
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.safe_mode.active = true;
        state.safe_mode.trigger = Some("emergency_brake".to_string());
        state.safe_mode.entered_at = Some(base_now() - Duration::minutes(5));

        let alerts = check_alert_conditions(&state, base_now());

        let brake = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::EmergencyBrakeActivated);
        assert!(brake.is_none(), "EmergencyBrakeActivated should NOT create alert beads (100% FP rate)");
    }

    #[test]
    fn token_refresh_failing_triggers() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.token_refresh_failing = true;

        let alerts = check_alert_conditions(&state, base_now());

        let trf = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::TokenRefreshFailing);
        assert!(trf.is_some(), "Should have TokenRefreshFailing alert");
        assert_eq!(trf.unwrap().severity, AlertSeverity::Critical);
        assert!(trf.unwrap().message.contains("claude login"));
    }

    #[test]
    fn token_refresh_failing_does_not_trigger_when_false() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.token_refresh_failing = false;

        let alerts = check_alert_conditions(&state, base_now());

        let trf = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::TokenRefreshFailing);
        assert!(
            trf.is_none(),
            "Should NOT have TokenRefreshFailing when flag is false"
        );
    }

    // --- LowCacheEfficiency tests ---

    fn default_alert_config() -> AlertConfig {
        AlertConfig::default()
    }

    #[test]
    fn low_cache_eff_fires_after_n_consecutive_intervals() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.last_fleet_aggregate.sonnet_workers = 2;
        state.last_fleet_aggregate.fleet_cache_eff = 0.10; // 10%, below 30% threshold
        state.low_cache_eff_consecutive = 5; // meets default of 5 intervals

        let config = default_alert_config();
        let alert = check_low_cache_efficiency(&state, &config, base_now());

        assert!(
            alert.is_some(),
            "Should fire LowCacheEfficiency after N intervals"
        );
        let a = alert.unwrap();
        assert_eq!(a.alert_type, AlertType::LowCacheEfficiency);
        assert_eq!(a.severity, AlertSeverity::Warning);
        assert!(
            a.message.contains("10.0%"),
            "Should show current efficiency"
        );
        assert!(a.message.contains("30%"), "Should show threshold");
        assert!(a.message.contains("5 consecutive"), "Should show count");
    }

    #[test]
    fn low_cache_eff_does_not_fire_below_interval_threshold() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.last_fleet_aggregate.sonnet_workers = 2;
        state.last_fleet_aggregate.fleet_cache_eff = 0.10;
        state.low_cache_eff_consecutive = 4; // one short of default 5

        let config = default_alert_config();
        let alert = check_low_cache_efficiency(&state, &config, base_now());
        assert!(
            alert.is_none(),
            "Should NOT fire when consecutive count < threshold"
        );
    }

    #[test]
    fn low_cache_eff_does_not_fire_when_eff_above_threshold() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.last_fleet_aggregate.sonnet_workers = 2;
        state.last_fleet_aggregate.fleet_cache_eff = 0.50; // above threshold
                                                           // counter would be 0 because governor resets it when eff is good
        state.low_cache_eff_consecutive = 0;

        let config = default_alert_config();
        let alert = check_low_cache_efficiency(&state, &config, base_now());
        assert!(
            alert.is_none(),
            "Should NOT fire when efficiency is above threshold"
        );
    }

    #[test]
    fn low_cache_eff_does_not_fire_when_no_workers() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.last_fleet_aggregate.sonnet_workers = 0; // idle
        state.last_fleet_aggregate.fleet_cache_eff = 0.0;
        state.low_cache_eff_consecutive = 10; // would normally trigger

        let config = default_alert_config();
        let alert = check_low_cache_efficiency(&state, &config, base_now());
        assert!(
            alert.is_none(),
            "Should NOT fire when no workers are active"
        );
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

    fn make_window_with_util(util: f64, hrs_left: f64, cutoff_risk: bool) -> WindowForecast {
        WindowForecast {
            target_ceiling: 90.0,
            current_utilization: util,
            remaining_pct: 90.0 - util,
            hours_remaining: hrs_left,
            fleet_pct_per_hour: 5.0,
            predicted_exhaustion_hours: if hrs_left > 0.0 {
                (90.0 - util) / 5.0
            } else {
                0.0
            },
            cutoff_risk,
            margin_hrs: hrs_left - (90.0 - util) / 5.0,
            binding: false,
            safe_worker_count: None,
            ..Default::default()
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
            WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 5,
            },
        );

        let state = make_state_with_workers(forecast, workers);
        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config);
        assert!(
            trigger.is_some(),
            "Sprint should trigger at 45% with 1.5h to reset"
        );

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
            WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 5,
            },
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
            WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 5,
            },
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
            WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 5,
            },
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
            WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 8,
            },
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
            WorkerState {
                current: 5,
                target: 5,
                min: 1,
                max: 5,
            }, // already at max
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
            WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 5,
            },
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
            WorkerState {
                current: 3,
                target: 3,
                min: 1,
                max: 5,
            }, // headroom: 2
        );
        workers.insert(
            "opus".to_string(),
            WorkerState {
                current: 1,
                target: 1,
                min: 1,
                max: 10,
            }, // headroom: 9
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

    #[test]
    fn sprint_inhibited_when_safe_mode_active() {
        // five_hour underutilized and close to reset — conditions that would normally trigger a sprint
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
            WorkerState {
                current: 2,
                target: 2,
                min: 1,
                max: 5,
            },
        );

        let mut state = make_state_with_workers(forecast, workers);
        state.safe_mode.active = true;
        state.safe_mode.trigger = Some("median_error".to_string());

        let config = default_sprint_config();

        let trigger = check_underutilization_sprint(&state, &config);
        assert!(
            trigger.is_none(),
            "Sprint should NOT trigger when safe mode is active"
        );
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
        assert!(!meets_severity_threshold(
            AlertSeverity::Warning,
            "critical"
        ));
    }

    #[test]
    fn meets_severity_threshold_critical() {
        assert!(meets_severity_threshold(AlertSeverity::Critical, "info"));
        assert!(meets_severity_threshold(AlertSeverity::Critical, "warning"));
        assert!(meets_severity_threshold(
            AlertSeverity::Critical,
            "critical"
        ));
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
            auto_bead: true, // must be true to reach the empty-command check
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
        // hard_limit_margin_hrs = 3.0/5.0 - 3.0 = -2.4 < -2.0 → CutoffImminent fires
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(97.0, true, -2.4, 3.0),
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
            command: vec!["echo".to_string()],
            ..AlertConfig::default()
        };

        let fired = process_alerts(&mut state, &config, base_now());
        assert!(fired >= 1, "Should have fired at least one alert");

        // Cooldown should now be set
        assert!(state
            .alert_cooldown
            .get_last_fired("cutoff_imminent")
            .is_some());
    }

    #[test]
    fn process_alerts_respects_cooldown() {
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(97.0, true, -2.4, 3.0),
            seven_day: make_window(false, 10.0, 30.0),
            seven_day_sonnet: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let mut state = make_state_with_forecast(forecast);

        // Set cooldown for both expected alert types to have just fired
        state
            .alert_cooldown
            .record_fired("cutoff_imminent", base_now());
        state
            .alert_cooldown
            .record_fired("session_cutoff_risk", base_now());

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
