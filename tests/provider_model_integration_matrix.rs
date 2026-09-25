//! Provider × model integration matrix (claudego-ce225f87).
//!
//! The governor scales three kinds of worker pools, and every dimension the
//! bead names — model attribution, pricing, usage-window accounting,
//! unknown-model behaviour and scaling decisions — behaves differently on
//! each. This file pins the matrix as integration tests: the cells walk a
//! usage record (or a capacity decision) through the full public path —
//! JSONL parse → model attribution → pricing resolution → window semantics →
//! worker distribution — rather than calling one private core.
//!
//! The three providers, with their representative `governor.yaml` fragments
//! (kept byte-identical in `docs/notes/provider-model-integration-coverage.md`
//! by `representative_provider_configs_stay_documented`):
//!
//! | Provider | Billing | `subscription` | Declared `windows` | Model |
//! |---|---|---|---|---|
//! | Sonnet subscription | OAuth usage windows | `true` | `[five_hour, seven_day]` | `claude-sonnet-5` |
//! | Opus subscription | OAuth usage windows incl. premium | `true` | all three | `claude-opus-5` |
//! | Pay-per-token | credits (`sdk-cli`) | `false` | undeclared → all | `glm-5.3-flash` |
//!
//! Matrix layout (each cell names its test):
//!
//! - **Model attribution** — `parse_usage_block`:
//!   `sonnet_subscription_usage_is_attributed_to_its_model`,
//!   `opus_subscription_usage_is_attributed_to_its_model`,
//!   `pay_per_token_usage_keeps_sdk_cli_entrypoint`,
//!   `pay_per_token_glm_usage_is_excluded_from_quota`,
//!   `missing_model_infers_from_path_hint`,
//!   `unattributable_usage_is_dropped_not_miscounted`.
//! - **Pricing** (through [`PricingEngine::from_config_path`] on the
//!   representative pricing block): `sonnet_exact_pricing_from_representative_config`,
//!   `opus_exact_pricing_from_representative_config`,
//!   `pay_per_token_glm_alias_prices_at_sonnet_class`,
//!   `unknown_versioned_variant_resolves_to_its_own_family_rate`,
//!   `unknown_family_rounds_up_to_the_most_expensive_configured_rate`,
//!   `synthetic_and_unknown_ids_price_at_the_default_sonnet_rate`.
//! - **Usage-window accounting** — `UsageData` per provider:
//!   `opus_carried_scoped_weekly_is_model_agnostic`,
//!   `rotation_of_scoped_weekly_onto_sonnet_is_detected`,
//!   `weekly_scoped_absent_means_no_scoped_constraint_for_any_provider`.
//! - **Scaling decisions** — [`distribute_workers_by_ledger_yield`] (the
//!   ledger-blind entry point, i.e. the pure cost order):
//!   `weekly_scoped_exhaustion_cannot_starve_the_sonnet_pool`,
//!   `opus_floor_holds_through_premium_window_exhaustion`,
//!   `undeclared_pool_growth_is_bounded_by_every_window_it_could_touch`,
//!   `scale_up_gives_the_first_slot_to_the_cheap_pay_per_token_pool`,
//!   `scale_down_sheds_the_expensive_priced_pool_first`,
//!   `expensive_opus_floor_wins_a_slot_over_cheaper_sonnet_at_total_one`,
//!   `unmeasured_adapters_rank_at_their_fallback_costs_in_the_shed`.
//! - **Documentation** — `representative_provider_configs_stay_documented`,
//!   `representative_provider_configs_parse_with_their_billing_affinity`.

use std::collections::HashMap;
use std::path::Path;

use claude_governor::collector::{parse_usage_block, ApiUsage, AssistantMessage, JsonlLine};
use claude_governor::config::{AgentConfig, GovernorConfig, ModelPricing};
use claude_governor::governor::distribute_workers_by_ledger_yield;
use claude_governor::poller::{LimitModel, LimitScope, UsageData, UsageLimit};
use claude_governor::pricing::PricingEngine;
use claude_governor::state::{self, ModelBurnRate};
use chrono::{Duration, Utc};

// ---------------------------------------------------------------------------
// Representative configuration examples
//
// These four fragments are the documented examples — the docs note must carry
// them byte-for-byte (see `representative_provider_configs_stay_documented`).
// ---------------------------------------------------------------------------

/// Sonnet subscription pool. Draws only the all-model windows: Sonnet
/// consumption never touches `weekly_scoped`, so the premium-window risk must
/// not bound this pool (docs/notes/human-reserve-policy.md).
const SONNET_SUBSCRIPTION_YAML: &str = r#"needle-sonnet:
  launch_cmd: "needle run --agent claude-anthropic-sonnet --identifier cgov-sonnet-{id}"
  session_pattern: "needle-claude-anthropic-sonnet-cgov-sonnet-*"
  heartbeat_dir: "~/.needle/state/heartbeats"
  min_workers: 0
  max_workers: 8
  subscription: true
  windows: [five_hour, seven_day]"#;

/// Opus premium subscription pool. Opus consumption DOES draw the scoped
/// weekly (premium) window, so the pool declares all three — and keeps a
/// `min_workers` floor because a dedicated premium strand must run even
/// through tight windows.
const OPUS_SUBSCRIPTION_YAML: &str = r#"needle-opus:
  launch_cmd: "needle run --agent claude-anthropic-opus --identifier cgov-opus-{id}"
  session_pattern: "needle-claude-anthropic-opus-cgov-opus-*"
  heartbeat_dir: "~/.needle/state/heartbeats"
  min_workers: 1
  max_workers: 1
  subscription: true
  windows: [five_hour, seven_day, weekly_scoped]"#;

/// Pay-per-token pool (GLM through a proxy, `sdk-cli` credits billing). No
/// OAuth usage windows exist for it; `windows` stays undeclared, which the
/// governor reads conservatively as ALL windows. Its GLM usage records never
/// consume Anthropic quota and are excluded from quota accounting upstream.
const PAY_PER_TOKEN_YAML: &str = r#"glm-payg:
  launch_cmd: "needle run --agent claude-code-glm-5.3-flash --identifier cgov-payg-{id}"
  session_pattern: "needle-claude-code-glm-5.3-flash-cgov-payg-*"
  heartbeat_dir: "~/.needle/state/heartbeats"
  min_workers: 0
  max_workers: 8
  subscription: false"#;

/// Representative pricing block (the models the three providers above run,
/// plus Fable — the account's most expensive model, which unknown families
/// round up to). Mirrors the seed `config/governor.yaml`.
const REPRESENTATIVE_PRICING_YAML: &str = r#"pricing:
  models:
    claude-opus-5:
      input_per_mtok: 5.0
      output_per_mtok: 25.0
      cache_write_5m_per_mtok: 6.25
      cache_write_1h_per_mtok: 10.0
      cache_read_per_mtok: 0.50
    claude-sonnet-5:
      input_per_mtok: 3.0
      output_per_mtok: 15.0
      cache_write_5m_per_mtok: 3.75
      cache_write_1h_per_mtok: 6.0
      cache_read_per_mtok: 0.30
    claude-haiku-4-5:
      input_per_mtok: 1.0
      output_per_mtok: 5.0
      cache_write_5m_per_mtok: 1.25
      cache_write_1h_per_mtok: 2.0
      cache_read_per_mtok: 0.10
    claude-fable-5:
      input_per_mtok: 10.0
      output_per_mtok: 50.0
      cache_write_5m_per_mtok: 12.5
      cache_write_1h_per_mtok: 20.0
      cache_read_per_mtok: 1.0
    glm-5:
      input_per_mtok: 3.0
      output_per_mtok: 15.0
      cache_write_5m_per_mtok: 3.75
      cache_write_1h_per_mtok: 6.0
      cache_read_per_mtok: 0.30"#;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The token counts every pricing test uses: 1M input / 500K output / 200K
/// cache-read / 100K cache-write-5m / 50K cache-write-1h — the same shape as
/// the `pricing.rs` unit tests, so the dollar figures stay comparable.
fn matrix_usage(model: &str) -> claude_governor::collector::UsageRecord {
    claude_governor::collector::UsageRecord {
        input_tokens: 1_000_000,
        output_tokens: 500_000,
        cache_read_tokens: 200_000,
        cache_write_5m_tokens: 100_000,
        cache_write_1h_tokens: 50_000,
        model: model.to_string(),
        session: "matrix-session".to_string(),
        session_entrypoint: "cli".to_string(),
    }
}

/// A JSONL assistant line carrying usage, an optional model id and an
/// optional entrypoint (absent = older writers, which the parser defaults
/// to `cli`).
fn usage_line(model: Option<&str>, entrypoint: Option<&str>) -> JsonlLine {
    JsonlLine {
        msg_type: Some("assistant".to_string()),
        message: Some(AssistantMessage {
            usage: Some(ApiUsage {
                input_tokens: 1_000,
                output_tokens: 500,
                cache_creation_input_tokens: Some(100),
                cache_read_input_tokens: Some(200),
                cache_creation: None,
            }),
            model: model.map(str::to_string),
        }),
        entrypoint: entrypoint.map(str::to_string),
    }
}

fn parse_record(path: &Path, model: Option<&str>, entrypoint: Option<&str>) -> Option<claude_governor::collector::UsageRecord> {
    let line = usage_line(model, entrypoint);
    parse_usage_block(&line, path)
}

/// Parse one of the representative YAML fragments the way governor.yaml
/// embeds it — a name-keyed entry under `agents:` — and return the pool.
fn pool_from_yaml(yaml: &str) -> AgentConfig {
    let agents: HashMap<String, AgentConfig> =
        serde_yaml::from_str(yaml).expect("representative pool config should deserialize");
    assert_eq!(agents.len(), 1, "each fragment documents exactly one pool");
    agents.into_values().next().expect("checked above")
}

/// Build one pool for the distribution fixtures. Built by deserialization so
/// only stable fields are named (same convention as tests/scale_down_ordering.rs).
fn pool(
    name: &str,
    adapter: &str,
    min_workers: u32,
    max_workers: u32,
    subscription: bool,
    windows: Option<&[&str]>,
) -> (String, AgentConfig) {
    let windows_json = match windows {
        Some(list) => serde_json::json!(list),
        None => serde_json::Value::Null,
    };
    let cfg: AgentConfig = serde_json::from_value(serde_json::json!({
        "launch_cmd": format!("needle run --agent {adapter} --identifier {name}-{{id}}"),
        "session_pattern": format!("cgov-matrix-{name}-*"),
        "heartbeat_dir": format!("/tmp/cgov-matrix-heartbeats/{name}"),
        "min_workers": min_workers,
        "max_workers": max_workers,
        "subscription": subscription,
        "windows": windows_json,
    }))
    .expect("pool fixture should deserialize");
    (name.to_string(), cfg)
}

fn pools(entries: Vec<(String, AgentConfig)>) -> HashMap<String, AgentConfig> {
    entries.into_iter().collect()
}

fn workers(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
    pairs
        .iter()
        .map(|(name, n)| (name.to_string(), *n))
        .collect()
}

/// An empirically measured burn rate keyed by the adapter name (the
/// `--agent` value), which is what `get_agent_cost_per_worker` looks up.
fn measured_burn(dollars_per_worker_per_hour: f64) -> ModelBurnRate {
    ModelBurnRate {
        pct_per_worker_per_hour: 1.5,
        dollars_per_worker_per_hour,
        samples: 40,
    }
}

/// Governor config parsed from the representative pricing block — the same
/// table the pricing cells exercise, reused so the distribution cells' cost
/// estimates come from the documented rates.
fn representative_pricing_config() -> GovernorConfig {
    GovernorConfig::parse_and_validate(REPRESENTATIVE_PRICING_YAML)
        .expect("representative pricing config should parse")
}

fn window_forecast(safe: Option<u32>) -> state::WindowForecast {
    state::WindowForecast {
        safe_worker_count: safe,
        safe_worker_count_p75: safe.map(|w| w.saturating_sub(1)),
        ..Default::default()
    }
}

/// A capacity forecast with explicit per-window safe counts; `None` (no
/// burn data) imposes no cap on any pool.
fn forecast(
    safe_5h: Option<u32>,
    safe_7d: Option<u32>,
    safe_weekly: Option<u32>,
) -> state::CapacityForecast {
    state::CapacityForecast {
        five_hour: window_forecast(safe_5h),
        seven_day: window_forecast(safe_7d),
        weekly_scoped: window_forecast(safe_weekly),
        binding_window: "seven_day".to_string(),
        ..Default::default()
    }
}

/// Comfortable headroom everywhere — for cells that test the cost/shed order
/// in isolation, so the window-affinity pass never clamps.
fn roomy_forecast() -> state::CapacityForecast {
    forecast(Some(10), Some(10), Some(10))
}

fn distribute(
    agents: &HashMap<String, AgentConfig>,
    current: &HashMap<String, u32>,
    target_total: u32,
    forecast: &state::CapacityForecast,
) -> HashMap<String, u32> {
    distribute_workers_by_ledger_yield(
        agents,
        current,
        target_total,
        &HashMap::new(),
        &representative_pricing_config(),
        false,
        forecast,
        None,
    )
}

/// Same, with explicit per-adapter burn rates.
fn distribute_with_burn(
    agents: &HashMap<String, AgentConfig>,
    current: &HashMap<String, u32>,
    target_total: u32,
    forecast: &state::CapacityForecast,
    burn: &HashMap<String, ModelBurnRate>,
) -> HashMap<String, u32> {
    distribute_workers_by_ledger_yield(
        agents,
        current,
        target_total,
        burn,
        &representative_pricing_config(),
        false,
        forecast,
        None,
    )
}

fn engine() -> PricingEngine {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("governor.yaml");
    std::fs::write(&path, REPRESENTATIVE_PRICING_YAML).expect("write pricing config");
    // The engine loads the whole table into memory at construction; dropping
    // the temp dir afterwards is safe and keeps /tmp clean.
    PricingEngine::from_config_path(&path).expect("engine from representative config")
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

// ---------------------------------------------------------------------------
// Cell: model attribution
// ---------------------------------------------------------------------------

#[test]
fn sonnet_subscription_usage_is_attributed_to_its_model() {
    let path = Path::new("/home/u/.claude/projects/sonnet-fleet/abc123.jsonl");
    let record = parse_record(path, Some("claude-sonnet-5"), Some("cli"))
        .expect("claude- model must be counted");

    assert_eq!(record.model, "claude-sonnet-5");
    assert_eq!(record.session_entrypoint, "cli");
    assert_eq!(record.session, "abc123");
    assert_eq!(record.input_tokens, 1_000);
    assert_eq!(record.output_tokens, 500);
    // Legacy cache_creation (no 5m/1h breakdown) counts as 5m, 1h stays 0.
    assert_eq!(record.cache_read_tokens, 200);
    assert_eq!(record.cache_write_5m_tokens, 100);
    assert_eq!(record.cache_write_1h_tokens, 0);
}

#[test]
fn opus_subscription_usage_is_attributed_to_its_model() {
    let path = Path::new("/home/u/.claude/projects/opus-fleet/def456.jsonl");
    // Older JSONL writers omit the entrypoint; the parser defaults it to
    // `cli` (subscription billing) rather than losing the record.
    let record = parse_record(path, Some("claude-opus-5"), None)
        .expect("claude- model must be counted");

    assert_eq!(record.model, "claude-opus-5");
    assert_eq!(record.session_entrypoint, "cli");
    assert_eq!(record.session, "def456");
}

#[test]
fn pay_per_token_usage_keeps_sdk_cli_entrypoint() {
    let path = Path::new("/home/u/.claude/projects/payg-fleet/ghi789.jsonl");
    let record = parse_record(path, Some("claude-sonnet-5"), Some("sdk-cli"))
        .expect("sdk-cli records are tracked for visibility, not dropped");

    // The credits-billing marker survives into the ledger: the governor
    // protects subscription (cli) quota, and per-record entrypoint is what
    // lets the ledger separate the two billing modes.
    assert_eq!(record.session_entrypoint, "sdk-cli");
    assert_eq!(record.model, "claude-sonnet-5");
}

#[test]
fn pay_per_token_glm_usage_is_excluded_from_quota() {
    let path = Path::new("/home/u/.claude/projects/payg-fleet/jkl012.jsonl");
    // GLM proxy models do not consume Anthropic quota: the record must be
    // dropped, never folded into the subscription windows.
    assert!(parse_record(path, Some("glm-5.3-flash"), Some("sdk-cli")).is_none());
}

#[test]
fn missing_model_infers_from_path_hint() {
    // The message carried no model id; the workspace path names the pool.
    let opus = parse_record(
        Path::new("/home/u/.claude/projects/opus-workers/s1.jsonl"),
        None,
        Some("cli"),
    )
    .expect("path hint resolves the model");
    assert_eq!(opus.model, "claude-opus");

    let sonnet = parse_record(
        Path::new("/home/u/.claude/projects/sonnet-workers/s2.jsonl"),
        None,
        Some("cli"),
    )
    .expect("path hint resolves the model");
    assert_eq!(sonnet.model, "claude-sonnet");
}

#[test]
fn unattributable_usage_is_dropped_not_miscounted() {
    // No model id and a generic path: the parser resolves "unknown", and an
    // unattributable record must never land in the quota ledger under a
    // guessed identity.
    let path = Path::new("/home/u/.claude/projects/unlabelled/s3.jsonl");
    assert!(parse_record(path, None, Some("cli")).is_none());
}

// ---------------------------------------------------------------------------
// Cell: pricing
// ---------------------------------------------------------------------------

/// One matrix usage record priced by the engine's public path — exact table
/// hit, prefix/family resolution, or built-in default.
fn priced_total(engine: &PricingEngine, model: &str) -> claude_governor::pricing::DollarBreakdown {
    engine.compute_dollars(&matrix_usage(model))
}

#[test]
fn sonnet_exact_pricing_from_representative_config() {
    let e = engine();
    let d = priced_total(&e, "claude-sonnet-5");

    assert!(approx(d.input_usd, 3.0));
    assert!(approx(d.output_usd, 7.5));
    assert!(approx(d.cache_read_usd, 0.06));
    assert!(approx(d.cache_write_5m_usd, 0.375));
    assert!(approx(d.cache_write_1h_usd, 0.30));
    assert!(approx(d.total_usd, 11.235));
}

#[test]
fn opus_exact_pricing_from_representative_config() {
    let e = engine();
    let d = priced_total(&e, "claude-opus-5");

    assert!(approx(d.input_usd, 5.0));
    assert!(approx(d.output_usd, 12.5));
    assert!(approx(d.cache_read_usd, 0.10));
    assert!(approx(d.cache_write_5m_usd, 0.625));
    assert!(approx(d.cache_write_1h_usd, 0.50));
    assert!(approx(d.total_usd, 18.725));
}

#[test]
fn pay_per_token_glm_alias_prices_at_sonnet_class() {
    let e = engine();
    // glm-5 carries an explicit Sonnet-class entry: dollar-equivalent burn
    // for the credits pool prices at the same table, not at a guess.
    let glm = priced_total(&e, "glm-5");
    let sonnet = priced_total(&e, "claude-sonnet-5");
    assert!(approx(glm.total_usd, sonnet.total_usd));
    assert!(approx(glm.total_usd, 11.235));
}

#[test]
fn unknown_versioned_variant_resolves_to_its_own_family_rate() {
    let e = engine();
    // A future versioned id ships before any config edit: longest-prefix
    // resolution must land it on its OWN family AND generation.
    assert!(approx(
        priced_total(&e, "claude-sonnet-5-20270101").total_usd,
        11.235
    ));
    assert!(approx(priced_total(&e, "claude-opus-5-1").total_usd, 18.725));
}

#[test]
fn unknown_family_rounds_up_to_the_most_expensive_configured_rate() {
    let e = engine();
    // An entirely new family must not be treated as cheap: underpricing
    // inflates the affordable worker count, so it rounds UP to Fable.
    let d = priced_total(&e, "claude-newthing-9");
    assert!(approx(d.input_usd, 10.0));
    assert!(approx(d.output_usd, 25.0));
    assert!(approx(d.total_usd, 37.45));
}

#[test]
fn synthetic_and_unknown_ids_price_at_the_default_sonnet_rate() {
    let e = engine();
    // Synthetic/non-model ids take the built-in Sonnet default — the engine
    // has a rate for everything, so no record is ever unpriceable.
    for model in ["unknown", "<synthetic>", ""] {
        let d = priced_total(&e, model);
        assert!(approx(d.total_usd, 11.235), "model {model:?} mispriced");
    }
}

#[test]
fn explicit_pricing_helper_matches_the_engine_for_known_models() {
    // The convenience helper and the engine must agree when the table hit is
    // exact — the helper is what callers without a config use.
    let e = engine();
    let pricing: ModelPricing = e
        .config()
        .get_pricing("claude-opus-5")
        .cloned()
        .expect("representative table carries opus");
    let explicit = claude_governor::pricing::compute_dollars_explicit(
        &matrix_usage("claude-opus-5"),
        &pricing,
    );
    assert!(approx(
        explicit.total_usd,
        priced_total(&e, "claude-opus-5").total_usd
    ));
}

// ---------------------------------------------------------------------------
// Cell: usage-window accounting
// ---------------------------------------------------------------------------

/// An entry from the usage API's `limits[]` array for the scoped weekly cap,
/// scoped to the model currently carrying it.
fn scoped_weekly_limit(display_name: &str, percent: f64) -> UsageLimit {
    UsageLimit {
        kind: Some("weekly_scoped".to_string()),
        group: None,
        percent: Some(percent),
        severity: None,
        resets_at: Some((Utc::now() + Duration::hours(100)).to_rfc3339()),
        scope: Some(LimitScope {
            model: Some(LimitModel {
                id: None,
                display_name: Some(display_name.to_string()),
            }),
        }),
        is_active: Some(true),
    }
}

/// The subscription-account reading for one provider: five_hour/seven_day
/// utilizations plus whichever scoped-weekly limits the account reports.
/// Finalizes the weekly_scoped_* fields exactly as `poll()` does after
/// building `UsageData` — projecting the model-agnostic `limits[]` source
/// into the flat fields — so the fixture carries the production data flow.
fn account_reading(
    five_hour_pct: f64,
    seven_day_pct: f64,
    scoped_limits: Vec<UsageLimit>,
) -> UsageData {
    let mut data = UsageData {
        five_hour_utilization: five_hour_pct,
        five_hour_resets_at: (Utc::now() + Duration::hours(4)).to_rfc3339(),
        five_hour_hours_remaining: 4.0,
        seven_day_utilization: seven_day_pct,
        seven_day_resets_at: (Utc::now() + Duration::hours(120)).to_rfc3339(),
        seven_day_hours_remaining: 120.0,
        weekly_scoped_utilization: 0.0,
        weekly_scoped_resets_at: String::new(),
        weekly_scoped_hours_remaining: 168.0,
        weekly_scoped_model: None,
        limits: scoped_limits,
        timestamp: Utc::now(),
        stale: false,
    };
    if let Some((model_name, window)) = data.scoped_weekly() {
        data.weekly_scoped_utilization = window.utilization;
        data.weekly_scoped_hours_remaining = window.hours_remaining().unwrap_or(168.0);
        data.weekly_scoped_resets_at = window.resets_at.clone();
        data.weekly_scoped_model = Some(model_name);
    }
    data
}

#[test]
fn opus_carried_scoped_weekly_is_model_agnostic() {
    // The premium weekly cap sits on Opus this period. `limits[].percent` is
    // the authoritative weekly_scoped utilization regardless of carrier, and
    // the resolved display name lets the status surface say WHICH model.
    let reading = account_reading(48.0, 55.0, vec![scoped_weekly_limit("Opus", 62.0)]);

    let (model, window) = reading
        .scoped_weekly()
        .expect("a weekly_scoped limit entry must be found");
    assert_eq!(model, "Opus");
    assert!(approx(window.utilization, 62.0));
    // The poller's finalization projects the model-agnostic percent into the
    // flat field the state machine consumes (state.rs weekly_scoped_pct).
    assert!(approx(reading.weekly_scoped_utilization, 62.0));
    assert!(!reading.is_weekly_scoped_sonnet());
}

#[test]
fn rotation_of_scoped_weekly_onto_sonnet_is_detected() {
    // During a rotation the scoped cap can land on Sonnet. That is the one
    // case where the Sonnet pool's `windows` declaration must gain
    // `weekly_scoped` again (docs/notes/human-reserve-policy.md) — the
    // detector is what makes the rotation observable.
    let reading = account_reading(48.0, 55.0, vec![scoped_weekly_limit("Sonnet", 30.0)]);

    assert!(reading.is_weekly_scoped_sonnet());
    let (model, window) = reading.scoped_weekly().expect("scoped entry present");
    assert_eq!(model, "Sonnet");
    assert!(approx(window.utilization, 30.0));
}

#[test]
fn weekly_scoped_absent_means_no_scoped_constraint_for_any_provider() {
    // Sonnet-shape account (scoped cap on another model or plan tier without
    // one) and the pay-per-token shape (no OAuth windows at all): with no
    // weekly_scoped entry in limits[], no provider may be treated as scoped-
    // constrained. A pool's own `windows` declaration decides affinity —
    // undeclared pools still get the conservative all-windows default, which
    // the scaling cells below pin.
    for (label, reading) in [
        ("sonnet-shape", account_reading(48.0, 55.0, vec![])),
        (
            "pay-per-token-shape",
            account_reading(0.0, 0.0, vec![]),
        ),
    ] {
        assert!(
            reading.scoped_weekly().is_none(),
            "{label}: absent scoped entry must resolve to None"
        );
        assert!(
            !reading.is_weekly_scoped_sonnet(),
            "{label}: nothing scoped to Sonnet"
        );
    }
}

// ---------------------------------------------------------------------------
// Cell: scaling decisions
// ---------------------------------------------------------------------------

#[test]
fn weekly_scoped_exhaustion_cannot_starve_the_sonnet_pool() {
    // The premium weekly window forecasts zero affordable workers. What must
    // happen, per pool:
    // - needle-sonnet (declares five_hour+seven_day): freed from the premium
    //   window's risk entirely — and it can even RECEIVE the slots freed by
    //   pools the window does bound;
    // - needle-opus (declares all three, floor 1): capped but its stated
    //   floor keeps the one worker running;
    // - glm-payg (undeclared → all windows, conservative): capped to 0.
    // The cap pass must REDISTRIBUTE, never shrink the authorized total.
    let agents = pools(vec![
        pool(
            "needle-sonnet",
            "claude-anthropic-sonnet",
            0,
            8,
            true,
            Some(&["five_hour", "seven_day"]),
        ),
        pool(
            "needle-opus",
            "claude-anthropic-opus",
            1,
            1,
            true,
            Some(&["five_hour", "seven_day", "weekly_scoped"]),
        ),
        pool("glm-payg", "claude-code-glm-5.3-flash", 0, 8, false, None),
    ]);
    let current = workers(&[("needle-sonnet", 4), ("needle-opus", 1), ("glm-payg", 2)]);
    // NoChange aggregate (7 -> 7): the affinity pass alone decides.
    let fc = forecast(Some(6), Some(8), Some(0));

    let result = distribute(&agents, &current, 7, &fc);

    let total: u32 = result.values().sum();
    assert_eq!(total, 7, "the cap pass redistributes, it must not shrink");
    assert_eq!(result["needle-sonnet"], 6, "sonnet pool is not starved — it receives the freed slots");
    assert_eq!(result["needle-opus"], 1, "opus keeps its stated floor worker");
    assert_eq!(result["glm-payg"], 0, "undeclared pool is bounded by the premium window too");
}

#[test]
fn opus_floor_holds_through_premium_window_exhaustion() {
    // Minimal form of the floor rule: `weekly_scoped` at zero affordable
    // workers cannot take the premium pool below its stated min_workers.
    let agents = pools(vec![pool(
        "needle-opus",
        "claude-anthropic-opus",
        1,
        1,
        true,
        Some(&["five_hour", "seven_day", "weekly_scoped"]),
    )]);
    let current = workers(&[("needle-opus", 1)]);
    let fc = forecast(Some(10), Some(10), Some(0));

    let result = distribute(&agents, &current, 1, &fc);

    assert_eq!(result["needle-opus"], 1, "the floor wins over the affinity cap");
}

#[test]
fn undeclared_pool_growth_is_bounded_by_every_window_it_could_touch() {
    // A pool with no `windows` declaration is read conservatively as
    // consuming ALL windows: its growth is bounded by the tightest safe
    // count of the three, here five_hour's 3. With nowhere to re-home the
    // freed slots (single pool), growth stays capped — the cap is absolute
    // for growth, and the give-back can never become a growth path.
    let agents = pools(vec![pool(
        "glm-payg",
        "claude-code-glm-5.3-flash",
        0,
        8,
        false,
        None,
    )]);
    let current = workers(&[("glm-payg", 0)]);
    let fc = forecast(Some(3), Some(9), Some(4));

    let result = distribute(&agents, &current, 5, &fc);

    assert_eq!(
        result["glm-payg"], 3,
        "growth must stop at the tightest window the pool could touch"
    );
}

#[test]
fn scale_up_gives_the_first_slot_to_the_cheap_pay_per_token_pool() {
    // Cost order on the way up: the credits pool burns the fewest
    // dollars-per-worker-hour, so it grows before either subscription pool.
    let agents = pools(vec![
        pool(
            "needle-sonnet",
            "claude-anthropic-sonnet",
            0,
            8,
            true,
            Some(&["five_hour", "seven_day"]),
        ),
        pool(
            "needle-opus",
            "claude-anthropic-opus",
            0,
            1,
            true,
            Some(&["five_hour", "seven_day", "weekly_scoped"]),
        ),
        pool("glm-payg", "claude-code-glm-5.3-flash", 0, 8, false, None),
    ]);
    let burn = HashMap::from([
        ("claude-anthropic-sonnet".to_string(), measured_burn(8.0)),
        ("claude-anthropic-opus".to_string(), measured_burn(17.0)),
        ("claude-code-glm-5.3-flash".to_string(), measured_burn(4.0)),
    ]);
    let current = workers(&[("needle-sonnet", 2), ("needle-opus", 1), ("glm-payg", 2)]);

    let result = distribute_with_burn(&agents, &current, 6, &roomy_forecast(), &burn);

    assert_eq!(result["glm-payg"], 3, "cheapest pool takes the growth");
    assert_eq!(result["needle-sonnet"], 2);
    assert_eq!(result["needle-opus"], 1);
}

#[test]
fn scale_down_sheds_the_expensive_priced_pool_first() {
    // Mirror of the up-move: the premium pool sheds before the cheaper
    // subscription and credits pools. (min_workers 0 here so the shed order
    // is isolated from the floor pass, which the two floor cells pin.)
    let agents = pools(vec![
        pool(
            "needle-sonnet",
            "claude-anthropic-sonnet",
            0,
            8,
            true,
            Some(&["five_hour", "seven_day"]),
        ),
        pool(
            "needle-opus",
            "claude-anthropic-opus",
            0,
            1,
            true,
            Some(&["five_hour", "seven_day", "weekly_scoped"]),
        ),
        pool("glm-payg", "claude-code-glm-5.3-flash", 0, 8, false, None),
    ]);
    let burn = HashMap::from([
        ("claude-anthropic-sonnet".to_string(), measured_burn(8.0)),
        ("claude-anthropic-opus".to_string(), measured_burn(17.0)),
        ("claude-code-glm-5.3-flash".to_string(), measured_burn(4.0)),
    ]);
    let current = workers(&[("needle-sonnet", 2), ("needle-opus", 1), ("glm-payg", 2)]);

    let result = distribute_with_burn(&agents, &current, 4, &roomy_forecast(), &burn);

    assert_eq!(result["needle-opus"], 0, "most expensive pool sheds first");
    assert_eq!(result["needle-sonnet"], 2, "mid-cost pool untouched");
    assert_eq!(result["glm-payg"], 2, "cheapest pool untouched");
}

#[test]
fn expensive_opus_floor_wins_a_slot_over_cheaper_sonnet_at_total_one() {
    // The documented distribute guarantee: each pool's min_workers floor is
    // satisfied BEFORE the cost sort fills the remainder, so an expensive
    // pinned pool (Opus, max 1) actually launches instead of never winning a
    // slot against the cheap, high-max Sonnet fleet.
    let agents = pools(vec![
        pool(
            "needle-sonnet",
            "claude-anthropic-sonnet",
            0,
            8,
            true,
            Some(&["five_hour", "seven_day"]),
        ),
        pool(
            "needle-opus",
            "claude-anthropic-opus",
            1,
            1,
            true,
            Some(&["five_hour", "seven_day", "weekly_scoped"]),
        ),
    ]);
    let burn = HashMap::from([
        ("claude-anthropic-sonnet".to_string(), measured_burn(4.0)),
        ("claude-anthropic-opus".to_string(), measured_burn(17.0)),
    ]);
    let current = workers(&[("needle-sonnet", 0), ("needle-opus", 0)]);

    let result = distribute_with_burn(&agents, &current, 1, &roomy_forecast(), &burn);

    assert_eq!(result["needle-opus"], 1, "the floor overrides the cost sort");
    assert_eq!(result["needle-sonnet"], 0, "no spare slot remains for the cheap pool");
}

#[test]
fn unmeasured_adapters_rank_at_their_fallback_costs_in_the_shed() {
    // Two unknown-model fallbacks meeting the shed order:
    // - an adapter priced in the table but unmeasured in the ledger takes
    //   the pricing heuristic (opus: $5 + $25*0.5 = $17.50/hr);
    // - an adapter with neither evidence takes the default Sonnet cost
    //   ($10.50/hr).
    // Both must rank ABOVE the measured $8/hr Sonnet pool when shedding.
    let agents = pools(vec![
        pool(
            "needle-sonnet",
            "claude-anthropic-sonnet",
            0,
            8,
            true,
            Some(&["five_hour", "seven_day"]),
        ),
        pool("opus-estimate", "claude-opus-5", 0, 4, true, None),
        pool("mystery-pool", "claude-code-mystery", 0, 4, false, None),
    ]);
    let burn = HashMap::from([(
        "claude-anthropic-sonnet".to_string(),
        measured_burn(8.0),
    )]);
    let current = workers(&[("needle-sonnet", 1), ("opus-estimate", 1), ("mystery-pool", 1)]);

    let result = distribute_with_burn(&agents, &current, 1, &roomy_forecast(), &burn);

    assert_eq!(result["needle-sonnet"], 1, "measured pool survives the shed");
    assert_eq!(result["opus-estimate"], 0, "pricing-estimate fallback ranks highest");
    assert_eq!(result["mystery-pool"], 0, "default-cost fallback sheds with it");
}

// ---------------------------------------------------------------------------
// Cell: documentation
// ---------------------------------------------------------------------------

const PROVIDER_DOCS: &str =
    include_str!("../docs/notes/provider-model-integration-coverage.md");

#[test]
fn representative_provider_configs_stay_documented() {
    // The docs note is the human-readable copy of the representative
    // configuration examples; these constants are the executable ones. The
    // two must not drift — the doc block is what an operator copies, and a
    // silently changed example is worse than none.
    for fragment in [
        SONNET_SUBSCRIPTION_YAML,
        OPUS_SUBSCRIPTION_YAML,
        PAY_PER_TOKEN_YAML,
        REPRESENTATIVE_PRICING_YAML,
    ] {
        assert!(
            PROVIDER_DOCS.contains(fragment),
            "docs note must carry the representative config verbatim:\n{fragment}"
        );
    }
}

#[test]
fn representative_provider_configs_parse_with_their_billing_affinity() {
    // The documented examples must stay loadable `AgentConfig`s with the
    // affinity the matrix assumes: subscription flags, window declarations
    // and bounds are the provider's contract with the governor.
    let sonnet = pool_from_yaml(SONNET_SUBSCRIPTION_YAML);
    assert!(sonnet.subscription);
    assert_eq!(sonnet.max_workers, 8);
    assert_eq!(sonnet.min_workers, 0);
    assert_eq!(sonnet.consumed_windows(), vec!["five_hour", "seven_day"]);

    let opus = pool_from_yaml(OPUS_SUBSCRIPTION_YAML);
    assert!(opus.subscription);
    assert_eq!(opus.min_workers, 1);
    assert_eq!(opus.max_workers, 1);
    assert_eq!(
        opus.consumed_windows(),
        vec!["five_hour", "seven_day", "weekly_scoped"]
    );

    let payg = pool_from_yaml(PAY_PER_TOKEN_YAML);
    assert!(!payg.subscription, "pay-per-token pool is not subscription-billed");
    assert_eq!(payg.windows, None, "undeclared windows = conservative default");
    assert_eq!(
        payg.consumed_windows(),
        vec!["five_hour", "seven_day", "weekly_scoped"],
        "absent declaration must read as ALL windows"
    );

    // And the representative pricing block parses with the rates the pricing
    // cells (and the shed-order fallback) rely on.
    let pricing = representative_pricing_config();
    assert_eq!(pricing.pricing.models.len(), 5);
    for model in [
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-haiku-4-5",
        "claude-fable-5",
        "glm-5",
    ] {
        assert!(
            pricing.get_pricing(model).is_some(),
            "representative table must carry {model}"
        );
    }
}
