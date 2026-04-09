//! Trajectory Simulator for Capacity Forecasting
//!
//! Projects future capacity utilization under configurable worker scenarios.
//! Walks forward from current state applying burn rates, promotion multipliers,
//! window resets, and ceiling constraints.
//!
//! Usage:
//!   let config = SimConfig::parse_workers("4:6h,2:6h")?;
//!   let trajectory = simulate(&state, &config, promotions)?;
//!   for point in &trajectory.points {
//!     println!("{}: five_hour={:.1}%", point.timestamp, point.windows["five_hour"]);
//!   }

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::schedule;
use crate::state::GovernorState;

/// Default output resolution in minutes
const DEFAULT_RESOLUTION_MINUTES: i64 = 15;

/// Simulation step size in minutes (internal simulation granularity)
const SIMULATION_STEP_MINUTES: i64 = 1;

/// Window names we track
const WINDOWS: [&str; 3] = ["five_hour", "seven_day", "seven_day_sonnet"];

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for trajectory simulation
#[derive(Debug, Clone)]
pub struct SimConfig {
    /// Worker count or schedule
    pub workers: WorkerSchedule,

    /// Hours to simulate
    pub hours: f64,

    /// Output resolution in minutes (default: 15)
    pub resolution_minutes: i64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            workers: WorkerSchedule::Fixed(1),
            hours: 24.0,
            resolution_minutes: DEFAULT_RESOLUTION_MINUTES,
        }
    }
}

impl SimConfig {
    /// Create a config with a fixed worker count
    pub fn fixed(workers: u32, hours: f64) -> Self {
        Self {
            workers: WorkerSchedule::Fixed(workers),
            hours,
            resolution_minutes: DEFAULT_RESOLUTION_MINUTES,
        }
    }

    /// Parse a worker schedule string
    ///
    /// Formats:
    /// - "4" -> Fixed(4)
    /// - "4:6h,2:6h" -> Schedule with 4 workers for 6h, then 2 workers for 6h
    pub fn parse_workers(s: &str) -> Result<Self> {
        // Check if it's a simple number (fixed workers)
        if let Ok(n) = s.parse::<u32>() {
            return Ok(Self {
                workers: WorkerSchedule::Fixed(n),
                hours: 24.0,
                resolution_minutes: DEFAULT_RESOLUTION_MINUTES,
            });
        }

        // Parse schedule format: "4:6h,2:6h"
        let segments = parse_worker_schedule(s)?;
        Ok(Self {
            workers: WorkerSchedule::Schedule(segments),
            hours: 24.0,
            resolution_minutes: DEFAULT_RESOLUTION_MINUTES,
        })
    }
}

/// Worker schedule - either fixed count or a time-based schedule
#[derive(Debug, Clone)]
pub enum WorkerSchedule {
    /// Fixed number of workers throughout
    Fixed(u32),

    /// Schedule segments: (workers, duration_hours)
    Schedule(Vec<WorkerSegment>),
}

/// A segment of a worker schedule
#[derive(Debug, Clone, Copy)]
pub struct WorkerSegment {
    pub workers: u32,
    pub duration_hours: f64,
}

impl WorkerSchedule {
    /// Get the worker count at a given offset (hours from start)
    pub fn workers_at(&self, hours_offset: f64) -> u32 {
        match self {
            WorkerSchedule::Fixed(n) => *n,
            WorkerSchedule::Schedule(segments) => {
                let mut accumulated = 0.0;
                for seg in segments {
                    accumulated += seg.duration_hours;
                    if hours_offset < accumulated {
                        return seg.workers;
                    }
                }
                // After all segments, use the last worker count
                segments.last().map(|s| s.workers).unwrap_or(0)
            }
        }
    }
}

/// Parse a worker schedule string like "4:6h,2:6h"
fn parse_worker_schedule(s: &str) -> Result<Vec<WorkerSegment>> {
    let mut segments = Vec::new();

    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        // Format: "N:Xh" where N is worker count, X is hours
        let colon_pos = part.find(':').ok_or_else(|| {
            SimError::ParseError(format!("Invalid segment '{}': missing ':'", part))
        })?;

        let workers_str = &part[..colon_pos];
        let duration_str = &part[colon_pos + 1..];

        let workers: u32 = workers_str
            .parse()
            .map_err(|_| SimError::ParseError(format!("Invalid worker count in '{}'", part)))?;

        // Duration should end with 'h'
        let duration_str = duration_str.trim();
        let hours_str = duration_str.strip_suffix('h').ok_or_else(|| {
            SimError::ParseError(format!(
                "Invalid duration '{}': must end with 'h'",
                duration_str
            ))
        })?;

        let duration_hours: f64 = hours_str
            .parse()
            .map_err(|_| SimError::ParseError(format!("Invalid duration hours in '{}'", part)))?;

        segments.push(WorkerSegment {
            workers,
            duration_hours,
        });
    }

    if segments.is_empty() {
        return Err(SimError::ParseError("Empty worker schedule".to_string()));
    }

    Ok(segments)
}

// ---------------------------------------------------------------------------
// Output Types
// ---------------------------------------------------------------------------

/// A single point in the trajectory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryPoint {
    /// Timestamp of this point
    pub timestamp: DateTime<Utc>,

    /// Hours from simulation start
    pub hours_offset: f64,

    /// Per-window utilization percentages
    pub windows: HashMap<String, f64>,

    /// Current promotion multiplier
    pub promo_multiplier: f64,

    /// Current worker count
    pub workers: u32,

    /// Events that occurred at this point (e.g., "window_reset:five_hour")
    pub events: Vec<String>,
}

/// Ceiling breach information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeilingBreach {
    /// Window that breached
    pub window: String,

    /// Timestamp of first breach
    pub timestamp: DateTime<Utc>,

    /// Hours offset when breach occurred
    pub hours_offset: f64,

    /// Utilization at breach time
    pub utilization: f64,

    /// Ceiling that was breached
    pub ceiling: f64,
}

/// Confidence cone for a single window (captured from state at simulation time)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowCone {
    /// Pessimistic exhaustion hours (fast-burn / p25 scenario)
    pub exh_hrs_p25: f64,
    /// Central exhaustion hours (mean burn rate / p50)
    pub exh_hrs_p50: f64,
    /// Optimistic exhaustion hours (slow-burn / p75 scenario)
    pub exh_hrs_p75: f64,
    /// Spread ratio: exh_hrs_p75 / exh_hrs_p25 (1.0 = no spread, higher = wider uncertainty)
    pub cone_ratio: f64,
}

/// Complete trajectory output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// Trajectory data points at the configured resolution
    pub points: Vec<TrajectoryPoint>,

    /// First ceiling breach per window (if any)
    pub breaches: Vec<CeilingBreach>,

    /// Simulation configuration summary
    pub config: TrajectoryConfig,

    /// Confidence cone per window (from state at simulation time)
    pub cone: std::collections::HashMap<String, WindowCone>,
}

/// Configuration summary included in trajectory output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryConfig {
    /// Worker schedule description
    pub workers: String,

    /// Total hours simulated
    pub hours: f64,

    /// Output resolution in minutes
    pub resolution_minutes: i64,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during simulation
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Missing required state: {0}")]
    MissingState(String),
}

pub type Result<T> = std::result::Result<T, SimError>;

// ---------------------------------------------------------------------------
// Simulation Engine
// ---------------------------------------------------------------------------

/// Context for simulation - extracted from governor state
struct SimContext {
    /// Start timestamp
    start: DateTime<Utc>,

    /// Per-window current utilization
    window_utilization: HashMap<String, f64>,

    /// Per-window target ceiling
    window_ceiling: HashMap<String, f64>,

    /// Per-window reset timestamps (when the window resets)
    window_resets_at: HashMap<String, DateTime<Utc>>,

    /// Per-model burn rate (pct per worker per hour)
    model_burn_rate: HashMap<String, f64>,

    /// Fleet-level EMA pct/hr per window (total fleet, not per worker).
    /// Used as fallback when per-model rates are zero.
    fleet_pct_hr_ema: HashMap<String, f64>,

    /// Worker count that was active when fleet_pct_hr_ema was last updated.
    /// Used to convert fleet total rate → per-worker rate.
    baseline_workers: u32,

    /// Active promotions from promotions.json
    promotions: Vec<schedule::Promotion>,
}

impl SimContext {
    /// Extract simulation context from governor state
    fn from_state(state: &GovernorState, promotions: Vec<schedule::Promotion>) -> Result<Self> {
        let mut window_utilization = HashMap::new();
        let mut window_ceiling = HashMap::new();
        let mut window_resets_at = HashMap::new();

        // Extract current utilization and ceilings from capacity forecast
        let forecast = &state.capacity_forecast;

        // Five hour window
        window_utilization.insert(
            "five_hour".to_string(),
            forecast.five_hour.current_utilization,
        );
        window_ceiling.insert("five_hour".to_string(), forecast.five_hour.target_ceiling);

        // Seven day window
        window_utilization.insert(
            "seven_day".to_string(),
            forecast.seven_day.current_utilization,
        );
        window_ceiling.insert("seven_day".to_string(), forecast.seven_day.target_ceiling);

        // Seven day sonnet window
        window_utilization.insert(
            "seven_day_sonnet".to_string(),
            forecast.seven_day_sonnet.current_utilization,
        );
        window_ceiling.insert(
            "seven_day_sonnet".to_string(),
            forecast.seven_day_sonnet.target_ceiling,
        );

        // Extract reset times from usage state
        if !state.usage.five_hour_resets_at.is_empty() {
            if let Ok(ts) = state.usage.five_hour_resets_at.parse::<DateTime<Utc>>() {
                window_resets_at.insert("five_hour".to_string(), ts);
            }
        }
        if !state.usage.sonnet_resets_at.is_empty() {
            if let Ok(ts) = state.usage.sonnet_resets_at.parse::<DateTime<Utc>>() {
                window_resets_at.insert("seven_day_sonnet".to_string(), ts);
            }
        }
        // Seven day window resets at the same time as seven_day_sonnet
        if let Some(ts) = window_resets_at.get("seven_day_sonnet") {
            window_resets_at.insert("seven_day".to_string(), *ts);
        }

        // Extract model burn rates
        let mut model_burn_rate = HashMap::new();
        for (model, rate) in &state.burn_rate.by_model {
            model_burn_rate.insert(model.clone(), rate.pct_per_worker_per_hour);
        }

        // If no burn rates, use a default estimate
        if model_burn_rate.is_empty() {
            // Default: ~1.5 pct/worker/hour for sonnet
            model_burn_rate.insert("default".to_string(), 1.5);
        }

        // Extract fleet EMA rates per window (total fleet pct/hr)
        let ema = &state.burn_rate.fleet_pct_hr_ema;
        let mut fleet_pct_hr_ema = HashMap::new();
        if ema.five_hour > 0.0 {
            fleet_pct_hr_ema.insert("five_hour".to_string(), ema.five_hour);
        }
        if ema.seven_day > 0.0 {
            fleet_pct_hr_ema.insert("seven_day".to_string(), ema.seven_day);
        }
        if ema.seven_day_sonnet > 0.0 {
            fleet_pct_hr_ema.insert("seven_day_sonnet".to_string(), ema.seven_day_sonnet);
        }

        // Baseline worker count: use last fleet aggregate's worker count so we can
        // convert fleet total pct/hr → per-worker rate for the fallback path.
        let baseline_workers = state.last_fleet_aggregate.sonnet_workers.max(1);

        Ok(Self {
            start: state.updated_at,
            window_utilization,
            window_ceiling,
            window_resets_at,
            model_burn_rate,
            fleet_pct_hr_ema,
            baseline_workers,
            promotions,
        })
    }

    /// Get the effective burn rate per worker per hour for a given window.
    ///
    /// Priority:
    /// 1. Per-model pct_per_worker_per_hour (from burn rate estimator)
    /// 2. Fleet EMA for this window divided by baseline_workers (when model rate is zero)
    /// 3. Static fallback: 1.5%/worker/hr
    fn get_burn_rate_for_window(&self, window: &str) -> f64 {
        // Try per-model rate
        let model_rate = self
            .model_burn_rate
            .get("claude-sonnet-4-20250514")
            .or_else(|| self.model_burn_rate.get("claude-sonnet-4-6"))
            .or_else(|| self.model_burn_rate.values().next())
            .copied()
            .unwrap_or(0.0);

        if model_rate > 0.0 {
            return model_rate;
        }

        // Fall back to fleet EMA / baseline_workers for this specific window
        if let Some(&fleet_rate) = self.fleet_pct_hr_ema.get(window) {
            return fleet_rate / self.baseline_workers as f64;
        }

        // Final fallback
        1.5
    }

    /// Get the promotion multiplier at a given time for a specific window using the schedule module
    fn promo_multiplier_at(&self, timestamp: DateTime<Utc>, window: &str) -> f64 {
        schedule::get_multiplier_at(timestamp, &self.promotions, window)
    }
}

/// Run a trajectory simulation
///
/// Walks forward from the current state, applying burn rates, promotion multipliers,
/// window resets, and detecting ceiling breaches.
pub fn simulate(
    state: &GovernorState,
    config: &SimConfig,
    promotions: Vec<schedule::Promotion>,
) -> Result<Trajectory> {
    let ctx = SimContext::from_state(state, promotions)?;

    let total_minutes = (config.hours * 60.0) as i64;
    let resolution_minutes = config.resolution_minutes;

    let mut points = Vec::new();
    let mut breaches = Vec::new();
    let mut breach_detected: HashMap<String, bool> = HashMap::new();

    // Track current window utilization
    let mut current_utilization = ctx.window_utilization.clone();

    // Track if a window reset happened (to detect events)
    let mut last_reset_check: HashMap<String, DateTime<Utc>> = HashMap::new();

    // Simulate minute by minute
    for minute in 0..=total_minutes {
        let hours_offset = minute as f64 / 60.0;
        let current_time = ctx.start + Duration::minutes(minute);

        // Get current worker count
        let workers = config.workers.workers_at(hours_offset);

        // Get base burn rate (same for all windows; promo multiplier is per-window)
        let base_burn_rate = ctx.get_burn_rate_for_window("five_hour"); // pct per worker per hour

        // Compute display multiplier as max across all windows (shows if any promotion is active)
        let display_promo_multiplier = WINDOWS
            .iter()
            .map(|w| ctx.promo_multiplier_at(current_time, w))
            .fold(1.0_f64, f64::max);

        // Track events for this step
        let mut events = Vec::new();

        // Update each window
        for window in &WINDOWS {
            // Check for window reset
            if let Some(&reset_time) = ctx.window_resets_at.get(*window) {
                let last_check = last_reset_check.get(*window).copied().unwrap_or(ctx.start);

                // If we crossed the reset time, utilization drops to 0
                if last_check < reset_time && current_time >= reset_time {
                    current_utilization.insert(window.to_string(), 0.0);
                    events.push(format!("window_reset:{}", window));
                }

                last_reset_check.insert(window.to_string(), current_time);
            }

            // Apply burn rate per minute, adjusted for this window's promo multiplier
            let promo_multiplier = ctx.promo_multiplier_at(current_time, window);
            let burn_per_minute = base_burn_rate * workers as f64 / 60.0 / promo_multiplier;

            // Apply burn rate (clamped to 0-100)
            let current = current_utilization.get(*window).copied().unwrap_or(0.0);
            let new_util = (current + burn_per_minute).clamp(0.0, 100.0);
            current_utilization.insert(window.to_string(), new_util);

            // Check for ceiling breach (only record first breach per window)
            if !breach_detected.get(*window).copied().unwrap_or(false) {
                if let Some(&ceiling) = ctx.window_ceiling.get(*window) {
                    if new_util >= ceiling {
                        breaches.push(CeilingBreach {
                            window: window.to_string(),
                            timestamp: current_time,
                            hours_offset,
                            utilization: new_util,
                            ceiling,
                        });
                        breach_detected.insert(window.to_string(), true);
                        events.push(format!("ceiling_breach:{}", window));
                    }
                }
            }
        }

        // Record point at resolution intervals
        if minute % resolution_minutes == 0 {
            points.push(TrajectoryPoint {
                timestamp: current_time,
                hours_offset,
                windows: current_utilization.clone(),
                promo_multiplier: display_promo_multiplier,
                workers,
                events,
            });
        }
    }

    // Build config summary
    let workers_desc = match &config.workers {
        WorkerSchedule::Fixed(n) => n.to_string(),
        WorkerSchedule::Schedule(segments) => segments
            .iter()
            .map(|s| format!("{}:{}", s.workers, s.duration_hours))
            .collect::<Vec<_>>()
            .join(","),
    };

    // Capture confidence cone from current state's window forecasts
    let mut cone = std::collections::HashMap::new();
    let forecast = &state.capacity_forecast;
    for (key, win) in [
        ("five_hour", &forecast.five_hour),
        ("seven_day", &forecast.seven_day),
        ("seven_day_sonnet", &forecast.seven_day_sonnet),
    ] {
        cone.insert(
            key.to_string(),
            WindowCone {
                exh_hrs_p25: win.exh_hrs_p25,
                exh_hrs_p50: win.exh_hrs_p50,
                exh_hrs_p75: win.exh_hrs_p75,
                cone_ratio: win.cone_ratio,
            },
        );
    }

    Ok(Trajectory {
        points,
        breaches,
        config: TrajectoryConfig {
            workers: workers_desc,
            hours: config.hours,
            resolution_minutes: config.resolution_minutes,
        },
        cone,
    })
}

/// Format exhaustion hours for display — handles infinity / very large values
fn format_exh_hrs(hrs: f64) -> String {
    if hrs.is_infinite() || hrs > 9999.0 {
        ">9999h".to_string()
    } else if hrs <= 0.0 {
        "now".to_string()
    } else if hrs < 1.0 {
        format!("{:.0}m", hrs * 60.0)
    } else if hrs < 24.0 {
        format!("{:.1}h", hrs)
    } else {
        format!("{:.1}d", hrs / 24.0)
    }
}

/// Format trajectory as ASCII table for human consumption
pub fn format_ascii_table(trajectory: &Trajectory) -> String {
    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "Trajectory Simulation ({} workers, {}h, {}min resolution)\n",
        trajectory.config.workers, trajectory.config.hours, trajectory.config.resolution_minutes
    ));
    output.push_str(&"=".repeat(80));
    output.push_str("\n\n");

    // Confidence cone section
    if !trajectory.cone.is_empty() {
        output.push_str("Confidence Cone (starting conditions)\n");
        output.push_str(&"-".repeat(60));
        output.push_str("\n");
        output.push_str(&format!(
            "{:<14} {:>10} {:>10} {:>10} {:>10}\n",
            "Window", "p25(fast)", "p50(mid)", "p75(slow)", "ConeRatio"
        ));
        for (key, label) in [
            ("five_hour", "5h"),
            ("seven_day", "7d"),
            ("seven_day_sonnet", "7d-sonnet"),
        ] {
            if let Some(c) = trajectory.cone.get(key) {
                let ratio = if c.cone_ratio > 0.0 {
                    format!("{:.1}x", c.cone_ratio)
                } else {
                    "—".to_string()
                };
                output.push_str(&format!(
                    "{:<14} {:>10} {:>10} {:>10} {:>10}\n",
                    label,
                    format_exh_hrs(c.exh_hrs_p25),
                    format_exh_hrs(c.exh_hrs_p50),
                    format_exh_hrs(c.exh_hrs_p75),
                    ratio,
                ));
            }
        }
        output.push_str("\n");
    }

    // Column headers
    output.push_str(&format!(
        "{:<20} {:>10} {:>10} {:>10} {:>6} {:>4}  Events\n",
        "Time", "5h%", "7d%", "7ds%", "Promo", "Wrk"
    ));
    output.push_str(&"-".repeat(80));
    output.push_str("\n");

    // Data rows
    for point in &trajectory.points {
        let time_str = point.timestamp.format("%Y-%m-%d %H:%M").to_string();
        let five_h = point.windows.get("five_hour").copied().unwrap_or(0.0);
        let seven_d = point.windows.get("seven_day").copied().unwrap_or(0.0);
        let seven_ds = point
            .windows
            .get("seven_day_sonnet")
            .copied()
            .unwrap_or(0.0);

        let events_str = if point.events.is_empty() {
            String::new()
        } else {
            point.events.join(", ")
        };

        output.push_str(&format!(
            "{:<20} {:>9.1}% {:>9.1}% {:>9.1}% {:>5.1}x {:>4}  {}\n",
            time_str, five_h, seven_d, seven_ds, point.promo_multiplier, point.workers, events_str
        ));
    }

    // Breach summary
    if !trajectory.breaches.is_empty() {
        output.push_str(&"\nCeiling Breaches:\n");
        output.push_str(&"-".repeat(40));
        output.push_str("\n");
        for breach in &trajectory.breaches {
            output.push_str(&format!(
                "  {} at {} ({:.1}h): {:.1}% >= {:.1}%\n",
                breach.window,
                breach.timestamp.format("%Y-%m-%d %H:%M"),
                breach.hours_offset,
                breach.utilization,
                breach.ceiling
            ));
        }
    }

    output
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BurnRateState, CapacityForecast, ScheduleState, UsageState};

    fn make_test_state() -> GovernorState {
        let mut state = GovernorState::default();

        // Set up usage
        state.usage = UsageState {
            sonnet_pct: 50.0,
            all_models_pct: 60.0,
            five_hour_pct: 30.0,
            sonnet_resets_at: "2026-03-21T03:00:00Z".to_string(),
            five_hour_resets_at: "2026-03-20T10:00:00Z".to_string(),
            stale: false,
        };

        // Set up capacity forecast with ceilings
        state.capacity_forecast = CapacityForecast {
            five_hour: crate::state::WindowForecast {
                target_ceiling: 85.0,
                current_utilization: 30.0,
                ..Default::default()
            },
            seven_day: crate::state::WindowForecast {
                target_ceiling: 90.0,
                current_utilization: 50.0,
                ..Default::default()
            },
            seven_day_sonnet: crate::state::WindowForecast {
                target_ceiling: 90.0,
                current_utilization: 50.0,
                ..Default::default()
            },
            ..Default::default()
        };

        // Set up burn rates
        state.burn_rate = BurnRateState {
            by_model: {
                let mut m = HashMap::new();
                m.insert(
                    "claude-sonnet-4-20250514".to_string(),
                    crate::state::ModelBurnRate {
                        pct_per_worker_per_hour: 2.0, // 2% per worker per hour
                        dollars_per_worker_per_hour: 5.0,
                        samples: 10,
                    },
                );
                m
            },
            ..Default::default()
        };

        state.updated_at = "2026-03-20T08:00:00Z".parse().unwrap();

        state
    }

    /// Helper: create a test promotion active March 1 - April 1 2026 with 2x off-peak
    fn test_promo() -> schedule::Promotion {
        schedule::Promotion {
            name: "Test Promo".to_string(),
            start_date: "2026-03-01".to_string(),
            end_date: "2026-04-01".to_string(),
            peak_start_hour_et: 8,
            peak_end_hour_et: 14,
            offpeak_multiplier: 2.0,
            applies_to: vec!["seven_day".to_string(), "seven_day_sonnet".to_string()],
        }
    }

    // --- Worker schedule parsing tests ---

    #[test]
    fn parse_fixed_workers() {
        let config = SimConfig::parse_workers("4").unwrap();
        assert!(matches!(config.workers, WorkerSchedule::Fixed(4)));
    }

    #[test]
    fn parse_schedule_single_segment() {
        let config = SimConfig::parse_workers("4:6h").unwrap();
        match config.workers {
            WorkerSchedule::Schedule(segments) => {
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].workers, 4);
                assert!((segments[0].duration_hours - 6.0).abs() < 0.001);
            }
            _ => panic!("Expected schedule"),
        }
    }

    #[test]
    fn parse_schedule_multiple_segments() {
        let config = SimConfig::parse_workers("4:6h,2:6h").unwrap();
        match config.workers {
            WorkerSchedule::Schedule(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].workers, 4);
                assert_eq!(segments[1].workers, 2);
            }
            _ => panic!("Expected schedule"),
        }
    }

    #[test]
    fn parse_schedule_invalid_format() {
        assert!(SimConfig::parse_workers("invalid").is_err());
        assert!(SimConfig::parse_workers("4:6").is_err()); // missing 'h'
        assert!(SimConfig::parse_workers(":6h").is_err()); // missing worker count
    }

    #[test]
    fn worker_schedule_at_offset() {
        let schedule = WorkerSchedule::Schedule(vec![
            WorkerSegment {
                workers: 4,
                duration_hours: 6.0,
            },
            WorkerSegment {
                workers: 2,
                duration_hours: 6.0,
            },
        ]);

        assert_eq!(schedule.workers_at(0.0), 4);
        assert_eq!(schedule.workers_at(3.0), 4);
        assert_eq!(schedule.workers_at(5.9), 4);
        assert_eq!(schedule.workers_at(6.0), 2);
        assert_eq!(schedule.workers_at(11.9), 2);
        assert_eq!(schedule.workers_at(12.0), 2); // after all segments, use last
    }

    // --- Simulation tests ---

    #[test]
    fn simulate_24h_with_known_burn_rate() {
        let state = make_test_state();
        let config = SimConfig::fixed(1, 24.0);

        let trajectory = simulate(&state, &config, vec![]).unwrap();

        // Should have points at 15-minute intervals over 24h
        // 24h * 4 points/hour + 1 (for t=0) = 97 points
        assert_eq!(trajectory.points.len(), 97);

        // First point should be at start
        assert_eq!(trajectory.points[0].hours_offset, 0.0);

        // Last point should be at 24h
        let last = trajectory.points.last().unwrap();
        assert!((last.hours_offset - 24.0).abs() < 0.01);

        // With 1 worker at 2% per hour, no promo (1x multiplier):
        // seven_day_sonnet resets at 19h (2026-03-21T03:00:00Z)
        // After 19h: 50% + 38% = 88%, then reset to 0%
        // After 5 more hours: 0% + 10% = ~10%
        let final_util = last.windows.get("seven_day_sonnet").copied().unwrap_or(0.0);
        assert!(
            (final_util - 10.0).abs() < 1.0,
            "Expected ~10%, got {}",
            final_util
        );
    }

    #[test]
    fn simulate_window_reset_drops_utilization() {
        // Set up state with a window reset in the near future
        let mut state = make_test_state();

        // Set five_hour to reset in 2 hours from start
        state.usage.five_hour_resets_at = "2026-03-20T10:00:00Z".to_string();
        state.capacity_forecast.five_hour.current_utilization = 60.0;
        state.updated_at = "2026-03-20T08:00:00Z".parse().unwrap();

        let config = SimConfig::fixed(1, 4.0); // 4 hours
        let trajectory = simulate(&state, &config, vec![]).unwrap();

        // Find the point just before reset (2h = 120 minutes)
        let before_reset = trajectory
            .points
            .iter()
            .find(|p| p.hours_offset <= 1.75 && p.hours_offset >= 1.5)
            .expect("Should have point before reset");

        // Find point after reset (2.25h)
        let after_reset = trajectory
            .points
            .iter()
            .find(|p| (p.hours_offset - 2.25).abs() < 0.01)
            .expect("Should have point after reset");

        let before_util = before_reset
            .windows
            .get("five_hour")
            .copied()
            .unwrap_or(0.0);
        let after_util = after_reset.windows.get("five_hour").copied().unwrap_or(0.0);

        // Utilization should drop significantly at reset
        assert!(
            after_util < before_util,
            "Utilization should drop at reset: before={}, after={}",
            before_util,
            after_util
        );

        // Check for reset event
        let reset_point = trajectory.points.iter().find(|p| {
            p.events
                .iter()
                .any(|e| e.contains("window_reset:five_hour"))
        });
        assert!(reset_point.is_some(), "Should have window_reset event");
    }

    #[test]
    fn simulate_promotion_transition() {
        // Test that promotion multiplier transitions from 2x to 1x at 08:00 ET peak boundary
        let mut state = make_test_state();

        // Start at 06:00 ET on Friday March 20, 2026 (EDT = UTC-4)
        // 06:00 ET = 10:00 UTC
        state.updated_at = "2026-03-20T10:00:00Z".parse().unwrap();
        state.schedule = ScheduleState {
            is_peak_hour: false,
            is_promo_active: true,
            promo_multiplier: 2.0,
            ..Default::default()
        };

        let config = SimConfig::fixed(1, 4.0); // 4 hours: 06:00-10:00 ET
        let promotions = vec![test_promo()];

        let trajectory = simulate(&state, &config, promotions).unwrap();

        // Find point before 08:00 ET (12:00 UTC) — at 1.75h offset = 07:45 ET
        let before_peak = trajectory
            .points
            .iter()
            .find(|p| (p.hours_offset - 1.75).abs() < 0.01)
            .expect("Should have point before peak");

        // Find point after 08:00 ET — at 2.25h offset = 08:15 ET
        let after_peak = trajectory
            .points
            .iter()
            .find(|p| (p.hours_offset - 2.25).abs() < 0.01)
            .expect("Should have point after peak");

        // Before 08:00 ET: off-peak with promo -> 2x multiplier
        assert!(
            (before_peak.promo_multiplier - 2.0).abs() < 0.01,
            "Expected 2.0 before peak, got {}",
            before_peak.promo_multiplier
        );

        // After 08:00 ET: peak hours -> 1x multiplier
        assert!(
            (after_peak.promo_multiplier - 1.0).abs() < 0.01,
            "Expected 1.0 after peak, got {}",
            after_peak.promo_multiplier
        );
    }

    #[test]
    fn simulate_ceiling_breach_detection() {
        let mut state = make_test_state();

        // Set up to breach quickly: start at 84% with ceiling at 85%
        state.capacity_forecast.seven_day_sonnet.current_utilization = 84.0;
        state.capacity_forecast.seven_day_sonnet.target_ceiling = 85.0;

        // Use 4 workers to burn faster
        let config = SimConfig::fixed(4, 1.0);
        let trajectory = simulate(&state, &config, vec![]).unwrap();

        // Should detect breach
        assert!(
            !trajectory.breaches.is_empty(),
            "Should have ceiling breach"
        );

        let breach = trajectory
            .breaches
            .iter()
            .find(|b| b.window == "seven_day_sonnet")
            .expect("Should have seven_day_sonnet breach");

        assert_eq!(breach.window, "seven_day_sonnet");
        assert!(breach.utilization >= 85.0);
        assert!(breach.hours_offset > 0.0);
    }

    #[test]
    fn simulate_variable_worker_schedule() {
        let state = make_test_state();

        // Schedule: 4 workers for 2h, then 1 worker for 2h
        let config = SimConfig {
            workers: WorkerSchedule::Schedule(vec![
                WorkerSegment {
                    workers: 4,
                    duration_hours: 2.0,
                },
                WorkerSegment {
                    workers: 1,
                    duration_hours: 2.0,
                },
            ]),
            hours: 4.0,
            resolution_minutes: 15,
        };

        let trajectory = simulate(&state, &config, vec![]).unwrap();
        let at_1h = trajectory
            .points
            .iter()
            .find(|p| (p.hours_offset - 1.0).abs() < 0.01)
            .unwrap();
        let at_3h = trajectory
            .points
            .iter()
            .find(|p| (p.hours_offset - 3.0).abs() < 0.01)
            .unwrap();

        assert_eq!(at_1h.workers, 4, "Should have 4 workers at 1h");
        assert_eq!(at_3h.workers, 1, "Should have 1 worker at 3h");
    }

    #[test]
    fn json_output_schema() {
        let state = make_test_state();
        let config = SimConfig::fixed(1, 1.0);

        let trajectory = simulate(&state, &config, vec![]).unwrap();
        let json = serde_json::to_value(&trajectory).unwrap();

        assert!(json.is_object());
        assert!(json.get("points").is_some());
        assert!(json.get("breaches").is_some());
        assert!(json.get("config").is_some());

        // Verify point structure
        let points = json.get("points").unwrap().as_array().unwrap();
        assert!(!points.is_empty());

        let first_point = &points[0];
        assert!(first_point.get("timestamp").is_some());
        assert!(first_point.get("hours_offset").is_some());
        assert!(first_point.get("windows").is_some());
        assert!(first_point.get("promo_multiplier").is_some());
        assert!(first_point.get("workers").is_some());
        assert!(first_point.get("events").is_some());

        // Verify config structure
        let cfg = json.get("config").unwrap();
        assert!(cfg.get("workers").is_some());
        assert!(cfg.get("hours").is_some());
        assert!(cfg.get("resolution_minutes").is_some());
    }

    #[test]
    fn ascii_table_format() {
        let state = make_test_state();
        let config = SimConfig::fixed(2, 2.0);

        let trajectory = simulate(&state, &config, vec![]).unwrap();
        let table = format_ascii_table(&trajectory);
        assert!(table.contains("5h%"));
        assert!(table.contains("7d%"));
        assert!(table.contains("7ds%"));

        // Should contain time stamps
        assert!(table.contains("2026-03-20"));

        // Should contain config summary
        assert!(table.contains("2 workers"));
        assert!(table.contains("2h"));
    }

    #[test]
    fn simulation_clamps_utilization_to_100() {
        let mut state = make_test_state();

        // Start high and burn hard
        state.capacity_forecast.seven_day_sonnet.current_utilization = 95.0;

        // 10 workers for 10 hours would exceed 100% without clamping
        let config = SimConfig::fixed(10, 10.0);
        let trajectory = simulate(&state, &config, vec![]).unwrap();
        for point in &trajectory.points {
            for (window, &util) in &point.windows {
                assert!(
                    util <= 100.0,
                    "{} utilization {} exceeds 100%",
                    window,
                    util
                );
            }
        }
    }
}
