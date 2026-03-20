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
}
