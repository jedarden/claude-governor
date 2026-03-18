# Claude Governor — System Design Plan

## Overview

Claude Governor is an automated capacity governor that monitors Claude Code subscription usage in real time and dynamically scales the number of active AI worker instances (initially Claude Code Sonnet via NEEDLE) to maximize utilization of the available plan without exceeding limits.

The system replaces the current `capacity-governor.sh` (screen-scraping, stateless, incomplete off-peak logic) with a reliable, accurate, and extensible daemon.

---

## Goals

1. **Maximize quota utilization** — consume as close to 100% of the weekly allocation as possible before reset, without going over.
2. **Respect promotion windows** — treat off-peak hours as 2x capacity and run more workers accordingly.
3. **Graceful operation** — never kill workers mid-task; only scale down idle workers.
4. **Accurate measurement** — replace the fragile TUI scraper with direct API calls.
5. **Adaptive burn rate** — learn actual per-worker consumption empirically rather than using a hardcoded constant.
6. **Extensibility** — support multiple configured systems beyond Sonnet (Opus, pay-per-token providers, etc.) via a plugin architecture.
7. **Observability** — structured state files, human-readable logs, and alerting beads when action is needed.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    claude-governor                       │
│                                                         │
│  ┌──────────────┐    ┌─────────────────┐               │
│  │  Usage Poller │    │  State Store    │               │
│  │  (API-based) │───▶│  (JSON files)   │               │
│  └──────────────┘    └────────┬────────┘               │
│                               │                         │
│  ┌──────────────┐    ┌────────▼────────┐               │
│  │  Scheduler   │◀───│  Rate Estimator │               │
│  │  (off-peak   │    │  (adaptive burn │               │
│  │   aware)     │    │   rate model)   │               │
│  └──────┬───────┘    └─────────────────┘               │
│         │                                               │
│  ┌──────▼───────────────────────────────────┐          │
│  │            Worker Manager                 │          │
│  │  ┌──────────────┐  ┌──────────────────┐  │          │
│  │  │  Scale Up    │  │  Scale Down      │  │          │
│  │  │  (needle run)│  │  (graceful only) │  │          │
│  │  └──────────────┘  └──────────────────┘  │          │
│  └───────────────────────────────────────────┘          │
│                                                         │
│  ┌──────────────────────────────────────────┐           │
│  │            Alert Manager                  │           │
│  │  (creates HUMAN-type beads near limits)   │           │
│  └──────────────────────────────────────────┘           │
└─────────────────────────────────────────────────────────┘
```

---

## Component Design

### 1. Usage Poller

**Replaces:** `claude-status.sh` (tmux TUI scraper)

**Source:** Direct HTTP call to `https://api.anthropic.com/api/oauth/usage`

```bash
# Read OAuth token from credentials file
ACCESS_TOKEN=$(jq -r '.claudeAiOauth.accessToken' ~/.claude/.credentials.json)
EXPIRES_AT=$(jq -r '.claudeAiOauth.expiresAt' ~/.claude/.credentials.json)

# Refresh if within 5 minutes of expiry
NOW_MS=$(($(date +%s) * 1000))
if (( NOW_MS + 300000 >= EXPIRES_AT )); then
    refresh_token
fi

# Fetch usage
curl -s \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "anthropic-beta: oauth-2025-04-20" \
    -H "User-Agent: claude-code/2.1.78" \
    "https://api.anthropic.com/api/oauth/usage"
```

**Output fields consumed:**
- `seven_day_sonnet.utilization` — weekly Sonnet usage %
- `seven_day_sonnet.resets_at` — exact ISO 8601 reset timestamp
- `seven_day.utilization` — all-models weekly usage %
- `five_hour.utilization` — session burst usage %
- `five_hour.resets_at` — session reset timestamp

**Token delta collection (supplementary):**

The API returns percentages only, not raw token counts. The Token Collector (see Component 2 below) maintains a separate time-series database by tailing `~/.claude/projects/**/*.jsonl`. Each polling cycle reads the **delta** in token counts since the previous snapshot — broken down by model and token type — and pairs it with the percentage change:

```
delta_input_tokens     (priced at model input rate)
delta_output_tokens    (priced at model output rate)
delta_cache_read_tokens  (priced at model cache-read rate)
delta_cache_write_tokens (priced at model cache-write rate)
```

Pairing `(delta_pct, delta_tokens_by_type_by_model, dollar_equivalent, timestamp, is_peak)` enables:
1. Model-specific burn rates — Sonnet and Opus consume quota at different token volumes per percent
2. Dollar-equivalent capacity estimation — remaining % translates to estimated remaining API-equivalent dollars
3. Promotion validation — tokens-per-percent should be ~2x during off-peak; measurable per model
4. Stable cross-plan burn rate in $/hr regardless of plan tier changes

**Polling frequency:** Every 5 minutes (vs. current 15 min). The API is lightweight (~200ms) and less prone to rate-limiting than the TUI scraper.

**Token refresh:** When `expiresAt - now < 5 minutes`, POST to `https://platform.claude.com/v1/oauth/token` with the refresh token. Update `~/.claude/.credentials.json` in place.

---

### 2. Token Collector

An **independent daemon** responsible solely for capturing model-specific token consumption with type-level granularity. It runs separately from the governor loop and can be started, stopped, and queried independently.

**Source:** Tails `~/.claude/projects/**/*.jsonl` for assistant messages containing `usage` blocks. Each API response includes:

```json
{
  "usage": {
    "input_tokens": 3241,
    "output_tokens": 847,
    "cache_creation_input_tokens": 10863,
    "cache_read_input_tokens": 6370,
    "service_tier": "standard"
  }
}
```

The session filename path encodes the model used; the `model` field in the message confirms it.

**Token type pricing** (USD per million tokens, Claude API public rates):

```json
{
  "claude-sonnet-4-6": {
    "input_per_mtok": 3.00,
    "output_per_mtok": 15.00,
    "cache_write_per_mtok": 3.75,
    "cache_read_per_mtok": 0.30
  },
  "claude-opus-4-6": {
    "input_per_mtok": 15.00,
    "output_per_mtok": 75.00,
    "cache_write_per_mtok": 18.75,
    "cache_read_per_mtok": 1.50
  },
  "claude-haiku-4-5": {
    "input_per_mtok": 0.80,
    "output_per_mtok": 4.00,
    "cache_write_per_mtok": 1.00,
    "cache_read_per_mtok": 0.08
  }
}
```

**Output — append-only JSONL** at `~/.needle/state/token-history.jsonl`. Every line is a single flat JSON object. Three record types per collection pass, identified by `"r"`. Records `i` and `f` are wide: every token-type measurement appears as a column on the same row, not as separate rows.

Token-type column suffixes used throughout:

| Suffix | Meaning | Pricing basis |
|---|---|---|
| `-input` | Fresh input tokens | base input rate |
| `-output` | Output tokens | output rate |
| `-r-cache` | Cache reads (hits) | 0.1× input |
| `-w-cache-5m` | Cache writes, 5-min TTL | 1.25× input |
| `-w-cache-1h` | Cache writes, 1-hour TTL | 2.0× input |

Each token-type column appears in two variants: `-n` (token count) and `-usd` (dollar cost).

#### Record Type `i` — Instance Wide Row

**One line per session per interval.** All five token types appear as columns on the same row — width is `(5 tok_types × 2 variants) + metadata + total + percentages`. Since each session runs one model, tok_type columns are not model-prefixed; the `model` field carries the model identity.

```
{"r":"i","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","sess":"needle-claude-anthropic-sonnet-alpha","sid":"ad5b2e01","model":"claude-sonnet-4-6","pk":0,"input-n":15410,"input-usd":0.0462,"output-n":2830,"output-usd":0.0425,"r-cache-n":104200,"r-cache-usd":0.0313,"w-cache-5m-n":4100,"w-cache-5m-usd":0.0154,"w-cache-1h-n":5500,"w-cache-1h-usd":0.0330,"total-usd":0.1684,"p5h":null,"p7d":null,"p7ds":null}
```

Two workers in the same interval produce two `i` lines, directly comparable column-for-column:

```
{"r":"i",...,"sess":"...alpha","model":"claude-sonnet-4-6","input-n":15410,"input-usd":0.0462,"output-n":2830,"output-usd":0.0425,"r-cache-n":104200,"r-cache-usd":0.0313,"w-cache-5m-n":4100,"w-cache-5m-usd":0.0154,"w-cache-1h-n":5500,"w-cache-1h-usd":0.0330,"total-usd":0.1684,"p5h":null,"p7d":null,"p7ds":null}
{"r":"i",...,"sess":"...bravo","model":"claude-sonnet-4-6","input-n":14430,"input-usd":0.0433,"output-n":2680,"output-usd":0.0402,"r-cache-n":94100,"r-cache-usd":0.0282,"w-cache-5m-n":3800,"w-cache-5m-usd":0.0143,"w-cache-1h-n":4900,"w-cache-1h-usd":0.0294,"total-usd":0.1554,"p5h":null,"p7d":null,"p7ds":null}
```

`p5h`, `p7d`, `p7ds` are `null` at write time. The governor annotates them in the SQLite mirror by apportioning each window's observed percentage delta across concurrent sessions, weighted by `total-usd`.

#### Record Type `f` — Fleet Wide Row

**One line per interval.** All models × all token types appear as columns, prefixed by model name: `[model]-[tok_type]-n` and `[model]-[tok_type]-usd`. Zero-filled for models with no activity in the interval. Ends with fleet-level totals, per-worker variance stats, and percentage deltas.

```
{"r":"f","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","pk":0,"workers":2,"claude-sonnet-4-6-input-n":29840,"claude-sonnet-4-6-input-usd":0.0924,"claude-sonnet-4-6-output-n":5510,"claude-sonnet-4-6-output-usd":0.0828,"claude-sonnet-4-6-r-cache-n":198300,"claude-sonnet-4-6-r-cache-usd":0.0595,"claude-sonnet-4-6-w-cache-5m-n":7900,"claude-sonnet-4-6-w-cache-5m-usd":0.0296,"claude-sonnet-4-6-w-cache-1h-n":10400,"claude-sonnet-4-6-w-cache-1h-usd":0.0624,"claude-opus-4-6-input-n":0,"claude-opus-4-6-input-usd":0,"claude-opus-4-6-output-n":0,"claude-opus-4-6-output-usd":0,"claude-opus-4-6-r-cache-n":0,"claude-opus-4-6-r-cache-usd":0,"claude-opus-4-6-w-cache-5m-n":0,"claude-opus-4-6-w-cache-5m-usd":0,"claude-opus-4-6-w-cache-1h-n":0,"claude-opus-4-6-w-cache-1h-usd":0,"total-usd":0.3201,"p75-usd-hr":2.147,"std-usd-hr":0.312,"p5h":0.66,"p7d":0.54,"p7ds":0.75}
```

The column set is fixed at startup from the pricing config — all configured models appear in every `f` row, zero-filled when inactive. This keeps the schema stable and rows directly comparable across time.

#### Record Type `w` — Window Forecast Row

**Three lines per interval** (one per window), written last. Self-contained for capacity forecast queries.

```
{"r":"w","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","win":"five_hour","snap":36.4,"reset":"2026-03-18T15:59:59Z","delta":0.66,"remain":63.6,"hrs_left":1.50,"fleet_pct_hr":7.92,"exh_hrs":8.03,"bind":0,"safe_w":null,"pk":0}
{"r":"w","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","win":"seven_day","snap":72.6,"reset":"2026-03-20T03:00:00Z","delta":0.54,"remain":27.4,"hrs_left":37.5,"fleet_pct_hr":6.48,"exh_hrs":4.23,"bind":0,"safe_w":null,"pk":0}
{"r":"w","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","win":"seven_day_sonnet","snap":63.5,"reset":"2026-03-20T03:59:59Z","delta":0.75,"remain":36.5,"hrs_left":37.5,"fleet_pct_hr":9.00,"exh_hrs":4.06,"bind":1,"safe_w":2,"pk":0}
```

---

**Fast-query SQLite mirror** at `~/.needle/state/token-history.db` (JSONL is authoritative; DB rebuilt on corruption). Table schemas are wide to match the records:

```sql
-- Type "i": one row per session per interval
CREATE TABLE i (
    ts TEXT, t0 TEXT, t1 TEXT, sess TEXT, sid TEXT, model TEXT, pk INTEGER,
    "input-n" INTEGER,     "input-usd" REAL,
    "output-n" INTEGER,    "output-usd" REAL,
    "r-cache-n" INTEGER,   "r-cache-usd" REAL,
    "w-cache-5m-n" INTEGER,"w-cache-5m-usd" REAL,
    "w-cache-1h-n" INTEGER,"w-cache-1h-usd" REAL,
    "total-usd" REAL,
    p5h REAL, p7d REAL, p7ds REAL   -- null until governor annotates
);
CREATE INDEX i_t0_sess ON i(t0, sess);
CREATE INDEX i_model_t0 ON i(model, t0);

-- Type "f": one wide row per interval; columns generated from pricing config
-- Example with claude-sonnet-4-6 and claude-opus-4-6 configured:
CREATE TABLE f (
    ts TEXT, t0 TEXT, t1 TEXT, pk INTEGER, workers INTEGER,
    "claude-sonnet-4-6-input-n" INTEGER,      "claude-sonnet-4-6-input-usd" REAL,
    "claude-sonnet-4-6-output-n" INTEGER,     "claude-sonnet-4-6-output-usd" REAL,
    "claude-sonnet-4-6-r-cache-n" INTEGER,    "claude-sonnet-4-6-r-cache-usd" REAL,
    "claude-sonnet-4-6-w-cache-5m-n" INTEGER, "claude-sonnet-4-6-w-cache-5m-usd" REAL,
    "claude-sonnet-4-6-w-cache-1h-n" INTEGER, "claude-sonnet-4-6-w-cache-1h-usd" REAL,
    "claude-opus-4-6-input-n" INTEGER,        "claude-opus-4-6-input-usd" REAL,
    "claude-opus-4-6-output-n" INTEGER,       "claude-opus-4-6-output-usd" REAL,
    "claude-opus-4-6-r-cache-n" INTEGER,      "claude-opus-4-6-r-cache-usd" REAL,
    "claude-opus-4-6-w-cache-5m-n" INTEGER,   "claude-opus-4-6-w-cache-5m-usd" REAL,
    "claude-opus-4-6-w-cache-1h-n" INTEGER,   "claude-opus-4-6-w-cache-1h-usd" REAL,
    "total-usd" REAL,
    "p75-usd-hr" REAL, "std-usd-hr" REAL,
    p5h REAL, p7d REAL, p7ds REAL
);
CREATE INDEX f_t0 ON f(t0);

-- Type "w": one row per window per interval
CREATE TABLE w (
    ts TEXT, t0 TEXT, t1 TEXT, win TEXT,
    snap REAL, reset TEXT, delta REAL,
    remain REAL, hrs_left REAL,
    fleet_pct_hr REAL, exh_hrs REAL,
    bind INTEGER, safe_w INTEGER, pk INTEGER
);
CREATE INDEX w_win_t0 ON w(win, t0);

-- Cross-instance comparison: all sessions side by side for a given interval
CREATE VIEW instance_compare AS
SELECT t0, sess, model,
    "total-usd", "input-usd", "output-usd",
    "r-cache-usd", "w-cache-5m-usd", "w-cache-1h-usd",
    "total-usd" / ((julianday(t1)-julianday(t0))*24) AS usd_per_hour,
    p7ds
FROM i ORDER BY t0 DESC, "total-usd" DESC;
```

**Standalone CLI:**
```bash
token-collector --collect             # one collection pass; write i+f+w lines
token-collector --daemon              # loop every N minutes
token-collector --query [--last N]    # recent w rows (window forecasts)
token-collector --compare [--at TS]   # instance_compare view for latest (or given) interval
token-collector --fleet [--last N]    # recent f rows showing all model×tok_type columns
token-collector --rebuild-db          # reconstruct SQLite from JSONL
```

---

### 3. State Store

**File:** `~/.needle/state/governor-state.json`

```json
{
  "updated_at": "2026-03-18T14:30:00Z",

  "usage": {
    "sonnet_pct": 72.0,
    "all_models_pct": 81.0,
    "five_hour_pct": 14.0,
    "sonnet_resets_at": "2026-03-20T03:59:59Z",
    "five_hour_resets_at": "2026-03-18T15:59:59Z"
  },

  "last_fleet_aggregate": {
    "t0": "2026-03-18T14:25:00Z",
    "t1": "2026-03-18T14:30:00Z",
    "sonnet_workers": 2,
    "sonnet_usd_total": 0.3201,
    "sonnet_p75_usd_hr": 2.147,
    "sonnet_std_usd_hr": 0.312,
    "window_pct_deltas": { "five_hour": 0.66, "seven_day": 0.54, "seven_day_sonnet": 0.75 }
  },

  "capacity_forecast": {
    "five_hour": {
      "remaining_pct":              63.6,
      "hours_remaining":            1.50,
      "fleet_pct_per_hour":         7.92,
      "predicted_exhaustion_hours": 8.03,
      "will_exhaust_before_reset":  false,
      "binding":                    false
    },
    "seven_day": {
      "remaining_pct":              27.4,
      "hours_remaining":            37.5,
      "fleet_pct_per_hour":         6.48,
      "predicted_exhaustion_hours": 4.23,
      "will_exhaust_before_reset":  true,
      "binding":                    false
    },
    "seven_day_sonnet": {
      "remaining_pct":              36.5,
      "hours_remaining":            37.5,
      "fleet_pct_per_hour":         9.00,
      "predicted_exhaustion_hours": 4.06,
      "will_exhaust_before_reset":  true,
      "binding":                    true,
      "safe_worker_count":          2
    },
    "binding_window":      "seven_day_sonnet",
    "dollars_per_pct_7d_s": 1.648,
    "estimated_remaining_dollars": 46.1
  },

  "schedule": {
    "is_peak_hour": false,
    "is_promo_active": true,
    "promo_multiplier": 2.0,
    "effective_hours_remaining": 84.5,
    "raw_hours_remaining": 37.5
  },

  "workers": {
    "claude-anthropic-sonnet": {
      "current": 2,
      "target": 3,
      "min": 1,
      "max": 5
    }
  },

  "burn_rate": {
    "by_model": {
      "claude-sonnet-4-6": {
        "pct_per_worker_per_hour": 1.35,
        "dollars_per_worker_per_hour": 5.54,
        "samples": 12
      },
      "claude-opus-4-6": {
        "pct_per_worker_per_hour": 3.80,
        "dollars_per_worker_per_hour": 9.21,
        "samples": 4
        // NOTE: Opus 4.6 is $5/$25/MTok (not $15/$75) — dollar rate ~1.7x Sonnet, not 4-5x
      }
    },
    "tokens_per_pct_peak": 69780,
    "tokens_per_pct_offpeak": 141350,
    "offpeak_ratio_observed": 2.03,
    "offpeak_ratio_expected": 2.0,
    "promotion_validated": true,
    "last_sample_at": "2026-03-18T14:15:00Z"
  },

  "alerts": []
}
```

**Previous state file** (`~/.needle/state/governor-state.prev.json`) is atomically written before each update, enabling burn rate calculation from `delta_pct / delta_time`.

---

### 3. Promotion and Schedule Awareness

**Peak window:** 08:00–14:00 US Eastern Time (weekdays only)
**Off-peak:** Everything else (all weekends, weekday evenings/nights)

**Promotion detection:**
- Governor ships with a `promotions.json` config file listing active promotions:

```json
[
  {
    "name": "March 2026 Off-Peak 2x",
    "start": "2026-03-13",
    "end": "2026-03-28",
    "peak_start_hour_et": 8,
    "peak_end_hour_et": 14,
    "offpeak_multiplier": 2.0,
    "applies_to": ["seven_day_sonnet", "seven_day", "five_hour"]
  }
]
```

- When no active promotion, `offpeak_multiplier = 1.0` (flat model)
- The schedule calculator returns the current effective multiplier at any moment

**Effective capacity calculation:**
```python
def effective_hours_remaining(reset_time, promotions):
    """Compute effective hours remaining accounting for off-peak bonuses."""
    now = datetime.now(UTC)
    total = 0.0
    t = now
    while t < reset_time:
        multiplier = get_multiplier_at(t, promotions)
        total += multiplier / 60  # per-minute granularity
        t += timedelta(minutes=1)
    return total
```

**Target rate calculation (corrected from existing governor):**
```
remaining_capacity = 100 - sonnet_pct
# effective_hours accounts for 2x off-peak windows
target_rate_per_effective_hour = remaining_capacity / effective_hours_remaining
target_rate_per_raw_hour = target_rate_per_effective_hour * current_multiplier
target_workers = floor(target_rate_per_raw_hour / burn_rate_per_worker_per_hour)
target_workers = clamp(target_workers, min_workers, max_workers)
```

---

### 5. Adaptive Burn Rate Estimator

**Problem:** The current `1.2% per worker per hour` constant is unvalidated, model-agnostic, and window-agnostic. Three independent windows (5h session, 7d all-models, 7d Sonnet) each have their own utilization curve and reset time. A single %/hr estimate cannot simultaneously optimize all three.

**Solution:** Compute burn rates **per model, per window** using per-instance delta records from the Token Collector. Variance across instances drives conservative planning.

#### Per-Instance Burn Rate (from `instance_deltas`)

```
# For each instance_delta record with annotated window_pct_deltas:
dollar_burn[session][window] = dollar_equiv.total / elapsed_hours
pct_burn[session][window]    = window_pct_deltas[window] / elapsed_hours
```

Dollar breakdown by token type (Sonnet 4.6, with corrected pricing):
```
delta_usd = (input       * 3.00
           + output      * 15.00
           + cw_5m       * 3.75   # 1.25x input; ephemeral_5m_input_tokens
           + cw_1h       * 6.00   # 2.0x input;  ephemeral_1h_input_tokens
           + cache_read  * 0.30   # 0.1x input
           ) / 1_000_000

# Opus 4.6 — $5/$25/MTok (NOT $15/$75 — those are legacy Opus 4.1/4 rates)
delta_usd = (input       * 5.00
           + output      * 25.00
           + cw_5m       * 6.25
           + cw_1h       * 10.00
           + cache_read  * 0.50
           ) / 1_000_000
```

#### Fleet-Level Burn Rate and Variance

Aggregate instance records for the same interval to get fleet-level statistics:

```
# Fleet burn rate for window W at time T, N workers active:
fleet_pct_per_hour[W] = sum(pct_burn[session][W] for all sessions) / elapsed_hours

# Per-worker distribution (used for safe_worker_count):
rates = [dollar_burn[s] for s in sessions]
mean     = avg(rates)
stddev   = stdev(rates)
p75      = percentile(rates, 75)

# Conservative estimate for capacity planning:
conservative_rate_per_worker = p75  # 75th percentile, not mean
```

High `stddev` signals task heterogeneity — some workers handling large documents, others doing light edits. Using `p75` rather than `mean` ensures the capacity forecast doesn't underestimate risk when variance is high.

#### Per-Window EMA and Capacity Forecast

Maintain a separate EMA per (model, window) pair:

```
# EMA update after each fleet_aggregate interval:
ema_pct_per_hour[model][window] = α * fleet_pct_per_hour[window] + (1-α) * prev_ema
α = 0.2

# Capacity forecast per window:
remaining_pct[W]   = 100 - snapshot_utilization[W]
hours_remaining[W] = (resets_at[W] - now).total_seconds() / 3600

fleet_pct_per_hour[W]         = ema_pct_per_hour[model][W] * workers_active
predicted_exhaustion_hours[W] = remaining_pct[W] / fleet_pct_per_hour[W]
will_exhaust_before_reset[W]  = predicted_exhaustion_hours[W] < hours_remaining[W]

# Safe worker count: max workers before exhaustion, using conservative rate
safe_worker_count[W] = floor(remaining_pct[W] / hours_remaining[W] / p75_rate_per_worker)

# Binding window: whichever will exhaust first
binding_window = argmin(predicted_exhaustion_hours[W] for W in windows)
```

The governor's `compute_target_workers()` uses `safe_worker_count[binding_window]` as its ceiling, replacing the previous single-window formula. When the 5h and 7d windows give contradictory limits, the more constraining one (lower `safe_worker_count`) wins.

#### Dollar-Based Remaining Capacity

```
dollars_per_pct[W] = ema_dollar_per_hour / ema_pct_per_hour[W]
estimated_remaining_dollars[W] = dollars_per_pct[W] * remaining_pct[W]
```

This is window-specific because different models weight the all-models vs. Sonnet-only windows differently.

#### Promotion Validation Signal (per window)

```
tokens_per_pct[model][window] = token_burn_sample / pct_burn_sample
```

Stored separately for peak vs. off-peak intervals per window. The 5h and 7d windows may validate differently if the promotion applies asymmetrically across them.

#### Guard Conditions

- Skip if `elapsed < 2 min` — too short to be meaningful
- Skip if worker count changed mid-interval — can't cleanly attribute
- Skip if `delta_pct[W] == 0` across all windows but tokens > 0 — API rounding artifact
- Discard samples > 3σ from current EMA per window
- When a window resets mid-interval, discard that interval's pct_delta for that window only (it spans two separate quota periods)

**Claude Code cache behavior note:** Cache reads dominate token counts (cheap, 0.1× input) while 1h cache writes are the most expensive token type per unit (2.0× input). Dollar-equivalent burn rate is the most accurate single measure of plan consumption rate, more so than raw token count or even pct/hr alone.

---

### 6. Worker Manager

#### Scale-Up
```bash
for i in $(seq 1 $((target - current))); do
    # Auto-discover workspace with largest bead backlog
    needle run --agent="$AGENT" --force
done
```
- Remove the hard-coded `--workspace` arg — let NEEDLE auto-discover the richest workspace
- One launch per loop tick (not batch) to avoid tmux session naming collisions

#### Scale-Down (Graceful Only)
```bash
# Find idle workers (status == "idle" in heartbeat files)
idle_sessions=$(find ~/.needle/state/heartbeats/ -name "needle-${AGENT}-*.json" \
    -exec jq -r 'select(.status == "idle") | .session' {} \;)

# Sort by launch order (reverse NATO = most recent first)
# Only kill workers not attached to a human terminal
for session in $idle_sessions; do
    attached=$(tmux display-message -t "$session" -p '#{session_attached}' 2>/dev/null)
    [[ "$attached" -gt 0 ]] && continue  # skip if human is watching
    tmux send-keys -t "$session" "C-c" 2>/dev/null  # graceful SIGINT
    sleep 2
    # Force kill if still alive after 10 seconds
    tmux kill-session -t "$session" 2>/dev/null
    ((killed++))
    [[ $killed -ge $((current - target)) ]] && break
done
```

**Key principle:** Never kill an `executing` worker. Only kill `idle` workers. If no idle workers are available but current > target, wait until the next cycle.

---

### 7. Hysteresis

**Problem:** Without hysteresis, minor fluctuations cause thrashing.

**Solution:** Dead-band hysteresis — only act if deviation exceeds threshold.

```
current_workers = count_active_workers()
target_workers = compute_target()

if current_workers < target_workers - HYSTERESIS_BAND:
    scale_up(target_workers - current_workers)
elif current_workers > target_workers + HYSTERESIS_BAND:
    scale_down_graceful(current_workers - target_workers)
# else: within band — no action
```

`HYSTERESIS_BAND = 1` (default): only act when deviation is ≥ 2 workers.

This prevents the current thrash pattern where 1 worker exits (idle_timeout) and is immediately relaunched every 15 minutes.

---

### 8. Alert Manager

Alerts are created as HUMAN-type NEEDLE beads that workers will not claim, surfacing them for human review.

**Alert conditions:**

| Condition | Alert Type | Action |
|---|---|---|
| `sonnet_pct >= 90` and `hours_remaining > 12` | `capacity_warning` | Create HUMAN bead: "Sonnet at 90% with 12h+ until reset — pace workers down" |
| `sonnet_pct >= 95` | `capacity_critical` | Reduce `SONNET_MAX` to 1; create HUMAN bead |
| `five_hour_pct >= 90` | `session_warning` | Log warning; no worker change |
| `five_hour_pct >= 100` | `session_exhausted` | Scale to 0 workers until session resets |
| `burn_rate_sample > baseline * 2` | `burn_rate_spike` | Log anomaly; increase polling rate |
| Reset in < 2h and `sonnet_pct < 50` | `underutilization` | Scale to SONNET_MAX to consume remaining budget |

**Deduplication:** Each alert type is only created once per governor cycle (store last-alerted timestamp per type in state file).

---

### 9. Emergency Brake

If `seven_day_sonnet.utilization >= 98%`, immediately scale all workers to 0 regardless of hysteresis or idle state. Log `EMERGENCY BRAKE APPLIED`. This is the hard stop that prevents plan overages.

---

## Configuration File

`~/.needle/config/governor.yaml`:

```yaml
# Governor configuration
loop_interval: 300          # seconds between cycles (5 minutes)
hysteresis_band: 1          # workers deviation before acting
log_file: ~/.needle/logs/governor.log
state_file: ~/.needle/state/governor-state.json

# Managed agents
agents:
  claude-anthropic-sonnet:
    enabled: true
    min_workers: 1
    max_workers: 5
    baseline_burn_rate: 1.2    # % per worker per hour (initial estimate)
    burn_rate_alpha: 0.2       # EMA smoothing factor
    workspace: ""              # empty = auto-discover
    launch_args: "--force"

  claude-anthropic-opus:
    enabled: false             # not managed by default
    min_workers: 0
    max_workers: 2
    baseline_burn_rate: 4.0    # Opus consumes ~3-4x more quota than Sonnet
    workspace: ""

# Promotion definitions
promotions_file: ~/.needle/config/promotions.json

# API pricing (USD per million tokens) — used for dollar-equivalent burn rate
# Source: https://platform.claude.com/docs/en/about-claude/pricing
# Update when Anthropic changes public pricing.
# NOTE: Two cache write tiers exist (5-minute TTL and 1-hour TTL) with different rates.
# The API response's usage.cache_creation sub-object distinguishes them.
pricing:
  claude-sonnet-4-6:
    input_per_mtok: 3.00
    output_per_mtok: 15.00
    cache_write_5m_per_mtok: 3.75   # 1.25x input
    cache_write_1h_per_mtok: 6.00   # 2.0x input
    cache_read_per_mtok: 0.30       # 0.1x input
  claude-sonnet-4-5:
    input_per_mtok: 3.00
    output_per_mtok: 15.00
    cache_write_5m_per_mtok: 3.75
    cache_write_1h_per_mtok: 6.00
    cache_read_per_mtok: 0.30
  claude-opus-4-6:                  # NOTE: Opus 4.6 is $5/$25, NOT $15/$75
    input_per_mtok: 5.00            # Opus 4.1/4 were $15/$75 — those are legacy models
    output_per_mtok: 25.00
    cache_write_5m_per_mtok: 6.25
    cache_write_1h_per_mtok: 10.00
    cache_read_per_mtok: 0.50
  claude-opus-4-5:
    input_per_mtok: 5.00
    output_per_mtok: 25.00
    cache_write_5m_per_mtok: 6.25
    cache_write_1h_per_mtok: 10.00
    cache_read_per_mtok: 0.50
  claude-haiku-4-5:                 # NOTE: Haiku 4.5 is $1/$5, not $0.80/$4 (those are Haiku 3.5)
    input_per_mtok: 1.00
    output_per_mtok: 5.00
    cache_write_5m_per_mtok: 1.25
    cache_write_1h_per_mtok: 2.00
    cache_read_per_mtok: 0.10
  claude-haiku-3-5:
    input_per_mtok: 0.80
    output_per_mtok: 4.00
    cache_write_5m_per_mtok: 1.00
    cache_write_1h_per_mtok: 1.60
    cache_read_per_mtok: 0.08

# Token collector
token_collector:
  enabled: true
  interval: 120              # seconds between collection passes
  jsonl_file: ~/.needle/state/token-history.jsonl
  db_file: ~/.needle/state/token-history.db
  source_glob: ~/.claude/projects/**/*.jsonl

# Alert thresholds
alerts:
  capacity_warning_pct: 90
  capacity_critical_pct: 95
  emergency_brake_pct: 98
  underutilization_threshold_pct: 50
  underutilization_hours_remaining: 2
```

---

## Implementation Plan

### Phase 1: Usage Poller (Foundation)

**Goal:** Replace `claude-status.sh` with reliable direct API polling.

1. Write `scripts/poll-usage.sh`:
   - Reads `~/.claude/.credentials.json` for OAuth token
   - Checks token expiry; refreshes if needed
   - Calls `/api/oauth/usage`
   - Outputs JSON to stdout
   - Handles errors (rate-limit, network, invalid token)

2. Write `scripts/parse-usage.py` (or inline Python):
   - Parses API response
   - Computes `hours_remaining` from `resets_at`
   - Outputs structured fields for governor consumption

3. Test: Run every minute for 30 minutes, verify output matches TUI `/status` values.

**Deliverable:** `scripts/poll-usage.sh` — standalone, can be used by other scripts.

---

### Phase 1b: Token Collector (Independent Data Capture)

**Goal:** Independently capture model-specific, token-type-specific consumption data. Runs as a separate daemon; the governor reads from its output but does not depend on it being active to function.

1. Write `scripts/token-collector.py`:
   - Walks `~/.claude/projects/**/*.jsonl` to find unprocessed lines (tracks cursor per file in `~/.needle/state/collector-cursors.json`)
   - Parses each assistant message's `usage` block — must read the nested `cache_creation` sub-object to split writes by tier:
     - `usage.input_tokens` — fresh input
     - `usage.output_tokens` — output
     - `usage.cache_read_input_tokens` — cache hits (0.1× input rate)
     - `usage.cache_creation.ephemeral_5m_input_tokens` — 5-min writes (1.25× input rate)
     - `usage.cache_creation.ephemeral_1h_input_tokens` — 1-hour writes (2.0× input rate)
     - Falls back to `usage.cache_creation_input_tokens` as 5m when sub-object absent
   - Extracts `model` from the message or infers from session path
   - Accumulates deltas per model per collection interval
   - Computes dollar equivalent for each token type using pricing from `governor.yaml`
   - Appends one JSONL record per model per interval to `token-history.jsonl`
   - Mirrors to `token-history.db` SQLite for fast queries
   - Exposes `--query` and `--summary` modes for inspection

2. Write cursor tracking (`collector-cursors.json`):
   - Stores `{filepath: byte_offset}` so restarts resume where they left off without re-scanning
   - New files detected via glob on each pass

3. `window_pct_deltas` annotation: on each governor poll cycle, the governor joins the interval's instance records to the concurrent API percentage snapshots to fill in `pct_delta_5h`, `pct_delta_7d`, `pct_delta_7d_s`. Apportionment across instances uses each instance's `dollar_equiv.total` as the weight (heavier-spending workers are attributed proportionally more of the observed percentage movement). This is the only field the collector cannot self-populate.

4. Write one `fleet_aggregate` record per interval after all `instance_delta` records, containing aggregated stats, `per_worker_stats` (mean/p75/stddev), `window_snapshots` from the API, and the full `capacity_forecast` block.

5. Test independently:
   - Verify dollar computation against known API pricing (unit test each tok_type × model)
   - Verify delta (not cumulative) — run twice, confirm second pass emits zero-n records
   - Verify correct model attribution when multiple models active in same session
   - Verify `f` record variance stats with synthetic data: 3 sessions, divergent `usd` values
   - Verify `w` record `bind` and `safe_w` selection when 5h is more constraining than 7d
   - Verify grep/jq queryability: `jq 'select(.r=="i" and .tok=="w-cache-1h")'` returns only those lines

**Deliverable:** `scripts/token-collector.py` — fully standalone, can be queried without the governor running.

---

### Phase 2: Schedule and Promotion Calculator

**Goal:** Accurate effective-hours calculation with off-peak awareness.

1. Write `scripts/schedule.py`:
   - `is_peak_now()` → bool
   - `current_multiplier()` → float (1.0 or 2.0 during promo)
   - `effective_hours_remaining(reset_time)` → float
   - Reads from `promotions.json`

2. Write `promotions.json` with the March 2026 promotion entry.

3. Unit tests:
   - Peak hour boundaries (7:59 AM ET → 1x, 8:00 AM ET → 1x peak, 2:01 PM ET → 2x)
   - Weekend classification
   - Past-promo-end returns 1x
   - Effective hours: 40h reset with 30h off-peak should be > 40

4. **Promotion validation against measured consumption:**

   The `offpeak_multiplier: 2.0` in `promotions.json` is taken from the official announcement — it needs to be confirmed against observed data before the governor trusts it for scheduling.

   **Validation approach:** Once the poller has accumulated ≥5 peak-hour samples and ≥5 off-peak samples with the same worker count, compute:
   ```
   observed_ratio = median(tokens_per_pct_offpeak_samples) / median(tokens_per_pct_peak_samples)
   ```
   - If `observed_ratio` is within 10% of `offpeak_multiplier` (e.g., 1.8–2.2 for a declared 2.0): **validated**, log confirmation.
   - If `observed_ratio < 1.2`: promotion may not be applying — log warning, fall back to 1x multiplier until re-validated.
   - If `observed_ratio > 2.5`: unexpected — log anomaly, use observed ratio instead of declared.

   Write validation result to `burn_rate.promotion_validated` in state file. The scheduler reads this flag: if `false`, it uses `offpeak_multiplier: 1.0` (conservative) rather than the declared 2.0.

   This guards against: the promotion ending early, the multiplier applying to some limit buckets but not others, or a future promotion with a different multiplier being misconfigured.

**Deliverable:** `scripts/schedule.py` + `config/promotions.json` + promotion validation logic in burn-rate estimator

---

### Phase 3: Adaptive Burn Rate Estimator

**Goal:** Empirically calibrate per-model burn rates in %/hr, tokens/hr, and $/hr.

1. Extend state store with `burn_rate.by_model`, `last_fleet_aggregate`, and `capacity_forecast` as specified in the State Store schema.

2. Write `scripts/burn-rate.py`:
   - Reads `instance_delta` records from `token-history.db` for the most recent complete interval
   - Computes per-instance `dollar_burn` and `pct_burn` per window from annotated `window_pct_deltas`
   - Computes fleet-level `per_worker_stats` (mean, p75, stddev) across active sessions
   - Maintains per-(model, window) EMA with `alpha = 0.2`
   - Generates `capacity_forecast` per window: `fleet_pct_per_hour`, `predicted_exhaustion_hours`, `will_exhaust_before_reset`, `safe_worker_count`
   - Identifies `binding_window` — the window that will exhaust soonest
   - Stores separate peak/off-peak `tokens_per_pct` per window for promotion validation
   - Guard conditions: skip short intervals, changed worker counts, window resets mid-interval, zero-delta API responses
   - Falls back to `baseline_pct_per_worker_per_hour` from `governor.yaml` until 3 valid samples per window

3. Update `compute_target_workers()` in governor to use `safe_worker_count[binding_window]` as the ceiling rather than the single-window formula.

4. Log per-window capacity forecast each cycle:
   ```
   [governor] 5h: 63.6% remaining, resets in 1.5h — OK (exhausts in 8h at current rate)
   [governor] 7d: 27.4% remaining, resets in 37.5h — OK (not binding)
   [governor] 7d-sonnet: 36.5% remaining, resets in 37.5h — BINDING (exhausts in 4.1h at 2 workers)
   [governor] → target: 2 workers (safe_worker_count from binding window)
   ```

**Deliverable:** `scripts/burn-rate.py` + updated state schema + per-window capacity forecast

---

### Phase 4: Core Governor Loop

**Goal:** Replace `capacity-governor.sh` with the full governor.

1. Write `scripts/governor.sh` (main daemon):
   ```
   while true; do
       usage=$(poll_usage)
       schedule=$(compute_schedule)
       burn_rate=$(compute_burn_rate)
       target=$(compute_target_workers usage schedule burn_rate)
       current=$(count_workers)
       apply_scaling current target
       check_alerts usage schedule
       write_state
       sleep $LOOP_INTERVAL
   done
   ```

2. Implement `compute_target_workers()` using corrected formula with effective hours.

3. Implement `apply_scaling()` with hysteresis band and graceful scale-down.

4. Implement emergency brake (>= 98% → scale to 0).

5. Write `scripts/worker-manager.sh`:
   - `scale_up(n)`: call `needle run` n times
   - `scale_down_graceful(n)`: find idle workers, send SIGINT, fall back to kill after timeout
   - `count_workers()`: use `needle list` + heartbeat status

6. `--dry-run` mode: compute and log everything but do not modify workers.

**Deliverable:** `scripts/governor.sh` — replaces `capacity-governor.sh`

---

### Phase 5: Alert Manager

**Goal:** Surfacing important state transitions to human attention.

1. Write `scripts/alerts.sh`:
   - Check each alert condition against thresholds
   - Check if alert already fired this period (dedup by state file timestamp)
   - Create HUMAN-type NEEDLE bead via `br create --type human`
   - Log alert to `governor.log`

2. Add `last_alerted` per-type tracking to state file.

3. Add "underutilization sprint" logic: if < 50% used and < 2h to reset, boost to SONNET_MAX.

**Deliverable:** `scripts/alerts.sh`

---

### Phase 6: Packaging and Deployment

**Goal:** Make the governor easy to install and run as a persistent daemon.

1. Write `install.sh`:
   - Copies scripts to `~/.needle/bin/`
   - Writes default `governor.yaml` if not present
   - Creates systemd user service OR launchd plist (cross-platform)
   - Optionally migrates from existing `capacity-governor.sh`

2. Write `governor.service` (systemd user unit):
   ```ini
   [Unit]
   Description=Claude Governor — quota-aware worker scaler
   After=network.target

   [Service]
   Type=simple
   ExecStart=%h/.needle/bin/governor.sh --loop
   Restart=on-failure
   RestartSec=60

   [Install]
   WantedBy=default.target
   ```

3. Write `README.md` with quickstart, configuration guide, and troubleshooting.

**Deliverable:** Installable package with systemd service

---

## File Layout

```
claude-governor/
├── scripts/
│   ├── governor.sh           # Main daemon (Phase 4)
│   ├── poll-usage.sh         # Direct API usage poller (Phase 1)
│   ├── token-collector.py    # Independent token delta collector (Phase 1b)
│   ├── worker-manager.sh     # Scale-up/down logic (Phase 4)
│   ├── alerts.sh             # Alert creation (Phase 5)
│   ├── schedule.py           # Peak/off-peak calculator (Phase 2)
│   └── burn-rate.py          # Model-specific burn rate EMA (Phase 3)
├── config/
│   ├── governor.yaml         # Main configuration (incl. pricing table)
│   └── promotions.json       # Promotion window definitions
├── systemd/
│   ├── governor.service      # Systemd user service — governor daemon
│   └── token-collector.service  # Systemd user service — token collector daemon
├── install.sh                # Installation helper
├── docs/
│   ├── research/
│   │   ├── usage-tracking.md
│   │   ├── off-hours-promotion.md
│   │   └── needle-architecture.md
│   └── plan/
│       └── plan.md           # This document
└── README.md
```

**Runtime state files** (written to `~/.needle/state/`):

| File | Written by | Purpose |
|---|---|---|
| `governor-state.json` | governor | Current scaling state, burn rates, capacity estimate |
| `governor-state.prev.json` | governor | Previous cycle snapshot for delta calculation |
| `token-history.jsonl` | token-collector | Append-only per-interval token delta records |
| `token-history.db` | token-collector | SQLite mirror for fast queries |
| `collector-cursors.json` | token-collector | File byte offsets to avoid re-processing |

---

## Key Improvements Over Existing Governor

| Area | Existing `capacity-governor.sh` | New Governor |
|---|---|---|
| **Usage source** | TUI screen-scraper (~10s, fragile) | Direct API call (~200ms, reliable) |
| **Off-peak math** | `effective_hours` computed but not used | Fully integrated into target calculation |
| **Burn rate** | Hardcoded `1.2%/worker/hr`, model-agnostic | Per-model EMA in %/hr, tokens/hr, and $/hr |
| **Token tracking** | None | Per-instance delta by type: input/output/cache-read/cw-5m/cw-1h |
| **Dollar equivalent** | None | $/hr burn and estimated remaining API-equivalent value per window |
| **Promotion validation** | Assumed correct | Cross-validated per window against observed tokens-per-percent ratio |
| **Capacity estimate** | % remaining only | Per-window forecast: exhaustion time, safe worker count, binding window |
| **Cross-instance comparison** | None | Per-worker variance (mean/p75/stddev) drives conservative planning |
| **Multi-window optimization** | Single window | All three windows tracked independently; binding window governs target |
| **Scale-down** | `tmux kill-session` (forceful) | Graceful SIGINT to idle workers only |
| **Hysteresis** | None — thrashes every cycle | ±1 worker dead band |
| **Workspace** | Hard-coded `kalshi-trading` | Auto-discovered by needle run |
| **Alerting** | Log-only | NEEDLE HUMAN-type beads + log |
| **Emergency stop** | None | Hard 98% brake |
| **Token expiry** | Not handled | Auto-refresh before expiry |
| **Configurability** | Shell constants only | `governor.yaml` |
| **Extensibility** | Sonnet only | Plugin per agent type |

---

## Risk Considerations

1. **API rate limiting on `/api/oauth/usage`:** Poll no faster than every 5 minutes. The API self-rate-limits and returns errors; governor should handle gracefully (use last known state on error).

2. **Token expiry during long operation:** Check expiry before each API call; refresh token proactively at 5-minute warning.

3. **Worker kill vs. human session:** Always check `#{session_attached}` before killing a tmux session. Attaching to a worker session is a valid debugging workflow.

4. **Bead state after forced kill:** If a worker is killed mid-task, the bead remains `IN_PROGRESS` until the stale claim threshold fires. Prefer graceful shutdown to avoid this.

5. **Promotion end date:** After March 28, the governor must correctly revert to 1x flat model. Test the `promotions.json` cutoff logic explicitly.

6. **Multiple accounts / credential rotation:** The poller assumes a single `~/.claude/.credentials.json`. If multiple accounts are used, parameterize the credentials path.

7. **Token collector lag:** The collector reads JSONL files that may be flushed with a short delay after each API call. For burn rate samples, use intervals ≥ 2 minutes to ensure all requests in the window have been written. The `pct_delta` annotation from the API snapshot is the authoritative signal; token deltas enrich it.

8. **Pricing staleness and model generation confusion:** The `pricing` block in `governor.yaml` must be manually updated when Anthropic changes API rates. Critically, Opus 4.6/4.5 pricing ($5/$25) is dramatically different from Opus 4.1/4 ($15/$75) — a stale config using the wrong generation will produce 3× dollar-equivalent errors. Log the pricing source URL and snapshot date in `governor.yaml`. When a new model version is detected in token records but not found in the pricing config, log a warning and fall back to the nearest known model's pricing rather than silently using wrong rates.

9. **Single vs. two-tier cache write fallback:** Not all API responses include the `cache_creation` sub-object (it may be absent in older response formats or certain service tiers). When absent, attribute all `cache_creation_input_tokens` to the 5-minute tier (1.25× rate) as the conservative fallback — this slightly underestimates cost rather than overestimating.

9. **Model attribution in multi-model sessions:** A single Claude Code session can make calls to multiple models (e.g., a tool-call routing to Haiku while the main conversation uses Sonnet). Token records must be attributed per-model from the `model` field in each response, not inferred from the session path alone.
