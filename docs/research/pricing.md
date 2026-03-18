# Anthropic API Pricing Research

Source: https://platform.claude.com/docs/en/about-claude/pricing

---

## Model Pricing

All prices in USD per million tokens (MTok). Two cache write tiers exist with different TTLs and price multipliers.

| Model | Input | Cache Write 5m | Cache Write 1h | Cache Read | Output |
|---|---|---|---|---|---|
| Claude Opus 4.6 | $5.00 | $6.25 | $10.00 | $0.50 | $25.00 |
| Claude Opus 4.5 | $5.00 | $6.25 | $10.00 | $0.50 | $25.00 |
| Claude Opus 4.1 | $15.00 | $18.75 | $30.00 | $1.50 | $75.00 |
| Claude Opus 4 | $15.00 | $18.75 | $30.00 | $1.50 | $75.00 |
| Claude Sonnet 4.6 | $3.00 | $3.75 | $6.00 | $0.30 | $15.00 |
| Claude Sonnet 4.5 | $3.00 | $3.75 | $6.00 | $0.30 | $15.00 |
| Claude Sonnet 4 | $3.00 | $3.75 | $6.00 | $0.30 | $15.00 |
| Claude Sonnet 3.7 (deprecated) | $3.00 | $3.75 | $6.00 | $0.30 | $15.00 |
| Claude Haiku 4.5 | $1.00 | $1.25 | $2.00 | $0.10 | $5.00 |
| Claude Haiku 3.5 | $0.80 | $1.00 | $1.60 | $0.08 | $4.00 |
| Claude Opus 3 (deprecated) | $15.00 | $18.75 | $30.00 | $1.50 | $75.00 |
| Claude Haiku 3 | $0.25 | $0.30 | $0.50 | $0.03 | $1.25 |

**Key corrections vs prior assumptions:**
- Opus 4.6/4.5 is **$5 input / $25 output**, NOT $15/$75 — that's the older Opus 4.1/4 pricing
- Haiku 4.5 is **$1 input / $5 output**, NOT $0.80/$4 — those are Haiku 3.5 rates
- There are **two cache write tiers** (5-minute and 1-hour), not one

---

## Prompt Caching — Price Multipliers

Cache pricing is expressed as multipliers on the base input price:

| Operation | Multiplier | TTL |
|---|---|---|
| Cache write (5-minute) | 1.25× base input | 5 minutes |
| Cache write (1-hour) | 2.0× base input | 1 hour |
| Cache read (hit) | 0.1× base input | Same as preceding write |

Break-even analysis:
- 5-minute write pays off after **1 cache read** (1.25x write + 0.1x read = 1.35x vs 2× without cache)
- 1-hour write pays off after **2 cache reads** (2.0x write + 2 × 0.1x = 2.2x vs 3× without cache)

### Two-Tier Cache Write in API Responses

The `usage` object in Claude API responses distinguishes cache write tiers via the `cache_creation` sub-object:

```json
{
  "usage": {
    "input_tokens": 3241,
    "output_tokens": 847,
    "cache_creation_input_tokens": 10863,
    "cache_read_input_tokens": 6370,
    "cache_creation": {
      "ephemeral_5m_input_tokens": 0,
      "ephemeral_1h_input_tokens": 10863
    }
  }
}
```

- `cache_creation_input_tokens` = total cache writes (sum of both tiers)
- `cache_creation.ephemeral_5m_input_tokens` = 5-minute TTL writes (1.25x)
- `cache_creation.ephemeral_1h_input_tokens` = 1-hour TTL writes (2.0x)
- `cache_read_input_tokens` = cache hits (0.1x)

**Implication for the token collector:** Dollar-equivalent calculations must read the nested `cache_creation` sub-object to correctly attribute 5m vs 1h cache writes. Using only `cache_creation_input_tokens` with a single rate will produce incorrect costs.

---

## Dollar Calculation Formula (Per API Response)

```python
def compute_dollar_equiv(usage, model_pricing):
    p = model_pricing  # prices in $/MTok

    # Cache write split from nested object
    cw_5m = usage.get('cache_creation', {}).get('ephemeral_5m_input_tokens', 0)
    cw_1h = usage.get('cache_creation', {}).get('ephemeral_1h_input_tokens', 0)

    # Fallback: if cache_creation sub-object absent, treat all as 5m
    if cw_5m == 0 and cw_1h == 0:
        cw_5m = usage.get('cache_creation_input_tokens', 0)

    cost = {
        'input':       usage['input_tokens']              * p['input_per_mtok']        / 1_000_000,
        'output':      usage['output_tokens']             * p['output_per_mtok']       / 1_000_000,
        'cache_w_5m':  cw_5m                              * p['cache_write_5m_per_mtok'] / 1_000_000,
        'cache_w_1h':  cw_1h                              * p['cache_write_1h_per_mtok'] / 1_000_000,
        'cache_read':  usage['cache_read_input_tokens']   * p['cache_read_per_mtok']   / 1_000_000,
    }
    cost['total'] = sum(cost.values())
    return cost
```

---

## Feature-Specific Pricing

### Fast Mode (Opus 4.6 only — research preview)

6× premium over standard rates:

| Input | Output |
|---|---|
| $30 / MTok | $150 / MTok |

- Applies to full context window including requests > 200k tokens
- Stacks with prompt caching and data residency multipliers
- Not available with Batch API

The `service_tier` field in API responses distinguishes fast mode usage. Fast mode requests should use `fast_mode` pricing, not standard pricing, in dollar calculations.

### Batch API (50% discount)

| Model | Batch Input | Batch Output |
|---|---|---|
| Claude Opus 4.6 / 4.5 | $2.50 / MTok | $12.50 / MTok |
| Claude Sonnet 4.6 / 4.5 / 4 | $1.50 / MTok | $7.50 / MTok |
| Claude Haiku 4.5 | $0.50 / MTok | $2.50 / MTok |
| Claude Haiku 3.5 | $0.40 / MTok | $2.00 / MTok |

Batch requests are identifiable via the `service_tier` field or by request context.

### Data Residency (US-only inference)

1.1× multiplier on all token categories (input, output, cache writes, cache reads) when `inference_geo: "us"` is set. Applies to Opus 4.6 and newer models only. Identifiable from the `inference_geo` field in API responses.

### Long Context Pricing

- **Opus 4.6 and Sonnet 4.6**: Full 1M context at standard rates — no premium.
- **Sonnet 4.5 and Sonnet 4** (with `context-1m-2025-08-07` beta header): premium above 200k input tokens:

| Tokens | Input | Output |
|---|---|---|
| ≤ 200k | $3.00 / MTok | $15.00 / MTok |
| > 200k | $6.00 / MTok | $22.50 / MTok |

The 200k threshold is based on `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`. If exceeded, the *entire request* is charged at the premium rate.

### Tool Use Overhead

Tool definitions and responses add input tokens priced at standard rates. Additionally, a system prompt is automatically injected:

| Model | `auto`/`none` choice | `any`/`tool` choice |
|---|---|---|
| Claude 4.x (Opus 4.6, Sonnet 4.6, Haiku 4.5, etc.) | 346 tokens | 313 tokens |

The bash tool adds 245 input tokens per call. The text editor tool (`text_editor_20250429`) adds 700 input tokens.

### Web Search

$10 per 1,000 searches, plus standard token costs for search-generated content. Tracked in `usage.server_tool_use.web_search_requests`.

### Web Fetch

No additional charge beyond standard token costs for fetched content.

---

## Governor Implications

### 1. Pricing Config Must Use Five Token Cost Fields Per Model

The governor's `pricing` config (and token collector's dollar calculations) must track:
- `input_per_mtok`
- `output_per_mtok`
- `cache_write_5m_per_mtok` (1.25× input)
- `cache_write_1h_per_mtok` (2.0× input)
- `cache_read_per_mtok` (0.1× input)

A single `cache_write_per_mtok` field is insufficient.

### 2. Service Tier Modifier Detection

The `service_tier` field in API responses may indicate `standard`, `batch`, or fast mode. Dollar calculations must apply the appropriate rate. Claude Code subscription usage is `standard` by default.

### 3. Heavy Cache Usage in Claude Code

Claude Code makes heavy use of 1-hour prompt caching (system prompt, conversation history, large file contexts). In practice, cache reads (`cache_read_input_tokens`) dominate token counts but cost only 0.1× input. This means:
- Raw token count is a poor proxy for dollar cost
- Cache-read-heavy workloads have much lower dollar cost per token than fresh-input workloads
- Dollar-equivalent burn rate provides a better model of plan consumption than token count alone

### 4. Tool Use Token Overhead

Each Claude Code session uses tools extensively (Bash, Read, Edit, etc.). The 346-token tool system prompt overhead is constant per request. At high tool-call frequency, this becomes a measurable fraction of input tokens.

### 5. Revised Burn Rate Baselines

Given correct Opus 4.6 pricing ($5/$25, not $15/$75):
- An Opus worker costs ~3.5× an equivalent Sonnet worker per token (output-heavy ratio)
- Previous assumption of "3–4× quota per hour" for Opus may still hold for plan-percent terms but dollar-equivalent cost is lower than previously estimated (Opus 4.6 is 1.67× Sonnet, not 5×)
