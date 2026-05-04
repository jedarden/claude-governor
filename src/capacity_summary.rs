//! Capacity Summary Generation for NEEDLE Integration
//!
//! This module provides capacity awareness for Claude Code workers via prompt injection.
//! The `generate_capacity_summary` function produces markdown suitable for injection
//! into CLAUDE.md, giving workers visibility into fleet capacity state.
//!
//! The `generate_status_dashboard` function produces the rich human-readable table
//! displayed by `cgov status`.
//!
//! ## Pressure Levels
//!
//! - **LOW**: `margin_hrs > hrs_left * 0.5` — no constraints, work normally
//! - **MEDIUM**: `margin_hrs > 0` but `<= hrs_left * 0.5` — be efficient, don't compromise quality
//! - **HIGH**: `cutoff_risk` active — actively conserve, prefer Haiku, skip optional steps
//!
//! ## Exit Codes
//!
//! The status command returns semantic exit codes for script-based checking:
//! - `0` = all windows safe
//! - `2` = cutoff_risk active
//! - `3` = emergency brake engaged

use crate::state::{CapacityForecast, GovernorState, WindowForecast};

/// Capacity pressure level for worker guidance
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureLevel {
    /// Ample headroom — no constraints, work normally
    Low,
    /// Moderate headroom — be efficient but don't compromise quality
    Medium,
    /// Cutoff risk active — actively conserve, prefer Haiku, skip optional steps
    High,
}

impl std::fmt::Display for PressureLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PressureLevel::Low => write!(f, "LOW"),
            PressureLevel::Medium => write!(f, "MEDIUM"),
            PressureLevel::High => write!(f, "HIGH"),
        }
    }
}

/// Status exit codes for script-based checking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusExitCode {
    /// All windows safe
    Safe = 0,
    /// Cutoff risk active (one or more windows)
    CutoffRisk = 2,
    /// Emergency brake engaged
    Emergency = 3,
}

impl StatusExitCode {
    /// Determine exit code from governor state
    pub fn from_state(state: &GovernorState) -> Self {
        // Check for emergency brake first (highest priority)
        if state.safe_mode.active {
            return StatusExitCode::Emergency;
        }

        // Check for cutoff risk in any window
        let forecast = &state.capacity_forecast;
        if forecast.five_hour.cutoff_risk
            || forecast.seven_day.cutoff_risk
            || forecast.seven_day_sonnet.cutoff_risk
        {
            return StatusExitCode::CutoffRisk;
        }

        StatusExitCode::Safe
    }

    /// Convert to process exit code
    pub fn as_exit_code(self) -> i32 {
        self as i32
    }
}

/// Determine the pressure level based on the binding window's metrics
///
/// Pressure is computed from the binding window (the most constrained window).
/// If no binding window is set, we use the window with the smallest margin.
pub fn compute_pressure_level(forecast: &CapacityForecast) -> PressureLevel {
    // If any window has cutoff_risk, pressure is HIGH
    if forecast.five_hour.cutoff_risk
        || forecast.seven_day.cutoff_risk
        || forecast.seven_day_sonnet.cutoff_risk
    {
        return PressureLevel::High;
    }

    // Find the binding window, or fall back to the most constrained
    let binding_window = if !forecast.binding_window.is_empty() {
        match forecast.binding_window.as_str() {
            "five_hour" => &forecast.five_hour,
            "seven_day" => &forecast.seven_day,
            "seven_day_sonnet" => &forecast.seven_day_sonnet,
            _ => find_most_constrained_window(forecast),
        }
    } else {
        find_most_constrained_window(forecast)
    };

    let margin_hrs = binding_window.margin_hrs;
    let hrs_left = binding_window.hours_remaining;

    // Avoid division by zero
    if hrs_left <= 0.0 {
        return PressureLevel::High;
    }

    // LOW: margin > 50% of remaining time
    if margin_hrs > hrs_left * 0.5 {
        PressureLevel::Low
    }
    // MEDIUM: margin > 0 but <= 50% of remaining time
    else if margin_hrs > 0.0 {
        PressureLevel::Medium
    }
    // HIGH: no margin (negative or zero)
    else {
        PressureLevel::High
    }
}

/// Find the window with the smallest (most constrained) margin
fn find_most_constrained_window(forecast: &CapacityForecast) -> &WindowForecast {
    let windows = [
        &forecast.five_hour,
        &forecast.seven_day,
        &forecast.seven_day_sonnet,
    ];

    windows
        .iter()
        .min_by(|a, b| {
            a.margin_hrs
                .partial_cmp(&b.margin_hrs)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(&forecast.seven_day_sonnet, |v| v)
}

/// Get behavioral recommendation based on pressure level
fn get_recommendation(level: PressureLevel) -> &'static str {
    match level {
        PressureLevel::Low => "No capacity constraints — work normally.",
        PressureLevel::Medium => {
            "Be efficient with capacity. Avoid unnecessary exploration, but don't compromise quality."
        }
        PressureLevel::High => {
            "Actively conserve capacity. Prefer Haiku subagents where possible, \
             skip optional steps, minimize speculative multi-file reads."
        }
    }
}

/// Generate a capacity summary suitable for NEEDLE prompt injection
///
/// This produces markdown that can be injected into a worker's CLAUDE.md
/// to give it visibility into the fleet's capacity state.
///
/// ## Output Format
///
/// ```markdown
/// ## Fleet Capacity (auto-injected by governor)
///
/// - Binding window: seven_day_sonnet — 26% headroom, resets in 37h
/// - Capacity pressure: HIGH (cutoff risk active)
/// - Recommendation: actively conserve — prefer Haiku subagents...
/// ```
pub fn generate_capacity_summary(state: &GovernorState) -> String {
    let forecast = &state.capacity_forecast;
    let pressure_level = compute_pressure_level(forecast);
    let recommendation = get_recommendation(pressure_level);

    // Find the binding window
    let binding = if !forecast.binding_window.is_empty() {
        match forecast.binding_window.as_str() {
            "five_hour" => &forecast.five_hour,
            "seven_day" => &forecast.seven_day,
            _ => &forecast.seven_day_sonnet,
        }
    } else {
        &forecast.seven_day_sonnet
    };

    let headroom_pct = binding.remaining_pct;
    let hours_remaining = binding.hours_remaining;
    let binding_name = if forecast.binding_window.is_empty() {
        "seven_day_sonnet"
    } else {
        &forecast.binding_window
    };

    // Format reset time
    let reset_time = if hours_remaining < 1.0 {
        format!("{:.0}m", hours_remaining * 60.0)
    } else {
        format!("{:.0}h", hours_remaining)
    };

    // Pressure reason
    let pressure_reason = match pressure_level {
        PressureLevel::Low => "ample headroom".to_string(),
        PressureLevel::Medium => "moderate headroom".to_string(),
        PressureLevel::High => {
            if binding.cutoff_risk {
                "cutoff risk active".to_string()
            } else {
                "low margin".to_string()
            }
        }
    };

    format!(
        r#"## Fleet Capacity (auto-injected by governor)

- Binding window: {} — {:.0}% headroom, resets in {}
- Capacity pressure: {} ({})
- Recommendation: {}"#,
        binding_name, headroom_pct, reset_time, pressure_level, pressure_reason, recommendation
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{FleetAggregate, ScheduleState, UsageState};

    /// Create a test forecast with configurable margin and hours remaining
    fn make_forecast(
        margin_hrs: f64,
        hours_remaining: f64,
        cutoff_risk: bool,
        binding: bool,
    ) -> WindowForecast {
        WindowForecast {
            margin_hrs,
            hours_remaining,
            cutoff_risk,
            remaining_pct: if margin_hrs > 0.0 {
                margin_hrs * 2.0
            } else {
                5.0
            },
            binding,
            ..WindowForecast::default()
        }
    }

    /// Create a minimal governor state for testing
    fn make_state(forecast: CapacityForecast) -> GovernorState {
        GovernorState {
            capacity_forecast: forecast,
            usage: UsageState::default(),
            last_fleet_aggregate: FleetAggregate::default(),
            schedule: ScheduleState::default(),
            workers: Default::default(),
            burn_rate: Default::default(),
            alerts: Default::default(),
            safe_mode: Default::default(),
            alert_cooldown: Default::default(),
            updated_at: chrono::Utc::now(),
            token_refresh_failing: false,
            low_cache_eff_consecutive: 0,
            alert_fp_telemetry: Default::default(),
            pending_predictions: Default::default(),
        }
    }

    // --- Pressure Level Tests ---

    #[test]
    fn pressure_low_with_ample_headroom() {
        // margin_hrs = 10, hrs_left = 10 → margin > 50% of hrs_left
        let forecast = CapacityForecast {
            seven_day_sonnet: make_forecast(10.0, 10.0, false, true),
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };

        assert_eq!(compute_pressure_level(&forecast), PressureLevel::Low);
    }

    #[test]
    fn pressure_medium_with_moderate_headroom() {
        // margin_hrs = 3, hrs_left = 10 → margin > 0 but < 50% of hrs_left
        let forecast = CapacityForecast {
            seven_day_sonnet: make_forecast(3.0, 10.0, false, true),
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };

        assert_eq!(compute_pressure_level(&forecast), PressureLevel::Medium);
    }

    #[test]
    fn pressure_high_with_cutoff_risk() {
        // cutoff_risk = true forces HIGH regardless of margin
        let forecast = CapacityForecast {
            seven_day_sonnet: make_forecast(10.0, 10.0, true, true),
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };

        assert_eq!(compute_pressure_level(&forecast), PressureLevel::High);
    }

    #[test]
    fn pressure_high_with_zero_margin() {
        // margin_hrs = 0 or negative → HIGH
        let forecast = CapacityForecast {
            seven_day_sonnet: make_forecast(0.0, 10.0, false, true),
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };

        assert_eq!(compute_pressure_level(&forecast), PressureLevel::High);
    }

    #[test]
    fn pressure_high_with_negative_margin() {
        // Negative margin = over budget
        let forecast = CapacityForecast {
            seven_day_sonnet: make_forecast(-5.0, 10.0, false, true),
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };

        assert_eq!(compute_pressure_level(&forecast), PressureLevel::High);
    }

    #[test]
    fn pressure_uses_binding_window() {
        // five_hour is binding with LOW pressure, seven_day_sonnet has HIGH
        let forecast = CapacityForecast {
            five_hour: make_forecast(10.0, 2.0, false, true),
            seven_day_sonnet: make_forecast(0.0, 40.0, true, false),
            binding_window: "five_hour".to_string(),
            ..CapacityForecast::default()
        };

        // Should use five_hour (binding) even though seven_day_sonnet has cutoff_risk
        // Actually, cutoff_risk on ANY window should force HIGH
        assert_eq!(compute_pressure_level(&forecast), PressureLevel::High);
    }

    // --- Exit Code Tests ---

    #[test]
    fn exit_code_safe_no_risks() {
        let forecast = CapacityForecast {
            five_hour: make_forecast(10.0, 2.0, false, false),
            seven_day: make_forecast(20.0, 40.0, false, false),
            seven_day_sonnet: make_forecast(15.0, 40.0, false, false),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        assert_eq!(StatusExitCode::from_state(&state), StatusExitCode::Safe);
        assert_eq!(StatusExitCode::from_state(&state).as_exit_code(), 0);
    }

    #[test]
    fn exit_code_cutoff_risk() {
        let forecast = CapacityForecast {
            five_hour: make_forecast(10.0, 2.0, false, false),
            seven_day: make_forecast(-5.0, 40.0, true, false),
            seven_day_sonnet: make_forecast(15.0, 40.0, false, false),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        assert_eq!(
            StatusExitCode::from_state(&state),
            StatusExitCode::CutoffRisk
        );
        assert_eq!(StatusExitCode::from_state(&state).as_exit_code(), 2);
    }

    #[test]
    fn exit_code_emergency_brake() {
        let forecast = CapacityForecast {
            five_hour: make_forecast(10.0, 2.0, false, false),
            seven_day: make_forecast(20.0, 40.0, false, false),
            seven_day_sonnet: make_forecast(15.0, 40.0, false, false),
            ..CapacityForecast::default()
        };
        let mut state = make_state(forecast);
        state.safe_mode.active = true;

        assert_eq!(
            StatusExitCode::from_state(&state),
            StatusExitCode::Emergency
        );
        assert_eq!(StatusExitCode::from_state(&state).as_exit_code(), 3);
    }

    #[test]
    fn exit_code_emergency_overrides_cutoff() {
        // Emergency brake takes precedence over cutoff_risk
        let forecast = CapacityForecast {
            five_hour: make_forecast(-5.0, 2.0, true, false),
            seven_day: make_forecast(-5.0, 40.0, true, false),
            seven_day_sonnet: make_forecast(-5.0, 40.0, true, false),
            ..CapacityForecast::default()
        };
        let mut state = make_state(forecast);
        state.safe_mode.active = true;

        assert_eq!(
            StatusExitCode::from_state(&state),
            StatusExitCode::Emergency
        );
    }

    // --- Summary Format Tests ---

    #[test]
    fn summary_is_valid_markdown() {
        let forecast = CapacityForecast {
            seven_day_sonnet: make_forecast(10.0, 37.0, false, true),
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        let summary = generate_capacity_summary(&state);

        // Should be valid markdown with header
        assert!(summary.starts_with("## Fleet Capacity"));
        assert!(summary.contains("- Binding window:"));
        assert!(summary.contains("- Capacity pressure:"));
        assert!(summary.contains("- Recommendation:"));
    }

    #[test]
    fn summary_low_pressure_output() {
        let forecast = CapacityForecast {
            seven_day_sonnet: WindowForecast {
                margin_hrs: 20.0,
                hours_remaining: 30.0,
                remaining_pct: 40.0,
                cutoff_risk: false,
                binding: true,
                ..WindowForecast::default()
            },
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        let summary = generate_capacity_summary(&state);

        assert!(summary.contains("LOW"));
        assert!(summary.contains("ample headroom"));
        assert!(summary.contains("work normally"));
    }

    #[test]
    fn summary_medium_pressure_output() {
        let forecast = CapacityForecast {
            seven_day_sonnet: WindowForecast {
                margin_hrs: 5.0,
                hours_remaining: 30.0,
                remaining_pct: 15.0,
                cutoff_risk: false,
                binding: true,
                ..WindowForecast::default()
            },
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        let summary = generate_capacity_summary(&state);

        assert!(summary.contains("MEDIUM"));
        assert!(summary.contains("moderate headroom"));
        assert!(summary.contains("Be efficient"));
    }

    #[test]
    fn summary_high_pressure_output() {
        let forecast = CapacityForecast {
            seven_day_sonnet: WindowForecast {
                margin_hrs: -5.0,
                hours_remaining: 30.0,
                remaining_pct: 5.0,
                cutoff_risk: true,
                binding: true,
                ..WindowForecast::default()
            },
            binding_window: "seven_day_sonnet".to_string(),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        let summary = generate_capacity_summary(&state);

        assert!(summary.contains("HIGH"));
        assert!(summary.contains("cutoff risk active"));
        assert!(summary.contains("Actively conserve"));
        assert!(summary.contains("Haiku"));
    }

    #[test]
    fn summary_formats_short_reset_time() {
        let forecast = CapacityForecast {
            five_hour: WindowForecast {
                margin_hrs: 1.0,
                hours_remaining: 0.5, // 30 minutes
                remaining_pct: 15.0,
                cutoff_risk: false,
                binding: true,
                ..WindowForecast::default()
            },
            binding_window: "five_hour".to_string(),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        let summary = generate_capacity_summary(&state);

        // Should show minutes for < 1 hour
        assert!(summary.contains("30m"));
    }

    #[test]
    fn summary_handles_no_binding_window() {
        // When binding_window is empty, should default to seven_day_sonnet
        let forecast = CapacityForecast {
            seven_day_sonnet: make_forecast(10.0, 37.0, false, false),
            binding_window: String::new(),
            ..CapacityForecast::default()
        };
        let state = make_state(forecast);

        let summary = generate_capacity_summary(&state);

        assert!(summary.contains("seven_day_sonnet"));
    }

    // --- Edge Cases ---

    #[test]
    fn pressure_handles_zero_hours_remaining() {
        // Division by zero protection
        let forecast = CapacityForecast {
            five_hour: make_forecast(5.0, 0.0, false, true),
            binding_window: "five_hour".to_string(),
            ..CapacityForecast::default()
        };

        assert_eq!(compute_pressure_level(&forecast), PressureLevel::High);
    }

    #[test]
    fn finds_most_constrained_window() {
        let forecast = CapacityForecast {
            five_hour: make_forecast(5.0, 2.0, false, false),
            seven_day: make_forecast(1.0, 40.0, false, false), // most constrained
            seven_day_sonnet: make_forecast(10.0, 40.0, false, false),
            binding_window: String::new(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };

        let constrained = find_most_constrained_window(&forecast);
        assert!((constrained.margin_hrs - 1.0).abs() < 0.01);
    }
}
