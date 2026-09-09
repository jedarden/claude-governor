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
        Ok(Self {
            config,
            pricing_map,
        })
    }

    /// Create a new pricing engine from a specific config path
    pub fn from_config_path(path: &std::path::Path) -> Result<Self> {
        let config = GovernorConfig::load_from_path(path)?;
        let pricing_map = config.pricing.models.clone();
        Ok(Self {
            config,
            pricing_map,
        })
    }

    /// Compute dollar costs for a usage record
    ///
    /// Uses the model field from the usage record to look up pricing.
    /// For unknown models, falls back to a similar model with a warning.
    pub fn compute_dollars(&self, usage: &UsageRecord) -> DollarBreakdown {
        let pricing = self.get_pricing_for_model(&usage.model);

        let input_usd = (usage.input_tokens as f64) * pricing.input_per_mtok / 1_000_000.0;
        let output_usd = (usage.output_tokens as f64) * pricing.output_per_mtok / 1_000_000.0;
        let cache_read_usd =
            (usage.cache_read_tokens as f64) * pricing.cache_read_per_mtok / 1_000_000.0;
        let cache_write_5m_usd =
            (usage.cache_write_5m_tokens as f64) * pricing.cache_write_5m_per_mtok / 1_000_000.0;
        let cache_write_1h_usd =
            (usage.cache_write_1h_tokens as f64) * pricing.cache_write_1h_per_mtok / 1_000_000.0;

        let total_usd =
            input_usd + output_usd + cache_read_usd + cache_write_5m_usd + cache_write_1h_usd;

        DollarBreakdown {
            input_usd,
            output_usd,
            cache_read_usd,
            cache_write_5m_usd,
            cache_write_1h_usd,
            total_usd,
        }
    }

    /// Get pricing for a specific model, resolving unconfigured ids automatically.
    ///
    /// Resolution order (see `find_fallback_model`):
    /// 1. Exact configured id.
    /// 2. Longest configured id that is a PREFIX of this one — so a versioned
    ///    variant resolves to its own family and version.
    /// 3. Family token, taking the most EXPENSIVE entry in that family.
    /// 4. The most expensive configured model overall.
    ///
    /// Steps 3 and 4 deliberately round UP. For a capacity governor,
    /// underpricing is the dangerous direction: it makes observed consumption
    /// look cheaper than it was, which inflates the affordable worker count and
    /// over-scales against a real budget. Overpricing only costs throughput.
    fn get_pricing_for_model(&self, model: &str) -> ModelPricing {
        // Direct lookup
        if let Some(pricing) = self.pricing_map.get(model) {
            return pricing.clone();
        }

        // Skip warning for synthetic/non-model entries
        if model == "unknown" || model == "<synthetic>" || model.is_empty() {
            return Self::default_sonnet_pricing();
        }

        // Fallback logic for unknown models
        let fallback = self.find_fallback_model(model);

        if fallback != model {
            // Log each unconfigured model ONCE per process rather than on every
            // poll. The undeduplicated version emitted the same line every cycle,
            // which made a real signal — "this account is consuming a model the
            // governor cannot price" — indistinguishable from log noise, and it
            // went unread for as long as the model was in use.
            //
            // This is the operator-facing half of model auto-detection: the
            // resolver keeps costing correct without a config edit, and this
            // line names what to add for an exact rate.
            if Self::note_unconfigured_model(model) {
                let priced_as = self
                    .pricing_map
                    .get(fallback)
                    .map(|p| format!("{}/{} per Mtok", p.input_per_mtok, p.output_per_mtok))
                    .unwrap_or_else(|| "built-in default".to_string());
                log::warn!(
                    "Model '{model}' is not configured; pricing it as '{fallback}' ({priced_as}). \
                     Add an exact entry under pricing.models in governor.yaml if this is wrong."
                );
            }
        }

        self.pricing_map
            .get(fallback)
            .cloned()
            .unwrap_or_else(|| Self::default_sonnet_pricing())
    }

    /// Record an unconfigured model id, returning true the first time it is seen.
    ///
    /// Process-local and intentionally unbounded: the set of distinct model ids
    /// an account can emit is tiny, and the daemon is long-lived, so this is a
    /// handful of short strings for the life of the process.
    fn note_unconfigured_model(model: &str) -> bool {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};
        static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
        let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
        match seen.lock() {
            Ok(mut set) => set.insert(model.to_string()),
            // A poisoned lock must not silence the warning entirely.
            Err(_) => true,
        }
    }

    /// Total price of a configured model, used to pick conservatively between
    /// same-family candidates. Input + output is enough to order them; cache
    /// rates track those on every real entry.
    fn price_rank(p: &ModelPricing) -> f64 {
        p.input_per_mtok + p.output_per_mtok
    }

    /// Resolve an unconfigured model id to the best configured stand-in.
    ///
    /// Auto-detection of new models in a deployed environment rests on step 1:
    /// Anthropic ships versioned variants of an existing family ("claude-fable-5"
    /// -> "claude-fable-5-1"), and a prefix match picks up the new id with the
    /// right family AND the right generation, with no config edit.
    ///
    /// The previous implementation knew only opus/sonnet/haiku, so
    /// "claude-fable-5-1" matched no family and fell through to
    /// claude-sonnet-4-20250514 — 3/15 per Mtok against Fable's 10/50, a 3.3x
    /// UNDERPRICE of the most expensive model on the account. It also iterated a
    /// HashMap and returned the first hit, so even a known family resolved
    /// non-deterministically between e.g. claude-opus-5 and
    /// claude-opus-4-20250514.
    fn find_fallback_model(&self, model: &str) -> &str {
        Self::resolve_model(&self.pricing_map, model)
    }

    /// Pure core of [`find_fallback_model`], taking the pricing map explicitly so
    /// it can be tested without constructing a whole engine and config.
    fn resolve_model<'a>(pricing_map: &'a HashMap<String, ModelPricing>, model: &str) -> &'a str {
        let model_lower = model.to_lowercase();

        // 1. Longest configured id that prefixes this one. Most specific wins,
        //    so claude-fable-5-1 takes claude-fable-5 over any shorter match.
        let mut best_prefix: Option<&str> = None;
        for key in pricing_map.keys() {
            if model_lower.starts_with(&key.to_lowercase())
                && best_prefix.is_none_or(|b| key.len() > b.len())
            {
                best_prefix = Some(key);
            }
        }
        if let Some(key) = best_prefix {
            return key;
        }

        // 2. Family token — most expensive entry in the family, deterministic.
        //    "fable" is listed first only for readability; matching is by token
        //    presence, and each family is resolved independently.
        const FAMILIES: &[&str] = &["fable", "opus", "sonnet", "haiku"];
        for family in FAMILIES {
            if !model_lower.contains(family) {
                continue;
            }
            let mut candidates: Vec<(&String, &ModelPricing)> = pricing_map
                .iter()
                .filter(|(k, _)| k.to_lowercase().contains(family))
                .collect();
            // Descending price, then id, so the choice never depends on HashMap
            // iteration order.
            candidates.sort_by(|(ak, ap), (bk, bp)| {
                Self::price_rank(bp)
                    .partial_cmp(&Self::price_rank(ap))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| ak.cmp(bk))
            });
            if let Some((key, _)) = candidates.first() {
                return key;
            }
        }

        // 3. Nothing recognisable: the most expensive configured model, so an
        //    entirely new family is never silently treated as cheap.
        let mut all: Vec<(&String, &ModelPricing)> = pricing_map.iter().collect();
        all.sort_by(|(ak, ap), (bk, bp)| {
            Self::price_rank(bp)
                .partial_cmp(&Self::price_rank(ap))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| ak.cmp(bk))
        });
        all.first()
            .map(|(k, _)| k.as_str())
            .unwrap_or("claude-sonnet-4-20250514")
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
    let cache_read_usd =
        (usage.cache_read_tokens as f64) * pricing.cache_read_per_mtok / 1_000_000.0;
    let cache_write_5m_usd =
        (usage.cache_write_5m_tokens as f64) * pricing.cache_write_5m_per_mtok / 1_000_000.0;
    let cache_write_1h_usd =
        (usage.cache_write_1h_tokens as f64) * pricing.cache_write_1h_per_mtok / 1_000_000.0;

    let total_usd =
        input_usd + output_usd + cache_read_usd + cache_write_5m_usd + cache_write_1h_usd;

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
            session_entrypoint: "cli".to_string(),
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

    // ── Model auto-detection (unconfigured ids) ─────────────────────────────

    fn resolver_map() -> HashMap<String, ModelPricing> {
        let mut m = HashMap::new();
        m.insert("claude-sonnet-5".to_string(), make_sonnet_pricing());
        m.insert(
            "claude-sonnet-4-20250514".to_string(),
            make_sonnet_pricing(),
        );
        m.insert("claude-opus-5".to_string(), make_opus_pricing());
        m.insert("claude-haiku-4-5".to_string(), make_haiku_pricing());
        // Fable is the most expensive model on the account.
        m.insert(
            "claude-fable-5".to_string(),
            ModelPricing {
                input_per_mtok: 10.0,
                output_per_mtok: 50.0,
                cache_write_5m_per_mtok: 12.5,
                cache_write_1h_per_mtok: 20.0,
                cache_read_per_mtok: 1.0,
            },
        );
        m
    }

    /// The live regression: a versioned variant must resolve to its OWN family.
    /// Before the fix "fable" was not a known family at all, so this fell through
    /// to claude-sonnet-4-20250514 and underpriced Fable by 3.3x.
    #[test]
    fn versioned_variant_resolves_to_its_own_family() {
        let m = resolver_map();
        assert_eq!(
            PricingEngine::resolve_model(&m, "claude-fable-5-1"),
            "claude-fable-5"
        );
    }

    #[test]
    fn prefix_match_prefers_the_most_specific_id() {
        let m = resolver_map();
        // Both claude-sonnet-5 and (no shorter entry) could match; the longest
        // configured prefix wins.
        assert_eq!(
            PricingEngine::resolve_model(&m, "claude-sonnet-5-20260901"),
            "claude-sonnet-5"
        );
    }

    /// Family fallback must be deterministic AND conservative: with two Opus
    /// entries the more expensive one is chosen, and repeated calls agree.
    /// The old implementation returned whichever key HashMap iteration yielded
    /// first.
    #[test]
    fn family_fallback_is_deterministic_and_rounds_up() {
        let mut m = resolver_map();
        m.insert(
            "claude-opus-cheap".to_string(),
            ModelPricing {
                input_per_mtok: 1.0,
                output_per_mtok: 2.0,
                cache_write_5m_per_mtok: 1.0,
                cache_write_1h_per_mtok: 1.0,
                cache_read_per_mtok: 0.1,
            },
        );
        let first = PricingEngine::resolve_model(&m, "opus-something-unversioned");
        assert_eq!(first, "claude-opus-5", "must pick the pricier Opus entry");
        for _ in 0..25 {
            assert_eq!(
                PricingEngine::resolve_model(&m, "opus-something-unversioned"),
                first,
                "resolution must not depend on HashMap iteration order"
            );
        }
    }

    /// An entirely unrecognised family must not be treated as cheap: for a
    /// capacity governor, underpricing inflates the affordable worker count.
    #[test]
    fn unknown_family_falls_back_to_the_most_expensive_model() {
        let m = resolver_map();
        assert_eq!(
            PricingEngine::resolve_model(&m, "claude-newthing-9"),
            "claude-fable-5"
        );
    }

    #[test]
    fn exact_configured_id_is_unchanged() {
        let m = resolver_map();
        assert_eq!(
            PricingEngine::resolve_model(&m, "claude-opus-5"),
            "claude-opus-5"
        );
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
            session_entrypoint: "cli".to_string(),
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
