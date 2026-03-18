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

**Token count collection (supplementary):**

The API returns percentages only, not raw token counts. Raw token counts are available locally from `~/.ccdash/tokens.db` (SQLite) and `~/.claude/projects/**/*.jsonl`. Each polling cycle also reads the cumulative token total to pair with the percentage snapshot:

```bash
# Cumulative tokens from ccdash db (fast — pre-aggregated)
TOKENS=$(sqlite3 ~/.ccdash/tokens.db \
    "SELECT SUM(total_input_tokens + total_output_tokens + total_cache_read_tokens)
     FROM file_aggregates
     WHERE earliest_timestamp >= datetime('now', '-7 days')" 2>/dev/null || echo 0)
```

Pairing `(utilization_pct, cumulative_tokens, timestamp, is_peak)` in each snapshot enables:
1. Computing tokens-per-percent-point, which should be ~2x higher during off-peak
2. Validating the promotion is working as expected
3. Expressing burn rate in tokens/hr (more stable than %/hr as plan tier changes)

**Polling frequency:** Every 5 minutes (vs. current 15 min). The API is lightweight (~200ms) and less prone to rate-limiting than the TUI scraper.

**Token refresh:** When `expiresAt - now < 5 minutes`, POST to `https://platform.claude.com/v1/oauth/token` with the refresh token. Update `~/.claude/.credentials.json` in place.

---

### 2. State Store

**File:** `~/.needle/state/governor-state.json`

```json
{
  "updated_at": "2026-03-18T14:30:00Z",

  "usage": {
    "sonnet_pct": 72.0,
    "all_models_pct": 81.0,
    "five_hour_pct": 14.0,
    "sonnet_resets_at": "2026-03-20T03:59:59Z",
    "five_hour_resets_at": "2026-03-18T15:59:59Z",
    "tokens_7d": 4821043,
    "tokens_5h": 312847,
    "tokens_source": "ccdash_db"
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
    "observed_pct_per_worker_per_hour": 1.35,
    "samples": 12,
    "last_sample_at": "2026-03-18T14:15:00Z",
    "baseline_pct_per_worker_per_hour": 1.2
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

### 4. Adaptive Burn Rate Estimator

**Problem:** The current `1.2% per worker per hour` constant is never validated. Percentage burn rate is also unstable — it encodes both actual consumption and plan tier (1% on a Max 20x plan represents far more tokens than 1% on a Pro plan).

**Solution:** Track burn rate in **both** percentage and tokens per worker per hour, derived from consecutive state snapshots.

```
# Percentage-based (used for target worker calculation against plan limit)
pct_burn_sample = (pct_now - pct_prev) / hours_since_prev / workers_active_during_interval

# Token-based (more stable; useful for cross-plan comparison and promotion validation)
token_burn_sample = (tokens_now - tokens_prev) / hours_since_prev / workers_active_during_interval
```

**Exponential moving average (EMA) to smooth noisy samples:**
```
new_burn_rate = alpha * latest_sample + (1 - alpha) * previous_burn_rate
alpha = 0.2  # weight recent observations more than old
```

**Tokens-per-percent ratio (promotion validation signal):**
```
tokens_per_pct = token_burn_sample / pct_burn_sample
```
During the off-peak promotion, `tokens_per_pct` should be approximately **2x** its peak-hour value — because consuming 1% off-peak requires twice the tokens. If the ratio stays flat across peak/off-peak transitions, the promotion is not applying correctly to that limit bucket. Store peak and off-peak samples separately to compute this ratio.

**Fallback:** Use baseline (`1.2%/worker/hr`) when fewer than 3 samples are available or when variance is high.

**Store:** `burn_rate` block in `governor-state.json`:
```json
"burn_rate": {
  "observed_pct_per_worker_per_hour": 1.35,
  "observed_tokens_per_worker_per_hour": 94200,
  "tokens_per_pct_peak": 69780,
  "tokens_per_pct_offpeak": 141350,
  "offpeak_ratio_observed": 2.03,
  "offpeak_ratio_expected": 2.0,
  "promotion_validated": true,
  "samples": 12,
  "last_sample_at": "2026-03-18T14:15:00Z",
  "baseline_pct_per_worker_per_hour": 1.2
}
```

**Why this matters:** If actual burn rate is 1.8%/hr, the governor sets targets 50% too high and risks hitting the limit. Token-based tracking also gives a cross-promotion calibration signal — if `offpeak_ratio_observed` diverges significantly from `offpeak_ratio_expected`, the `promotions.json` multiplier needs adjustment.

---

### 5. Worker Manager

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

### 6. Hysteresis

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

### 7. Alert Manager

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

### 8. Emergency Brake

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

**Goal:** Empirically calibrate the per-worker burn rate.

1. Extend state store to track `burn_rate` block with EMA.

2. Write `scripts/burn-rate.py`:
   - Reads current and previous state snapshots
   - Computes sample: `(pct_now - pct_prev) / elapsed_hours / avg_workers`
   - Updates EMA: `new = alpha * sample + (1 - alpha) * prev`
   - Guards: skip sample if elapsed < 2 min or worker count changed mid-interval
   - Falls back to baseline if samples < 3

3. Add `workers_active` to state snapshot so burn rate can be correctly attributed.

**Deliverable:** `scripts/burn-rate.py` + updated state schema

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
│   ├── worker-manager.sh     # Scale-up/down logic (Phase 4)
│   ├── alerts.sh             # Alert creation (Phase 5)
│   ├── schedule.py           # Peak/off-peak calculator (Phase 2)
│   └── burn-rate.py          # Adaptive burn rate EMA (Phase 3)
├── config/
│   ├── governor.yaml         # Main configuration
│   └── promotions.json       # Promotion window definitions
├── systemd/
│   └── governor.service      # Systemd user service unit
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

---

## Key Improvements Over Existing Governor

| Area | Existing `capacity-governor.sh` | New Governor |
|---|---|---|
| **Usage source** | TUI screen-scraper (~10s, fragile) | Direct API call (~200ms, reliable) |
| **Off-peak math** | `effective_hours` computed but not used | Fully integrated into target calculation |
| **Burn rate** | Hardcoded `1.2%/worker/hr` | Adaptive EMA in both %/hr and tokens/hr |
| **Promotion validation** | Assumed correct | Cross-validated against observed tokens-per-percent ratio |
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
