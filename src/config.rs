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
#[derive(Debug, Deserialize)]
pub struct GovernorConfig {
    /// Model pricing configuration
    pub pricing: PricingConfig,
}

/// Pricing configuration for all models
#[derive(Debug, Deserialize)]
pub struct PricingConfig {
    /// Map of model name to pricing details
    pub models: std::collections::HashMap<String, ModelPricing>,
}

/// Per-model pricing rates (USD per million tokens)
#[derive(Debug, Deserialize, Clone)]
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
