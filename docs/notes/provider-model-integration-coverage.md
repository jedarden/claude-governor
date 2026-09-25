# Provider × model integration coverage

The governor scales three kinds of worker pools, and the behaviour the fleet
depends on — which usage records count, what they cost, which windows they
draw down, what happens when a model id is unknown, and how the pools are
scaled — differs by provider. This note documents the three representative
provider configurations and the 25-test matrix that pins them.

**The executable source of truth is
`tests/provider_model_integration_matrix.rs`.** The YAML blocks below are kept
byte-identical to the constants in that file; a drift fails
`representative_provider_configs_stay_documented`, so an operator copying an
example from here copies exactly what the tests exercise.

## The three providers

| Provider | Billing | `subscription` | Declared `windows` | Model |
|---|---|---|---|---|
| Sonnet subscription | OAuth usage windows | `true` | `[five_hour, seven_day]` | `claude-sonnet-5` |
| Opus subscription | OAuth usage windows incl. premium | `true` | all three | `claude-opus-5` |
| Pay-per-token | credits (`sdk-cli`) | `false` | undeclared → all | `glm-5.3-flash` |

### Sonnet subscription pool

Sonnet consumption draws the all-model windows only — it never touches the
scoped weekly (premium) window — so the pool declares exactly
`[five_hour, seven_day]` and is freed from premium-window risk
(see [human-reserve-policy.md](human-reserve-policy.md)).

```yaml
needle-sonnet:
  launch_cmd: "needle run --agent claude-anthropic-sonnet --identifier cgov-sonnet-{id}"
  session_pattern: "needle-claude-anthropic-sonnet-cgov-sonnet-*"
  heartbeat_dir: "~/.needle/state/heartbeats"
  min_workers: 0
  max_workers: 8
  subscription: true
  windows: [five_hour, seven_day]
```

### Opus premium subscription pool

Opus consumption DOES draw the scoped weekly window, so the pool declares all
three windows. The `min_workers: 1` floor is deliberate: a dedicated premium
strand must keep running even through tight windows, and the distribution
pass honours a stated floor over the window-affinity cap.

```yaml
needle-opus:
  launch_cmd: "needle run --agent claude-anthropic-opus --identifier cgov-opus-{id}"
  session_pattern: "needle-claude-anthropic-opus-cgov-opus-*"
  heartbeat_dir: "~/.needle/state/heartbeats"
  min_workers: 1
  max_workers: 1
  subscription: true
  windows: [five_hour, seven_day, weekly_scoped]
```

### Pay-per-token pool

A GLM proxy pool billed in credits (`sdk-cli` entrypoint). It has no OAuth
usage windows, so `windows` stays undeclared — which the governor reads
conservatively as ALL windows, bounding the pool by every window it could
conceivably touch. Its GLM usage records do not consume Anthropic quota and
are excluded from quota accounting at parse time.

```yaml
glm-payg:
  launch_cmd: "needle run --agent claude-code-glm-5.3-flash --identifier cgov-payg-{id}"
  session_pattern: "needle-claude-code-glm-5.3-flash-cgov-payg-*"
  heartbeat_dir: "~/.needle/state/heartbeats"
  min_workers: 0
  max_workers: 8
  subscription: false
```

### Representative pricing block

The models the three providers run, plus Fable — the account's most expensive
model, which entirely-unknown families round UP to (underpricing is the
dangerous direction for a capacity governor). Mirrors the seed
`config/governor.yaml`.

```yaml
pricing:
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
      cache_read_per_mtok: 0.30
```

## The matrix

Each cell walks the full public path — JSONL parse → model attribution →
pricing resolution → window semantics → worker distribution — in
`tests/provider_model_integration_matrix.rs`.

| Dimension | Sonnet subscription | Opus subscription | Pay-per-token |
|---|---|---|---|
| Model attribution | `sonnet_subscription_usage_is_attributed_to_its_model` | `opus_subscription_usage_is_attributed_to_its_model` (+ path inference, `missing_model_infers_from_path_hint`) | `pay_per_token_usage_keeps_sdk_cli_entrypoint`; GLM quota exclusion `pay_per_token_glm_usage_is_excluded_from_quota`; `unattributable_usage_is_dropped_not_miscounted` |
| Pricing | `sonnet_exact_pricing_from_representative_config` | `opus_exact_pricing_from_representative_config` | `pay_per_token_glm_alias_prices_at_sonnet_class` |
| Unknown-model behaviour | `unknown_versioned_variant_resolves_to_its_own_family_rate`, `synthetic_and_unknown_ids_price_at_the_default_sonnet_rate` | same (family-level resolution) | `unknown_family_rounds_up_to_the_most_expensive_configured_rate`; shed-order fallbacks `unmeasured_adapters_rank_at_their_fallback_costs_in_the_shed` |
| Usage-window accounting | `rotation_of_scoped_weekly_onto_sonnet_is_detected`; `weekly_scoped_absent_means_no_scoped_constraint_for_any_provider` | `opus_carried_scoped_weekly_is_model_agnostic` | no OAuth windows; undeclared `windows` → conservative all-windows default (`representative_provider_configs_parse_with_their_billing_affinity`) |
| Scaling decisions | `weekly_scoped_exhaustion_cannot_starve_the_sonnet_pool`; `expensive_opus_floor_wins_a_slot_over_cheaper_sonnet_at_total_one` | `opus_floor_holds_through_premium_window_exhaustion`; `scale_down_sheds_the_expensive_priced_pool_first` | `undeclared_pool_growth_is_bounded_by_every_window_it_could_touch`; `scale_up_gives_the_first_slot_to_the_cheap_pay_per_token_pool` |

## Behaviour notes the matrix pins

- **Attribution**: a usage record counts toward quota only when its model id
  starts with `claude-`. GLM proxy records are dropped (they spend credits,
  not Anthropic quota); an id that cannot be resolved at all is dropped
  rather than miscounted. `sdk-cli` records are kept for visibility with
  their billing marker intact.
- **Pricing resolution order**: exact id → longest configured prefix (so a
  versioned variant lands on its own family and generation) → most expensive
  entry of the model's family → most expensive configured model overall →
  built-in Sonnet default. The round-up bias is deliberate.
- **Window affinity**: an undeclared `windows` list means ALL windows. The
  one case where the Sonnet pool's declaration must gain `weekly_scoped`
  again is a rotation of the scoped cap onto Sonnet — detectable via
  `UsageData::is_weekly_scoped_sonnet`.
- **Distribution**: the window-affinity cap REDISTRIBUTES between pools and
  never shrinks an authorized total; stated `min_workers` floors win over
  both the cap and the cost sort; scale-ups prefer the cheapest pool,
  scale-downs shed the most expensive first, with unmeasured adapters ranked
  at their fallback costs ($17.50/hr for a table-priced but unmeasured
  adapter, $10.50/hr for one with neither).

Related: [human-reserve-policy.md](human-reserve-policy.md),
[../hysteresis-and-smooth-scaling.md](../hysteresis-and-smooth-scaling.md),
[burn-attribution-semantics.md](burn-attribution-semantics.md).
