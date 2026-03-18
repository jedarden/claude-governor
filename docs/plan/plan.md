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

**Output — append-only JSONL** at `~/.needle/state/token-history.jsonl`. One record per collection interval per model:

```json
{
  "ts": "2026-03-18T14:30:00Z",
  "interval_minutes": 5,
  "is_peak": false,
  "workers_active": 2,
  "model": "claude-sonnet-4-6",
  "delta": {
    "input_tokens": 45230,
    "output_tokens": 8340,
    "cache_read_tokens": 312000,
    "cache_write_tokens": 28500
  },
  "dollar_equiv": {
    "input": 0.1357,
    "output": 0.1251,
    "cache_read": 0.0936,
    "cache_write": 0.1069,
    "total": 0.4613
  },
  "plan_pct_delta": 0.8
}
```

`plan_pct_delta` is filled in by the governor when it joins the token record to the concurrent API percentage snapshot. The collector records tokens; the governor annotates with the percent movement seen at the same interval.

**Fast-query SQLite mirror** at `~/.needle/state/token-history.db` (written by collector alongside JSONL; JSONL is authoritative):

```sql
CREATE TABLE token_intervals (
    ts          TEXT NOT NULL,
    model       TEXT NOT NULL,
    is_peak     INTEGER NOT NULL,
    workers     INTEGER NOT NULL,
    in_tok      INTEGER, out_tok INTEGER,
    cr_tok      INTEGER, cw_tok  INTEGER,
    usd_total   REAL,
    pct_delta   REAL
);
CREATE INDEX idx_ts_model ON token_intervals(ts, model);
```

**Standalone CLI:**
```bash
token-collector --collect      # run one collection pass, append to JSONL+DB
token-collector --daemon       # loop every N minutes
token-collector --query        # print recent intervals as table
token-collector --summary      # model totals for current 7d window
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

  "token_deltas": {
    "claude-sonnet-4-6": {
      "interval_minutes": 5,
      "input_tokens": 45230,
      "output_tokens": 8340,
      "cache_read_tokens": 312000,
      "cache_write_tokens": 28500,
      "dollar_equiv": {
        "input": 0.1357,
        "output": 0.1251,
        "cache_read": 0.0936,
        "cache_write": 0.1069,
        "total": 0.4613
      }
    },
    "claude-opus-4-6": {
      "interval_minutes": 5,
      "input_tokens": 3100,
      "output_tokens": 920,
      "cache_read_tokens": 18400,
      "cache_write_tokens": 3200,
      "dollar_equiv": {
        "input": 0.0465,
        "output": 0.0690,
        "cache_read": 0.0276,
        "cache_write": 0.0600,
        "total": 0.2031
      }
    }
  },

  "capacity_estimate": {
    "claude-sonnet-4-6": {
      "remaining_pct": 28.0,
      "dollars_per_pct_observed": 1.648,
      "estimated_remaining_dollars": 46.1,
      "estimated_total_plan_dollar_value": 164.8
    }
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
        "dollars_per_worker_per_hour": 24.37,
        "samples": 4
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

**Problem:** The current `1.2% per worker per hour` constant is both unvalidated and model-agnostic. Opus workers consume far more quota per hour than Sonnet workers. Percentage burn rate is also plan-tier-unstable (1% on Max 20x represents far more tokens than 1% on Pro). Dollar-equivalent burn rate is stable across both dimensions.

**Solution:** Track burn rate **per model** in three parallel units — %/hr, tokens/hr, and $/hr — derived from consecutive state snapshots joined to Token Collector records.

```
# Per model, per interval
pct_burn_sample   = delta_pct   / elapsed_hours / workers_active
token_burn_sample = delta_tokens / elapsed_hours / workers_active   # sum of all token types
dollar_burn_sample = delta_usd  / elapsed_hours / workers_active

# Dollar breakdown by token type (Sonnet example):
delta_usd = (delta_input * 3.00 + delta_output * 15.00
           + delta_cache_write * 3.75 + delta_cache_read * 0.30) / 1_000_000
```

**Exponential moving average (EMA) per model:**
```
new_rate[model] = alpha * latest_sample[model] + (1 - alpha) * prev_rate[model]
alpha = 0.2
```

**Dollar-based remaining capacity estimate:**
```
dollars_per_pct[model] = ema_dollar_burn / ema_pct_burn   # $/% observed ratio
estimated_remaining_dollars = dollars_per_pct * remaining_pct
estimated_plan_value = dollars_per_pct * 100
```
This lets the governor answer: "you have approximately $X of API-equivalent value remaining," independent of plan tier. It also surfaces the effective $/month value being extracted from the subscription.

**Tokens-per-percent ratio (promotion validation signal, per model):**
```
tokens_per_pct[model] = token_burn_sample / pct_burn_sample
```
Stored separately for peak vs. off-peak intervals. During the off-peak promotion, the ratio should be ~2x, because the plan limit doubles but actual token consumption does not change. If the ratio stays flat, the promotion is not applying to that model's limit bucket.

**Guard conditions for sample validity:**
- Skip if `elapsed < 2 min` (too short to be meaningful)
- Skip if worker count changed mid-interval (burn rate can't be attributed cleanly)
- Skip if `delta_pct == 0` and `delta_tokens > 0` (possible API rounding artifact)
- Discard outliers > 3σ from current EMA

**Fallback per model:** Use configured `baseline_pct_per_worker_per_hour` from `governor.yaml` until 3 valid samples are available.

**Why model separation matters:** A fleet of 2 Sonnet + 1 Opus workers doesn't burn quota at `3 * sonnet_rate`. The Opus worker may burn 3–4x the quota per hour. A model-agnostic governor will systematically overshoot when Opus is active.

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
# Update when Anthropic changes public pricing
pricing:
  claude-sonnet-4-6:
    input_per_mtok: 3.00
    output_per_mtok: 15.00
    cache_write_per_mtok: 3.75
    cache_read_per_mtok: 0.30
  claude-opus-4-6:
    input_per_mtok: 15.00
    output_per_mtok: 75.00
    cache_write_per_mtok: 18.75
    cache_read_per_mtok: 1.50
  claude-haiku-4-5:
    input_per_mtok: 0.80
    output_per_mtok: 4.00
    cache_write_per_mtok: 1.00
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
   - Parses each assistant message's `usage` block: `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`
   - Extracts `model` from the message or infers from session path
   - Accumulates deltas per model per collection interval
   - Computes dollar equivalent for each token type using pricing from `governor.yaml`
   - Appends one JSONL record per model per interval to `token-history.jsonl`
   - Mirrors to `token-history.db` SQLite for fast queries
   - Exposes `--query` and `--summary` modes for inspection

2. Write cursor tracking (`collector-cursors.json`):
   - Stores `{filepath: byte_offset}` so restarts resume where they left off without re-scanning
   - New files detected via glob on each pass

3. `plan_pct_delta` annotation: on each governor poll cycle, join the most recent token collector record to the concurrent API percent snapshot to fill in `plan_pct_delta`. This is the only field the collector cannot populate itself.

4. Test independently:
   - Verify dollar computation against known API pricing
   - Verify delta (not cumulative) — run twice, confirm second pass only counts new messages
   - Verify correct model attribution when multiple models active in same session

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

1. Extend state store with `burn_rate.by_model` block, `token_deltas`, and `capacity_estimate` as specified in the State Store schema.

2. Write `scripts/burn-rate.py`:
   - Joins Token Collector records to API percent snapshots over matching intervals
   - Computes per-model samples: `pct_burn`, `token_burn`, `dollar_burn` (all per worker per hour)
   - Applies EMA per model with `alpha = 0.2`
   - Stores separate `tokens_per_pct_peak` and `tokens_per_pct_offpeak` for promotion validation
   - Computes `dollars_per_pct[model]` → `capacity_estimate` block
   - Guard conditions: skip short intervals, changed worker counts, zero-delta API responses
   - Falls back to `baseline_pct_per_worker_per_hour` from `governor.yaml` until 3 valid samples

3. Add `workers_active` and `workers_by_model` to each governor state snapshot so burn rate can be attributed to the correct model mix.

4. Dollar-based capacity estimate — updated each cycle:
   ```
   dollars_per_pct = ema_dollar_burn_per_worker_hr / ema_pct_burn_per_worker_hr
   remaining_dollars = dollars_per_pct * remaining_pct
   ```
   Log this alongside the percentage: "72% used, ~$46 API-equivalent remaining."

**Deliverable:** `scripts/burn-rate.py` + updated state schema + capacity estimate output

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
| **Token tracking** | None | Per-model delta by type: input/output/cache-read/cache-write |
| **Dollar equivalent** | None | $/hr burn and estimated remaining API-equivalent value |
| **Promotion validation** | Assumed correct | Cross-validated against observed tokens-per-percent ratio |
| **Capacity estimate** | % remaining only | % + estimated $ remaining based on observed $/% ratio |
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

8. **Pricing staleness:** The `pricing` block in `governor.yaml` must be manually updated when Anthropic changes API rates. Dollar-equivalent figures become misleading if rates drift. Log the pricing version and date so stale configs are detectable.

9. **Model attribution in multi-model sessions:** A single Claude Code session can make calls to multiple models (e.g., a tool-call routing to Haiku while the main conversation uses Sonnet). Token records must be attributed per-model from the `model` field in each response, not inferred from the session path alone.
