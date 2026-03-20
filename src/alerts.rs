//! Alert Condition Checker and Cooldown Deduplication
//!
//! This module handles:
//! - Alert condition evaluation from governor state
//! - Per-type cooldown deduplication to prevent alert spam
//! - Alert severity classification

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    use crate::state::{
        BurnRateState, CapacityForecast, FleetAggregate, GovernorState, ScheduleState,
        SafeModeState, UsageState, WindowForecast,
    };
    use chrono::{Duration, Utc};

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
                promo_multiplier: 1.0,
                effective_hours_remaining: 0.0,
                raw_hours_remaining: 0.0,
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

    // --- Update cooldown test ---

    #[test]
    fn update_cooldown_records_timestamp() {
        let mut cooldown = AlertCooldown::new();
        let now = base_now();

        update_cooldown(&mut cooldown, AlertType::CutoffImminent, now);

        let recorded = cooldown.get_last_fired(&AlertType::CutoffImminent.to_string());
        assert_eq!(recorded, Some(now));
    }
}
