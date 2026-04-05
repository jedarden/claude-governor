//! Configuration loading for Claude Governor
//!
//! Loads configuration from `governor.yaml` which contains:
//! - Model pricing tables
//! - Target utilization settings
//! - Other governor settings

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Main governor configuration loaded from governor.yaml
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct GovernorConfig {
    /// Model pricing configuration
    pub pricing: PricingConfig,

    /// Underutilization sprint configuration
    #[serde(default)]
    pub sprint: SprintConfig,

    /// Daemon configuration
    #[serde(default)]
    pub daemon: DaemonConfig,

    /// Alert configuration
    #[serde(default)]
    pub alerts: AlertConfig,

    /// Composite risk optimization configuration
    #[serde(default)]
    pub composite_risk: CompositeRiskConfig,

    /// Agent configurations (worker pools that can be scaled by the governor)
    #[serde(default)]
    pub agents: std::collections::HashMap<String, AgentConfig>,
}

/// Per-agent configuration for worker pool scaling
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct AgentConfig {
    /// Command to launch a new worker instance
    /// Example: "needle run --agent claude-code-glm-5 --workspace /home/coding/claude-governor"
    pub launch_cmd: String,

    /// Glob pattern for matching tmux session names
    /// Example: "needle-claude-*"
    pub session_pattern: String,

    /// Directory containing heartbeat JSON files (supports ~ expansion)
    /// Example: "~/.needle/state/heartbeats"
    pub heartbeat_dir: String,

    /// Minimum workers for this agent (default: 0)
    #[serde(default)]
    pub min_workers: u32,

    /// Maximum workers for this agent (default: 8)
    #[serde(default = "default_max_workers")]
    pub max_workers: u32,
}

fn default_max_workers() -> u32 { 8 }

impl AgentConfig {
    /// Get the heartbeat directory with tilde (~) expanded to the home directory
    pub fn heartbeat_dir_expanded(&self) -> PathBuf {
        if self.heartbeat_dir.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                return home.join(self.heartbeat_dir.strip_prefix('~').unwrap_or("").trim_start_matches('/'));
            }
        }
        PathBuf::from(&self.heartbeat_dir)
    }

    /// Extract the session prefix from the session pattern
    /// E.g., "needle-claude-*" -> "needle-claude"
    pub fn session_prefix(&self) -> &str {
        // Strip trailing wildcards to get the prefix
        self.session_pattern
            .trim_end_matches('*')
            .trim_end_matches('-')
    }
}

/// Daemon mode selection
#[derive(Debug, Deserialize, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DaemonMode {
    /// Auto-detect: use systemd if available, otherwise tmux
    Auto,
    /// Use systemd user units
    Systemd,
    /// Use tmux sessions
    Tmux,
}

impl Default for DaemonMode {
    fn default() -> Self {
        Self::Auto
    }
}

/// Daemon configuration
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct DaemonConfig {
    /// Loop interval in seconds (default: 300)
    ///
    /// 300s (5 min) is the minimum useful polling interval — the Anthropic API
    /// percentage values don't change visibly at shorter intervals, which keeps
    /// window_pct_deltas at zero and breaks the burn-rate EMA.
    #[serde(default = "default_loop_interval_secs")]
    pub loop_interval_secs: u64,

    /// Hysteresis band for scaling decisions (default: 1.0)
    /// Scaling only occurs when target differs from current by more than this
    #[serde(default = "default_hysteresis_band")]
    pub hysteresis_band: f64,

    /// Maximum workers to scale up per cycle (default: 1)
    #[serde(default = "default_max_scale_up_per_cycle")]
    pub max_scale_up_per_cycle: u32,

    /// Maximum workers to scale down per cycle (default: 1)
    #[serde(default = "default_max_scale_down_per_cycle")]
    pub max_scale_down_per_cycle: u32,

    /// Minimum time between scale operations in seconds (default: 60)
    #[serde(default = "default_min_scale_interval_secs")]
    pub min_scale_interval_secs: u64,

    /// Target utilization ceiling percentage (default: 90.0)
    #[serde(default = "default_target_ceiling")]
    pub target_ceiling: f64,

    /// Daemon mode: auto, systemd, or tmux (default: auto)
    #[serde(default)]
    pub mode: DaemonMode,

    /// Pre-scale look-ahead for multiplier transitions in minutes (default: 30, 0 = disabled)
    ///
    /// When a peak/off-peak transition is within this window, the governor
    /// pre-emptively scales down if the post-transition multiplier is lower.
    /// Conservative-only: pre-scales down before losing bonus, never pre-scales up.
    #[serde(default = "default_pre_scale_minutes")]
    pub pre_scale_minutes: u64,
}

fn default_loop_interval_secs() -> u64 { 300 }
fn default_hysteresis_band() -> f64 { 1.0 }
fn default_max_scale_up_per_cycle() -> u32 { 1 }
fn default_max_scale_down_per_cycle() -> u32 { 1 }
fn default_min_scale_interval_secs() -> u64 { 60 }
fn default_target_ceiling() -> f64 { 90.0 }
fn default_pre_scale_minutes() -> u64 { 30 }

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            loop_interval_secs: default_loop_interval_secs(), // 300
            hysteresis_band: default_hysteresis_band(),
            max_scale_up_per_cycle: default_max_scale_up_per_cycle(),
            max_scale_down_per_cycle: default_max_scale_down_per_cycle(),
            min_scale_interval_secs: default_min_scale_interval_secs(),
            target_ceiling: default_target_ceiling(),
            mode: DaemonMode::Auto,
            pre_scale_minutes: default_pre_scale_minutes(),
        }
    }
}

/// Sprint trigger configuration
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct SprintConfig {
    /// Utilization percentage below which sprint triggers (default: 50%)
    #[serde(default = "default_underutilization_threshold_pct")]
    pub underutilization_threshold_pct: f64,

    /// Hours remaining below which sprint triggers (default: 2 hours)
    #[serde(default = "default_underutilization_hours_remaining")]
    pub underutilization_hours_remaining: f64,

    /// End-of-window sprint: horizon in minutes (default: 90)
    /// Sprint only triggers if window resets within this time
    #[serde(default = "default_horizon_minutes")]
    pub horizon_minutes: f64,

    /// End-of-window sprint: minimum remaining headroom percentage (default: 15%)
    /// Sprint only triggers if remaining headroom > this value
    #[serde(default = "default_min_headroom_pct")]
    pub min_headroom_pct: f64,

    /// End-of-window sprint: max workers boost (default: 3)
    /// Temporarily raises max_workers by this amount during sprint
    #[serde(default = "default_max_workers_boost")]
    pub max_workers_boost: u32,

    /// End-of-window sprint: confidence cone max ratio (default: 2.0)
    /// Sprint blocked if cone ratio exceeds this (predictions too uncertain)
    #[serde(default = "default_max_cone_ratio")]
    pub max_cone_ratio: f64,

    /// End-of-window sprint: minimum headroom to continue sprint (default: 5%)
    /// Sprint ends if headroom drops below this
    #[serde(default = "default_sprint_end_headroom_pct")]
    pub sprint_end_headroom_pct: f64,
}

fn default_underutilization_threshold_pct() -> f64 {
    50.0
}

fn default_underutilization_hours_remaining() -> f64 {
    2.0
}

fn default_horizon_minutes() -> f64 {
    90.0
}

fn default_min_headroom_pct() -> f64 {
    15.0
}

fn default_max_workers_boost() -> u32 {
    3
}

fn default_max_cone_ratio() -> f64 {
    2.0
}

fn default_sprint_end_headroom_pct() -> f64 {
    5.0
}

impl Default for SprintConfig {
    fn default() -> Self {
        Self {
            underutilization_threshold_pct: default_underutilization_threshold_pct(),
            underutilization_hours_remaining: default_underutilization_hours_remaining(),
            horizon_minutes: default_horizon_minutes(),
            min_headroom_pct: default_min_headroom_pct(),
            max_workers_boost: default_max_workers_boost(),
            max_cone_ratio: default_max_cone_ratio(),
            sprint_end_headroom_pct: default_sprint_end_headroom_pct(),
        }
    }
}

/// Composite risk optimization configuration
///
/// When enabled, the governor considers all windows' risk levels together
/// rather than just the binding window's ceiling. This allows scaling higher
/// when one window is near reset but another has ample capacity.
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct CompositeRiskConfig {
    /// Enable composite risk optimization (default: false)
    ///
    /// When false, uses strict binding-window-only ceiling.
    /// When true, uses weighted composite risk across all windows.
    #[serde(default)]
    pub enabled: bool,

    /// Cost threshold for composite risk optimization (default: 0.0)
    ///
    /// - 0.0: strict binding-window behavior (only binding window constrains)
    /// - Higher values: allow more workers when non-binding windows have ample capacity
    ///
    /// The cost function weights each window by (margin_hrs / hours_remaining).
    /// A window near exhaustion (low margin) has higher cost/urgency.
    /// Setting cost_threshold > 0 allows scaling higher when the composite
    /// weighted cost is below the threshold.
    #[serde(default)]
    pub cost_threshold: f64,

    /// Weight for the binding window in composite calculation (default: 2.0)
    ///
    /// The binding window gets extra weight to ensure it remains the primary
    /// constraint even when considering other windows.
    #[serde(default = "default_binding_weight")]
    pub binding_weight: f64,
}

fn default_binding_weight() -> f64 { 2.0 }

impl Default for CompositeRiskConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cost_threshold: 0.0,
            binding_weight: default_binding_weight(),
        }
    }
}

/// Alert configuration
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct AlertConfig {
    /// Command to execute when an alert fires (default: br create --type human)
    /// The alert message is appended as the last argument.
    #[serde(default = "default_alert_command")]
    pub command: Vec<String>,

    /// Cooldown period in minutes between repeated alerts of the same type
    #[serde(default = "default_cooldown_minutes")]
    pub cooldown_minutes: i64,

    /// Whether alerts are enabled (default: true)
    #[serde(default = "default_alerts_enabled")]
    pub enabled: bool,

    /// Minimum severity level to fire alerts (default: "warning")
    /// Options: "info", "warning", "critical"
    #[serde(default = "default_min_severity")]
    pub min_severity: String,

    /// fleet_cache_eff below this fraction triggers a LowCacheEfficiency alert
    /// (default: 0.30 — warn when less than 30% of input tokens come from cache)
    #[serde(default = "default_low_cache_eff_threshold")]
    pub low_cache_eff_threshold: f64,

    /// Number of consecutive intervals with low cache efficiency before alerting
    /// (default: 5 — roughly 25 minutes at 5-minute collection intervals)
    #[serde(default = "default_low_cache_eff_intervals")]
    pub low_cache_eff_intervals: u32,
}

fn default_alert_command() -> Vec<String> {
    vec!["br".to_string(), "create".to_string(), "--type".to_string(), "human".to_string()]
}

fn default_cooldown_minutes() -> i64 {
    60
}

fn default_alerts_enabled() -> bool {
    true
}

fn default_min_severity() -> String {
    "warning".to_string()
}

fn default_low_cache_eff_threshold() -> f64 {
    0.30
}

fn default_low_cache_eff_intervals() -> u32 {
    5
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            command: default_alert_command(),
            cooldown_minutes: default_cooldown_minutes(),
            enabled: default_alerts_enabled(),
            min_severity: default_min_severity(),
            low_cache_eff_threshold: default_low_cache_eff_threshold(),
            low_cache_eff_intervals: default_low_cache_eff_intervals(),
        }
    }
}

/// Pricing configuration for all models
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct PricingConfig {
    /// Map of model name to pricing details
    pub models: std::collections::HashMap<String, ModelPricing>,
}

/// Per-model pricing rates (USD per million tokens)
#[derive(Debug, Deserialize, Clone, serde::Serialize)]
pub struct ModelPricing {
    /// Input tokens price per million tokens
    pub input_per_mtok: f64,

    /// Output tokens price per million tokens
    pub output_per_mtok: f64,

    /// Cache write (5m) price per million tokens
    pub cache_write_5m_per_mtok: f64,

    /// Cache write (1h) price per million tokens
    pub cache_write_1h_per_mtok: f64,

    /// Cache read price per million tokens
    pub cache_read_per_mtok: f64,
}

impl GovernorConfig {
    /// Load configuration from the default path
    ///
    /// Default paths (tried in order):
    /// 1. `$XDG_CONFIG_HOME/claude-governor/governor.yaml`
    /// 2. `~/.config/claude-governor/governor.yaml`
    /// 3. `./config/governor.yaml` (for development)
    pub fn load() -> Result<Self> {
        let paths = Self::config_paths();

        for path in &paths {
            if path.exists() {
                return Self::load_from_path(path);
            }
        }

        // If no config found, try to create default in the first location
        let first_path = &paths[0];
        Self::create_default_config(first_path)?;
        Self::load_from_path(first_path)
    }

    /// Load configuration from a specific path
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: GovernorConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        Ok(config)
    }

    /// Get the default config paths to try
    fn config_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // XDG config directory
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            paths.push(PathBuf::from(xdg_config).join("claude-governor/governor.yaml"));
        }

        // Fallback to ~/.config
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".config/claude-governor/governor.yaml"));
        }

        // Development path
        paths.push(PathBuf::from("config/governor.yaml"));

        paths
    }

    /// Create a default config file at the given path
    fn create_default_config(path: &Path) -> Result<()> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
        }

        let default_yaml = include_str!("../config/governor.yaml");

        fs::write(path, default_yaml)
            .with_context(|| format!("Failed to write default config: {}", path.display()))?;

        log::info!("Created default config at: {}", path.display());
        Ok(())
    }

    /// Get pricing for a specific model
    ///
    /// Returns None if the model is not found
    pub fn get_pricing(&self, model: &str) -> Option<&ModelPricing> {
        self.pricing.models.get(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pricing_config() {
        let yaml = r#"
pricing:
  models:
    claude-sonnet-4-20250514:
      input_per_mtok: 3.0
      output_per_mtok: 15.0
      cache_write_5m_per_mtok: 3.75
      cache_write_1h_per_mtok: 6.0
      cache_read_per_mtok: 0.30
"#;

        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.pricing.models.len(), 1);

        let pricing = config.pricing.models.get("claude-sonnet-4-20250514").unwrap();
        assert_eq!(pricing.input_per_mtok, 3.0);
        assert_eq!(pricing.output_per_mtok, 15.0);
        assert_eq!(pricing.cache_write_5m_per_mtok, 3.75);
        assert_eq!(pricing.cache_write_1h_per_mtok, 6.0);
        assert_eq!(pricing.cache_read_per_mtok, 0.30);

        // Default sprint config
        assert_eq!(config.sprint.underutilization_threshold_pct, 50.0);
        assert_eq!(config.sprint.underutilization_hours_remaining, 2.0);
    }

    #[test]
    fn test_sprint_config_defaults() {
        let yaml = r#"
pricing:
  models: {}
"#;
        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.sprint.underutilization_threshold_pct, 50.0);
        assert_eq!(config.sprint.underutilization_hours_remaining, 2.0);
        assert_eq!(config.sprint.horizon_minutes, 90.0);
        assert_eq!(config.sprint.min_headroom_pct, 15.0);
        assert_eq!(config.sprint.max_workers_boost, 3);
        assert_eq!(config.sprint.max_cone_ratio, 2.0);
        assert_eq!(config.sprint.sprint_end_headroom_pct, 5.0);
    }

    #[test]
    fn test_sprint_config_custom() {
        let yaml = r#"
pricing:
  models: {}
sprint:
  underutilization_threshold_pct: 40.0
  underutilization_hours_remaining: 1.5
  horizon_minutes: 60.0
  min_headroom_pct: 20.0
  max_workers_boost: 5
  max_cone_ratio: 1.5
  sprint_end_headroom_pct: 3.0
"#;
        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.sprint.underutilization_threshold_pct, 40.0);
        assert_eq!(config.sprint.underutilization_hours_remaining, 1.5);
        assert_eq!(config.sprint.horizon_minutes, 60.0);
        assert_eq!(config.sprint.min_headroom_pct, 20.0);
        assert_eq!(config.sprint.max_workers_boost, 5);
        assert_eq!(config.sprint.max_cone_ratio, 1.5);
        assert_eq!(config.sprint.sprint_end_headroom_pct, 3.0);
    }

    #[test]
    fn test_model_pricing_clone() {
        let pricing = ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_5m_per_mtok: 3.75,
            cache_write_1h_per_mtok: 6.0,
            cache_read_per_mtok: 0.30,
        };

        let cloned = pricing.clone();
        assert_eq!(cloned.input_per_mtok, 3.0);
        assert_eq!(cloned.output_per_mtok, 15.0);
    }

    #[test]
    fn test_daemon_config_defaults() {
        let yaml = r#"
pricing:
  models: {}
"#;
        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.daemon.loop_interval_secs, 300);
        assert!((config.daemon.hysteresis_band - 1.0).abs() < 1e-9);
        assert_eq!(config.daemon.max_scale_up_per_cycle, 1);
        assert_eq!(config.daemon.max_scale_down_per_cycle, 1);
        assert_eq!(config.daemon.min_scale_interval_secs, 60);
        assert!((config.daemon.target_ceiling - 90.0).abs() < 1e-9);
    }

    #[test]
    fn test_daemon_config_custom() {
        let yaml = r#"
pricing:
  models: {}
daemon:
  loop_interval_secs: 120
  hysteresis_band: 2.0
  max_scale_up_per_cycle: 2
  max_scale_down_per_cycle: 2
  min_scale_interval_secs: 30
  target_ceiling: 85.0
"#;
        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.daemon.loop_interval_secs, 120);
        assert!((config.daemon.hysteresis_band - 2.0).abs() < 1e-9);
        assert_eq!(config.daemon.max_scale_up_per_cycle, 2);
        assert_eq!(config.daemon.max_scale_down_per_cycle, 2);
        assert_eq!(config.daemon.min_scale_interval_secs, 30);
        assert!((config.daemon.target_ceiling - 85.0).abs() < 1e-9);
    }

    #[test]
    fn test_agents_config_empty() {
        let yaml = r#"
pricing:
  models: {}
"#;
        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.agents.is_empty());
    }

    #[test]
    fn test_agents_config_parsing() {
        let yaml = r#"
pricing:
  models: {}
agents:
  needle-sonnet:
    launch_cmd: "needle run --agent claude-code-glm-5 --workspace /home/coding/claude-governor"
    session_pattern: "needle-claude-*"
    heartbeat_dir: "~/.needle/state/heartbeats"
    min_workers: 0
    max_workers: 8
  needle-opus:
    launch_cmd: "needle run --agent claude-opus --workspace /home/coding/project"
    session_pattern: "opus-*"
    heartbeat_dir: "/var/lib/heartbeats"
    min_workers: 1
    max_workers: 4
"#;
        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.agents.len(), 2);

        let needle_sonnet = config.agents.get("needle-sonnet").unwrap();
        assert_eq!(needle_sonnet.launch_cmd, "needle run --agent claude-code-glm-5 --workspace /home/coding/claude-governor");
        assert_eq!(needle_sonnet.session_pattern, "needle-claude-*");
        assert_eq!(needle_sonnet.heartbeat_dir, "~/.needle/state/heartbeats");
        assert_eq!(needle_sonnet.min_workers, 0);
        assert_eq!(needle_sonnet.max_workers, 8);

        let needle_opus = config.agents.get("needle-opus").unwrap();
        assert_eq!(needle_opus.min_workers, 1);
        assert_eq!(needle_opus.max_workers, 4);
    }

    #[test]
    fn test_agent_config_defaults() {
        let yaml = r#"
pricing:
  models: {}
agents:
  default-agent:
    launch_cmd: "echo test"
    session_pattern: "test-*"
    heartbeat_dir: "/tmp/heartbeats"
"#;
        let config: GovernorConfig = serde_yaml::from_str(yaml).unwrap();
        let agent = config.agents.get("default-agent").unwrap();
        assert_eq!(agent.min_workers, 0); // default
        assert_eq!(agent.max_workers, 8); // default
    }

    #[test]
    fn test_agent_config_heartbeat_dir_expansion() {
        let agent = AgentConfig {
            launch_cmd: "echo test".to_string(),
            session_pattern: "test-*".to_string(),
            heartbeat_dir: "~/.needle/heartbeats".to_string(),
            min_workers: 0,
            max_workers: 8,
        };
        let expanded = agent.heartbeat_dir_expanded();
        assert!(expanded.to_string_lossy().contains(".needle"));
        assert!(!expanded.to_string_lossy().starts_with('~'));

        // Test absolute path (no expansion needed)
        let agent_abs = AgentConfig {
            launch_cmd: "echo test".to_string(),
            session_pattern: "test-*".to_string(),
            heartbeat_dir: "/var/lib/heartbeats".to_string(),
            min_workers: 0,
            max_workers: 8,
        };
        let expanded_abs = agent_abs.heartbeat_dir_expanded();
        assert_eq!(expanded_abs.to_string_lossy(), "/var/lib/heartbeats");
    }

    #[test]
    fn test_agent_config_session_prefix() {
        let agent = AgentConfig {
            launch_cmd: "echo test".to_string(),
            session_pattern: "needle-claude-*".to_string(),
            heartbeat_dir: "/tmp".to_string(),
            min_workers: 0,
            max_workers: 8,
        };
        assert_eq!(agent.session_prefix(), "needle-claude");

        // Test pattern without trailing dash
        let agent2 = AgentConfig {
            launch_cmd: "echo test".to_string(),
            session_pattern: "worker*".to_string(),
            heartbeat_dir: "/tmp".to_string(),
            min_workers: 0,
            max_workers: 8,
        };
        assert_eq!(agent2.session_prefix(), "worker");
    }
}
