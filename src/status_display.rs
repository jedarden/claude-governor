//! Rich Status Display for cgov status Command
//!
//! Provides a human-readable dashboard for `cgov status` with:
//! - Per-window table (Used/Ceiling/Remain/Resets/Risk)
//! - Worker count and target
//! - Burn rate
//! - Peak/promo status
//! - Last-cycle timing
//!
//! Exit codes:
//! - 0 = all windows safe
//! - 2 = cutoff_risk active
//! - 3 = emergency brake engaged

use crate::capacity_summary::{compute_pressure_level, PressureLevel, StatusExitCode};
use crate::state::{GovernorState, WindowForecast};
#[cfg(test)]
use crate::state::CapacityForecast;
use chrono::DateTime;
use chrono::Utc;
use std::collections::HashMap;

/// Format a duration in hours to a human-readable string
fn format_hours(hours: f64) -> String {
    if hours <= 0.0 {
        "now".to_string()
    } else if hours < 1.0 {
        format!("{:.0}m", hours * 60.0)
    } else if hours < 24.0 {
        format!("{:.1}h", hours)
    } else {
        let days = hours / 24.0;
        format!("{:.1}d", days)
    }
}

/// Format exhaustion hours — handles infinity / very large values
fn format_exh_hrs(hrs: f64) -> String {
    if hrs.is_infinite() || hrs > 9999.0 {
        ">9999h".to_string()
    } else {
        format_hours(hrs)
    }
}

/// Format a timestamp as a relative time string
fn format_relative_time(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let diff = now.signed_duration_since(ts);
    let secs = diff.num_seconds();

    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Risk indicator for a window
fn risk_indicator(window: &WindowForecast) -> &'static str {
    if window.cutoff_risk {
        "CUTOFF"
    } else if window.margin_hrs < 1.0 {
        "LOW"
    } else if window.margin_hrs < 6.0 {
        "WARN"
    } else {
        "ok"
    }
}

/// Generate the rich human-readable status dashboard
pub fn format_status_dashboard(state: &GovernorState, now: DateTime<Utc>) -> String {
    let mut output = String::new();

    // Header with pressure level
    let pressure = compute_pressure_level(&state.capacity_forecast);
    let pressure_emoji = match pressure {
        PressureLevel::Low => "🟢",
        PressureLevel::Medium => "🟡",
        PressureLevel::High => "🔴",
    };

    output.push_str(&format!(
        "Claude Governor Status {}\n",
        pressure_emoji
    ));
    output.push_str(&format!(
        "Pressure: {} ({})\n",
        pressure,
        match pressure {
            PressureLevel::Low => "ample headroom",
            PressureLevel::Medium => "moderate headroom",
            PressureLevel::High => "capacity constrained",
        }
    ));
    output.push_str("\n");

    // Per-window capacity table
    output.push_str("Window Capacity\n");
    output.push_str("---------------\n");

    let windows: [(&str, &WindowForecast); 3] = [
        ("5h", &state.capacity_forecast.five_hour),
        ("7d", &state.capacity_forecast.seven_day),
        ("7d-sonnet", &state.capacity_forecast.seven_day_sonnet),
    ];

    // Table header
    output.push_str(&format!(
        "{:<10} {:>6} {:>8} {:>8} {:>8} {:>8}\n",
        "Window", "Used%", "Ceiling%", "Remain%", "Resets", "Risk"
    ));

    for (name, win) in &windows {
        let binding_marker = if win.binding { " *" } else { "" };
        let risk = risk_indicator(win);
        let resets = format_hours(win.hours_remaining);

        output.push_str(&format!(
            "{:<10} {:>5.1}% {:>7.0}% {:>7.1}% {:>8} {:>8}{}\n",
            name,
            win.current_utilization,
            win.target_ceiling,
            win.remaining_pct,
            resets,
            risk,
            binding_marker
        ));
    }

    // Legend for binding marker
    output.push_str(" * = binding window\n");
    output.push_str("\n");

    // Confidence Cone section
    output.push_str("Confidence Cone\n");
    output.push_str("---------------\n");
    output.push_str(&format!(
        "{:<10} {:>10} {:>10} {:>10} {:>10}\n",
        "Window", "p25(fast)", "p50(mid)", "p75(slow)", "ConeRatio"
    ));
    for (name, win) in &windows {
        let p25 = format_exh_hrs(win.exh_hrs_p25);
        let p50 = format_exh_hrs(win.exh_hrs_p50);
        let p75 = format_exh_hrs(win.exh_hrs_p75);
        let ratio = if win.cone_ratio > 0.0 {
            format!("{:.1}x", win.cone_ratio)
        } else {
            "—".to_string()
        };
        output.push_str(&format!(
            "{:<10} {:>10} {:>10} {:>10} {:>10}\n",
            name, p25, p50, p75, ratio
        ));
    }
    output.push_str("\n");

    // Workers section
    output.push_str("Workers\n");
    output.push_str("-------\n");

    if state.workers.is_empty() {
        output.push_str("No workers configured\n");
    } else {
        let fleet = &state.last_fleet_aggregate;
        let total_current: u32 = state.workers.values().map(|w| w.current).sum();
        let total_target: u32 = state.workers.values().map(|w| w.target).sum();

        output.push_str(&format!(
            "Fleet: {} current / {} target\n",
            total_current, total_target
        ));

        // Per-agent worker details
        for (agent_id, worker) in &state.workers {
            output.push_str(&format!(
                "  {}: {} current / {} target (range: {}-{})\n",
                agent_id, worker.current, worker.target, worker.min, worker.max
            ));
        }

        // Fleet aggregate info
        if fleet.sonnet_workers > 0 {
            output.push_str(&format!(
                "  Rate: ${:.2}/hr total, ${:.2}/hr p75\n",
                fleet.sonnet_usd_total, fleet.sonnet_p75_usd_hr
            ));
        }
    }

    output.push_str("\n");

    // Burn rate section
    output.push_str("Burn Rate\n");
    output.push_str("---------\n");

    let burn = &state.burn_rate;
    if burn.by_model.is_empty() {
        output.push_str("No burn rate data yet (collecting...)\n");
    } else {
        // Aggregate across models
        let total_pct: f64 = burn.by_model.values().map(|m| m.pct_per_worker_per_hour).sum();
        let total_usd: f64 = burn.by_model.values().map(|m| m.dollars_per_worker_per_hour).sum();
        let samples: u32 = burn.by_model.values().map(|m| m.samples).max().unwrap_or(0);

        output.push_str(&format!(
            "Fleet: {:.2}%/hr, ${:.2}/hr per worker\n",
            total_pct, total_usd
        ));

        if samples > 0 {
            output.push_str(&format!("  Samples: {} EMA cycles\n", samples));
        }

        // Last sample time
        if let Some(last_sample) = burn.last_sample_at {
            output.push_str(&format!(
                "  Last sample: {}\n",
                format_relative_time(last_sample, now)
            ));
        }
    }

    output.push_str("\n");

    // Schedule / Promotion section
    output.push_str("Schedule\n");
    output.push_str("--------\n");

    let schedule = &state.schedule;
    let peak_status = if schedule.is_peak_hour {
        "PEAK"
    } else {
        "off-peak"
    };
    output.push_str(&format!("Status: {}\n", peak_status));

    if schedule.is_promo_active && schedule.promo_multiplier > 1.0 {
        let validated = if state.burn_rate.promotion_validated {
            "validated"
        } else {
            "unvalidated"
        };
        output.push_str(&format!(
            "Promo: {:.1}x multiplier ({})\n",
            schedule.promo_multiplier, validated
        ));
        output.push_str(&format!(
            "Effective time: {} remaining\n",
            format_hours(schedule.effective_hours_remaining)
        ));
    }

    output.push_str("\n");

    // Last cycle timing
    output.push_str("Last Cycle\n");
    output.push_str("----------\n");

    output.push_str(&format!(
        "State updated: {}\n",
        format_relative_time(state.updated_at, now)
    ));

    let fleet = &state.last_fleet_aggregate;
    if fleet.t1 > state.updated_at {
        // Fleet aggregate is newer than state
        output.push_str(&format!(
            "Fleet aggregate: {}\n",
            format_relative_time(fleet.t1, now)
        ));
    }

    // Safe mode warning
    if state.safe_mode.active {
        output.push_str("\n");
        output.push_str("WARNING  SAFE MODE ACTIVE\n");
        output.push_str(&format!(
            "  Trigger: {}\n",
            state.safe_mode.trigger.as_deref().unwrap_or("unknown")
        ));
        if let Some(entered) = state.safe_mode.entered_at {
            output.push_str(&format!(
                "  Since: {}\n",
                format_relative_time(entered, now)
            ));
        }
        if let Some(err) = state.safe_mode.median_error_at_entry {
            output.push_str(&format!(
                "  Error at entry: {:.1} pct-pts (exit threshold: 8.0)\n",
                err
            ));
        }
        if state.safe_mode.predictions_since_entry > 0 {
            output.push_str(&format!(
                "  Predictions since entry: {}\n",
                state.safe_mode.predictions_since_entry
            ));
        }
        output.push_str(&format!(
            "  Effect: ceiling -{:.0}%, hysteresis x2, sprint+cross-window disabled\n",
            5.0_f64
        ));
    }

    // Emergency brake / cutoff risk summary
    let exit_code = StatusExitCode::from_state(state);
    if exit_code != StatusExitCode::Safe {
        output.push_str("\n");
        match exit_code {
            StatusExitCode::CutoffRisk => {
                output.push_str("⚠️  CUTOFF RISK: One or more windows may exceed ceiling before reset\n");
            }
            StatusExitCode::Emergency => {
                output.push_str("🚨 EMERGENCY: Governor is in safe mode\n");
            }
            _ => {}
        }
    }

    output
}

/// Format status as JSON for machine consumption
pub fn format_status_json(state: &GovernorState) -> serde_json::Value {
    let pressure = compute_pressure_level(&state.capacity_forecast);
    let exit_code = StatusExitCode::from_state(state);

    let windows: HashMap<&str, &WindowForecast> = [
        ("five_hour", &state.capacity_forecast.five_hour),
        ("seven_day", &state.capacity_forecast.seven_day),
        ("seven_day_sonnet", &state.capacity_forecast.seven_day_sonnet),
    ]
    .into_iter()
    .collect();

    let total_current: u32 = state.workers.values().map(|w| w.current).sum();
    let total_target: u32 = state.workers.values().map(|w| w.target).sum();

    serde_json::json!({
        "pressure": pressure.to_string().to_lowercase(),
        "exit_code": exit_code.as_exit_code(),
        "binding_window": state.capacity_forecast.binding_window,
        "windows": windows.iter().map(|(k, v)| {
            (*k, serde_json::json!({
                "used_pct": v.current_utilization,
                "ceiling_pct": v.target_ceiling,
                "remain_pct": v.remaining_pct,
                "resets_in_hrs": v.hours_remaining,
                "risk": risk_indicator(v),
                "binding": v.binding,
                "cutoff_risk": v.cutoff_risk,
                "exh_hrs_p25": if v.exh_hrs_p25.is_infinite() { serde_json::Value::Null } else { serde_json::json!(v.exh_hrs_p25) },
                "exh_hrs_p50": if v.exh_hrs_p50.is_infinite() { serde_json::Value::Null } else { serde_json::json!(v.exh_hrs_p50) },
                "exh_hrs_p75": if v.exh_hrs_p75.is_infinite() { serde_json::Value::Null } else { serde_json::json!(v.exh_hrs_p75) },
                "cone_ratio": v.cone_ratio,
            }))
        }).collect::<std::collections::HashMap<&str, serde_json::Value>>(),
        "workers": {
            "current": total_current,
            "target": total_target,
            "by_agent": state.workers,
        },
        "burn_rate": {
            "pct_per_worker_per_hour": state.burn_rate.by_model.values()
                .map(|m| m.pct_per_worker_per_hour)
                .sum::<f64>(),
            "dollars_per_worker_per_hour": state.burn_rate.by_model.values()
                .map(|m| m.dollars_per_worker_per_hour)
                .sum::<f64>(),
            "samples": state.burn_rate.by_model.values()
                .map(|m| m.samples)
                .max()
                .unwrap_or(0),
            "promotion_validated": state.burn_rate.promotion_validated,
        },
        "schedule": {
            "is_peak_hour": state.schedule.is_peak_hour,
            "is_promo_active": state.schedule.is_promo_active,
            "promo_multiplier": state.schedule.promo_multiplier,
        },
        "safe_mode": {
            "active": state.safe_mode.active,
            "trigger": state.safe_mode.trigger,
        },
        "updated_at": state.updated_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        BurnRateState, CalibrationState, FleetAggregate, ModelBurnRate, SafeModeState,
        ScheduleState, UsageState, WindowForecast, WorkerState,
    };
    use chrono::TimeZone;

    fn make_test_state() -> GovernorState {
        let mut workers = HashMap::new();
        workers.insert(
            "claude-anthropic-sonnet".to_string(),
            WorkerState {
                current: 2,
                target: 3,
                min: 1,
                max: 5,
            },
        );

        let mut by_model = HashMap::new();
        by_model.insert(
            "claude-sonnet-4-6".to_string(),
            ModelBurnRate {
                pct_per_worker_per_hour: 1.35,
                dollars_per_worker_per_hour: 5.54,
                samples: 12,
            },
        );

        GovernorState {
            updated_at: Utc::now() - chrono::Duration::minutes(5),
            usage: UsageState {
                sonnet_pct: 63.5,
                all_models_pct: 72.6,
                five_hour_pct: 36.4,
                sonnet_resets_at: "2026-03-20T04:00:00Z".to_string(),
                five_hour_resets_at: "2026-03-18T16:00:00Z".to_string(),
                stale: false,
            },
            last_fleet_aggregate: FleetAggregate {
                t0: Utc::now() - chrono::Duration::minutes(10),
                t1: Utc::now() - chrono::Duration::minutes(5),
                sonnet_workers: 2,
                sonnet_usd_total: 11.08,
                sonnet_p75_usd_hr: 6.50,
                sonnet_std_usd_hr: 1.20,
                window_pct_deltas: Default::default(),
                fleet_cache_eff: 0.0,
                cache_eff_p25: 0.0,
            },
            capacity_forecast: CapacityForecast {
                five_hour: WindowForecast {
                    target_ceiling: 85.0,
                    current_utilization: 36.4,
                    remaining_pct: 48.6,
                    hours_remaining: 1.5,
                    fleet_pct_per_hour: 7.92,
                    predicted_exhaustion_hours: 6.14,
                    cutoff_risk: false,
                    margin_hrs: 4.64,
                    binding: false,
                    safe_worker_count: None,
                    ..Default::default()
                },
                seven_day: WindowForecast {
                    target_ceiling: 90.0,
                    current_utilization: 72.6,
                    remaining_pct: 17.4,
                    hours_remaining: 37.5,
                    fleet_pct_per_hour: 6.48,
                    predicted_exhaustion_hours: 2.69,
                    cutoff_risk: true,
                    margin_hrs: -34.81,
                    binding: false,
                    safe_worker_count: None,
                    ..Default::default()
                },
                seven_day_sonnet: WindowForecast {
                    target_ceiling: 90.0,
                    current_utilization: 63.5,
                    remaining_pct: 26.5,
                    hours_remaining: 37.5,
                    fleet_pct_per_hour: 9.0,
                    predicted_exhaustion_hours: 2.94,
                    cutoff_risk: true,
                    margin_hrs: -34.56,
                    binding: true,
                    safe_worker_count: Some(2),
                    ..Default::default()
                },
                binding_window: "seven_day_sonnet".to_string(),
                dollars_per_pct_7d_s: 1.648,
                estimated_remaining_dollars: 46.1,
            },
            schedule: ScheduleState {
                is_peak_hour: false,
                is_promo_active: true,
                promo_multiplier: 2.0,
                promo_multiplier_five_hour: 2.0,
                effective_hours_remaining: 75.0,
                effective_hours_remaining_five_hour: 75.0,
                raw_hours_remaining: 37.5,
                ..Default::default()
            },
            workers,
            burn_rate: BurnRateState {
                by_model,
                tokens_per_pct_peak: 69780,
                tokens_per_pct_offpeak: 141350,
                offpeak_ratio_observed: 2.03,
                offpeak_ratio_expected: 2.0,
                promotion_validated: true,
                promotion_peak_samples: 0,
                promotion_offpeak_samples: 0,
                last_sample_at: Some(Utc::now() - chrono::Duration::minutes(15)),
                calibration: CalibrationState::default(),
                ..Default::default()
            },
            alerts: vec![],
            safe_mode: SafeModeState::default(),
            alert_cooldown: Default::default(),
            token_refresh_failing: false,
            low_cache_eff_consecutive: 0,
        }
    }

    #[test]
    fn format_dashboard_includes_all_sections() {
        let state = make_test_state();
        let output = format_status_dashboard(&state, Utc::now());

        // Check sections
        assert!(output.contains("Window Capacity"), "missing window section");
        assert!(output.contains("Workers"), "missing workers section");
        assert!(output.contains("Burn Rate"), "missing burn rate section");
        assert!(output.contains("Schedule"), "missing schedule section");
        assert!(output.contains("Last Cycle"), "missing last cycle section");
    }

    #[test]
    fn format_dashboard_shows_window_table() {
        let state = make_test_state();
        let output = format_status_dashboard(&state, Utc::now());

        // Window names
        assert!(output.contains("5h"));
        assert!(output.contains("7d"));
        assert!(output.contains("7d-sonnet"));

        // Column headers
        assert!(output.contains("Used%"));
        assert!(output.contains("Ceiling%"));
        assert!(output.contains("Remain%"));
        assert!(output.contains("Resets"));
        assert!(output.contains("Risk"));

        // Risk indicators
        assert!(output.contains("CUTOFF"));
    }

    #[test]
    fn format_dashboard_shows_binding_window() {
        let state = make_test_state();
        let output = format_status_dashboard(&state, Utc::now());

        assert!(output.contains("* = binding window"));
        // 7d-sonnet should have the binding marker
        assert!(output.contains("7d-sonnet"));
    }

    #[test]
    fn format_dashboard_shows_workers() {
        let state = make_test_state();
        let output = format_status_dashboard(&state, Utc::now());

        assert!(output.contains("2 current / 3 target"));
        assert!(output.contains("claude-anthropic-sonnet"));
    }

    #[test]
    fn format_dashboard_shows_burn_rate() {
        let state = make_test_state();
        let output = format_status_dashboard(&state, Utc::now());

        assert!(output.contains("%/hr"));
        assert!(output.contains("/hr per worker"));
        assert!(output.contains("Samples"));
    }

    #[test]
    fn format_dashboard_shows_promo_status() {
        let state = make_test_state();
        let output = format_status_dashboard(&state, Utc::now());

        assert!(output.contains("Promo:"));
        assert!(output.contains("2.0x multiplier"));
        assert!(output.contains("validated"));
    }

    #[test]
    fn format_dashboard_shows_cutoff_risk_warning() {
        let state = make_test_state();
        let output = format_status_dashboard(&state, Utc::now());

        assert!(output.contains("CUTOFF RISK"));
    }

    #[test]
    fn format_dashboard_shows_safe_mode() {
        let mut state = make_test_state();
        state.safe_mode.active = true;
        state.safe_mode.trigger = Some("median_error".to_string());
        state.safe_mode.entered_at = Some(Utc::now() - chrono::Duration::hours(1));

        let output = format_status_dashboard(&state, Utc::now());

        assert!(output.contains("SAFE MODE ACTIVE"));
        assert!(output.contains("median_error"));
    }

    #[test]
    fn format_json_includes_all_fields() {
        let state = make_test_state();
        let json = format_status_json(&state);

        assert!(json.get("pressure").is_some());
        assert!(json.get("exit_code").is_some());
        assert!(json.get("binding_window").is_some());
        assert!(json.get("windows").is_some());
        assert!(json.get("workers").is_some());
        assert!(json.get("burn_rate").is_some());
        assert!(json.get("schedule").is_some());
    }

    #[test]
    fn format_json_exit_code_matches_state() {
        let state = make_test_state();
        let json = format_status_json(&state);

        // State has cutoff_risk = true, so exit_code should be 2
        assert_eq!(json["exit_code"], 2);
    }

    #[test]
    fn format_hours_formats_correctly() {
        assert_eq!(format_hours(0.0), "now");
        assert_eq!(format_hours(-1.0), "now");
        assert_eq!(format_hours(0.5), "30m");
        assert_eq!(format_hours(2.5), "2.5h");
        assert_eq!(format_hours(36.0), "1.5d");
    }

    #[test]
    fn risk_indicator_returns_correct() {
        let mut win = WindowForecast::default();

        win.cutoff_risk = true;
        assert_eq!(risk_indicator(&win), "CUTOFF");

        win.cutoff_risk = false;
        win.margin_hrs = 0.5;
        assert_eq!(risk_indicator(&win), "LOW");

        win.margin_hrs = 3.0;
        assert_eq!(risk_indicator(&win), "WARN");

        win.margin_hrs = 10.0;
        assert_eq!(risk_indicator(&win), "ok");
    }

    #[test]
    fn pressure_level_high_when_cutoff() {
        let state = make_test_state();
        let pressure = compute_pressure_level(&state.capacity_forecast);

        // State has cutoff_risk = true
        assert_eq!(pressure, PressureLevel::High);
    }

    #[test]
    fn exit_code_from_state() {
        let state = make_test_state();
        let exit_code = StatusExitCode::from_state(&state);

        // State has cutoff_risk but not emergency
        assert_eq!(exit_code, StatusExitCode::CutoffRisk);
        assert_eq!(exit_code.as_exit_code(), 2);

        // With emergency brake
        let mut state_emergency = state.clone();
        state_emergency.safe_mode.active = true;
        let exit_code_emergency = StatusExitCode::from_state(&state_emergency);
        assert_eq!(exit_code_emergency, StatusExitCode::Emergency);
        assert_eq!(exit_code_emergency.as_exit_code(), 3);
    }
}
