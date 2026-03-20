//! Pricing engine - computes dollar-equivalent costs from token counts
//!
//! This module provides:
//! - Dollar computation for usage records
//! - Model pricing lookup from configuration
//! - Graceful fallback for unknown models

use crate::collector::UsageRecord;
use crate::config::{GovernorConfig, ModelPricing};
use anyhow::Result;
use std::collections::HashMap;

/// Dollar breakdown for a single usage record
///
/// Contains USD costs for each token type plus a total.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DollarBreakdown {
    /// Cost of input tokens in USD
    pub input_usd: f64,

    /// Cost of output tokens in USD
    pub output_usd: f64,

    /// Cost of cache read tokens in USD
    pub cache_read_usd: f64,

    /// Cost of 5-minute cache write tokens in USD
    pub cache_write_5m_usd: f64,

    /// Cost of 1-hour cache write tokens in USD
    pub cache_write_1h_usd: f64,

    /// Total cost in USD (sum of all components)
    pub total_usd: f64,
}

impl DollarBreakdown {
    /// Create a new zero-initialized DollarBreakdown
    pub fn zero() -> Self {
        Self {
            input_usd: 0.0,
            output_usd: 0.0,
            cache_read_usd: 0.0,
            cache_write_5m_usd: 0.0,
            cache_write_1h_usd: 0.0,
            total_usd: 0.0,
        }
    }

    /// Check if this is a zero breakdown (no cost)
    pub fn is_zero(&self) -> bool {
        self.total_usd == 0.0
    }
}

/// Pricing engine for computing dollar costs from usage records
pub struct PricingEngine {
    /// Loaded configuration
    config: GovernorConfig,

    /// Cached pricing map for quick lookup
    pricing_map: HashMap<String, ModelPricing>,
}

impl PricingEngine {
    /// Create a new pricing engine by loading configuration
    pub fn new() -> Result<Self> {
        let config = GovernorConfig::load()?;
        let pricing_map = config.pricing.models.clone();
        Ok(Self { config, pricing_map })
    }

    /// Create a new pricing engine from a specific config path
    pub fn from_config_path(path: &std::path::Path) -> Result<Self> {
        let config = GovernorConfig::load_from_path(path)?;
        let pricing_map = config.pricing.models.clone();
        Ok(Self { config, pricing_map })
    }

    /// Compute dollar costs for a usage record
    ///
    /// Uses the model field from the usage record to look up pricing.
    /// For unknown models, falls back to a similar model with a warning.
    pub fn compute_dollars(&self, usage: &UsageRecord) -> DollarBreakdown {
        let pricing = self.get_pricing_for_model(&usage.model);

        let input_usd = (usage.input_tokens as f64) * pricing.input_per_mtok / 1_000_000.0;
        let output_usd = (usage.output_tokens as f64) * pricing.output_per_mtok / 1_000_000.0;
        let cache_read_usd = (usage.cache_read_tokens as f64) * pricing.cache_read_per_mtok / 1_000_000.0;
        let cache_write_5m_usd = (usage.cache_write_5m_tokens as f64) * pricing.cache_write_5m_per_mtok / 1_000_000.0;
        let cache_write_1h_usd = (usage.cache_write_1h_tokens as f64) * pricing.cache_write_1h_per_mtok / 1_000_000.0;

        let total_usd = input_usd + output_usd + cache_read_usd + cache_write_5m_usd + cache_write_1h_usd;

        DollarBreakdown {
            input_usd,
            output_usd,
            cache_read_usd,
            cache_write_5m_usd,
            cache_write_1h_usd,
            total_usd,
        }
    }

    /// Get pricing for a specific model with fallback
    ///
    /// If the exact model is not found, falls back to a similar model:
    /// - "opus" models -> latest Opus pricing
    /// - "sonnet" models -> latest Sonnet pricing
    /// - "haiku" models -> latest Haiku pricing
    /// - Default -> Sonnet pricing (most common)
    fn get_pricing_for_model(&self, model: &str) -> ModelPricing {
        // Direct lookup
        if let Some(pricing) = self.pricing_map.get(model) {
            return pricing.clone();
        }

        // Fallback logic for unknown models
        let fallback = self.find_fallback_model(model);

        if fallback != model {
            log::warn!(
                "Unknown model '{}', falling back to '{}' for pricing",
                model,
                fallback
            );
        }

        self.pricing_map
            .get(fallback)
            .cloned()
            .unwrap_or_else(|| Self::default_sonnet_pricing())
    }

    /// Find a fallback model for pricing
    fn find_fallback_model(&self, model: &str) -> &str {
        let model_lower = model.to_lowercase();

        // Try to find by model family
        for (key, _) in &self.pricing_map {
            let key_lower = key.to_lowercase();
            if model_lower.contains("opus") && key_lower.contains("opus") {
                return key;
            }
            if model_lower.contains("sonnet") && key_lower.contains("sonnet") {
                return key;
            }
            if model_lower.contains("haiku") && key_lower.contains("haiku") {
                return key;
            }
        }

        // Default to latest Sonnet
        "claude-sonnet-4-20250514"
    }

    /// Default Sonnet 4.6 pricing (used as ultimate fallback)
    fn default_sonnet_pricing() -> ModelPricing {
        ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_5m_per_mtok: 3.75,
            cache_write_1h_per_mtok: 6.0,
            cache_read_per_mtok: 0.30,
        }
    }

    /// Get a reference to the configuration
    pub fn config(&self) -> &GovernorConfig {
        &self.config
    }
}

/// Compute dollar costs for a usage record using explicit pricing
///
/// This is a convenience function that doesn't require a PricingEngine instance.
pub fn compute_dollars_explicit(usage: &UsageRecord, pricing: &ModelPricing) -> DollarBreakdown {
    let input_usd = (usage.input_tokens as f64) * pricing.input_per_mtok / 1_000_000.0;
    let output_usd = (usage.output_tokens as f64) * pricing.output_per_mtok / 1_000_000.0;
    let cache_read_usd = (usage.cache_read_tokens as f64) * pricing.cache_read_per_mtok / 1_000_000.0;
    let cache_write_5m_usd = (usage.cache_write_5m_tokens as f64) * pricing.cache_write_5m_per_mtok / 1_000_000.0;
    let cache_write_1h_usd = (usage.cache_write_1h_tokens as f64) * pricing.cache_write_1h_per_mtok / 1_000_000.0;

    let total_usd = input_usd + output_usd + cache_read_usd + cache_write_5m_usd + cache_write_1h_usd;

    DollarBreakdown {
        input_usd,
        output_usd,
        cache_read_usd,
        cache_write_5m_usd,
        cache_write_1h_usd,
        total_usd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_usage(model: &str) -> UsageRecord {
        UsageRecord {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_read_tokens: 200_000,
            cache_write_5m_tokens: 100_000,
            cache_write_1h_tokens: 50_000,
            model: model.to_string(),
            session: "test-session".to_string(),
        }
    }

    fn make_sonnet_pricing() -> ModelPricing {
        ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_5m_per_mtok: 3.75,
            cache_write_1h_per_mtok: 6.0,
            cache_read_per_mtok: 0.30,
        }
    }

    fn make_opus_pricing() -> ModelPricing {
        ModelPricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
            cache_write_5m_per_mtok: 6.25,
            cache_write_1h_per_mtok: 10.0,
            cache_read_per_mtok: 0.50,
        }
    }

    fn make_haiku_pricing() -> ModelPricing {
        ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_write_5m_per_mtok: 1.25,
            cache_write_1h_per_mtok: 2.0,
            cache_read_per_mtok: 0.10,
        }
    }

    #[test]
    fn test_sonnet_pricing() {
        let usage = make_test_usage("claude-sonnet-4-20250514");
        let pricing = make_sonnet_pricing();
        let breakdown = compute_dollars_explicit(&usage, &pricing);

        // 1M input * $3/MTok = $3.00
        assert!((breakdown.input_usd - 3.0).abs() < 0.001);
        // 500K output * $15/MTok = $7.50
        assert!((breakdown.output_usd - 7.5).abs() < 0.001);
        // 200K cache_read * $0.30/MTok = $0.06
        assert!((breakdown.cache_read_usd - 0.06).abs() < 0.001);
        // 100K cache_write_5m * $3.75/MTok = $0.375
        assert!((breakdown.cache_write_5m_usd - 0.375).abs() < 0.001);
        // 50K cache_write_1h * $6.00/MTok = $0.30
        assert!((breakdown.cache_write_1h_usd - 0.30).abs() < 0.001);
        // Total = $3 + $7.50 + $0.06 + $0.375 + $0.30 = $11.235
        assert!((breakdown.total_usd - 11.235).abs() < 0.001);
    }

    #[test]
    fn test_opus_pricing() {
        let usage = make_test_usage("claude-opus-4-20250514");
        let pricing = make_opus_pricing();
        let breakdown = compute_dollars_explicit(&usage, &pricing);

        // 1M input * $5/MTok = $5.00
        assert!((breakdown.input_usd - 5.0).abs() < 0.001);
        // 500K output * $25/MTok = $12.50
        assert!((breakdown.output_usd - 12.50).abs() < 0.001);
        // 200K cache_read * $0.50/MTok = $0.10
        assert!((breakdown.cache_read_usd - 0.10).abs() < 0.001);
        // 100K cache_write_5m * $6.25/MTok = $0.625
        assert!((breakdown.cache_write_5m_usd - 0.625).abs() < 0.001);
        // 50K cache_write_1h * $10.00/MTok = $0.50
        assert!((breakdown.cache_write_1h_usd - 0.50).abs() < 0.001);
        // Total = $5 + $12.50 + $0.10 + $0.625 + $0.50 = $18.725
        assert!((breakdown.total_usd - 18.725).abs() < 0.001);
    }

    #[test]
    fn test_haiku_pricing() {
        let usage = make_test_usage("claude-haiku-4-20241022");
        let pricing = make_haiku_pricing();
        let breakdown = compute_dollars_explicit(&usage, &pricing);

        // 1M input * $1/MTok = $1.00
        assert!((breakdown.input_usd - 1.0).abs() < 0.001);
        // 500K output * $5/MTok = $2.50
        assert!((breakdown.output_usd - 2.50).abs() < 0.001);
        // 200K cache_read * $0.10/MTok = $0.02
        assert!((breakdown.cache_read_usd - 0.02).abs() < 0.001);
        // 100K cache_write_5m * $1.25/MTok = $0.125
        assert!((breakdown.cache_write_5m_usd - 0.125).abs() < 0.001);
        // 50K cache_write_1h * $2.00/MTok = $0.10
        assert!((breakdown.cache_write_1h_usd - 0.10).abs() < 0.001);
        // Total = $1 + $2.50 + $0.02 + $0.125 + $0.10 = $3.745
        assert!((breakdown.total_usd - 3.745).abs() < 0.001);
    }

    #[test]
    fn test_zero_tokens() {
        let usage = UsageRecord::zero("test".to_string(), "session".to_string());
        let pricing = make_sonnet_pricing();
        let breakdown = compute_dollars_explicit(&usage, &pricing);

        assert_eq!(breakdown.input_usd, 0.0);
        assert_eq!(breakdown.output_usd, 0.0);
        assert_eq!(breakdown.cache_read_usd, 0.0);
        assert_eq!(breakdown.cache_write_5m_usd, 0.0);
        assert_eq!(breakdown.cache_write_1h_usd, 0.0);
        assert_eq!(breakdown.total_usd, 0.0);
        assert!(breakdown.is_zero());
    }

    #[test]
    fn test_dollar_breakdown_zero() {
        let zero = DollarBreakdown::zero();
        assert!(zero.is_zero());
        assert_eq!(zero.total_usd, 0.0);
    }

    #[test]
    fn test_explicit_compute() {
        let usage = UsageRecord {
            input_tokens: 2_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            model: "test".to_string(),
            session: "session".to_string(),
        };

        let pricing = ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_write_5m_per_mtok: 3.75,
            cache_write_1h_per_mtok: 6.0,
            cache_read_per_mtok: 0.30,
        };

        let breakdown = compute_dollars_explicit(&usage, &pricing);
        // 2M input * $3/MTok = $6.00
        // 1M output * $15/MTok = $15.00
        // Total = $21.00
        assert!((breakdown.total_usd - 21.0).abs() < 0.001);
    }
}
