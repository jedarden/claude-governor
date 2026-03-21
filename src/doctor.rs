//! Doctor - Health diagnostic checks for Claude Governor
//!
//! Implements `cgov doctor` subcommand with comprehensive health checks.
//! Each check returns pass/warn/fail with remediation text.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

/// Check result status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckStatus::Pass => write!(f, "PASS"),
            CheckStatus::Warn => write!(f, "WARN"),
            CheckStatus::Fail => write!(f, "FAIL"),
        }
    }
}

/// Result of a single health check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    /// Check identifier (e.g., "daemon_running")
    pub check: String,
    /// Status: pass, warn, or fail
    pub status: CheckStatus,
    /// Human-readable message
    pub message: String,
    /// Remediation text (what to do if check fails/warns)
    pub remediation: Option<String>,
}

impl CheckResult {
    fn pass(check: &'static str, message: impl Into<String>) -> Self {
        Self {
            check: check.to_string(),
            status: CheckStatus::Pass,
            message: message.into(),
            remediation: None,
        }
    }

    fn warn(check: &'static str, message: impl Into<String>, remediation: impl Into<String>) -> Self {
        Self {
            check: check.to_string(),
            status: CheckStatus::Warn,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }

    fn fail(check: &'static str, message: impl Into<String>, remediation: impl Into<String>) -> Self {
        Self {
            check: check.to_string(),
            status: CheckStatus::Fail,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }
}

/// Aggregated doctor report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Timestamp when the report was generated
    pub timestamp: DateTime<Utc>,
    /// All check results
    pub checks: Vec<CheckResult>,
    /// Summary counts
    pub passed: usize,
    pub warned: usize,
    pub failed: usize,
    /// Overall status (pass if no failures, warn if warnings, fail if any failures)
    pub overall: CheckStatus,
}

impl DoctorReport {
    /// Create a new report from check results
    pub fn new(checks: Vec<CheckResult>) -> Self {
        let passed = checks.iter().filter(|c| c.status == CheckStatus::Pass).count();
        let warned = checks.iter().filter(|c| c.status == CheckStatus::Warn).count();
        let failed = checks.iter().filter(|c| c.status == CheckStatus::Fail).count();

        let overall = if failed > 0 {
            CheckStatus::Fail
        } else if warned > 0 {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        };

        Self {
            timestamp: Utc::now(),
            checks,
            passed,
            warned,
            failed,
            overall,
        }
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn default_state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("claude-governor")
        .join("governor-state.json")
}

fn default_config_path() -> PathBuf {
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg_config).join("claude-governor/governor.yaml")
    } else if let Some(home) = dirs::home_dir() {
        home.join(".config/claude-governor/governor.yaml")
    } else {
        PathBuf::from("config/governor.yaml")
    }
}

fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".needle")
        .join("state")
        .join("token-history.db")
}

fn default_heartbeat_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".needle")
        .join("state")
        .join("heartbeats")
}

fn credentials_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join(".credentials.json")
}

fn default_log_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("claude-governor")
        .join("governor.log")
}

fn default_jsonl_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".needle")
        .join("state")
        .join("token-history.jsonl")
}

fn default_promotions_path() -> PathBuf {
    PathBuf::from("config/promotions.json")
}

// ---------------------------------------------------------------------------
// Service detection helpers
// ---------------------------------------------------------------------------

const GOVERNOR_SERVICE: &str = "claude-governor.service";
const COLLECTOR_SERVICE: &str = "claude-token-collector.service";
const GOVERNOR_SESSION: &str = "cgov-governor";
const COLLECTOR_SESSION: &str = "cgov-collector";

/// Check if systemd user sessions are available
fn systemd_user_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if a systemd user service is active
fn systemd_service_is_active(service: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", service])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if tmux is available
fn tmux_available_check() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if a tmux session exists
fn tmux_session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Daemon running status with detection method
enum DaemonStatus {
    RunningSystemd,
    RunningTmux,
    ActiveState(i64), // seconds old
    Stopped,
}

/// Detect if the governor daemon is running
fn detect_daemon_status() -> DaemonStatus {
    // Priority 1: systemd
    if systemd_user_available() && systemd_service_is_active(GOVERNOR_SERVICE) {
        return DaemonStatus::RunningSystemd;
    }

    // Priority 2: tmux
    if tmux_available_check() && tmux_session_exists(GOVERNOR_SESSION) {
        return DaemonStatus::RunningTmux;
    }

    // Priority 3: state file freshness (fallback)
    let state_path = default_state_path();
    if let Ok(state) = crate::state::load_state(&state_path) {
        let age_secs = (Utc::now() - state.updated_at).num_seconds().abs();
        if age_secs < 300 {
            return DaemonStatus::ActiveState(age_secs);
        }
    }

    DaemonStatus::Stopped
}

/// Collector running status with detection method
enum CollectorStatus {
    RunningSystemd,
    RunningTmux,
    ActiveFleet(i64), // seconds old
    Stopped,
}

/// Detect if the token collector is running
fn detect_collector_status() -> CollectorStatus {
    // Priority 1: systemd
    if systemd_user_available() && systemd_service_is_active(COLLECTOR_SERVICE) {
        return CollectorStatus::RunningSystemd;
    }

    // Priority 2: tmux
    if tmux_available_check() && tmux_session_exists(COLLECTOR_SESSION) {
        return CollectorStatus::RunningTmux;
    }

    // Priority 3: fleet aggregate freshness (fallback)
    let state_path = default_state_path();
    if let Ok(state) = crate::state::load_state(&state_path) {
        let age_secs = (Utc::now() - state.last_fleet_aggregate.t1).num_seconds().abs();
        if age_secs < 300 {
            return CollectorStatus::ActiveFleet(age_secs);
        }
    }

    CollectorStatus::Stopped
}

// ---------------------------------------------------------------------------
// Individual health checks
// ---------------------------------------------------------------------------

/// Check if the governor daemon is running (via systemd, tmux, or state file)
fn check_daemon_running() -> CheckResult {
    match detect_daemon_status() {
        DaemonStatus::RunningSystemd => {
            // Also verify state freshness as a sanity check
            let state_path = default_state_path();
            match crate::state::load_state(&state_path) {
                Ok(state) => {
                    let age_secs = (Utc::now() - state.updated_at).num_seconds().abs();
                    if age_secs < 300 {
                        CheckResult::pass("daemon_running", format!("running (systemd), state {}s old", age_secs))
                    } else {
                        CheckResult::warn(
                            "daemon_running",
                            format!("systemd active but state {}s old", age_secs),
                            "Daemon may be stuck; check logs: journalctl --user -u claude-governor",
                        )
                    }
                }
                Err(_) => CheckResult::pass("daemon_running", "running (systemd)"),
            }
        }
        DaemonStatus::RunningTmux => {
            CheckResult::pass("daemon_running", "running (tmux)")
        }
        DaemonStatus::ActiveState(age_secs) => {
            CheckResult::pass("daemon_running", format!("active (state {}s old)", age_secs))
        }
        DaemonStatus::Stopped => {
            let state_path = default_state_path();
            if state_path.exists() {
                if let Ok(state) = crate::state::load_state(&state_path) {
                    let age_secs = (Utc::now() - state.updated_at).num_seconds().abs();
                    CheckResult::fail(
                        "daemon_running",
                        format!("stopped (state {}s old)", age_secs),
                        "Start the governor: cgov start governor",
                    )
                } else {
                    CheckResult::fail(
                        "daemon_running",
                        "stopped (state unreadable)",
                        "Check state file permissions or run 'cgov init'",
                    )
                }
            } else {
                CheckResult::warn(
                    "daemon_running",
                    "not initialized",
                    "Run 'cgov enable' to start the governor daemon",
                )
            }
        }
    }
}

/// Check if the token collector is running (via systemd, tmux, or fleet data)
fn check_collector_running() -> CheckResult {
    match detect_collector_status() {
        CollectorStatus::RunningSystemd => {
            // Also verify fleet freshness as a sanity check
            let state_path = default_state_path();
            match crate::state::load_state(&state_path) {
                Ok(state) => {
                    let age_secs = (Utc::now() - state.last_fleet_aggregate.t1).num_seconds().abs();
                    if age_secs < 300 {
                        CheckResult::pass("collector_running", format!("running (systemd), fleet {}s old", age_secs))
                    } else {
                        CheckResult::warn(
                            "collector_running",
                            format!("systemd active but fleet {}s old", age_secs),
                            "Collector may be stuck; check logs: journalctl --user -u claude-token-collector",
                        )
                    }
                }
                Err(_) => CheckResult::pass("collector_running", "running (systemd)"),
            }
        }
        CollectorStatus::RunningTmux => {
            CheckResult::pass("collector_running", "running (tmux)")
        }
        CollectorStatus::ActiveFleet(age_secs) => {
            CheckResult::pass("collector_running", format!("active (fleet {}s old)", age_secs))
        }
        CollectorStatus::Stopped => {
            let state_path = default_state_path();
            if state_path.exists() {
                if let Ok(state) = crate::state::load_state(&state_path) {
                    let age_secs = (Utc::now() - state.last_fleet_aggregate.t1).num_seconds().abs();
                    if state.last_fleet_aggregate.sonnet_workers == 0 && age_secs > 3600 {
                        CheckResult::warn(
                            "collector_running",
                            "no fleet data recorded yet",
                            "Start the collector: cgov start collector",
                        )
                    } else if age_secs < 900 {
                        CheckResult::warn(
                            "collector_running",
                            format!("stopped (fleet {}s old)", age_secs),
                            "Start the collector: cgov start collector",
                        )
                    } else {
                        CheckResult::fail(
                            "collector_running",
                            format!("stopped (fleet {}s old)", age_secs),
                            "Restart the collector: cgov restart collector",
                        )
                    }
                } else {
                    CheckResult::fail(
                        "collector_running",
                        "stopped (state unreadable)",
                        "Check state file permissions",
                    )
                }
            } else {
                CheckResult::warn(
                    "collector_running",
                    "not initialized",
                    "Run 'cgov enable' to start the collector",
                )
            }
        }
    }
}

/// Check state file freshness
fn check_state_file_freshness() -> CheckResult {
    let state_path = default_state_path();

    if !state_path.exists() {
        return CheckResult::fail(
            "state_freshness",
            "State file does not exist",
            "Run 'cgov init' to initialize the state file",
        );
    }

    match fs::metadata(&state_path) {
        Ok(metadata) => {
            let modified = metadata.modified().ok();
            match modified {
                Some(m) => {
                    let modified_dt: DateTime<Utc> = m.into();
                    let age_secs = (Utc::now() - modified_dt).num_seconds().abs();

                    if age_secs < 120 {
                        CheckResult::pass("state_freshness", format!("Modified {}s ago", age_secs))
                    } else if age_secs < 600 {
                        CheckResult::warn(
                            "state_freshness",
                            format!("Modified {}s ago", age_secs),
                            "Governor may be slow to update",
                        )
                    } else {
                        CheckResult::fail(
                            "state_freshness",
                            format!("Modified {}s ago (stale)", age_secs),
                            "Restart the governor: cgov restart",
                        )
                    }
                }
                None => CheckResult::warn(
                    "state_freshness",
                    "Cannot determine modification time",
                    "Check file system",
                ),
            }
        }
        Err(e) => CheckResult::fail(
            "state_freshness",
            format!("Cannot access state file: {}", e),
            "Check file permissions",
        ),
    }
}

/// Check OAuth token validity
fn check_oauth_token_validity() -> CheckResult {
    let creds_path = credentials_path();

    if !creds_path.exists() {
        return CheckResult::fail(
            "oauth_token",
            "Credentials file not found",
            "Run 'claude login' to authenticate with Anthropic",
        );
    }

    match fs::read_to_string(&creds_path) {
        Ok(content) => {
            // Parse as generic JSON to check expiresAt
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(json) => {
                    let expires_at = json
                        .get("claudeAiOauth")
                        .and_then(|o| o.get("expiresAt"))
                        .and_then(|v| v.as_i64());

                    match expires_at {
                        Some(expires_ms) => {
                            let now_ms = Utc::now().timestamp_millis();
                            let remaining_secs = (expires_ms - now_ms) / 1000;

                            if remaining_secs > 3600 {
                                CheckResult::pass(
                                    "oauth_token",
                                    format!("Token valid for {}m", remaining_secs / 60),
                                )
                            } else if remaining_secs > 300 {
                                CheckResult::warn(
                                    "oauth_token",
                                    format!("Token expires in {}m", remaining_secs / 60),
                                    "Token will auto-refresh, but monitor for failures",
                                )
                            } else if remaining_secs > 0 {
                                CheckResult::warn(
                                    "oauth_token",
                                    format!("Token expires in {}s", remaining_secs),
                                    "Token refresh may be failing; run 'claude login' if issues persist",
                                )
                            } else {
                                CheckResult::fail(
                                    "oauth_token",
                                    "Token has expired",
                                    "Run 'claude login' to re-authenticate",
                                )
                            }
                        }
                        None => CheckResult::warn(
                            "oauth_token",
                            "Cannot determine token expiry",
                            "Credentials file format may be outdated",
                        ),
                    }
                }
                Err(e) => CheckResult::fail(
                    "oauth_token",
                    format!("Invalid credentials JSON: {}", e),
                    "Run 'claude login' to regenerate credentials",
                ),
            }
        }
        Err(e) => CheckResult::fail(
            "oauth_token",
            format!("Cannot read credentials: {}", e),
            "Check file permissions on ~/.claude/.credentials.json",
        ),
    }
}

/// Check heartbeat file consistency
fn check_heartbeat_consistency() -> CheckResult {
    let heartbeat_dir = default_heartbeat_dir();

    if !heartbeat_dir.exists() {
        return CheckResult::warn(
            "heartbeat_files",
            "Heartbeat directory does not exist",
            "Workers have not been started yet, or heartbeat_dir is misconfigured",
        );
    }

    match fs::read_dir(&heartbeat_dir) {
        Ok(entries) => {
            let files: Vec<_> = entries.filter_map(|e| e.ok()).collect();

            if files.is_empty() {
                return CheckResult::warn(
                    "heartbeat_files",
                    "No heartbeat files found",
                    "Start workers with 'cgov start' or check agent configuration",
                );
            }

            let now = Utc::now();
            let mut fresh_count = 0;
            let mut stale_count = 0;

            for file in &files {
                let path = file.path();
                if path.extension().map_or(false, |ext| ext == "json") {
                    if let Ok(metadata) = fs::metadata(path) {
                        if let Ok(modified) = metadata.modified() {
                            let modified_dt: DateTime<Utc> = modified.into();
                            let age_secs = (now - modified_dt).num_seconds().abs();
                            if age_secs < 300 {
                                fresh_count += 1;
                            } else {
                                stale_count += 1;
                            }
                        }
                    }
                }
            }

            if fresh_count > 0 && stale_count == 0 {
                CheckResult::pass(
                    "heartbeat_files",
                    format!("{} fresh heartbeat files", fresh_count),
                )
            } else if fresh_count > 0 && stale_count > 0 {
                CheckResult::warn(
                    "heartbeat_files",
                    format!("{} fresh, {} stale heartbeat files", fresh_count, stale_count),
                    "Some workers may have stopped; check 'cgov workers'",
                )
            } else if stale_count > 0 {
                CheckResult::warn(
                    "heartbeat_files",
                    format!("{} stale heartbeat files, no fresh", stale_count),
                    "Workers may have stopped; restart with 'cgov restart'",
                )
            } else {
                CheckResult::warn(
                    "heartbeat_files",
                    "No .json heartbeat files found",
                    "Check heartbeat directory configuration",
                )
            }
        }
        Err(e) => CheckResult::fail(
            "heartbeat_files",
            format!("Cannot read heartbeat directory: {}", e),
            "Check directory permissions",
        ),
    }
}

/// Check SQLite integrity
fn check_sqlite_integrity() -> CheckResult {
    let db_path = default_db_path();

    if !db_path.exists() {
        return CheckResult::warn(
            "sqlite_integrity",
            "SQLite database does not exist",
            "Database will be created on first collection; run 'cgov collect' or wait for collector",
        );
    }

    // Use rusqlite to run PRAGMA integrity_check
    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => {
            match conn.query_row("PRAGMA integrity_check;", [], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(result) => {
                    if result == "ok" {
                        // Also check for the known FrankenSQLite corruption pattern
                        match conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |row| {
                            row.get::<_, i64>(0)
                        }) {
                            Ok(table_count) => {
                                if table_count >= 3 {
                                    CheckResult::pass(
                                        "sqlite_integrity",
                                        format!("Database integrity OK ({} tables)", table_count),
                                    )
                                } else {
                                    CheckResult::warn(
                                        "sqlite_integrity",
                                        format!("Database has only {} tables", table_count),
                                        "Run 'cgov token-history --rebuild-db' to rebuild from JSONL",
                                    )
                                }
                            }
                            Err(_) => CheckResult::pass("sqlite_integrity", "Database integrity OK"),
                        }
                    } else {
                        CheckResult::fail(
                            "sqlite_integrity",
                            format!("Integrity check: {}", result),
                            "Run 'cgov token-history --rebuild-db' to rebuild from JSONL source",
                        )
                    }
                }
                Err(e) => CheckResult::fail(
                    "sqlite_integrity",
                    format!("Integrity check failed: {}", e),
                    "Run 'cgov token-history --rebuild-db' to rebuild from JSONL source",
                ),
            }
        }
        Err(e) => CheckResult::fail(
            "sqlite_integrity",
            format!("Cannot open database: {}", e),
            "Database may be corrupted; run 'cgov token-history --rebuild-db'",
        ),
    }
}

/// Check config file parseable
fn check_config_parseable() -> CheckResult {
    let config_path = default_config_path();

    if !config_path.exists() {
        return CheckResult::fail(
            "config_parseable",
            format!("Config file not found: {}", config_path.display()),
            "Run 'cgov init' to create default configuration",
        );
    }

    match crate::config::GovernorConfig::load_from_path(&config_path) {
        Ok(config) => {
            let model_count = config.pricing.models.len();
            let agent_count = config.agents.len();

            CheckResult::pass(
                "config_parseable",
                format!("Config OK ({} models, {} agents)", model_count, agent_count),
            )
        }
        Err(e) => CheckResult::fail(
            "config_parseable",
            format!("Config parse error: {}", e),
            "Fix syntax errors in governor.yaml or run 'cgov config --edit'",
        ),
    }
}

/// Check tmux availability
fn check_tmux_available() -> CheckResult {
    match Command::new("tmux").arg("-V").output() {
        Ok(output) => {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                CheckResult::pass("tmux_available", version)
            } else {
                CheckResult::warn(
                    "tmux_available",
                    "tmux found but not executable",
                    "Install tmux for daemon mode support",
                )
            }
        }
        Err(_) => {
            // Check if systemd is available as alternative
            let systemd_available = Command::new("systemctl")
                .args(["--user", "status"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);

            if systemd_available {
                CheckResult::pass(
                    "tmux_available",
                    "systemd available (tmux not required)",
                )
            } else {
                CheckResult::warn(
                    "tmux_available",
                    "Neither tmux nor systemd user sessions available",
                    "Install tmux or enable systemd user sessions (loginctl enable-linger $USER)",
                )
            }
        }
    }
}

/// Check burn rate sample count
fn check_burn_rate_samples() -> CheckResult {
    let state_path = default_state_path();

    if !state_path.exists() {
        return CheckResult::warn(
            "burn_rate_samples",
            "No state file found",
            "Run governor to collect burn rate data",
        );
    }

    match crate::state::load_state(&state_path) {
        Ok(state) => {
            let total_samples: u32 = state
                .burn_rate
                .by_model
                .values()
                .map(|m| m.samples)
                .sum();

            if total_samples >= 5 {
                CheckResult::pass(
                    "burn_rate_samples",
                    format!("{} samples collected", total_samples),
                )
            } else if total_samples >= 3 {
                CheckResult::warn(
                    "burn_rate_samples",
                    format!("Only {} samples (need 5+ per window)", total_samples),
                    "Let the governor run longer to collect more data",
                )
            } else {
                CheckResult::fail(
                    "burn_rate_samples",
                    format!("Insufficient samples ({}) — using baseline fallback", total_samples),
                    "Run governor for at least 30 minutes to collect burn rate data",
                )
            }
        }
        Err(e) => CheckResult::fail(
            "burn_rate_samples",
            format!("Cannot read state: {}", e),
            "Check state file permissions",
        ),
    }
}

/// Check alert cooldown state
fn check_alert_cooldown() -> CheckResult {
    let state_path = default_state_path();

    if !state_path.exists() {
        return CheckResult::pass("alert_cooldown", "No active alerts (no state file)");
    }

    match crate::state::load_state(&state_path) {
        Ok(state) => {
            let active_cooldowns = state.alert_cooldown.last_fired.len();

            if active_cooldowns == 0 {
                CheckResult::pass("alert_cooldown", "No active alert cooldowns")
            } else {
                let now = Utc::now();
                let recent_alerts: Vec<_> = state
                    .alert_cooldown
                    .last_fired
                    .iter()
                    .filter(|(_, &ts)| (now - ts).num_seconds() < 3600)
                    .collect();

                if recent_alerts.is_empty() {
                    CheckResult::pass(
                        "alert_cooldown",
                        format!("{} cooldowns active (all older than 1h)", active_cooldowns),
                    )
                } else {
                    let types: Vec<_> = recent_alerts.iter().map(|(k, _)| k.as_str()).collect();
                    CheckResult::warn(
                        "alert_cooldown",
                        format!("Recent alerts: {}", types.join(", ")),
                        "Check 'cgov status' for current capacity state",
                    )
                }
            }
        }
        Err(_) => CheckResult::pass("alert_cooldown", "Cannot determine (state unreadable)"),
    }
}

/// Check prediction accuracy (calibration)
fn check_prediction_accuracy() -> CheckResult {
    let state_path = default_state_path();

    if !state_path.exists() {
        return CheckResult::warn(
            "prediction_accuracy",
            "No state file found",
            "Run governor to collect calibration data",
        );
    }

    match crate::state::load_state(&state_path) {
        Ok(state) => {
            let cal = &state.burn_rate.calibration;

            if cal.predictions_scored < 5 {
                return CheckResult::warn(
                    "prediction_accuracy",
                    format!("Only {} predictions scored (need 5+)", cal.predictions_scored),
                    "Let the governor run longer to calibrate predictions",
                );
            }

            let error = cal.median_error_7ds.abs();

            if error < 5.0 {
                CheckResult::pass(
                    "prediction_accuracy",
                    format!("median error {:.1}% ({} scored)", error, cal.predictions_scored),
                )
            } else if error < 10.0 {
                CheckResult::warn(
                    "prediction_accuracy",
                    format!("median error {:.1}% ({} scored)", error, cal.predictions_scored),
                    "Predictions are within acceptable range but could improve with more data",
                )
            } else {
                CheckResult::fail(
                    "prediction_accuracy",
                    format!("median error {:.1}% ({} scored)", error, cal.predictions_scored),
                    "Safe mode may activate; check for unusual usage patterns",
                )
            }
        }
        Err(e) => CheckResult::fail(
            "prediction_accuracy",
            format!("Cannot read state: {}", e),
            "Check state file permissions",
        ),
    }
}

/// Check disk space
fn check_disk_space() -> CheckResult {
    // Check disk space for the state directory
    let state_dir = default_state_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Use df command to check disk space
    match Command::new("df").arg("-h").arg(&state_dir).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Parse df output - second line has the values
            let lines: Vec<&str> = stdout.lines().collect();
            if lines.len() >= 2 {
                let parts: Vec<&str> = lines[1].split_whitespace().collect();
                if parts.len() >= 5 {
                    let avail = parts[3]; // Available space
                    let use_pct = parts[4].trim_end_matches('%'); // Use %

                    if let Ok(use_pct) = use_pct.parse::<u8>() {
                        if use_pct < 80 {
                            return CheckResult::pass(
                                "disk_space",
                                format!("{} available ({}% used)", avail, use_pct),
                            );
                        } else if use_pct < 95 {
                            return CheckResult::warn(
                                "disk_space",
                                format!("{} available ({}% used)", avail, use_pct),
                                "Consider cleaning up old logs or state files",
                            );
                        } else {
                            return CheckResult::fail(
                                "disk_space",
                                format!("{} available ({}% used)", avail, use_pct),
                                "Free up disk space immediately to prevent data loss",
                            );
                        }
                    }
                }
            }
            CheckResult::warn(
                "disk_space",
                "Could not parse df output",
                "Check disk space manually: df -h",
            )
        }
        Err(e) => CheckResult::warn(
            "disk_space",
            format!("Cannot run df: {}", e),
            "Check disk space manually: df -h",
        ),
    }
}

/// Check API reachability (lightweight HEAD to Anthropic API)
fn check_api_reachability() -> CheckResult {
    let start = Instant::now();
    let creds_path = credentials_path();

    // Read the OAuth token for authentication
    let token = fs::read_to_string(&creds_path).ok().and_then(|content| {
        serde_json::from_str::<serde_json::Value>(&content).ok().and_then(|json| {
            json.get("claudeAiOauth")
                .and_then(|o| o.get("accessToken"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
    });

    let agent = ureq::AgentBuilder::new()
        .timeout_read(std::time::Duration::from_secs(5))
        .timeout_write(std::time::Duration::from_secs(5))
        .build();

    let req = agent.get("https://api.anthropic.com/api/oauth/usage")
        .set("anthropic-beta", "oauth-2025-04-20");
    let req = if let Some(ref tok) = token {
        req.set("Authorization", &format!("Bearer {}", tok))
    } else {
        req
    };

    match req.call() {
        Ok(response) => {
            let elapsed_ms = start.elapsed().as_millis();
            let status = response.status();
            if status == 200 {
                if elapsed_ms < 2000 {
                    CheckResult::pass("api_reachability", format!("200 OK ({}ms)", elapsed_ms))
                } else {
                    CheckResult::warn(
                        "api_reachability",
                        format!("200 OK but slow ({}ms)", elapsed_ms),
                        "API is reachable but latency is high; check network conditions",
                    )
                }
            } else if status == 401 {
                CheckResult::fail(
                    "api_reachability",
                    format!("HTTP {} (auth error)", status),
                    "Run 'claude login' to refresh credentials",
                )
            } else {
                CheckResult::fail(
                    "api_reachability",
                    format!("HTTP {}", status),
                    "Anthropic API returned an unexpected status; check https://status.anthropic.com",
                )
            }
        }
        Err(ureq::Error::Status(code, _)) => {
            CheckResult::fail(
                "api_reachability",
                format!("HTTP {} (error)", code),
                "Anthropic API returned an error; check https://status.anthropic.com",
            )
        }
        Err(_) => {
            CheckResult::fail(
                "api_reachability",
                format!("Unreachable (timeout after {}ms)", start.elapsed().as_millis()),
                "Check network connectivity and DNS resolution for api.anthropic.com",
            )
        }
    }
}

/// Check model generation pricing — detect legacy pricing rates
fn check_model_generation() -> CheckResult {
    let config_path = default_config_path();

    if !config_path.exists() {
        return CheckResult::pass("model_generation", "No config file (skipped)");
    }

    match crate::config::GovernorConfig::load_from_path(&config_path) {
        Ok(config) => {
            // Known legacy pricing signatures to detect
            // Opus 4 at $15/$75 input/output is the legacy rate
            let legacy_models: Vec<&str> = config
                .pricing
                .models
                .iter()
                .filter(|(_, p)| {
                    // Detect legacy Opus pricing: $15/MTok input, $75/MTok output
                    (p.input_per_mtok - 15.0).abs() < 0.1 && (p.output_per_mtok - 75.0).abs() < 0.1
                })
                .map(|(name, _)| name.as_str())
                .collect();

            if legacy_models.is_empty() {
                CheckResult::pass("model_generation", "rates consistent")
            } else {
                CheckResult::fail(
                    "model_generation",
                    format!("legacy pricing detected: {}", legacy_models.join(", ")),
                    "Update pricing in ~/.config/claude-governor/governor.yaml",
                )
            }
        }
        Err(_) => CheckResult::pass("model_generation", "Cannot check (config unreadable)"),
    }
}

/// Check promotion dates — verify promotions are not expired
fn check_promotion_dates() -> CheckResult {
    let promo_path = default_promotions_path();

    if !promo_path.exists() {
        return CheckResult::pass("promotion_dates", "No promotions configured");
    }

    let promos: Vec<crate::schedule::Promotion> = match fs::read_to_string(&promo_path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
    {
        Some(p) => p,
        None => return CheckResult::warn(
            "promotion_dates",
            "Cannot parse promotions.json",
            "Fix JSON syntax in config/promotions.json",
        ),
    };

    if promos.is_empty() {
        return CheckResult::pass("promotion_dates", "No promotions configured");
    }

    let now = Utc::now();
    let mut warnings = Vec::new();
    let mut failures = Vec::new();

    for promo in &promos {
        // Parse end_date (YYYY-MM-DD)
        let end_naive = match chrono::NaiveDate::parse_from_str(&promo.end_date, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => {
                warnings.push(format!("{}: invalid end_date format", promo.name));
                continue;
            }
        };

        let end_dt = end_naive.and_hms_opt(23, 59, 59).unwrap().and_utc();

        if end_dt < now {
            failures.push(format!("{}: expired", promo.name));
        } else {
            let hours_until_expiry = (end_dt - now).num_hours();
            if hours_until_expiry < 48 {
                warnings.push(format!("{}: expires in {}h", promo.name, hours_until_expiry));
            }
        }
    }

    if !failures.is_empty() {
        CheckResult::fail(
            "promotion_dates",
            failures.join("; "),
            "Remove expired promotions from config/promotions.json",
        )
    } else if !warnings.is_empty() {
        CheckResult::warn("promotion_dates", warnings.join("; "), "Update or extend promotion dates")
    } else {
        let active: Vec<_> = promos.iter().filter(|p| {
            chrono::NaiveDate::parse_from_str(&p.start_date, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc() <= now)
                .unwrap_or(false)
        }).collect();
        if active.is_empty() {
            CheckResult::pass("promotion_dates", "future promotions configured")
        } else {
            CheckResult::pass("promotion_dates", format!("{} active/future promotion(s)", promos.len()))
        }
    }
}

/// Check JSONL/DB row count sync
fn check_jsonl_db_sync() -> CheckResult {
    let jsonl_path = default_jsonl_path();
    let db_path = default_db_path();

    if !jsonl_path.exists() && !db_path.exists() {
        return CheckResult::warn(
            "jsonl_db_sync",
            "Neither JSONL nor DB found",
            "Run 'cgov collect' to start collecting token data",
        );
    }

    if !db_path.exists() {
        return CheckResult::fail(
            "jsonl_db_sync",
            "SQLite database missing",
            "Run 'cgov token-history --rebuild-db' to create from JSONL",
        );
    }

    if !jsonl_path.exists() {
        return CheckResult::warn(
            "jsonl_db_sync",
            "JSONL source missing (DB exists)",
            "JSONL is the authoritative source; check for accidental deletion",
        );
    }

    // Count JSONL lines
    let jsonl_lines = match fs::read_to_string(&jsonl_path) {
        Ok(content) => content.lines().filter(|l| !l.trim().is_empty()).count() as i64,
        Err(e) => {
            return CheckResult::warn(
                "jsonl_db_sync",
                format!("Cannot read JSONL: {}", e),
                "Check file permissions on token-history.jsonl",
            );
        }
    };

    // Count DB rows
    let db_rows = match rusqlite::Connection::open(&db_path) {
        Ok(conn) => {
            conn.query_row(
                "SELECT SUM(cnt) FROM (SELECT COUNT(*) AS cnt FROM sqlite_master WHERE type='table' AND name LIKE 'usage_%')",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .unwrap_or(0)
        }
        Err(_) => {
            return CheckResult::fail(
                "jsonl_db_sync",
                "Cannot open database",
                "Run 'cgov token-history --rebuild-db' to rebuild from JSONL",
            );
        }
    };

    if db_rows == 0 {
        return CheckResult::fail(
            "jsonl_db_sync",
            format!("DB empty ({} JSONL rows)", jsonl_lines),
            "Run 'cgov token-history --rebuild-db' to import JSONL into DB",
        );
    }

    if jsonl_lines == 0 {
        return CheckResult::warn(
            "jsonl_db_sync",
            format!("JSONL empty ({} DB rows)", db_rows),
            "JSONL is authoritative; DB may be from a previous session",
        );
    }

    let divergence = if jsonl_lines > db_rows {
        (jsonl_lines - db_rows) as f64 / jsonl_lines as f64
    } else {
        (db_rows - jsonl_lines) as f64 / jsonl_lines as f64
    };

    if divergence < 0.01 {
        CheckResult::pass(
            "jsonl_db_sync",
            format!("{} / {} rows ({:.1}%)", jsonl_lines, db_rows, (1.0 - divergence) * 100.0),
        )
    } else {
        CheckResult::warn(
            "jsonl_db_sync",
            format!("{} / {} rows (diverge {:.1}%)", jsonl_lines, db_rows, divergence * 100.0),
            "Run 'cgov token-history --rebuild-db' to resync DB from JSONL",
        )
    }
}

/// Check log file existence and size
fn check_log_file() -> CheckResult {
    let log_path = default_log_path();

    if !log_path.exists() {
        // Check if parent directory is writable
        let parent = log_path.parent();
        if let Some(dir) = parent {
            if dir.exists() {
                return CheckResult::warn(
                    "log_file",
                    "Log file not yet created",
                    "Log will be created on first daemon run; run 'cgov start'",
                );
            }
        }
        return CheckResult::warn(
            "log_file",
            format!("Log directory missing: {}", log_path.display()),
            "Create log directory or run 'cgov init'",
        );
    }

    match fs::metadata(&log_path) {
        Ok(metadata) => {
            let size_bytes = metadata.len();
            let size_mb = size_bytes as f64 / (1024.0 * 1024.0);

            if !metadata.permissions().readonly() {
                if size_mb < 100.0 {
                    if size_mb < 1.0 {
                        CheckResult::pass("log_file", format!("{:.1} KB", size_bytes as f64 / 1024.0))
                    } else {
                        CheckResult::pass("log_file", format!("{:.1} MB", size_mb))
                    }
                } else {
                    CheckResult::warn(
                        "log_file",
                        format!("{:.1} MB", size_mb),
                        "Consider rotating logs: truncate or archive governor.log",
                    )
                }
            } else {
                CheckResult::fail(
                    "log_file",
                    format!("{:.1} MB (read-only)", size_mb),
                    "Fix permissions: chmod +w <log_path>",
                )
            }
        }
        Err(e) => CheckResult::fail(
            "log_file",
            format!("Cannot access log: {}", e),
            "Check file permissions on the log file",
        ),
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run all health checks and return a report
pub fn run_doctor() -> DoctorReport {
    let checks = vec![
        // Plan Component 19 core checks (ordered per spec)
        check_oauth_token_validity(),
        check_api_reachability(),
        check_collector_running(),
        check_burn_rate_samples(),
        check_config_parseable(),
        check_model_generation(),
        check_promotion_dates(),
        check_sqlite_integrity(),
        check_jsonl_db_sync(),
        check_daemon_running(),
        check_log_file(),
        check_prediction_accuracy(),
        // Additional operational checks
        check_state_file_freshness(),
        check_heartbeat_consistency(),
        check_tmux_available(),
        check_alert_cooldown(),
        check_disk_space(),
    ];

    DoctorReport::new(checks)
}

/// Format the doctor report for human consumption
pub fn format_doctor_human(report: &DoctorReport) -> String {
    let mut output = String::new();

    // Header matching plan spec format
    let ts = report.timestamp.format("%Y-%m-%d %H:%M UTC");
    output.push_str(&format!("cgov doctor — {}\n", ts));
    output.push_str(&"─".repeat(44));
    output.push('\n');

    for check in &report.checks {
        let status_icon = match check.status {
            CheckStatus::Pass => "✓",
            CheckStatus::Warn => "⚠",
            CheckStatus::Fail => "✗",
        };

        // Format: "✓ check_name     message" with alignment
        output.push_str(&format!(
            "{} {:22} {}\n",
            status_icon, check.check, check.message
        ));

        if let Some(ref remediation) = check.remediation {
            output.push_str(&format!("  → {}\n", remediation));
        }
    }

    output.push_str(&"─".repeat(44));
    output.push('\n');
    output.push_str(&format!(
        "{} passed · {} warning · {} failed\n",
        report.passed, report.warned, report.failed
    ));

    output
}

/// Format the doctor report as JSON
pub fn format_doctor_json(report: &DoctorReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|e| {
        serde_json::json!({"error": format!("Serialization error: {}", e)}).to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_result_pass() {
        let result = CheckResult::pass("test", "All good");
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.check, "test");
        assert!(result.remediation.is_none());
    }

    #[test]
    fn test_check_result_warn() {
        let result = CheckResult::warn("test", "Something's off", "Do something");
        assert_eq!(result.status, CheckStatus::Warn);
        assert_eq!(result.remediation, Some("Do something".to_string()));
    }

    #[test]
    fn test_check_result_fail() {
        let result = CheckResult::fail("test", "It's broken", "Fix it now");
        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(result.remediation, Some("Fix it now".to_string()));
    }

    #[test]
    fn test_doctor_report_counts() {
        let checks = vec![
            CheckResult::pass("a", "ok"),
            CheckResult::pass("b", "ok"),
            CheckResult::warn("c", "warn", "fix"),
            CheckResult::fail("d", "fail", "fix now"),
        ];

        let report = DoctorReport::new(checks);
        assert_eq!(report.passed, 2);
        assert_eq!(report.warned, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.overall, CheckStatus::Fail);
    }

    #[test]
    fn test_doctor_report_overall_warn() {
        let checks = vec![
            CheckResult::pass("a", "ok"),
            CheckResult::warn("b", "warn", "fix"),
        ];

        let report = DoctorReport::new(checks);
        assert_eq!(report.overall, CheckStatus::Warn);
    }

    #[test]
    fn test_doctor_report_overall_pass() {
        let checks = vec![
            CheckResult::pass("a", "ok"),
            CheckResult::pass("b", "ok"),
        ];

        let report = DoctorReport::new(checks);
        assert_eq!(report.overall, CheckStatus::Pass);
    }

    #[test]
    fn test_status_serialization() {
        let pass = CheckStatus::Pass;
        let json = serde_json::to_string(&pass).unwrap();
        assert_eq!(json, "\"pass\"");

        let warn = CheckStatus::Warn;
        let json = serde_json::to_string(&warn).unwrap();
        assert_eq!(json, "\"warn\"");

        let fail = CheckStatus::Fail;
        let json = serde_json::to_string(&fail).unwrap();
        assert_eq!(json, "\"fail\"");
    }

    #[test]
    fn test_report_serialization() {
        let report = DoctorReport::new(vec![
            CheckResult::pass("test_check", "All good"),
        ]);

        let json = serde_json::to_string(&report).unwrap();
        // Compact JSON format has no space after colon
        assert!(json.contains("\"passed\":1"));
        assert!(json.contains("\"overall\":\"pass\""));
    }
}
