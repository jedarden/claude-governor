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
use std::fs;
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
const USER_AGENT: &str = "claude-code/2.1.78";

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
    pub utilization: f64,
    #[serde(rename = "resets_at")]
    pub resets_at: String,
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
}

/// Full usage response from the API
#[derive(Debug, Deserialize)]
pub struct UsageResponse {
    #[serde(rename = "seven_day_sonnet")]
    pub seven_day_sonnet: UsageWindow,
    #[serde(rename = "seven_day")]
    pub seven_day: UsageWindow,
    #[serde(rename = "five_hour")]
    pub five_hour: UsageWindow,
}

/// Formatted usage data for human or machine consumption
#[derive(Debug, Clone)]
pub struct UsageData {
    pub seven_day_sonnet_utilization: f64,
    pub seven_day_sonnet_resets_at: String,
    pub seven_day_sonnet_hours_remaining: f64,
    pub seven_day_utilization: f64,
    pub seven_day_resets_at: String,
    pub seven_day_hours_remaining: f64,
    pub five_hour_utilization: f64,
    pub five_hour_resets_at: String,
    pub five_hour_hours_remaining: f64,
    pub timestamp: DateTime<Utc>,
    pub stale: bool,
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
    /// Create a new poller instance
    pub fn new() -> Result<Self> {
        let home_dir =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let credentials_path = home_dir.join(CREDENTIALS_PATH);

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

    /// Read and parse the credentials file
    fn read_credentials(&self) -> Result<Credentials> {
        let content = fs::read_to_string(&self.credentials_path).map_err(|_| {
            anyhow::anyhow!(PollerError::CredentialsNotFound(
                self.credentials_path.clone()
            ))
        })?;

        serde_json::from_str(&content).map_err(|e: serde_json::Error| {
            anyhow::anyhow!(PollerError::InvalidCredentials(e.to_string()))
        })
    }

    /// Write updated credentials back to the file
    fn write_credentials(&self, creds: &Credentials) -> Result<()> {
        let content =
            serde_json::to_string_pretty(creds).context("Failed to serialize credentials")?;

        fs::write(&self.credentials_path, content).context("Failed to write credentials file")?;

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

        // Compute hours remaining for each window
        let seven_day_sonnet_hours = usage.seven_day_sonnet.hours_remaining().unwrap_or(0.0);
        let seven_day_hours = usage.seven_day.hours_remaining().unwrap_or(0.0);
        let five_hour_hours = usage.five_hour.hours_remaining().unwrap_or(0.0);

        let data = UsageData {
            seven_day_sonnet_utilization: usage.seven_day_sonnet.utilization,
            seven_day_sonnet_resets_at: usage.seven_day_sonnet.resets_at.clone(),
            seven_day_sonnet_hours_remaining: seven_day_sonnet_hours,
            seven_day_utilization: usage.seven_day.utilization,
            seven_day_resets_at: usage.seven_day.resets_at.clone(),
            seven_day_hours_remaining: seven_day_hours,
            five_hour_utilization: usage.five_hour.utilization,
            five_hour_resets_at: usage.five_hour.resets_at.clone(),
            five_hour_hours_remaining: five_hour_hours,
            timestamp: Utc::now(),
            stale: false,
        };

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
            utilization: 75.0,
            resets_at: "2026-03-18T20:00:00Z".to_string(),
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
            seven_day_sonnet: UsageWindow {
                utilization: 75.5,
                resets_at: "2026-03-20T03:59:59Z".to_string(),
            },
            seven_day: UsageWindow {
                utilization: 60.0,
                resets_at: "2026-03-20T03:00:00Z".to_string(),
            },
            five_hour: UsageWindow {
                utilization: 30.0,
                resets_at: "2026-03-18T15:59:59Z".to_string(),
            },
        };

        assert_eq!(response.seven_day_sonnet.utilization, 75.5);
        assert_eq!(response.five_hour.utilization, 30.0);
    }
}
