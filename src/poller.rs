//! Usage Poller - OAuth token management and API polling
//!
//! This module handles:
//! - Reading OAuth credentials from ~/.claude/.credentials.json
//! - Refreshing tokens when near expiry
//! - Polling the Anthropic usage API
//! - Computing hours_remaining from resets_at timestamps

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use thiserror::Error;
use ureq::Agent;

/// Anthropic OAuth credentials file location
const CREDENTIALS_PATH: &str = ".claude/.credentials.json";

/// Seconds before expiry to trigger refresh (5 minutes)
const REFRESH_THRESHOLD_SECS: i64 = 300;

/// Seconds to wait between refresh retry attempts
const REFRESH_RETRY_DELAY_SECS: u64 = 5;

/// Maximum consecutive refresh failures before escalation
const MAX_REFRESH_FAILURES: u32 = 3;

/// API endpoints
const API_BASE: &str = "https://api.anthropic.com";
const USAGE_ENDPOINT: &str = "/api/oauth/usage";
const TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";

/// User-Agent header (mimics Claude Code)
const USER_AGENT: &str = "claude-code/2.1.114";

/// Poller errors
#[derive(Error, Debug)]
pub enum PollerError {
    #[error("Credentials file not found at {0}")]
    CredentialsNotFound(PathBuf),

    #[error("Invalid credentials format: {0}")]
    InvalidCredentials(String),

    #[error("Token refresh failed: {0}")]
    TokenRefreshFailed(String),

    #[error("API request failed: {0}")]
    ApiRequestFailed(String),

    #[error("API returned error: {0}")]
    ApiError(String),

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("Consecutive refresh failures exceeded threshold")]
    MaxRefreshFailures,
}

/// OAuth credentials from ~/.claude/.credentials.json
#[derive(Debug, Deserialize, Serialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: OAuthData,
}

#[derive(Debug, Deserialize, Serialize)]
struct OAuthData {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
    #[serde(rename = "expiresAt")]
    expires_at: i64,
    #[serde(default)]
    scopes: Vec<String>,
}

/// Token refresh request payload
#[derive(Debug, Serialize)]
struct RefreshRequest {
    #[serde(rename = "grantType")]
    grant_type: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
}

/// Token refresh response
#[derive(Debug, Deserialize)]
struct RefreshResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
    #[serde(rename = "expiresAt")]
    expires_at: i64,
}

/// Usage window data from the API
#[derive(Debug, Deserialize, Clone)]
pub struct UsageWindow {
    /// Name of the window (e.g., "five_hour", "seven_day", "weekly_scoped")
    #[serde(default)]
    pub name: String,
    pub utilization: f64,
    #[serde(rename = "resets_at")]
    pub resets_at: String,
    /// Whether this window's limit is currently active.
    ///
    /// Absent/null indicates the field was not populated in the API response;
    /// treat as active (not inactive) in this case. Only `false` definitively
    /// marks the window as structurally inactive.
    #[serde(default)]
    pub is_active: Option<bool>,
}

impl UsageWindow {
    /// Parse the resets_at timestamp and compute hours remaining
    pub fn hours_remaining(&self) -> Result<f64> {
        let reset_time: DateTime<Utc> = self
            .resets_at
            .parse()
            .context(format!("Failed to parse resets_at: {}", self.resets_at))?;
        let now = Utc::now();
        let duration = reset_time.signed_duration_since(now);
        Ok(duration.num_seconds() as f64 / 3600.0)
    }

    /// Set the window name (for when it's not deserialized with one)
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

/// A single entry in the usage API's generic `limits[]` array.
///
/// The usage API returns a list of active limits, each tagged with a `kind`
/// (`"session"`, `"weekly_all"`, `"weekly_scoped"`). This is the generalized
/// shape that will eventually replace the legacy top-level `seven_day` /
/// `five_hour` / `weekly_scoped` fields; for now it is parsed additively
/// alongside them (see child bead bf-3a3x7 for the window-generalization step).
///
/// Every field is optional with a `#[serde(default)]` so a single odd or
/// forward-incompatible entry in the array never fails the whole poll — the
/// same tolerance already applied to the legacy windows.
#[derive(Debug, Deserialize, Clone)]
pub struct UsageLimit {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    /// **Model-agnostic weekly_scoped utilization source field.**
    ///
    /// For entries where `kind == "weekly_scoped"`, this `percent` field is the
    /// authoritative source of the model-agnostic weekly_scoped utilization value,
    /// regardless of which model (Fable, Opus, Sonnet, etc.) is carrying the scoped
    /// cap this period.
    ///
    /// Data flow: API → `limits[].percent` → `UsageData.weekly_scoped_utilization`
    /// → `UsageState.weekly_scoped_pct` (state.rs:76-77).
    ///
    /// See `scoped_weekly()` (poller.rs:244-261) for the extraction logic that
    /// finds the weekly_scoped entry and reads this field.
    #[serde(default)]
    pub percent: Option<f64>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub resets_at: Option<String>,
    #[serde(default)]
    pub scope: Option<LimitScope>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

/// `scope` block on a model-scoped limit (e.g. a `weekly_scoped` cap).
#[derive(Debug, Deserialize, Clone)]
pub struct LimitScope {
    #[serde(default)]
    pub model: Option<LimitModel>,
}

/// The model a scoped limit applies to.
#[derive(Debug, Deserialize, Clone)]
pub struct LimitModel {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Full usage response from the API
///
/// Each window is optional: the usage API legitimately returns `null` for a
/// window that is not an active limit for the account (e.g. no separate sonnet
/// limit, or a window with no recorded usage yet). Treating these as required
/// structs made a single null crash the entire poll, leaving the governor with
/// no capacity data. They are `Option` so a null window is tolerated.
#[derive(Debug, Deserialize)]
pub struct UsageResponse {
    #[serde(rename = "weekly_scoped", default)]
    pub weekly_scoped: Option<UsageWindow>,
    #[serde(rename = "seven_day", default)]
    pub seven_day: Option<UsageWindow>,
    #[serde(rename = "five_hour", default)]
    pub five_hour: Option<UsageWindow>,
    /// Generic per-limit array returned alongside the legacy windows. Absent
    /// when the API response omits it (older/region variants); never fails the
    /// poll. Parsed additively — the legacy fields above are unchanged.
    #[serde(default)]
    pub limits: Option<Vec<UsageLimit>>,
}

/// Extract `(utilization, resets_at, hours_remaining)` from an optional window.
///
/// A null/absent window is treated as non-binding: 0% utilization with a far-off
/// reset, so the governor does not restrict scaling on a window the API did not
/// report as an active limit.
fn window_or_default(window: &Option<UsageWindow>) -> (f64, String, f64) {
    match window {
        Some(w) => (
            w.utilization,
            w.resets_at.clone(),
            w.hours_remaining().unwrap_or(0.0),
        ),
        None => (0.0, String::new(), 168.0),
    }
}

/// Formatted usage data for human or machine consumption
#[derive(Debug, Clone)]
pub struct UsageData {
    pub weekly_scoped_utilization: f64,
    pub weekly_scoped_resets_at: String,
    pub weekly_scoped_hours_remaining: f64,
    /// Resolved display name of the model the `weekly_scoped` window is scoped
    /// to (e.g. `"Fable"`), derived from [`UsageData::scoped_weekly`]. `None`
    /// when no active model-scoped weekly cap is present this period —
    /// consistent with the null-tolerance the windows already apply
    /// ([`window_or_default`] treats a null window as non-binding). Metadata
    /// only: the binding key stays the generic `"weekly_scoped"`; this field
    /// just lets downstream modules label which model that window tracks.
    pub weekly_scoped_model: Option<String>,
    pub seven_day_utilization: f64,
    pub seven_day_resets_at: String,
    pub seven_day_hours_remaining: f64,
    pub five_hour_utilization: f64,
    pub five_hour_resets_at: String,
    pub five_hour_hours_remaining: f64,
    /// Parsed entries from the generic `limits[]` array (empty when the API
    /// omits the array). Enables model-scoped lookups such as
    /// [`UsageData::scoped_weekly`].
    pub limits: Vec<UsageLimit>,
    /// Instant at which this usage reading was received from the API.
    ///
    /// A stale fallback clones this value from the last successful reading,
    /// so consumers can distinguish the age of the data from the time of the
    /// cycle that happens to report it.
    pub timestamp: DateTime<Utc>,
    pub stale: bool,
}

impl UsageData {
    /// The active model-scoped weekly cap, if any.
    ///
    /// **Model-agnostic data extraction from limits[].**
    ///
    /// Finds the `limits[]` entry with `kind == "weekly_scoped"` and returns
    /// its model display name (falling back to `"Scoped"`, matching the
    /// `usage-statusline.sh` reference) plus its `percent` / `resets_at` as a
    /// [`UsageWindow`].
    ///
    /// **Critical field access**: The `percent` field on the weekly_scoped entry
    /// is the authoritative source for model-agnostic weekly_scoped utilization.
    /// This field is read at line 256 below (`utilization: limit.percent.unwrap_or(0.0)`)
    /// and flows into `UsageState.weekly_scoped_pct` via the poll() method.
    ///
    /// Returns `None` when this account/period has no active model-scoped
    /// weekly cap — callers must not treat the scoped cap as a binding limit
    /// in that case. This never errors: a missing entry is normal.
    pub fn scoped_weekly(&self) -> Option<(String, UsageWindow)> {
        self.limits.iter().find_map(|limit| {
            if limit.kind.as_deref() != Some("weekly_scoped") {
                return None;
            }
            let model_name = limit
                .scope
                .as_ref()
                .and_then(|s| s.model.as_ref())
                .and_then(|m| m.display_name.clone())
                .unwrap_or_else(|| "Scoped".to_string());
            let window = UsageWindow {
                name: "weekly_scoped".to_string(),
                // **Model-agnostic weekly_scoped pct source: reads from limits[].percent**
                utilization: limit.percent.unwrap_or(0.0),
                resets_at: limit.resets_at.clone().unwrap_or_default(),
                is_active: None,
            };
            Some((model_name, window))
        })
    }

    /// Check if the weekly_scoped window is currently tracking Sonnet.
    ///
    /// Returns `true` if `weekly_scoped_model` indicates a Sonnet model
    /// (e.g. "Sonnet"), `false` for other models or when no model is known.
    ///
    /// **Note:** The legacy `sonnet_pct` field is deprecated and should NOT be used
    /// for weekly_scoped calculations. Use `weekly_scoped_pct` (model-agnostic) instead.
    /// See state.rs lines 53-56 for the deprecated sonnet_pct field documentation.
    pub fn is_weekly_scoped_sonnet(&self) -> bool {
        match self.weekly_scoped_model.as_deref() {
            Some(model) => {
                // Case-insensitive check for "Sonnet" display name
                model.eq_ignore_ascii_case("Sonnet")
            }
            None => false,
        }
    }
}

/// Consecutive refresh failure counter
static mut REFRESH_FAILURE_COUNT: u32 = 0;

/// Usage Poller
pub struct Poller {
    credentials_path: PathBuf,
    agent: Agent,
    last_usage: Option<UsageData>,
}

impl Poller {
    /// Create a new poller instance with the default credentials path
    pub fn new() -> Result<Self> {
        Self::with_credentials_path(None)
    }

    /// Create a new poller instance with a custom credentials path
    ///
    /// If `path` is None, uses the default ~/.claude/.credentials.json
    /// If `path` is Some, expands ~ to home directory if present
    pub fn with_credentials_path(path: Option<String>) -> Result<Self> {
        let credentials_path = if let Some(path_str) = path {
            // Expand ~ to home directory if present
            if path_str.starts_with('~') {
                let home_dir = dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
                home_dir.join(
                    path_str
                        .strip_prefix('~')
                        .unwrap_or("")
                        .trim_start_matches('/'),
                )
            } else {
                PathBuf::from(path_str)
            }
        } else {
            // Use default path
            let home_dir = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
            home_dir.join(CREDENTIALS_PATH)
        };

        // Build ureq agent with rustls TLS
        let agent = Agent::new();

        Ok(Self {
            credentials_path,
            agent,
            last_usage: None,
        })
    }

    /// Get the credentials file path
    #[allow(dead_code)]
    pub fn credentials_path(&self) -> &PathBuf {
        &self.credentials_path
    }

    /// Read and parse the credentials file with validation.
    ///
    /// Validates that credentials are not corrupted (empty tokens, zero expiry).
    /// This early detection prevents HTTP 400 errors from the refresh endpoint
    /// when trying to use empty refresh tokens (see bead bf-56ywhe).
    fn read_credentials(&self) -> Result<Credentials> {
        let content = fs::read_to_string(&self.credentials_path).map_err(|_| {
            anyhow::anyhow!(PollerError::CredentialsNotFound(
                self.credentials_path.clone()
            ))
        })?;

        let creds: Credentials = serde_json::from_str(&content).map_err(|e: serde_json::Error| {
            anyhow::anyhow!(PollerError::InvalidCredentials(e.to_string()))
        })?;

        // Validate credentials are not corrupted
        if creds.claude_ai_oauth.access_token.is_empty() {
            anyhow::bail!(
                "Credentials corrupted: access_token is empty (file: {})",
                self.credentials_path.display()
            );
        }
        if creds.claude_ai_oauth.refresh_token.is_empty() {
            anyhow::bail!(
                "Credentials corrupted: refresh_token is empty (file: {})",
                self.credentials_path.display()
            );
        }
        if creds.claude_ai_oauth.expires_at == 0 {
            anyhow::bail!(
                "Credentials corrupted: expires_at is zero (file: {})",
                self.credentials_path.display()
            );
        }

        Ok(creds)
    }

    /// Write updated credentials back to the file using atomic write pattern.
    ///
    /// Uses temp file + atomic rename to prevent corruption from concurrent writes
    /// or crashes mid-write. This addresses the root cause of recurring OAuth token
    /// refresh failures (see bead bf-56ywhe).
    fn write_credentials(&self, creds: &Credentials) -> Result<()> {
        let content =
            serde_json::to_string_pretty(creds).context("Failed to serialize credentials")?;

        // Create temp file in same directory as target (ensures same filesystem)
        let temp_path = self.credentials_path.with_extension("tmp");

        // Write to temp file
        {
            let mut file = File::create(&temp_path)
                .context("Failed to create temp credentials file")?;
            file.write_all(content.as_bytes())
                .context("Failed to write temp credentials file")?;
            // fsync to ensure data is on disk before rename
            file.sync_all()
                .context("Failed to sync temp credentials file")?;
        }

        // Atomic rename - overwrites target if it exists
        fs::rename(&temp_path, &self.credentials_path)
            .context("Failed to rename temp credentials file")?;

        Ok(())
    }

    /// Check if the token needs refresh
    fn needs_refresh(&self, expires_at: i64) -> bool {
        let now_ms = Utc::now().timestamp_millis();
        let threshold_ms = REFRESH_THRESHOLD_SECS * 1000;
        now_ms + threshold_ms >= expires_at
    }

    /// Refresh the OAuth token
    fn refresh_token(&self, refresh_token: &str) -> Result<RefreshResponse> {
        let payload = RefreshRequest {
            grant_type: "refresh_token".to_string(),
            refresh_token: refresh_token.to_string(),
        };

        let json_payload =
            serde_json::to_string(&payload).context("Failed to serialize refresh request")?;

        let response = self
            .agent
            .post(TOKEN_ENDPOINT)
            .set("Content-Type", "application/json")
            .set("User-Agent", USER_AGENT)
            .send_string(&json_payload)
            .map_err(|e| {
                anyhow::anyhow!(PollerError::TokenRefreshFailed(format!(
                    "HTTP error: {}",
                    e
                )))
            })?;

        if response.status() != 200 {
            let status = response.status();
            let text = response.into_string().unwrap_or_default();
            return Err(anyhow::anyhow!(PollerError::TokenRefreshFailed(format!(
                "HTTP {}: {}",
                status, text
            ))));
        }

        let response_text = response
            .into_string()
            .map_err(|e| anyhow::anyhow!(PollerError::ParseError(e.to_string())))?;

        let refresh_response: RefreshResponse =
            serde_json::from_str(&response_text).map_err(|e: serde_json::Error| {
                anyhow::anyhow!(PollerError::ParseError(e.to_string()))
            })?;

        Ok(refresh_response)
    }

    /// Get a valid access token, refreshing if necessary
    fn get_access_token(&self) -> Result<String> {
        let mut creds = self.read_credentials()?;

        if self.needs_refresh(creds.claude_ai_oauth.expires_at) {
            log::debug!("Token expiring soon, refreshing...");

            let refresh_token = creds.claude_ai_oauth.refresh_token.clone();

            // Attempt refresh with retry
            let refresh_response = self.attempt_refresh(&refresh_token)?;

            // Update credentials with new token data
            creds.claude_ai_oauth.access_token = refresh_response.access_token;
            creds.claude_ai_oauth.refresh_token = refresh_response.refresh_token;
            creds.claude_ai_oauth.expires_at = refresh_response.expires_at;

            // Write updated credentials
            self.write_credentials(&creds)?;

            // Reset failure counter on success
            unsafe {
                REFRESH_FAILURE_COUNT = 0;
            }

            log::debug!("Token refreshed successfully");

            Ok(creds.claude_ai_oauth.access_token)
        } else {
            Ok(creds.claude_ai_oauth.access_token)
        }
    }

    /// Attempt token refresh with retry logic
    fn attempt_refresh(&self, refresh_token: &str) -> Result<RefreshResponse> {
        // First attempt
        match self.refresh_token(refresh_token) {
            Ok(response) => return Ok(response),
            Err(e) => {
                log::warn!("Token refresh attempt 1 failed: {}", e);
            }
        }

        // Retry after delay
        log::info!(
            "Retrying token refresh in {} seconds...",
            REFRESH_RETRY_DELAY_SECS
        );
        std::thread::sleep(std::time::Duration::from_secs(REFRESH_RETRY_DELAY_SECS));

        match self.refresh_token(refresh_token) {
            Ok(response) => {
                log::info!("Token refresh retry succeeded");
                return Ok(response);
            }
            Err(e) => {
                log::warn!("Token refresh attempt 2 failed: {}", e);
            }
        }

        // Increment failure counter
        unsafe {
            REFRESH_FAILURE_COUNT += 1;
            if REFRESH_FAILURE_COUNT >= MAX_REFRESH_FAILURES {
                return Err(anyhow::anyhow!(PollerError::MaxRefreshFailures));
            }
        }

        Err(anyhow::anyhow!(PollerError::TokenRefreshFailed(
            "Refresh failed after retry".to_string()
        )))
    }

    /// Get the consecutive refresh failure count
    pub fn refresh_failure_count() -> u32 {
        unsafe { REFRESH_FAILURE_COUNT }
    }

    /// Fetch usage from the API
    fn fetch_usage(&self, access_token: &str) -> Result<UsageResponse> {
        let url = format!("{}{}", API_BASE, USAGE_ENDPOINT);

        let response = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", access_token))
            .set("anthropic-beta", "oauth-2025-04-20")
            .set("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| {
                anyhow::anyhow!(PollerError::ApiRequestFailed(format!("HTTP error: {}", e)))
            })?;

        if response.status() != 200 {
            let status = response.status();
            let text = response.into_string().unwrap_or_default();
            return Err(anyhow::anyhow!(PollerError::ApiError(format!(
                "HTTP {}: {}",
                status, text
            ))));
        }

        let response_text = response
            .into_string()
            .map_err(|e| anyhow::anyhow!(PollerError::ParseError(e.to_string())))?;

        let usage: UsageResponse =
            serde_json::from_str(&response_text).map_err(|e: serde_json::Error| {
                anyhow::anyhow!(PollerError::ParseError(e.to_string()))
            })?;

        Ok(usage)
    }

    /// Poll usage data
    ///
    /// This is the main entry point for polling usage. It handles:
    /// - Token refresh if needed
    /// - API call to fetch usage
    /// - Fallback to stale data on refresh failure
    pub fn poll(&mut self) -> Result<UsageData> {
        let access_token = match self.get_access_token() {
            Ok(token) => token,
            Err(e) => {
                // Check if this is a refresh failure
                if e.downcast_ref::<PollerError>().is_some() {
                    log::warn!("Token refresh failed, checking for stale data...");

                    if let Some(last) = &self.last_usage {
                        let age = Utc::now().signed_duration_since(last.timestamp);
                        log::warn!("Using stale data (age: {}s)", age.num_seconds());

                        // Return stale data with the stale flag set
                        return Ok(UsageData {
                            stale: true,
                            ..last.clone()
                        });
                    }

                    log::error!("No stale data available");
                }
                return Err(e);
            }
        };

        let usage = self.fetch_usage(&access_token)?;
        // Capture the reading's timestamp at the poll boundary. This value is
        // deliberately retained when the reading is later served from the
        // stale cache; it is not the timestamp of a governor cycle.
        let reading_at = Utc::now();

        // Extract per-window fields, tolerating windows the API returns as null
        // (a null window is treated as non-binding rather than failing the poll).
        let (seven_day_utilization, seven_day_resets_at, seven_day_hours) =
            window_or_default(&usage.seven_day);
        let (five_hour_utilization, five_hour_resets_at, five_hour_hours) =
            window_or_default(&usage.five_hour);

        let mut data = UsageData {
            // weekly_scoped fields are populated below from the model-agnostic limits[] source
            weekly_scoped_utilization: 0.0,
            weekly_scoped_resets_at: String::new(),
            weekly_scoped_hours_remaining: 168.0,
            weekly_scoped_model: None,
            seven_day_utilization,
            seven_day_resets_at,
            seven_day_hours_remaining: seven_day_hours,
            five_hour_utilization,
            five_hour_resets_at,
            five_hour_hours_remaining: five_hour_hours,
            limits: usage.limits.unwrap_or_default(),
            timestamp: reading_at,
            stale: false,
        };

        // Populate weekly_scoped from the model-agnostic limits[] array.
        //
        // **Data flow from model-agnostic source:**
        // 1. API response: `limits[].percent` (where `kind == "weekly_scoped"`)
        // 2. Extracted by: `scoped_weekly()` method (poller.rs:256)
        // 3. Flows into: `UsageData.weekly_scoped_utilization`
        // 4. Ultimately stored in: `UsageState.weekly_scoped_pct` (state.rs:76-77)
        //
        // This is the authoritative source for weekly_scoped utilization regardless
        // of which model (Fable, Opus, Sonnet, etc.) is carrying the scoped cap.
        if let Some((model_name, window)) = data.scoped_weekly() {
            data.weekly_scoped_utilization = window.utilization;
            data.weekly_scoped_hours_remaining = window.hours_remaining().unwrap_or(168.0);
            data.weekly_scoped_resets_at = window.resets_at;
            data.weekly_scoped_model = Some(model_name);
        }

        // Update last usage
        self.last_usage = Some(data.clone());

        Ok(data)
    }

    /// Check if the poller should create a HUMAN alert bead
    pub fn should_alert(&self) -> bool {
        Self::refresh_failure_count() >= MAX_REFRESH_FAILURES
    }
}

impl Default for Poller {
    fn default() -> Self {
        Self::new().expect("Failed to create Poller")
    }
}

/// Source of usage data for a governor cycle.
///
/// The real [`Poller`] hits the Anthropic usage API; tests substitute a mock so a
/// cycle can be exercised without credentials or network access.
pub trait UsagePoller {
    /// Fetch the current usage snapshot.
    fn poll_usage(&mut self) -> Result<UsageData>;
}

impl UsagePoller for Poller {
    fn poll_usage(&mut self) -> Result<UsageData> {
        self.poll()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a test credentials file
    fn create_test_credentials(dir: &TempDir, expires_at_ms: i64) -> PathBuf {
        let creds_path = dir.path().join(".credentials.json");
        let creds = Credentials {
            claude_ai_oauth: OAuthData {
                access_token: "test_access_token".to_string(),
                refresh_token: "test_refresh_token".to_string(),
                expires_at: expires_at_ms,
                scopes: vec!["user:inference".to_string()],
            },
        };
        fs::write(&creds_path, serde_json::to_string_pretty(&creds).unwrap()).unwrap();
        creds_path
    }

    #[test]
    fn test_usage_window_hours_remaining() {
        let window = UsageWindow {
            name: String::new(),
            utilization: 75.0,
            resets_at: "2026-03-18T20:00:00Z".to_string(),
            is_active: None,
        };
        // This test just ensures parsing works; the actual value depends on current time
        let result = window.hours_remaining();
        assert!(result.is_ok());
    }

    #[test]
    fn test_needs_refresh_true() {
        // Expired token
        let now_ms = Utc::now().timestamp_millis();
        let expired = now_ms - 1000;
        assert!(Poller::new().unwrap().needs_refresh(expired));

        // Token expiring in 2 minutes (within 5 minute threshold)
        let soon = now_ms + (120 * 1000);
        assert!(Poller::new().unwrap().needs_refresh(soon));
    }

    #[test]
    fn test_needs_refresh_false() {
        // Token valid for 10 minutes (outside 5 minute threshold)
        let future = Utc::now().timestamp_millis() + (600 * 1000);
        assert!(!Poller::new().unwrap().needs_refresh(future));
    }

    #[test]
    fn test_credentials_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let creds_path =
            create_test_credentials(&temp_dir, Utc::now().timestamp_millis() + 3600000);

        let content = fs::read_to_string(&creds_path).unwrap();
        let creds: Credentials = serde_json::from_str(&content).unwrap();

        assert_eq!(creds.claude_ai_oauth.access_token, "test_access_token");
        assert_eq!(creds.claude_ai_oauth.refresh_token, "test_refresh_token");
    }

    #[test]
    fn test_refresh_request_serialization() {
        let req = RefreshRequest {
            grant_type: "refresh_token".to_string(),
            refresh_token: "test_token".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"grantType\":\"refresh_token\""));
        assert!(json.contains("\"refreshToken\":\"test_token\""));
    }

    #[test]
    fn test_usage_data_from_response() {
        let response = UsageResponse {
            weekly_scoped: Some(UsageWindow {
                name: String::new(),
                utilization: 75.5,
                resets_at: "2026-03-20T03:59:59Z".to_string(),
                is_active: None,
            }),
            seven_day: Some(UsageWindow {
                name: String::new(),
                utilization: 60.0,
                resets_at: "2026-03-20T03:00:00Z".to_string(),
                is_active: None,
            }),
            five_hour: Some(UsageWindow {
                name: String::new(),
                utilization: 30.0,
                resets_at: "2026-03-18T15:59:59Z".to_string(),
                is_active: None,
            }),
            limits: None,
        };

        assert_eq!(response.weekly_scoped.as_ref().unwrap().utilization, 75.5);
        assert_eq!(response.five_hour.as_ref().unwrap().utilization, 30.0);
    }

    #[test]
    fn test_null_window_is_tolerated() {
        // A window the API returns as null must not fail deserialization, and
        // must be treated as a non-binding limit (0% utilization).
        let json = r#"{"weekly_scoped": null,
                       "seven_day": {"utilization": 42.0, "resets_at": "2026-03-20T03:00:00Z"},
                       "five_hour": {"utilization": 10.0, "resets_at": "2026-03-18T15:59:59Z"}}"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("null window must parse");
        assert!(resp.weekly_scoped.is_none());
        let (util, resets_at, _hours) = window_or_default(&resp.weekly_scoped);
        assert_eq!(util, 0.0);
        assert!(resets_at.is_empty());
        assert_eq!(resp.seven_day.as_ref().unwrap().utilization, 42.0);
    }

    /// Build a `UsageData` carrying only the parsed `limits[]`, so the
    /// `scoped_weekly()` accessor can be exercised in isolation.
    fn usage_data_with_limits(limits: Vec<UsageLimit>) -> UsageData {
        let mut data = UsageData {
            weekly_scoped_utilization: 0.0,
            weekly_scoped_resets_at: String::new(),
            weekly_scoped_hours_remaining: 0.0,
            weekly_scoped_model: None,
            seven_day_utilization: 0.0,
            seven_day_resets_at: String::new(),
            seven_day_hours_remaining: 0.0,
            five_hour_utilization: 0.0,
            five_hour_resets_at: String::new(),
            five_hour_hours_remaining: 0.0,
            limits,
            timestamp: Utc::now(),
            stale: false,
        };
        // Mirror poll(): resolve the carrier field from the parsed limits so the
        // returned UsageData is realistic.
        data.weekly_scoped_model = data.scoped_weekly().map(|(model, _)| model);
        data
    }

    #[test]
    fn test_limits_array_parses_alongside_legacy_windows() {
        // The real captured shape: legacy top-level windows coexist with the
        // generic limits[] array. Both must parse from a single response.
        let json = r#"{
            "weekly_scoped": null,
            "seven_day": {"utilization": 42.0, "resets_at": "2026-08-01T03:00:00Z"},
            "five_hour": {"utilization": 10.0, "resets_at": "2026-07-26T20:00:00Z"},
            "limits": [
                {"kind": "session", "group": "default", "percent": 10, "severity": "low",
                 "resets_at": "2026-07-26T20:00:00Z", "scope": null, "is_active": true},
                {"kind": "weekly_all", "percent": 45, "resets_at": "2026-08-01T03:00:00Z",
                 "scope": null, "is_active": true},
                {"kind": "weekly_scoped", "percent": 79, "resets_at": "2026-08-01T03:59:59Z",
                 "scope": {"model": {"id": "claude-fable-5", "display_name": "Fable"}},
                 "is_active": true}
            ]
        }"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("limits response must parse");

        // Legacy parsing is untouched.
        assert!(resp.weekly_scoped.is_none());
        assert_eq!(resp.seven_day.as_ref().unwrap().utilization, 42.0);
        assert_eq!(resp.five_hour.as_ref().unwrap().utilization, 10.0);

        // The generic array parsed all three entries.
        let limits = resp.limits.expect("limits array should be present");
        assert_eq!(limits.len(), 3);
        assert_eq!(limits[0].kind.as_deref(), Some("session"));
        assert_eq!(limits[2].kind.as_deref(), Some("weekly_scoped"));
    }

    #[test]
    fn test_scoped_weekly_returns_real_captured_shape() {
        // Real captured weekly_scoped entry: display_name "Fable", percent 79.
        let json = r#"{
            "seven_day": {"utilization": 42.0, "resets_at": "2026-08-01T03:00:00Z"},
            "five_hour": {"utilization": 10.0, "resets_at": "2026-07-26T20:00:00Z"},
            "limits": [
                {"kind": "session", "percent": 10, "resets_at": "2026-07-26T20:00:00Z",
                 "scope": null},
                {"kind": "weekly_scoped", "percent": 79, "resets_at": "2026-08-01T03:59:59Z",
                 "scope": {"model": {"id": "claude-fable-5", "display_name": "Fable"}}}
            ]
        }"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("response must parse");
        let data = usage_data_with_limits(resp.limits.unwrap_or_default());

        let (model, window) = data
            .scoped_weekly()
            .expect("weekly_scoped entry should be found");
        assert_eq!(model, "Fable");
        assert_eq!(window.utilization, 79.0);
        assert_eq!(window.resets_at, "2026-08-01T03:59:59Z");
    }

    #[test]
    fn test_scoped_weekly_none_when_entry_omitted() {
        // limits[] present but has no weekly_scoped entry -> None, no panic.
        let json = r#"{
            "five_hour": {"utilization": 10.0, "resets_at": "2026-07-26T20:00:00Z"},
            "limits": [
                {"kind": "session", "percent": 10, "resets_at": "2026-07-26T20:00:00Z",
                 "scope": null},
                {"kind": "weekly_all", "percent": 45, "resets_at": "2026-08-01T03:00:00Z",
                 "scope": null}
            ]
        }"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("response must parse");
        let data = usage_data_with_limits(resp.limits.unwrap_or_default());
        assert!(data.scoped_weekly().is_none());
    }

    #[test]
    fn test_scoped_weekly_none_when_limits_absent() {
        // limits[] key entirely absent -> None, no panic, no error.
        let json = r#"{
            "seven_day": {"utilization": 42.0, "resets_at": "2026-08-01T03:00:00Z"},
            "five_hour": {"utilization": 10.0, "resets_at": "2026-07-26T20:00:00Z"}
        }"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("response must parse");
        assert!(resp.limits.is_none());
        let data = usage_data_with_limits(resp.limits.unwrap_or_default());
        assert!(data.scoped_weekly().is_none());
    }

    #[test]
    fn test_scoped_weekly_falls_back_to_scoped_label() {
        // A weekly_scoped entry whose scope lacks display_name falls back to
        // the "Scoped" label, matching usage-statusline.sh.
        let json = r#"{
            "limits": [
                {"kind": "weekly_scoped", "percent": 50, "resets_at": "2026-08-01T03:59:59Z",
                 "scope": {"model": {}}}
            ]
        }"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("response must parse");
        let data = usage_data_with_limits(resp.limits.unwrap_or_default());
        let (model, window) = data
            .scoped_weekly()
            .expect("weekly_scoped entry should be found even without a label");
        assert_eq!(model, "Scoped");
        assert_eq!(window.utilization, 50.0);
    }

    #[test]
    fn test_weekly_scoped_model_carries_resolved_display_name() {
        // A fixture reporting weekly_scoped=Fable: the carrier field resolves to
        // Some("Fable"), matching scoped_weekly()'s resolved display name. This
        // proves the resolved model name leaves the poller as data (the field
        // is populated the same way poll() populates it).
        let json = r#"{
            "limits": [
                {"kind": "weekly_scoped", "percent": 79, "resets_at": "2026-08-01T03:59:59Z",
                 "scope": {"model": {"id": "claude-fable-5", "display_name": "Fable"}}}
            ]
        }"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("response must parse");
        let data = usage_data_with_limits(resp.limits.unwrap_or_default());

        assert_eq!(data.weekly_scoped_model.as_deref(), Some("Fable"));
        // The carrier field agrees with the accessor it is derived from.
        assert_eq!(data.scoped_weekly().unwrap().0, "Fable");
    }

    #[test]
    fn test_weekly_scoped_model_none_when_no_scoped_cap() {
        // No weekly_scoped entry -> the carrier field is None, consistent with
        // scoped_weekly() returning None and window_or_default treating a null
        // window as non-binding.
        let json = r#"{
            "limits": [
                {"kind": "session", "percent": 10, "resets_at": "2026-07-26T20:00:00Z",
                 "scope": null},
                {"kind": "weekly_all", "percent": 45, "resets_at": "2026-08-01T03:00:00Z",
                 "scope": null}
            ]
        }"#;
        let resp: UsageResponse = serde_json::from_str(json).expect("response must parse");
        let data = usage_data_with_limits(resp.limits.unwrap_or_default());

        assert!(data.weekly_scoped_model.is_none());
        assert!(data.scoped_weekly().is_none());
    }

    #[test]
    fn test_is_weekly_scoped_sonnet_true_for_sonnet() {
        // When weekly_scoped_model is "Sonnet", return true
        let mut data = usage_data_with_limits(vec![]);
        data.weekly_scoped_model = Some("Sonnet".to_string());
        assert!(data.is_weekly_scoped_sonnet());
    }

    #[test]
    fn test_is_weekly_scoped_sonnet_true_for_sonnet_case_insensitive() {
        // Case-insensitive matching
        let mut data = usage_data_with_limits(vec![]);
        data.weekly_scoped_model = Some("sonnet".to_string());
        assert!(data.is_weekly_scoped_sonnet());

        data.weekly_scoped_model = Some("SONNET".to_string());
        assert!(data.is_weekly_scoped_sonnet());
    }

    #[test]
    fn test_is_weekly_scoped_sonnet_false_for_other_models() {
        // For non-Sonnet models, return false
        let mut data = usage_data_with_limits(vec![]);
        data.weekly_scoped_model = Some("Fable".to_string());
        assert!(!data.is_weekly_scoped_sonnet());

        data.weekly_scoped_model = Some("Opus".to_string());
        assert!(!data.is_weekly_scoped_sonnet());

        data.weekly_scoped_model = Some("Haiku".to_string());
        assert!(!data.is_weekly_scoped_sonnet());
    }

    #[test]
    fn test_is_weekly_scoped_sonnet_false_when_none() {
        // When weekly_scoped_model is None, return false
        let data = usage_data_with_limits(vec![]);
        assert!(data.weekly_scoped_model.is_none());
        assert!(!data.is_weekly_scoped_sonnet());
    }
}
