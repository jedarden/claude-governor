//! Alert Condition Checker and Episode Lifecycle
//!
//! This module handles:
//! - Alert condition evaluation from governor state
//! - Stateful, episode-based deduplication: one bead per condition episode
//! - Alert severity classification
//! - Firing alerts via configured command (default: bf create --type human)
//! - Logging alerts to governor.log
//!
//! # Episode lifecycle
//!
//! An *episode* is one continuous stretch during which a given condition (keyed by
//! alert type plus an optional scope such as the window or agent name) is true. The
//! lifecycle is:
//!
//! 1. **Open** — the first cycle the condition is true, one bead is created and its id
//!    is recorded in `governor-state.json` under `open_alert_beads`.
//! 2. **Recur** — while the condition stays true, no new bead is created. The existing
//!    bead is refreshed with current numbers at most once per `cooldown_minutes`.
//! 3. **Resolve** — the first cycle the condition is no longer reported, the bead is
//!    auto-closed and the entry is removed.
//!
//! This replaces the previous cooldown-only dedup, under which a condition that stayed
//! true simply minted a brand-new bead every time the cooldown elapsed. The cooldown is
//! retained only as an anti-flap floor on *opening* a new episode, so a condition that
//! rapidly toggles cannot produce a bead per toggle.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use crate::burn_rate::MIN_VALIDATION_SAMPLES;
use crate::config::AlertConfig;
use crate::state::{AlertCooldown, CapacityForecast, GovernorState, OpenAlertBead};

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
    /// Subscription-flagged agent using sdk-cli billing instead of cli
    SubscriptionBillingDrift,
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
            AlertType::SubscriptionBillingDrift => write!(f, "subscription_billing_drift"),
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
    /// What this condition is about, when a single alert type can describe more than one
    /// subject: the window name for cutoff alerts, the agent name for billing drift.
    /// Two conditions of the same type but different scope are separate episodes and get
    /// separate beads; `None` means the type itself is the whole identity.
    #[serde(default)]
    pub scope: Option<String>,
}

impl AlertCondition {
    /// Construct a condition whose identity is its alert type alone.
    pub fn new(
        alert_type: AlertType,
        message: String,
        severity: AlertSeverity,
        detected_at: DateTime<Utc>,
    ) -> Self {
        Self {
            alert_type,
            message,
            severity,
            detected_at,
            scope: None,
        }
    }

    /// Attach a scope, making this condition a distinct episode from other conditions of
    /// the same type with a different scope.
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    /// The key identifying this condition's episode: `alert_type` or `alert_type:scope`.
    ///
    /// This is the key under which the open bead and the anti-flap cooldown are stored.
    pub fn episode_key(&self) -> String {
        episode_key(self.alert_type, self.scope.as_deref())
    }
}

/// Build an episode key from an alert type and optional scope.
pub fn episode_key(alert_type: AlertType, scope: Option<&str>) -> String {
    match scope {
        Some(s) => format!("{}:{}", alert_type, s),
        None => alert_type.to_string(),
    }
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
        ("weekly_scoped", &forecast.weekly_scoped),
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

/// Check whether a *new* episode may open for the given episode key.
///
/// This is the anti-flap floor, not a repeat interval: it is consulted only when no
/// episode is currently open for the key. Returns true if:
/// - No previous episode opened for this key, OR
/// - The cooldown period has elapsed since the last episode opened
pub fn should_open_episode(
    key: &str,
    cooldown: &AlertCooldown,
    now: DateTime<Utc>,
    cooldown_minutes: i64,
) -> bool {
    match cooldown.get_last_fired(key) {
        None => true, // Never fired before
        Some(last_fired) => {
            let elapsed = (now - last_fired).num_minutes();
            elapsed >= cooldown_minutes
        }
    }
}

/// Check if an alert should be fired based on cooldown, keyed by alert type alone.
///
/// Convenience wrapper over [`should_open_episode`] for unscoped alert types.
pub fn should_fire(
    alert_type: AlertType,
    cooldown: &AlertCooldown,
    now: DateTime<Utc>,
    cooldown_minutes: i64,
) -> bool {
    should_open_episode(&alert_type.to_string(), cooldown, now, cooldown_minutes)
}

/// Record that an episode opened for the given key.
pub fn record_episode_opened(cooldown: &mut AlertCooldown, key: &str, now: DateTime<Utc>) {
    cooldown.record_fired(key, now);
}

/// Update cooldown state after firing an alert, keyed by alert type alone.
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
/// 3. Executes the configured command (default: bf create --type human "...")
/// 4. Logs the alert to governor.log (with log rotation if config provided)
///
/// Returns `Ok(Some(bead_id))` when a bead was created and its id could be parsed from
/// the command's output, `Ok(None)` when no bead was created (alerts disabled, severity
/// below threshold, `auto_bead` off) or the id could not be determined, and `Err` when
/// the alert could not be fired at all.
///
/// Callers should not invoke this per cycle for a condition that is already open — see
/// [`process_alert_episodes`], which calls it exactly once per condition episode.
///
/// The optional (log_max_bytes, log_backup_count) tuple enables log rotation.
/// If None, logs are written without rotation (legacy behavior).
pub fn fire_alert(
    alert: &AlertCondition,
    config: &AlertConfig,
    log_rotation_config: Option<(u64, u32)>,
) -> Result<Option<String>, String> {
    // Check if alerts are enabled
    if !config.enabled {
        log::debug!("[alert] alerts disabled, skipping {}", alert.alert_type);
        return Ok(None);
    }

    // Check severity threshold
    if !meets_severity_threshold(alert.severity, &config.min_severity) {
        log::debug!(
            "[alert] severity {:?} below threshold '{}', skipping {}",
            alert.severity,
            config.min_severity,
            alert.alert_type
        );
        return Ok(None);
    }

    log::info!(
        "[alert] firing [{}] {}: {}",
        alert.severity,
        alert.alert_type,
        alert.message
    );

    // Log to alert log file regardless of auto_bead setting
    if let Err(e) = log_alert_to_file(alert, log_rotation_config) {
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
        return Ok(None);
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
    let mut bead_id = None;
    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                bead_id = parse_bead_id(&stdout);
                match &bead_id {
                    Some(id) => log::info!("[alert] created bead {} for {}", id, alert.alert_type),
                    None => log::warn!(
                        "[alert] command succeeded but no bead id found in output — this episode's bead cannot be refreshed or auto-closed"
                    ),
                }
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
    if let Err(e) = log_alert_to_file(alert, log_rotation_config) {
        log::warn!("[alert] failed to write to governor.log: {}", e);
    }

    Ok(bead_id)
}

/// Extract a bead id from an alert-creation command's stdout.
///
/// Handles the two shapes the default `bf create` emits:
/// - `--json` output, where the id lives at `.data.id` (envelope) or `.id`
/// - plain output, which is the bare id, possibly prefixed by a human-readable phrase
///
/// Returns None when nothing that looks like a bead id is present, in which case the
/// episode is tracked without a bead to refresh or close.
fn parse_bead_id(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }

    // JSON output: {"version":1,"kind":"create","data":{"id":"bf-2sf9o",...}} or {"id":"..."}
    for line in trimmed.lines().rev() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            let id = value
                .get("data")
                .and_then(|d| d.get("id"))
                .or_else(|| value.get("id"))
                .and_then(|v| v.as_str());
            if let Some(id) = id {
                return Some(id.to_string());
            }
        }
    }

    // Plain output: scan back-to-front for the last token shaped like a bead id.
    for line in trimmed.lines().rev() {
        if let Some(token) = line
            .split_whitespace()
            .rev()
            .find(|t| looks_like_bead_id(t.trim_matches(|c: char| c == '.' || c == ',')))
        {
            return Some(
                token
                    .trim_matches(|c: char| c == '.' || c == ',')
                    .to_string(),
            );
        }
    }

    None
}

/// Whether a token has the shape of a bead id: `<prefix>-<suffix>`, where the prefix is
/// alphanumeric and the suffix is at least three alphanumeric characters (e.g. `bf-5k6yv`,
/// `docs-878a`).
fn looks_like_bead_id(token: &str) -> bool {
    let Some((prefix, suffix)) = token.split_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_alphanumeric())
        && suffix.len() >= 3
        && suffix.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Close the bead tracking an alert episode whose condition has cleared.
///
/// Invokes `<close_command...> <bead_id> --reason <reason>`. Errors are reported to the
/// caller but are not fatal: a bead that was already closed by hand, or a workspace that
/// has moved, must not wedge the governor cycle.
pub fn close_alert_bead(bead_id: &str, config: &AlertConfig, reason: &str) -> Result<(), String> {
    if config.close_command.is_empty() {
        return Err("no alert close command configured".to_string());
    }

    let mut cmd = Command::new(&config.close_command[0]);
    if config.close_command.len() > 1 {
        cmd.args(&config.close_command[1..]);
    }
    cmd.arg(bead_id).arg("--reason").arg(reason);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            log::info!("[alert] closed bead {}: {}", bead_id, reason);
            Ok(())
        }
        Ok(output) => Err(format!(
            "close command failed for {}: {}",
            bead_id,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) => Err(format!(
            "could not run close command for {}: {}",
            bead_id, e
        )),
    }
}

/// Refresh an open episode's bead with the condition's current numbers.
///
/// Invokes `<update_command...> <bead_id> --notes <notes>`. Used instead of creating a
/// second bead when a condition is still true on a later cycle.
pub fn refresh_alert_bead(bead_id: &str, config: &AlertConfig, notes: &str) -> Result<(), String> {
    if config.update_command.is_empty() {
        return Err("no alert update command configured".to_string());
    }

    let mut cmd = Command::new(&config.update_command[0]);
    if config.update_command.len() > 1 {
        cmd.args(&config.update_command[1..]);
    }
    cmd.arg(bead_id).arg("--notes").arg(notes);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            log::debug!("[alert] refreshed bead {}", bead_id);
            Ok(())
        }
        Ok(output) => Err(format!(
            "update command failed for {}: {}",
            bead_id,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) => Err(format!(
            "could not run update command for {}: {}",
            bead_id, e
        )),
    }
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
///
/// If log_rotation_config is Some((max_bytes, backup_count)), performs log rotation
/// before writing if the current log file exceeds max_bytes.
fn log_alert_to_file(
    alert: &AlertCondition,
    log_rotation_config: Option<(u64, u32)>,
) -> std::io::Result<()> {
    let path = default_alert_log_path();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Perform log rotation if config is provided
    if let Some((max_bytes, backup_count)) = log_rotation_config {
        // Check if rotation is needed
        if let Ok(metadata) = std::fs::metadata(&path) {
            if metadata.len() >= max_bytes {
                // Rotate logs
                rotate_log_file(&path, backup_count)?;
            }
        }
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

/// Rotate log files at the given path.
///
/// Rotation scheme:
/// - .backup_count is deleted
/// - .backup_count-1 -> .backup_count
/// - ...
/// - .1 -> .2
/// - original -> .1
/// - New empty file is created
fn rotate_log_file(path: &std::path::PathBuf, backup_count: u32) -> std::io::Result<()> {
    log::warn!(
        "[alert] Log file size exceeds limit, rotating (keeping {} backup(s))",
        backup_count
    );

    // Perform rotation starting from the highest backup number down to 1
    for i in (1..backup_count).rev() {
        let old_file = path.with_extension(&format!("log.{}", i));
        let new_file = path.with_extension(&format!("log.{}", i + 1));

        if old_file.exists() {
            std::fs::rename(&old_file, &new_file)?;
        }
    }

    // Rename current log to .1
    let backup_1 = path.with_extension("log.1");
    if path.exists() {
        std::fs::rename(path, &backup_1)?;
    }

    // Create new empty log file
    std::fs::File::create(path)?;

    // Clean up oldest backup if we have more than backup_count
    let oldest_backup = path.with_extension(&format!("log.{}", backup_count + 1));
    if oldest_backup.exists() {
        std::fs::remove_file(&oldest_backup)?;
    }

    Ok(())
}

/// An episode that resolved this cycle because its condition is no longer true.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEpisode {
    /// The episode key that resolved
    pub key: String,
    /// The bead that was tracking it, if one was created
    pub bead_id: Option<String>,
    /// How long the condition was continuously true, in hours
    pub duration_hours: f64,
    /// How many cycles observed the condition
    pub observations: u32,
    /// Whether the close command ran successfully. False when there was no bead to close
    /// or the close command failed.
    pub closed: bool,
}

/// What the episode lifecycle did this cycle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpisodeOutcome {
    /// Episode keys that opened this cycle — one bead each
    pub opened: Vec<String>,
    /// Conditions that were still true but already had an open episode, so no bead was created
    pub suppressed: usize,
    /// Episodes whose condition cleared this cycle
    pub resolved: Vec<ResolvedEpisode>,
}

/// Drive the alert-episode lifecycle for one governor cycle.
///
/// For each condition in `conditions`:
/// - if no episode is open for its key, open one (subject to the anti-flap cooldown),
///   firing the alert and recording the created bead id in `state.open_alert_beads`;
/// - if an episode is already open, do **not** create another bead — record the sighting
///   and refresh the existing bead's notes at most once per `cooldown_minutes`.
///
/// Then, for every open episode whose condition is absent from `conditions`, close its
/// bead and drop the entry. The cooldown timestamp is deliberately left in place on
/// resolution so a flapping condition cannot open a new episode immediately.
///
/// Returns a summary of what happened; callers use it for logging and telemetry.
pub fn process_alert_episodes(
    state: &mut GovernorState,
    config: &AlertConfig,
    conditions: &[AlertCondition],
    now: DateTime<Utc>,
    log_rotation_config: Option<(u64, u32)>,
) -> EpisodeOutcome {
    let mut outcome = EpisodeOutcome::default();

    let active_keys: std::collections::HashSet<String> =
        conditions.iter().map(|c| c.episode_key()).collect();

    // --- Resolve episodes whose condition is no longer true ---
    let cleared: Vec<String> = state
        .open_alert_beads
        .keys()
        .filter(|k| !active_keys.contains(*k))
        .cloned()
        .collect();

    for key in cleared {
        let Some(episode) = state.open_alert_beads.remove(&key) else {
            continue;
        };
        let duration_hours = (now - episode.opened_at).num_seconds() as f64 / 3600.0;
        let mut closed = false;

        if let Some(bead_id) = &episode.bead_id {
            let reason = format!(
                "Condition cleared: {} was continuously true for {:.1}h across {} governor cycles, and is no longer detected. Auto-closed by claude-governor.",
                key, duration_hours, episode.observations
            );
            match close_alert_bead(bead_id, config, &reason) {
                Ok(()) => closed = true,
                Err(e) => log::warn!("[alert] could not auto-close bead for {}: {}", key, e),
            }
        }

        log::info!(
            "[alert] episode resolved: {} after {:.1}h ({} observations){}",
            key,
            duration_hours,
            episode.observations,
            match &episode.bead_id {
                Some(id) if closed => format!(", closed bead {}", id),
                Some(id) => format!(", bead {} left open", id),
                None => String::new(),
            }
        );

        outcome.resolved.push(ResolvedEpisode {
            key,
            bead_id: episode.bead_id,
            duration_hours,
            observations: episode.observations,
            closed,
        });
    }

    // --- Open new episodes / record recurrences of open ones ---
    for alert in conditions {
        let key = alert.episode_key();

        if let Some(episode) = state.open_alert_beads.get_mut(&key) {
            // Already tracked: this is the same episode, not a new incident.
            episode.last_seen = now;
            episode.observations = episode.observations.saturating_add(1);
            episode.last_message = alert.message.clone();
            outcome.suppressed += 1;

            let last_touch = episode.last_refreshed_at.unwrap_or(episode.opened_at);
            let refresh_due = (now - last_touch).num_minutes() >= config.cooldown_minutes;

            if refresh_due {
                if let Some(bead_id) = episode.bead_id.clone() {
                    let notes = format!(
                        "Still active as of {}. Open for {:.1}h across {} governor cycles.\nLatest: [{}] {}",
                        now.to_rfc3339(),
                        (now - episode.opened_at).num_seconds() as f64 / 3600.0,
                        episode.observations,
                        alert.severity,
                        alert.message,
                    );
                    if let Err(e) = refresh_alert_bead(&bead_id, config, &notes) {
                        log::warn!("[alert] could not refresh bead for {}: {}", key, e);
                    }
                }
                episode.last_refreshed_at = Some(now);
            }

            log::debug!(
                "[alert] {} still active (episode open since {}, {} observations) — no new bead",
                key,
                episode.opened_at.to_rfc3339(),
                episode.observations
            );
            continue;
        }

        // New episode. The cooldown is an anti-flap floor on opening, not a repeat interval.
        if !should_open_episode(&key, &state.alert_cooldown, now, config.cooldown_minutes) {
            log::debug!(
                "[alert] {} re-triggered within the anti-flap window — not opening a new episode",
                key
            );
            continue;
        }

        match fire_alert(alert, config, log_rotation_config) {
            Ok(bead_id) => {
                record_episode_opened(&mut state.alert_cooldown, &key, now);
                state.open_alert_beads.insert(
                    key.clone(),
                    OpenAlertBead {
                        bead_id,
                        alert_type: alert.alert_type.to_string(),
                        scope: alert.scope.clone(),
                        opened_at: now,
                        last_seen: now,
                        observations: 1,
                        last_message: alert.message.clone(),
                        last_refreshed_at: None,
                    },
                );
                outcome.opened.push(key);
            }
            Err(e) => log::warn!("[alert] alert fire failed for {}: {}", key, e),
        }
    }

    outcome
}

/// Process all pending alerts through the episode lifecycle.
///
/// Convenience wrapper that evaluates conditions and runs [`process_alert_episodes`].
/// Returns the number of *new episodes* opened — not the number of active conditions,
/// which may be larger when conditions from earlier cycles are still true.
pub fn process_alerts(
    state: &mut GovernorState,
    config: &AlertConfig,
    now: DateTime<Utc>,
    agents: &std::collections::HashMap<String, crate::config::AgentConfig>,
) -> usize {
    let conditions = check_alert_conditions(state, now, agents);
    process_alert_episodes(state, config, &conditions, now, None)
        .opened
        .len()
}

/// Check for subscription billing drift: subscription agents using sdk-cli instead of cli
///
/// This detects when a subscription-flagged agent (subscription: true) has workers
/// using sdk-cli billing instead of the expected cli billing. This is a P1 operational
/// problem because workers are burning SDK credits instead of free subscription quota.
///
/// Detection logic:
/// 1. For each subscription-flagged agent
/// 2. Read heartbeat files to identify active worker sessions
/// 3. Check if those sessions have recent JSONL files with sdk-cli entrypoint
/// 4. Fire alert if any subscription agent has sdk-cli sessions
fn check_subscription_billing_drift(
    _state: &GovernorState,
    agents: &std::collections::HashMap<String, crate::config::AgentConfig>,
    now: DateTime<Utc>,
    alerts: &mut Vec<AlertCondition>,
) {
    use std::fs;

    for (agent_name, agent_config) in agents {
        // Only check subscription-flagged agents
        if !agent_config.subscription {
            continue;
        }

        // Get heartbeat directory path
        let heartbeat_dir = agent_config.heartbeat_dir_expanded();

        // Check if heartbeat directory exists
        if !heartbeat_dir.exists() {
            continue;
        }

        // Read heartbeat files to find active worker sessions
        let mut sdk_cli_sessions = Vec::new();

        if let Ok(entries) = fs::read_dir(&heartbeat_dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }

                // Read heartbeat JSON to get session ID
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(heartbeat) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(session_id) = heartbeat.get("session").and_then(|v| v.as_str())
                        {
                            // Check if this session has sdk-cli entrypoint
                            if let Some(entrypoint) = get_session_entrypoint(session_id) {
                                if entrypoint == "sdk-cli" {
                                    sdk_cli_sessions.push(session_id.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fire alert if any subscription agent workers are using sdk-cli
        if !sdk_cli_sessions.is_empty() {
            let msg = format!(
                "Agent {} workers using sdk-cli billing (expected cli). claude-print may be misconfigured or missing. Check: claude-print --check. Affected sessions: {}",
                agent_name,
                sdk_cli_sessions.join(", ")
            );
            alerts.push(
                AlertCondition::new(
                    AlertType::SubscriptionBillingDrift,
                    msg,
                    AlertSeverity::Critical,
                    now,
                )
                .with_scope(agent_name.clone()),
            );
        }
    }
}

/// Get the entrypoint for a session by reading its JSONL file
///
/// Returns the entrypoint type ("cli" or "sdk-cli") if found, None otherwise
fn get_session_entrypoint(session_id: &str) -> Option<String> {
    use std::fs;

    // Construct path to JSONL file in ~/.claude/projects/**/<session_id>.jsonl
    let home_dir = dirs::home_dir()?;
    let projects_dir = home_dir.join(".claude").join("projects");

    // Search for the JSONL file in subdirectories
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.filter_map(Result::ok) {
            if entry.path().is_dir() {
                let jsonl_path = entry.path().join(format!("{}.jsonl", session_id));
                if jsonl_path.exists() {
                    // Read the JSONL file to find entrypoint
                    if let Ok(content) = fs::read_to_string(&jsonl_path) {
                        // Parse lines to find entrypoint field
                        for line in content.lines() {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                                if let Some(entrypoint) =
                                    json.get("entrypoint").and_then(|v| v.as_str())
                                {
                                    return Some(entrypoint.to_string());
                                }
                                // Also check for promptSource field (another indicator of sdk-cli)
                                if let Some(prompt_source) =
                                    json.get("promptSource").and_then(|v| v.as_str())
                                {
                                    if prompt_source == "sdk" {
                                        return Some("sdk-cli".to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Check all alert conditions from governor state
///
/// Returns a list of all currently active alert conditions (before cooldown filtering).
/// Callers should use `should_fire` to filter based on cooldown state.
pub fn check_alert_conditions(
    state: &GovernorState,
    now: DateTime<Utc>,
    agents: &std::collections::HashMap<String, crate::config::AgentConfig>,
) -> Vec<AlertCondition> {
    let mut alerts = Vec::new();
    let forecast = &state.capacity_forecast;

    // Check CutoffImminent: any window with cutoff_risk=1 and margin_hrs < -2
    check_cutoff_imminent(forecast, now, &mut alerts);

    // Check SonnetCutoffRisk: weekly_scoped cutoff_risk=1
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
        alerts.push(AlertCondition::new(
            AlertType::PromotionNotApplying,
            msg,
            AlertSeverity::Warning,
            now,
        ));
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
            alerts.push(AlertCondition::new(
                AlertType::PromotionRatioAnomaly,
                msg,
                AlertSeverity::Warning,
                now,
            ));
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
        alerts.push(AlertCondition::new(
            AlertType::CollectorOffline,
            msg,
            AlertSeverity::Warning,
            now,
        ));
    }

    // Check TokenRefreshFailing: poller detected auth issues
    if state.token_refresh_failing {
        let msg = "OAuth token refresh failing — Claude Code sessions may be unable to make API calls. Run: claude login".to_string();
        alerts.push(AlertCondition::new(
            AlertType::TokenRefreshFailing,
            msg,
            AlertSeverity::Critical,
            now,
        ));
    }

    // Check SubscriptionBillingDrift: subscription agents using sdk-cli instead of cli
    check_subscription_billing_drift(state, agents, now, &mut alerts);

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
        ("weekly_scoped", &forecast.weekly_scoped),
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
            alerts.push(
                AlertCondition::new(AlertType::CutoffImminent, msg, AlertSeverity::Critical, now)
                    .with_scope(name),
            );
            return;
        }
    }
}

/// Check for SonnetCutoffRisk: weekly_scoped cutoff_risk=1 AND hard_limit_margin_hrs < 0 AND utilization >= 85%
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
    let win = &forecast.weekly_scoped;

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
        alerts.push(
            AlertCondition::new(
                AlertType::SonnetCutoffRisk,
                msg,
                AlertSeverity::Warning,
                now,
            )
            .with_scope("weekly_scoped"),
        );
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
        alerts.push(
            AlertCondition::new(
                AlertType::SessionCutoffRisk,
                msg,
                AlertSeverity::Warning,
                now,
            )
            .with_scope("five_hour"),
        );
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
        Some(AlertCondition::new(
            AlertType::LowCacheEfficiency,
            msg,
            AlertSeverity::Warning,
            now,
        ))
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
        ("weekly_scoped", &forecast.weekly_scoped),
    ];

    let all_abundant = windows
        .iter()
        .all(|(_, win)| win.hours_remaining > 0.0 && win.margin_hrs > win.hours_remaining * 0.5);

    if all_abundant {
        let msg = "All windows have abundant capacity: safe to increase worker count".to_string();
        alerts.push(AlertCondition::new(
            AlertType::Underutilization,
            msg,
            AlertSeverity::Info,
            now,
        ));
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
            open_alert_beads: Default::default(),
            token_refresh_failing: false,
            low_cache_eff_consecutive: 0,
            alert_fp_telemetry: AlertFpTelemetry::default(),
            pending_predictions: Default::default(),
            current_api_snapshot: Default::default(),
            previous_api_snapshot: Default::default(),
            p5h_delta: None,
            p7d_delta: None,
            p7ds_delta: None,
            baseline_burn_rates: HashMap::new(),
            consecutive_absent_polls: HashMap::new(),
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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

        let imminent = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::CutoffImminent);
        assert!(
            imminent.is_some(),
            "Should have CutoffImminent alert at 97% util with negative hard limit margin"
        );
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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window_with_util_and_margin(96.0, true, -4.2, 5.0),
            binding_window: "weekly_scoped".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "seven_day".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window_with_util_and_margin(86.0, true, -16.2, 26.2),
            binding_window: "weekly_scoped".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window_with_util_and_margin(96.0, true, -26.2, 27.0),
            binding_window: "weekly_scoped".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(true, 84.0, 87.7), // cutoff_risk=1 BUT margin=84h (safe!)
            binding_window: "weekly_scoped".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window_with_util_and_margin(40.0, true, -108.0, 112.0), // cutoff_risk=1, util=40% < 50%
            binding_window: "weekly_scoped".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 20.0, 30.0),
            binding_window: "weekly_scoped".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 20.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
        // hard_limit_margin_hrs = 3.0/5.0 - 30.0 = -29.4 for weekly_scoped
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(97.0, true, -1.4, 2.0),
            seven_day: make_window(false, 10.0, 30.0),
            weekly_scoped: make_window_with_util_and_margin(97.0, true, -29.4, 30.0),
            binding_window: "weekly_scoped".to_string(),
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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let state = make_state_with_forecast(forecast);

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

        let brake = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::EmergencyBrakeActivated);
        assert!(
            brake.is_none(),
            "EmergencyBrakeActivated should NOT create alert beads (100% FP rate)"
        );
    }

    #[test]
    fn token_refresh_failing_triggers() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        state.token_refresh_failing = true;

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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

        let alerts = check_alert_conditions(&state, base_now(), &std::collections::HashMap::new());

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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(55.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
            weekly_scoped: make_window_with_util(45.0, 100.0, false),
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
        let alert = AlertCondition::new(
            AlertType::CutoffImminent,
            "test".to_string(),
            AlertSeverity::Critical,
            base_now(),
        );

        let config = AlertConfig {
            enabled: false,
            ..AlertConfig::default()
        };

        let result = fire_alert(&alert, &config, None);
        assert!(result.is_ok());
    }

    #[test]
    fn fire_alert_below_severity_skips() {
        let alert = AlertCondition::new(
            AlertType::Underutilization,
            "test".to_string(),
            AlertSeverity::Info,
            base_now(),
        );

        let config = AlertConfig {
            min_severity: "critical".to_string(),
            ..AlertConfig::default()
        };

        let result = fire_alert(&alert, &config, None);
        assert!(result.is_ok());
    }

    #[test]
    fn fire_alert_empty_command_returns_error() {
        let alert = AlertCondition::new(
            AlertType::CutoffImminent,
            "test".to_string(),
            AlertSeverity::Critical,
            base_now(),
        );

        let config = AlertConfig {
            command: vec![],
            auto_bead: true, // must be true to reach the empty-command check
            ..AlertConfig::default()
        };

        let result = fire_alert(&alert, &config, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no alert command"));
    }

    #[test]
    fn log_alert_to_file_creates_file() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        // Override the log path by using a temp file directly
        let log_path = temp_dir.path().join("governor.log");

        let alert = AlertCondition::new(
            AlertType::CutoffImminent,
            "Test alert message".to_string(),
            AlertSeverity::Critical,
            base_now(),
        );

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
            weekly_scoped: make_window(false, 5.0, 30.0),
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

        let fired = process_alerts(
            &mut state,
            &config,
            base_now(),
            &std::collections::HashMap::new(),
        );
        assert!(fired >= 1, "Should have opened at least one episode");

        // The anti-flap cooldown is keyed by episode key, not bare alert type
        assert!(state
            .alert_cooldown
            .get_last_fired("cutoff_imminent:five_hour")
            .is_some());
        // ...and the episode is now tracked so later cycles don't create a second bead
        assert!(state
            .open_alert_beads
            .contains_key("cutoff_imminent:five_hour"));
    }

    #[test]
    fn process_alerts_respects_anti_flap_cooldown() {
        let forecast = CapacityForecast {
            five_hour: make_window_with_util_and_margin(97.0, true, -2.4, 3.0),
            seven_day: make_window(false, 10.0, 30.0),
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        };
        let mut state = make_state_with_forecast(forecast);

        // Both expected episodes resolved moments ago — the anti-flap floor should stop
        // them re-opening (and minting fresh beads) straight away.
        state
            .alert_cooldown
            .record_fired("cutoff_imminent:five_hour", base_now());
        state
            .alert_cooldown
            .record_fired("session_cutoff_risk:five_hour", base_now());

        let config = AlertConfig {
            enabled: true,
            cooldown_minutes: 60,
            command: vec!["echo".to_string()],
            ..AlertConfig::default()
        };

        let fired = process_alerts(
            &mut state,
            &config,
            base_now(),
            &std::collections::HashMap::new(),
        );
        assert_eq!(
            fired, 0,
            "Should have opened zero episodes within the anti-flap window"
        );
        assert!(
            state.open_alert_beads.is_empty(),
            "No episodes should be tracked when opening was suppressed"
        );
    }

    // --- Episode lifecycle tests ---

    /// Forecast that makes CutoffImminent + SessionCutoffRisk fire on five_hour.
    fn cutoff_forecast() -> CapacityForecast {
        CapacityForecast {
            five_hour: make_window_with_util_and_margin(97.0, true, -2.4, 3.0),
            seven_day: make_window(false, 10.0, 30.0),
            weekly_scoped: make_window(false, 5.0, 30.0),
            binding_window: "five_hour".to_string(),
            dollars_per_pct_7d_s: 0.0,
            estimated_remaining_dollars: 0.0,
        }
    }

    /// Config whose bead commands are inert but succeed, with a parseable "bead id".
    fn echoing_config() -> AlertConfig {
        AlertConfig {
            enabled: true,
            auto_bead: true,
            cooldown_minutes: 60,
            command: vec!["echo".to_string(), "bf-test1".to_string()],
            close_command: vec!["true".to_string()],
            update_command: vec!["true".to_string()],
            ..AlertConfig::default()
        }
    }

    fn cutoff_condition(now: DateTime<Utc>) -> AlertCondition {
        AlertCondition::new(
            AlertType::CutoffImminent,
            "Window five_hour at cutoff risk".to_string(),
            AlertSeverity::Critical,
            now,
        )
        .with_scope("five_hour")
    }

    #[test]
    fn episode_key_includes_scope() {
        let scoped = cutoff_condition(base_now());
        assert_eq!(scoped.episode_key(), "cutoff_imminent:five_hour");

        let unscoped = AlertCondition::new(
            AlertType::CollectorOffline,
            "offline".to_string(),
            AlertSeverity::Warning,
            base_now(),
        );
        assert_eq!(unscoped.episode_key(), "collector_offline");
    }

    #[test]
    fn persistent_condition_creates_exactly_one_bead() {
        // The regression this whole design exists for: a condition that stays true for
        // days used to mint a fresh bead every cooldown window (226 sonnet_cutoff_risk
        // beads accumulated that way). It must now produce exactly one.
        let mut state = make_state_with_forecast(cutoff_forecast());
        let config = echoing_config();
        let conditions = vec![cutoff_condition(base_now())];

        let first = process_alert_episodes(&mut state, &config, &conditions, base_now(), None);
        assert_eq!(first.opened, vec!["cutoff_imminent:five_hour".to_string()]);
        assert_eq!(first.suppressed, 0);

        // Simulate two days of cycles, well past many cooldown windows.
        let mut opened_after = 0;
        for hour in 1..48 {
            let now = base_now() + Duration::hours(hour);
            let conditions = vec![cutoff_condition(now)];
            let outcome = process_alert_episodes(&mut state, &config, &conditions, now, None);
            opened_after += outcome.opened.len();
            assert_eq!(
                outcome.suppressed, 1,
                "condition should be recorded, not re-fired"
            );
        }

        assert_eq!(
            opened_after, 0,
            "A continuously-true condition must not open a second episode"
        );

        let episode = state
            .open_alert_beads
            .get("cutoff_imminent:five_hour")
            .expect("episode should still be open");
        assert_eq!(episode.bead_id.as_deref(), Some("bf-test1"));
        assert_eq!(episode.observations, 48);
        assert_eq!(episode.scope.as_deref(), Some("five_hour"));
    }

    #[test]
    fn cleared_condition_resolves_and_closes_bead() {
        let mut state = make_state_with_forecast(cutoff_forecast());
        let config = echoing_config();

        process_alert_episodes(
            &mut state,
            &config,
            &[cutoff_condition(base_now())],
            base_now(),
            None,
        );
        assert!(state
            .open_alert_beads
            .contains_key("cutoff_imminent:five_hour"));

        // Condition clears three hours later.
        let later = base_now() + Duration::hours(3);
        let outcome = process_alert_episodes(&mut state, &config, &[], later, None);

        assert_eq!(outcome.resolved.len(), 1);
        let resolved = &outcome.resolved[0];
        assert_eq!(resolved.key, "cutoff_imminent:five_hour");
        assert_eq!(resolved.bead_id.as_deref(), Some("bf-test1"));
        assert!(resolved.closed, "close command should have run");
        assert!((resolved.duration_hours - 3.0).abs() < 0.01);
        assert!(
            state.open_alert_beads.is_empty(),
            "resolved episodes must be dropped from state"
        );
    }

    #[test]
    fn resolved_episode_keeps_cooldown_as_anti_flap_floor() {
        let mut state = make_state_with_forecast(cutoff_forecast());
        let config = echoing_config();

        process_alert_episodes(
            &mut state,
            &config,
            &[cutoff_condition(base_now())],
            base_now(),
            None,
        );

        // Clears, then immediately re-triggers — the classic flap.
        let t1 = base_now() + Duration::minutes(5);
        process_alert_episodes(&mut state, &config, &[], t1, None);

        let t2 = base_now() + Duration::minutes(10);
        let outcome =
            process_alert_episodes(&mut state, &config, &[cutoff_condition(t2)], t2, None);
        assert!(
            outcome.opened.is_empty(),
            "Flapping within the cooldown must not open a new episode"
        );

        // Well past the cooldown, a genuinely new episode is allowed.
        let t3 = base_now() + Duration::minutes(90);
        let outcome =
            process_alert_episodes(&mut state, &config, &[cutoff_condition(t3)], t3, None);
        assert_eq!(
            outcome.opened,
            vec!["cutoff_imminent:five_hour".to_string()],
            "A new episode after the cooldown should open normally"
        );
    }

    #[test]
    fn distinct_scopes_are_distinct_episodes() {
        let mut state = make_state_with_forecast(CapacityForecast::default());
        let config = echoing_config();

        let a = AlertCondition::new(
            AlertType::SubscriptionBillingDrift,
            "agent-a drifting".to_string(),
            AlertSeverity::Critical,
            base_now(),
        )
        .with_scope("agent-a");
        let b = AlertCondition::new(
            AlertType::SubscriptionBillingDrift,
            "agent-b drifting".to_string(),
            AlertSeverity::Critical,
            base_now(),
        )
        .with_scope("agent-b");

        let outcome = process_alert_episodes(&mut state, &config, &[a, b], base_now(), None);
        assert_eq!(
            outcome.opened.len(),
            2,
            "Same alert type on two agents is two incidents, so two beads"
        );
        assert_eq!(state.open_alert_beads.len(), 2);
    }

    #[test]
    fn episode_refresh_is_throttled_to_cooldown() {
        let mut state = make_state_with_forecast(cutoff_forecast());
        let config = echoing_config();

        process_alert_episodes(
            &mut state,
            &config,
            &[cutoff_condition(base_now())],
            base_now(),
            None,
        );

        // 30 minutes in: below the 60-minute throttle, so no refresh recorded.
        let t1 = base_now() + Duration::minutes(30);
        process_alert_episodes(&mut state, &config, &[cutoff_condition(t1)], t1, None);
        assert!(state.open_alert_beads["cutoff_imminent:five_hour"]
            .last_refreshed_at
            .is_none());

        // 70 minutes in: throttle elapsed, bead refreshed in place (still no new bead).
        let t2 = base_now() + Duration::minutes(70);
        let outcome =
            process_alert_episodes(&mut state, &config, &[cutoff_condition(t2)], t2, None);
        assert!(outcome.opened.is_empty());
        assert_eq!(
            state.open_alert_beads["cutoff_imminent:five_hour"].last_refreshed_at,
            Some(t2)
        );
    }

    #[test]
    fn episode_tracked_even_when_auto_bead_disabled() {
        // auto_bead off means no bead to close, but the episode is still deduplicated so
        // the governor log gets one line per incident rather than one per cooldown window.
        let mut state = make_state_with_forecast(cutoff_forecast());
        let config = AlertConfig {
            auto_bead: false,
            ..echoing_config()
        };

        let outcome = process_alert_episodes(
            &mut state,
            &config,
            &[cutoff_condition(base_now())],
            base_now(),
            None,
        );
        assert_eq!(outcome.opened.len(), 1);
        let episode = &state.open_alert_beads["cutoff_imminent:five_hour"];
        assert!(
            episode.bead_id.is_none(),
            "no bead is created when auto_bead is off"
        );

        let later = base_now() + Duration::hours(5);
        let outcome =
            process_alert_episodes(&mut state, &config, &[cutoff_condition(later)], later, None);
        assert!(outcome.opened.is_empty());
        assert_eq!(outcome.suppressed, 1);
    }

    #[test]
    fn episode_message_is_updated_in_place() {
        let mut state = make_state_with_forecast(cutoff_forecast());
        let config = echoing_config();

        process_alert_episodes(
            &mut state,
            &config,
            &[cutoff_condition(base_now())],
            base_now(),
            None,
        );

        let later = base_now() + Duration::hours(2);
        let evolved = AlertCondition::new(
            AlertType::CutoffImminent,
            "Window five_hour at cutoff risk: now 99.2% utilized".to_string(),
            AlertSeverity::Critical,
            later,
        )
        .with_scope("five_hour");
        process_alert_episodes(&mut state, &config, &[evolved], later, None);

        let episode = &state.open_alert_beads["cutoff_imminent:five_hour"];
        assert!(episode.last_message.contains("99.2%"));
        assert_eq!(episode.last_seen, later);
        assert_eq!(episode.opened_at, base_now());
    }

    // --- Bead id parsing ---

    #[test]
    fn parse_bead_id_from_envelope_json() {
        let out = r#"{"version":1,"kind":"create","data":{"id":"bf-2sf9o","title":"x"}}"#;
        assert_eq!(parse_bead_id(out), Some("bf-2sf9o".to_string()));
    }

    #[test]
    fn parse_bead_id_from_flat_json() {
        assert_eq!(
            parse_bead_id(r#"{"id":"docs-878a"}"#),
            Some("docs-878a".to_string())
        );
    }

    #[test]
    fn parse_bead_id_from_plain_output() {
        assert_eq!(parse_bead_id("bf-5k6yv\n"), Some("bf-5k6yv".to_string()));
        assert_eq!(
            parse_bead_id("Created bead bf-5k6yv.\n"),
            Some("bf-5k6yv".to_string())
        );
    }

    #[test]
    fn parse_bead_id_returns_none_without_an_id() {
        assert_eq!(parse_bead_id(""), None);
        assert_eq!(parse_bead_id("  \n "), None);
        assert_eq!(parse_bead_id("something went wrong"), None);
    }

    #[test]
    fn alert_log_path_is_in_home_directory() {
        let path = default_alert_log_path();
        assert!(path.to_string_lossy().contains(".needle"));
        assert!(path.to_string_lossy().contains("governor.log"));
    }
}
