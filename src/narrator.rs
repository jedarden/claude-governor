//! Decision Narration and Audit Log
//!
//! This module provides template-based text generation for governor decisions
//! and maintains a persistent JSONL audit log at `~/.needle/state/governor-decisions.jsonl`.
//!
//! Every scaling decision generates a decision entry with:
//! - Human-readable explanation with specific data points
//! - Structured data for programmatic access
//!
//! The `cgov explain` subcommand reads this log to provide transparency.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::state::{GovernorState, WindowForecast};

// ---------------------------------------------------------------------------
// ScaleAction enum
// ---------------------------------------------------------------------------

/// Types of scaling actions the governor can take
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleAction {
    /// Increased worker count
    ScaleUp,
    /// Decreased worker count
    ScaleDown,
    /// No change needed (steady state)
    Hold,
    /// Sprint mode activated (underutilization response)
    SprintActivate,
    /// Sprint mode deactivated (goal achieved or expired)
    SprintDeactivate,
    /// Pre-scale adjustment before predicted exhaustion
    PreScale,
    /// Emergency brake engaged (98%+ utilization)
    EmergencyBrakeEngage,
    /// Emergency brake released (below threshold)
    EmergencyBrakeRelease,
    /// Off-peak promotion transition
    PromotionTransition,
    /// Window moved from safe to cutoff risk
    CutoffRiskTransitionSafeToRisk,
    /// Window moved from cutoff risk to safe
    CutoffRiskTransitionRiskToSafe,
    /// Prediction accuracy score update
    PredictionAccuracyScore,
}

impl std::fmt::Display for ScaleAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScaleAction::ScaleUp => write!(f, "scale_up"),
            ScaleAction::ScaleDown => write!(f, "scale_down"),
            ScaleAction::Hold => write!(f, "hold"),
            ScaleAction::SprintActivate => write!(f, "sprint_activate"),
            ScaleAction::SprintDeactivate => write!(f, "sprint_deactivate"),
            ScaleAction::PreScale => write!(f, "pre_scale"),
            ScaleAction::EmergencyBrakeEngage => write!(f, "emergency_brake_engage"),
            ScaleAction::EmergencyBrakeRelease => write!(f, "emergency_brake_release"),
            ScaleAction::PromotionTransition => write!(f, "promotion_transition"),
            ScaleAction::CutoffRiskTransitionSafeToRisk => write!(f, "cutoff_risk_transition_safe_to_risk"),
            ScaleAction::CutoffRiskTransitionRiskToSafe => write!(f, "cutoff_risk_transition_risk_to_safe"),
            ScaleAction::PredictionAccuracyScore => write!(f, "prediction_accuracy_score"),
        }
    }
}

// ---------------------------------------------------------------------------
// DecisionEntry
// ---------------------------------------------------------------------------

/// A single decision entry in the audit log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEntry {
    /// ISO 8601 timestamp when decision was made
    pub ts: DateTime<Utc>,

    /// The type of action taken
    pub action: ScaleAction,

    /// Worker count before action (aggregate across all agents)
    pub from: u32,

    /// Worker count after action (aggregate across all agents)
    pub to: u32,

    /// Human-readable explanation with specific data points
    pub reason: String,

    /// What triggered this decision (e.g., "seven_day_sonnet margin_hrs=-2.5")
    pub trigger: String,

    /// The binding window at decision time
    pub binding_window: String,

    /// Margin hours before action (for the binding window)
    pub margin_before: f64,

    /// Margin hours after action (predicted)
    pub margin_after: f64,

    /// Additional context as key-value pairs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Decision narration
// ---------------------------------------------------------------------------

/// Context for narrating a decision
pub struct DecisionContext<'a> {
    /// State before the action
    pub before: &'a GovernorState,
    /// State after the action
    pub after: &'a GovernorState,
    /// The action taken
    pub action: ScaleAction,
    /// What triggered the decision
    pub trigger: String,
    /// Agent ID if action was agent-specific
    pub agent_id: Option<String>,
    /// Old worker count for the affected agent(s)
    pub workers_before: u32,
    /// New worker count for the affected agent(s)
    pub workers_after: u32,
}

/// Generate a human-readable explanation for a decision
///
/// This is a pure template-based text generator - no LLM involved.
/// Templates include specific numbers (window names, percentages, hours, worker counts).
pub fn narrate_decision(ctx: &DecisionContext) -> DecisionEntry {
    let binding_window = ctx.after.capacity_forecast.binding_window.clone();
    let margin_before = get_margin_for_window(ctx.before, &binding_window);
    let margin_after = get_margin_for_window(ctx.after, &binding_window);

    let reason = generate_reason(ctx, &binding_window, margin_before, margin_after);

    DecisionEntry {
        ts: Utc::now(),
        action: ctx.action,
        from: ctx.workers_before,
        to: ctx.workers_after,
        reason,
        trigger: ctx.trigger.clone(),
        binding_window,
        margin_before,
        margin_after,
        context: None,
    }
}

/// Get margin hours for a specific window
fn get_margin_for_window(state: &GovernorState, window: &str) -> f64 {
    let forecast = &state.capacity_forecast;
    match window {
        "five_hour" => forecast.five_hour.margin_hrs,
        "seven_day" => forecast.seven_day.margin_hrs,
        "seven_day_sonnet" => forecast.seven_day_sonnet.margin_hrs,
        _ => 0.0,
    }
}

/// Get the window forecast for a specific window name
fn get_window_forecast<'a>(state: &'a GovernorState, window: &str) -> Option<&'a WindowForecast> {
    let forecast = &state.capacity_forecast;
    match window {
        "five_hour" => Some(&forecast.five_hour),
        "seven_day" => Some(&forecast.seven_day),
        "seven_day_sonnet" => Some(&forecast.seven_day_sonnet),
        _ => None,
    }
}

/// Generate the human-readable reason text
fn generate_reason(
    ctx: &DecisionContext,
    binding_window: &str,
    margin_before: f64,
    margin_after: f64,
) -> String {
    match ctx.action {
        ScaleAction::ScaleUp => {
            let win = get_window_forecast(ctx.after, binding_window);
            let util_info = win.map_or(String::new(), |w| {
                format!(
                    " at {:.1}% utilization with {:.1}h remaining",
                    w.current_utilization, w.hours_remaining
                )
            });
            format!(
                "Scaled up from {} to {} workers. Binding window '{}'{} had margin {:.1}h, now predicted {:.1}h.",
                ctx.workers_before, ctx.workers_after, binding_window, util_info, margin_before, margin_after
            )
        }
        ScaleAction::ScaleDown => {
            let win = get_window_forecast(ctx.after, binding_window);
            let util_info = win.map_or(String::new(), |w| {
                format!(
                    " at {:.1}% utilization",
                    w.current_utilization
                )
            });
            format!(
                "Scaled down from {} to {} workers. Binding window '{}'{} had margin {:.1}h, now predicted {:.1}h.",
                ctx.workers_before, ctx.workers_after, binding_window, util_info, margin_before, margin_after
            )
        }
        ScaleAction::Hold => {
            let win = get_window_forecast(ctx.after, binding_window);
            let margin_info = win.map_or(String::new(), |w| {
                format!(
                    "Margin: {:.1}h, utilization: {:.1}%, hours remaining: {:.1}h.",
                    w.margin_hrs, w.current_utilization, w.hours_remaining
                )
            });
            format!(
                "Holding at {} workers. Binding window: '{}'. {}",
                ctx.workers_after, binding_window, margin_info
            )
        }
        ScaleAction::SprintActivate => {
            let agent = ctx.agent_id.as_deref().unwrap_or("unknown");
            let win = get_window_forecast(ctx.before, binding_window);
            let trigger_info = win.map_or(ctx.trigger.clone(), |w| {
                format!(
                    "{} at {:.1}% utilization, {:.1}h to reset",
                    binding_window, w.current_utilization, w.hours_remaining
                )
            });
            format!(
                "Sprint activated for agent '{}': boosting from {} to {} workers. Trigger: underutilization on {}.",
                agent, ctx.workers_before, ctx.workers_after, trigger_info
            )
        }
        ScaleAction::SprintDeactivate => {
            let agent = ctx.agent_id.as_deref().unwrap_or("unknown");
            format!(
                "Sprint deactivated for agent '{}': restoring from {} to {} workers. Goal achieved or window reset.",
                agent, ctx.workers_before, ctx.workers_after
            )
        }
        ScaleAction::PreScale => {
            let win = get_window_forecast(ctx.before, binding_window);
            let exhaustion_info = win.map_or(String::new(), |w| {
                format!(" predicted exhaustion in {:.1}h", w.predicted_exhaustion_hours)
            });
            format!(
                "Pre-emptive scale from {} to {} workers. Binding window '{}'{} to prevent cutoff. Margin: {:.1}h -> {:.1}h.",
                ctx.workers_before, ctx.workers_after, binding_window, exhaustion_info, margin_before, margin_after
            )
        }
        ScaleAction::EmergencyBrakeEngage => {
            let win = get_window_forecast(ctx.before, binding_window);
            let util_info = win.map_or(String::new(), |w| {
                format!(" at {:.1}% utilization", w.current_utilization)
            });
            format!(
                "EMERGENCY BRAKE ENGAGED: Scaled all workers from {} to 0. Binding window '{}'{} exceeded 98% threshold. Immediate halt to prevent cutoff.",
                ctx.workers_before, binding_window, util_info
            )
        }
        ScaleAction::EmergencyBrakeRelease => {
            let win = get_window_forecast(ctx.after, binding_window);
            let util_info = win.map_or(String::new(), |w| {
                format!(" now at {:.1}% utilization", w.current_utilization)
            });
            format!(
                "Emergency brake released. Binding window '{}'{}. Resuming with {} workers.",
                binding_window, util_info, ctx.workers_after
            )
        }
        ScaleAction::PromotionTransition => {
            let schedule = &ctx.after.schedule;
            let promo_status = if schedule.is_promo_active {
                format!("active ({}x multiplier)", schedule.promo_multiplier)
            } else {
                "inactive".to_string()
            };
            let burn = &ctx.after.burn_rate;
            format!(
                "Off-peak promotion transition. Promotion: {}. Observed off-peak ratio: {:.2}, expected: {:.2}. Effective hours remaining: {:.1}h.",
                promo_status, burn.offpeak_ratio_observed, burn.offpeak_ratio_expected, schedule.effective_hours_remaining
            )
        }
        ScaleAction::CutoffRiskTransitionSafeToRisk => {
            let win = get_window_forecast(ctx.after, binding_window);
            let details = win.map_or(String::new(), |w| {
                format!(
                    " at {:.1}% utilization, {:.1}h remaining, margin {:.1}h",
                    w.current_utilization, w.hours_remaining, w.margin_hrs
                )
            });
            format!(
                "CUTOFF RISK ALERT: Window '{}' transitioned from safe to at-risk{}. Workers: {} -> {}. Predicted exhaustion in {:.1}h.",
                binding_window,
                details,
                ctx.workers_before,
                ctx.workers_after,
                win.map(|w| w.predicted_exhaustion_hours).unwrap_or(0.0)
            )
        }
        ScaleAction::CutoffRiskTransitionRiskToSafe => {
            let win = get_window_forecast(ctx.after, binding_window);
            let details = win.map_or(String::new(), |w| {
                format!(
                    " now at {:.1}% utilization, margin {:.1}h",
                    w.current_utilization, w.margin_hrs
                )
            });
            format!(
                "Cutoff risk cleared: Window '{}' is now safe{}. Workers restored to {}.",
                binding_window, details, ctx.workers_after
            )
        }
        ScaleAction::PredictionAccuracyScore => {
            let cal = &ctx.after.burn_rate.calibration;
            format!(
                "Prediction calibration update. Predictions scored: {}, median error (7d-s): {:.1}%, auto-tuned alpha: {:.2}, hysteresis: {:.1}.",
                cal.predictions_scored, cal.median_error_7ds, cal.auto_tuned_alpha, cal.auto_tuned_hysteresis
            )
        }
    }
}

// ---------------------------------------------------------------------------
// JSONL Audit Log
// ---------------------------------------------------------------------------

/// Default path for the decisions audit log
pub fn default_decisions_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".needle")
        .join("state")
        .join("governor-decisions.jsonl")
}

/// Append a decision entry to the JSONL audit log
///
/// Creates the parent directory if it doesn't exist.
/// Each entry is appended as a single JSON line.
pub fn append_decision(entry: &DecisionEntry) -> std::io::Result<()> {
    append_decision_to_path(entry, &default_decisions_path())
}

/// Append a decision entry to a specific path
pub fn append_decision_to_path(entry: &DecisionEntry, path: &PathBuf) -> std::io::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open file for append (create if doesn't exist)
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    // Serialize and write as a single line
    let json = serde_json::to_string(entry)?;
    writeln!(file, "{}", json)?;

    Ok(())
}

/// Read the last N decisions from the audit log
///
/// Returns decisions in reverse chronological order (most recent first).
/// Returns fewer than N if the log has fewer entries.
pub fn read_last_decisions(n: usize) -> std::io::Result<Vec<DecisionEntry>> {
    read_last_decisions_from_path(n, &default_decisions_path())
}

/// Read the last N decisions from a specific path
pub fn read_last_decisions_from_path(n: usize, path: &PathBuf) -> std::io::Result<Vec<DecisionEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    // Read all lines
    let all_entries: Vec<DecisionEntry> = reader
        .lines()
        .filter_map(|line| {
            line.ok().and_then(|l| {
                serde_json::from_str::<DecisionEntry>(&l).ok()
            })
        })
        .collect();

    // Take the last N entries
    let start = if all_entries.len() > n {
        all_entries.len() - n
    } else {
        0
    };

    // Return in reverse chronological order
    let mut result: Vec<DecisionEntry> = all_entries[start..].to_vec();
    result.reverse();
    Ok(result)
}

/// Read all decisions from the audit log
pub fn read_all_decisions() -> std::io::Result<Vec<DecisionEntry>> {
    read_all_decisions_from_path(&default_decisions_path())
}

/// Read all decisions from a specific path
pub fn read_all_decisions_from_path(path: &PathBuf) -> std::io::Result<Vec<DecisionEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let entries: Vec<DecisionEntry> = reader
        .lines()
        .filter_map(|line| {
            line.ok().and_then(|l| {
                serde_json::from_str::<DecisionEntry>(&l).ok()
            })
        })
        .collect();

    Ok(entries)
}

// ---------------------------------------------------------------------------
// Formatting for CLI
// ---------------------------------------------------------------------------

/// Format a decision entry for human consumption
pub fn format_decision_human(entry: &DecisionEntry) -> String {
    let action_emoji = match entry.action {
        ScaleAction::ScaleUp => "📈",
        ScaleAction::ScaleDown => "📉",
        ScaleAction::Hold => "➡️",
        ScaleAction::SprintActivate => "🚀",
        ScaleAction::SprintDeactivate => "🏁",
        ScaleAction::PreScale => "⚡",
        ScaleAction::EmergencyBrakeEngage => "🛑",
        ScaleAction::EmergencyBrakeRelease => "✅",
        ScaleAction::PromotionTransition => "🔄",
        ScaleAction::CutoffRiskTransitionSafeToRisk => "⚠️",
        ScaleAction::CutoffRiskTransitionRiskToSafe => "✓",
        ScaleAction::PredictionAccuracyScore => "📊",
    };

    let mut output = String::new();
    output.push_str(&format!(
        "{} [{}] {}\n",
        action_emoji,
        entry.ts.format("%Y-%m-%d %H:%M:%S UTC"),
        entry.action.to_string().to_uppercase().replace('_', " ")
    ));
    output.push_str(&format!("  Workers: {} -> {}\n", entry.from, entry.to));
    output.push_str(&format!("  Binding: {} (margin {:.1}h -> {:.1}h)\n",
        entry.binding_window, entry.margin_before, entry.margin_after));
    output.push_str(&format!("  Trigger: {}\n", entry.trigger));
    output.push_str(&format!("  {}\n", entry.reason));

    output
}

/// Format multiple decisions for human consumption
pub fn format_decisions_human(entries: &[DecisionEntry]) -> String {
    if entries.is_empty() {
        return "No decisions recorded.\n".to_string();
    }

    let mut output = String::new();
    output.push_str(&format!("Last {} decision(s):\n\n", entries.len()));

    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            output.push_str("\n---\n\n");
        }
        output.push_str(&format_decision_human(entry));
    }

    output
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        BurnRateState, CalibrationState, CapacityForecast, FleetAggregate, GovernorState,
        ScheduleState, UsageState, WindowForecast,
    };
    use tempfile::TempDir;

    // Helper to create a test state
    fn make_test_state(
        binding_window: &str,
        margin_hrs: f64,
        utilization: f64,
        hours_remaining: f64,
        cutoff_risk: bool,
    ) -> GovernorState {
        let window = WindowForecast {
            target_ceiling: 90.0,
            current_utilization: utilization,
            remaining_pct: 90.0 - utilization,
            hours_remaining,
            fleet_pct_per_hour: 5.0,
            predicted_exhaustion_hours: hours_remaining - margin_hrs,
            cutoff_risk,
            margin_hrs,
            binding: true,
            safe_worker_count: None,
        };

        GovernorState {
            updated_at: Utc::now(),
            usage: UsageState::default(),
            last_fleet_aggregate: FleetAggregate::default(),
            capacity_forecast: CapacityForecast {
                five_hour: if binding_window == "five_hour" { window.clone() } else { WindowForecast::default() },
                seven_day: if binding_window == "seven_day" { window.clone() } else { WindowForecast::default() },
                seven_day_sonnet: if binding_window == "seven_day_sonnet" { window.clone() } else { WindowForecast::default() },
                binding_window: binding_window.to_string(),
                dollars_per_pct_7d_s: 1.5,
                estimated_remaining_dollars: 50.0,
            },
            schedule: ScheduleState {
                is_peak_hour: false,
                is_promo_active: true,
                promo_multiplier: 2.0,
                effective_hours_remaining: 75.0,
                raw_hours_remaining: 37.5,
            },
            workers: Default::default(),
            burn_rate: BurnRateState {
                offpeak_ratio_observed: 2.0,
                offpeak_ratio_expected: 2.0,
                calibration: CalibrationState {
                    predictions_scored: 25,
                    median_error_7ds: -3.5,
                    auto_tuned_alpha: 0.25,
                    auto_tuned_hysteresis: 1.0,
                    last_tuned_at: None,
                },
                ..BurnRateState::default()
            },
            alerts: Vec::new(),
            safe_mode: Default::default(),
            alert_cooldown: Default::default(),
            token_refresh_failing: false,
        }
    }

    #[test]
    fn test_narrate_scale_up() {
        let before = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);
        let after = make_test_state("seven_day_sonnet", 3.0, 65.0, 37.5, false);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::ScaleUp,
            trigger: "margin_hrs dropped below 5h threshold".to_string(),
            agent_id: None,
            workers_before: 2,
            workers_after: 3,
        };

        let entry = narrate_decision(&ctx);

        assert_eq!(entry.action, ScaleAction::ScaleUp);
        assert_eq!(entry.from, 2);
        assert_eq!(entry.to, 3);
        assert!(entry.reason.contains("Scaled up from 2 to 3 workers"));
        assert!(entry.reason.contains("seven_day_sonnet"));
        assert!(entry.reason.contains("65.0% utilization"));
        assert!(entry.binding_window == "seven_day_sonnet");
        assert!((entry.margin_before - 5.0).abs() < 0.01);
        assert!((entry.margin_after - 3.0).abs() < 0.01);
    }

    #[test]
    fn test_narrate_scale_down() {
        let before = make_test_state("five_hour", 2.0, 80.0, 2.5, false);
        let after = make_test_state("five_hour", 3.0, 75.0, 2.5, false);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::ScaleDown,
            trigger: "utilization approaching threshold".to_string(),
            agent_id: None,
            workers_before: 5,
            workers_after: 3,
        };

        let entry = narrate_decision(&ctx);

        assert_eq!(entry.action, ScaleAction::ScaleDown);
        assert_eq!(entry.from, 5);
        assert_eq!(entry.to, 3);
        assert!(entry.reason.contains("Scaled down from 5 to 3 workers"));
        assert!(entry.reason.contains("five_hour"));
        assert!(entry.reason.contains("75.0% utilization"));
    }

    #[test]
    fn test_narrate_sprint_activate() {
        let before = make_test_state("five_hour", 5.0, 45.0, 1.5, false);
        let after = make_test_state("five_hour", 3.0, 50.0, 1.5, false);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::SprintActivate,
            trigger: "underutilization at 45%".to_string(),
            agent_id: Some("sonnet-worker".to_string()),
            workers_before: 2,
            workers_after: 8,
        };

        let entry = narrate_decision(&ctx);

        assert_eq!(entry.action, ScaleAction::SprintActivate);
        assert!(entry.reason.contains("Sprint activated"));
        assert!(entry.reason.contains("sonnet-worker"));
        assert!(entry.reason.contains("boosting from 2 to 8"));
        assert!(entry.reason.contains("underutilization"));
        assert!(entry.reason.contains("45.0%"));
        assert!(entry.reason.contains("1.5h"));
    }

    #[test]
    fn test_narrate_emergency_brake() {
        let before = make_test_state("seven_day_sonnet", -2.0, 98.5, 37.5, true);
        let after = make_test_state("seven_day_sonnet", -2.0, 98.5, 37.5, true);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::EmergencyBrakeEngage,
            trigger: "utilization exceeded 98%".to_string(),
            agent_id: None,
            workers_before: 5,
            workers_after: 0,
        };

        let entry = narrate_decision(&ctx);

        assert_eq!(entry.action, ScaleAction::EmergencyBrakeEngage);
        assert!(entry.reason.contains("EMERGENCY BRAKE ENGAGED"));
        assert!(entry.reason.contains("5 to 0"));
        assert!(entry.reason.contains("98% threshold"));
        assert!(entry.reason.contains("Immediate halt"));
    }

    #[test]
    fn test_narrate_cutoff_risk_transition() {
        let before = make_test_state("seven_day", 5.0, 75.0, 30.0, false);
        let after = make_test_state("seven_day", -3.0, 85.0, 30.0, true);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::CutoffRiskTransitionSafeToRisk,
            trigger: "margin_hrs dropped below 0".to_string(),
            agent_id: None,
            workers_before: 3,
            workers_after: 2,
        };

        let entry = narrate_decision(&ctx);

        assert_eq!(entry.action, ScaleAction::CutoffRiskTransitionSafeToRisk);
        assert!(entry.reason.contains("CUTOFF RISK ALERT"));
        assert!(entry.reason.contains("safe to at-risk"));
        assert!(entry.reason.contains("seven_day"));
        assert!(entry.reason.contains("85.0% utilization"));
        assert!(entry.reason.contains("Predicted exhaustion"));
    }

    #[test]
    fn test_narrate_cutoff_risk_to_safe() {
        let before = make_test_state("seven_day_sonnet", -1.0, 85.0, 30.0, true);
        let after = make_test_state("seven_day_sonnet", 5.0, 75.0, 30.0, false);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::CutoffRiskTransitionRiskToSafe,
            trigger: "margin_hrs recovered".to_string(),
            agent_id: None,
            workers_before: 2,
            workers_after: 3,
        };

        let entry = narrate_decision(&ctx);

        assert_eq!(entry.action, ScaleAction::CutoffRiskTransitionRiskToSafe);
        assert!(entry.reason.contains("Cutoff risk cleared"));
        assert!(entry.reason.contains("now safe"));
        assert!(entry.reason.contains("75.0% utilization"));
    }

    #[test]
    fn test_jsonl_append_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-decisions.jsonl");

        let before = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);
        let after = make_test_state("seven_day_sonnet", 3.0, 65.0, 37.5, false);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::ScaleUp,
            trigger: "test".to_string(),
            agent_id: None,
            workers_before: 2,
            workers_after: 3,
        };

        // Create and append entry
        let entry = narrate_decision(&ctx);
        append_decision_to_path(&entry, &path).unwrap();

        // Read it back
        let entries = read_last_decisions_from_path(10, &path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, ScaleAction::ScaleUp);
        assert_eq!(entries[0].from, 2);
        assert_eq!(entries[0].to, 3);
        assert!(entries[0].reason.contains("Scaled up"));
    }

    #[test]
    fn test_jsonl_multiple_entries_order() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-decisions.jsonl");

        let state = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);

        // Create and append multiple entries
        for i in 0..5 {
            let ctx = DecisionContext {
                before: &state,
                after: &state,
                action: ScaleAction::Hold,
                trigger: format!("test trigger {}", i),
                agent_id: None,
                workers_before: i,
                workers_after: i,
            };
            let entry = narrate_decision(&ctx);
            append_decision_to_path(&entry, &path).unwrap();
        }

        // Read last 3
        let entries = read_last_decisions_from_path(3, &path).unwrap();
        assert_eq!(entries.len(), 3);

        // Should be in reverse chronological order (most recent first)
        // The last appended entries have triggers "test trigger 3" and "test trigger 4"
        assert!(entries[0].trigger.contains("4") || entries[0].trigger.contains("3"));
        assert!(entries[1].trigger.contains("3") || entries[1].trigger.contains("2"));
        assert!(entries[2].trigger.contains("2") || entries[2].trigger.contains("1"));
    }

    #[test]
    fn test_read_last_decisions_count() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-decisions.jsonl");

        let state = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);

        // Create 10 entries
        for i in 0..10 {
            let ctx = DecisionContext {
                before: &state,
                after: &state,
                action: ScaleAction::Hold,
                trigger: format!("entry {}", i),
                agent_id: None,
                workers_before: i,
                workers_after: i,
            };
            let entry = narrate_decision(&ctx);
            append_decision_to_path(&entry, &path).unwrap();
        }

        // Request 5, should get exactly 5
        let entries = read_last_decisions_from_path(5, &path).unwrap();
        assert_eq!(entries.len(), 5);

        // Request 20, should get 10 (all available)
        let entries = read_last_decisions_from_path(20, &path).unwrap();
        assert_eq!(entries.len(), 10);
    }

    #[test]
    fn test_format_decision_human() {
        let before = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);
        let after = make_test_state("seven_day_sonnet", 3.0, 65.0, 37.5, false);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::ScaleUp,
            trigger: "test trigger".to_string(),
            agent_id: None,
            workers_before: 2,
            workers_after: 3,
        };

        let entry = narrate_decision(&ctx);
        let formatted = format_decision_human(&entry);

        assert!(formatted.contains("SCALE UP"));
        assert!(formatted.contains("Workers: 2 -> 3"));
        assert!(formatted.contains("Binding: seven_day_sonnet"));
        assert!(formatted.contains("Trigger: test trigger"));
    }

    #[test]
    fn test_format_multiple_decisions() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-decisions.jsonl");

        let state = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);

        for i in 0..3 {
            let ctx = DecisionContext {
                before: &state,
                after: &state,
                action: ScaleAction::Hold,
                trigger: format!("entry {}", i),
                agent_id: None,
                workers_before: i,
                workers_after: i,
            };
            let entry = narrate_decision(&ctx);
            append_decision_to_path(&entry, &path).unwrap();
        }

        let entries = read_last_decisions_from_path(3, &path).unwrap();
        let formatted = format_decisions_human(&entries);

        assert!(formatted.contains("Last 3 decision(s)"));
        assert!(formatted.contains("---"));
    }

    #[test]
    fn test_empty_decisions() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nonexistent.jsonl");

        let entries = read_last_decisions_from_path(5, &path).unwrap();
        assert!(entries.is_empty());

        let formatted = format_decisions_human(&entries);
        assert!(formatted.contains("No decisions recorded"));
    }

    #[test]
    fn test_scale_action_display() {
        assert_eq!(ScaleAction::ScaleUp.to_string(), "scale_up");
        assert_eq!(ScaleAction::EmergencyBrakeEngage.to_string(), "emergency_brake_engage");
        assert_eq!(ScaleAction::CutoffRiskTransitionSafeToRisk.to_string(), "cutoff_risk_transition_safe_to_risk");
    }

    #[test]
    fn test_prediction_accuracy_score() {
        let state = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);

        let ctx = DecisionContext {
            before: &state,
            after: &state,
            action: ScaleAction::PredictionAccuracyScore,
            trigger: "calibration update".to_string(),
            agent_id: None,
            workers_before: 3,
            workers_after: 3,
        };

        let entry = narrate_decision(&ctx);

        assert!(entry.reason.contains("Predictions scored: 25"));
        assert!(entry.reason.contains("median error"));
        assert!(entry.reason.contains("-3.5%"));
        assert!(entry.reason.contains("auto-tuned alpha: 0.25"));
    }

    #[test]
    fn test_promotion_transition() {
        let state = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);

        let ctx = DecisionContext {
            before: &state,
            after: &state,
            action: ScaleAction::PromotionTransition,
            trigger: "off-peak hours started".to_string(),
            agent_id: None,
            workers_before: 3,
            workers_after: 3,
        };

        let entry = narrate_decision(&ctx);

        assert!(entry.reason.contains("Off-peak promotion"));
        assert!(entry.reason.contains("active (2x multiplier)"));
        assert!(entry.reason.contains("Observed off-peak ratio: 2.00"));
        assert!(entry.reason.contains("Effective hours remaining: 75.0h"));
    }

    #[test]
    fn test_sprint_deactivate() {
        let state = make_test_state("five_hour", 3.0, 55.0, 1.5, false);

        let ctx = DecisionContext {
            before: &state,
            after: &state,
            action: ScaleAction::SprintDeactivate,
            trigger: "sprint goal achieved".to_string(),
            agent_id: Some("sonnet-worker".to_string()),
            workers_before: 8,
            workers_after: 2,
        };

        let entry = narrate_decision(&ctx);

        assert!(entry.reason.contains("Sprint deactivated"));
        assert!(entry.reason.contains("sonnet-worker"));
        assert!(entry.reason.contains("restoring from 8 to 2"));
        assert!(entry.reason.contains("Goal achieved"));
    }

    #[test]
    fn test_emergency_brake_release() {
        let state = make_test_state("seven_day_sonnet", 5.0, 95.0, 37.5, false);

        let ctx = DecisionContext {
            before: &state,
            after: &state,
            action: ScaleAction::EmergencyBrakeRelease,
            trigger: "utilization dropped below 98%".to_string(),
            agent_id: None,
            workers_before: 0,
            workers_after: 2,
        };

        let entry = narrate_decision(&ctx);

        assert!(entry.reason.contains("Emergency brake released"));
        assert!(entry.reason.contains("now at 95.0% utilization"));
        assert!(entry.reason.contains("Resuming with 2 workers"));
    }

    #[test]
    fn test_pre_scale() {
        let before = make_test_state("seven_day_sonnet", 1.0, 80.0, 30.0, false);
        let after = make_test_state("seven_day_sonnet", 2.0, 80.0, 30.0, false);

        let ctx = DecisionContext {
            before: &before,
            after: &after,
            action: ScaleAction::PreScale,
            trigger: "predicted exhaustion soon".to_string(),
            agent_id: None,
            workers_before: 3,
            workers_after: 2,
        };

        let entry = narrate_decision(&ctx);

        assert!(entry.reason.contains("Pre-emptive scale from 3 to 2"));
        assert!(entry.reason.contains("prevent cutoff"));
        assert!(entry.reason.contains("predicted exhaustion"));
        assert!(entry.reason.contains("Margin: 1.0h -> 2.0h"));
    }

    #[test]
    fn test_jsonl_format_is_valid() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-format.jsonl");

        let state = make_test_state("seven_day_sonnet", 5.0, 60.0, 37.5, false);

        let ctx = DecisionContext {
            before: &state,
            after: &state,
            action: ScaleAction::ScaleUp,
            trigger: "test".to_string(),
            agent_id: Some("agent-1".to_string()),
            workers_before: 2,
            workers_after: 3,
        };

        let entry = narrate_decision(&ctx);
        append_decision_to_path(&entry, &path).unwrap();

        // Read raw file content
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        // Should be exactly one line
        assert_eq!(lines.len(), 1);

        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert!(parsed.is_object());
        assert!(parsed.get("ts").is_some());
        assert!(parsed.get("action").is_some());
        assert!(parsed.get("reason").is_some());
        assert_eq!(parsed["action"], "scale_up");
    }
}
