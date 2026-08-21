# Claude Governor — System Design Plan

## Overview

Claude Governor is an automated capacity governor that monitors Claude Code subscription usage in real time and predicts whether running worker processes will be stopped by hitting a usage window limit before that window resets. When the forecast shows workers will exhaust a window early, the governor scales down the fleet to a safe level; when capacity remains, it allows or adds workers.

The primary output of the entire measurement pipeline is a per-window **exhaustion prediction**: given the current fleet burn rate, will the `five_hour`, `seven_day`, or `seven_day_sonnet` window reach 100% before its reset time? If yes for any window, workers will be forcibly stopped by the platform. The governor exists to detect and prevent that outcome.

All token-level measurement — input, output, cache reads, cache writes, dollar equivalents, per-instance granularity, and promotion-period tracking — exists as inputs to make that exhaustion prediction as accurate as possible.

The system replaces the current `capacity-governor.sh` (screen-scraping, stateless, incomplete off-peak logic) with a reliable, accurate, and extensible daemon.

---

## Goals

1. **Predict worker cutoff** — for each usage window (`five_hour`, `seven_day`, `seven_day_sonnet`), forecast whether the fleet will exhaust the window before reset (`exh_hrs < hrs_left`). This is the primary output that drives all scaling decisions.
2. **Prevent premature exhaustion** — scale down to `safe_worker_count` when any window is on track to exhaust early; hold or scale up when headroom exists.
3. **Respect promotion windows** — account for off-peak 2x capacity when forecasting exhaustion; during off-peak hours the effective remaining capacity is doubled, so more workers can safely run.
4. **Graceful operation** — never kill workers mid-task; only scale down idle workers.
5. **Accurate measurement** — replace the fragile TUI scraper with direct API calls; capture token-type granularity to produce accurate dollar-equivalent burn rates per model.
6. **Adaptive burn rate** — learn actual per-worker consumption empirically (p75 EMA) rather than using a hardcoded constant, so exhaustion predictions improve over time.
7. **Extensibility** — support multiple configured systems beyond Sonnet (Opus, pay-per-token providers, etc.) via a plugin architecture.
8. **Observability** — structured state files, human-readable logs, and alerting beads when cutoff risk is imminent.
9. **Zero runtime dependencies** — ships as a single statically-linked Rust binary (`cgov`). No interpreter, no shared libraries, no package manager. Copy the binary to any Linux amd64 machine and it runs. Build dependencies (Rust crates) are compiled into the binary: `ureq` + `rustls` (HTTPS), `serde` + `serde_json` + `serde_yaml` (serialization), `rusqlite` with `bundled` (SQLite compiled in), `clap` (CLI), `chrono` (datetime).
10. **Worker-system agnostic** — the governor does not hardcode NEEDLE or any specific worker launcher. Each agent config specifies a `launch_cmd` (shell command to start a worker), `session_pattern` (tmux glob to find running workers), and `heartbeat_dir` (directory of JSON status files). Any system that writes heartbeat files and runs in tmux sessions can be governed.

**Note on code examples:** Pseudocode throughout this document uses Python-like syntax for readability. The implementation is Rust.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    claude-governor                       │
│                                                         │
│  ┌──────────────┐    ┌─────────────────┐               │
│  │  Usage Poller │    │  State Store    │               │
│  │  (API-based) │───▶│  (JSON files)   │◀──────────┐   │
│  └──────────────┘    └────────┬────────┘            │   │
│                               │                      │   │
│  ┌──────────────┐    ┌────────▼────────┐            │   │
│  │  Scheduler   │◀───│  Rate Estimator │            │   │
│  │  (off-peak   │    │  (adaptive burn │            │   │
│  │   aware)     │    │   rate model)   │            │   │
│  └──────┬───────┘    └─────────────────┘            │   │
│         │                                            │   │
│  ┌──────▼───────────────────────────────────┐       │   │
│  │            Worker Manager                 │       │   │
│  │  ┌──────────────┐  ┌──────────────────┐  │       │   │
│  │  │  Scale Up    │  │  Scale Down      │  │       │   │
│  │  │  (needle run)│  │  (graceful only) │  │       │   │
│  │  └──────────────┘  └──────────────────┘  │       │   │
│  └───────────────────────────────────────────┘       │   │
│                                                      │   │
│  ┌──────────────────────────────────────────┐        │   │
│  │            Alert Manager                  │        │   │
│  │  (creates HUMAN-type beads near limits)   │        │   │
│  └──────────────────────────────────────────┘        │   │
│                                                      │   │
│  ┌────────────────────────────────────────────────┐  │   │
│  │   Token Collector (independent daemon)          │──┘   │
│  │   tails ~/.claude/projects/**/*.jsonl           │      │
│  └────────────────────────────────────────────────┘      │
└─────────────────────────────────────────────────────────┘
                          │ reads state
              ┌───────────▼───────────┐
              │      cgov CLI         │
              │  humans: status TUI   │
              │  robots: --json/exit  │
              └───────────────────────┘
```

---

## Component Design

### 1. Usage Poller

**Replaces:** `claude-status.sh` (tmux TUI scraper)

**Source:** Direct HTTP call to `https://api.anthropic.com/api/oauth/usage`

```
# Read OAuth token from credentials file
creds = read_json("~/.claude/.credentials.json")
access_token = creds.claudeAiOauth.accessToken
expires_at = creds.claudeAiOauth.expiresAt

# Refresh if within 5 minutes of expiry
if now_ms() + 300000 >= expires_at:
    access_token = refresh_token(creds)

# Fetch usage via HTTPS
usage = https_get_json("https://api.anthropic.com/api/oauth/usage", headers={
    "Authorization": "Bearer {access_token}",
    "anthropic-beta": "oauth-2025-04-20",
    "User-Agent": "claude-code/2.1.78",
})
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

**Token refresh failure handling:** If the refresh POST fails:
1. Retry once after 5 seconds
2. If retry fails, use last-known usage data for this cycle — log `[poller] WARN: token refresh failed, using stale data (age: Xs)`
3. If refresh fails for 3 consecutive cycles (~15 minutes), create a HUMAN-type alert bead: `"OAuth token refresh failing — run: claude login"`
4. Never crash the governor loop on auth failure — stale data is always preferable to no governor

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
    "cache_write_5m_per_mtok": 3.75,
    "cache_read_per_mtok": 0.30
  },
  "claude-opus-4-6": {
    "input_per_mtok": 5.00,
    "output_per_mtok": 25.00,
    "cache_write_5m_per_mtok": 6.25,
    "cache_read_per_mtok": 0.50
  },
  "claude-haiku-4-5": {
    "input_per_mtok": 1.00,
    "output_per_mtok": 5.00,
    "cache_write_5m_per_mtok": 1.25,
    "cache_read_per_mtok": 0.10
  }
}
```

Cache write rates shown above are the 5-minute TTL tier (standard API). The 1-hour TTL tier (Bedrock only, 2.0× input) is documented in the Configuration File section.

**Output — append-only JSONL** at `~/.needle/state/token-history.jsonl`. Every line is a single flat JSON object. Three record types per collection pass, identified by `"r"`. Records `i` and `f` are wide: every token-type measurement appears as a column on the same row, not as separate rows.

**Naming convention:** JSONL records use compact field names for storage efficiency (`safe_w`, `fleet_pct_hr`, `exh_hrs`). The governor state file (`governor-state.json`) and prose use readable equivalents (`safe_worker_count`, `fleet_pct_per_hour`, `predicted_exhaustion_hours`). Both refer to the same values; the mapping is unambiguous from context.

Token-type column suffixes used throughout:

| Suffix | Meaning | Pricing basis | Notes |
|---|---|---|---|
| `-input` | Fresh input tokens | base input rate | |
| `-output` | Output tokens | output rate | |
| `-r-cache` | Cache reads (hits) | 0.1× input | |
| `-w-cache` | Cache writes | 1.25× input | Standard API always uses `{"type":"ephemeral"}` = 5-min TTL |
| `-w-cache-1h` | Cache writes, 1-hour TTL | 2.0× input | Bedrock only (`ENABLE_PROMPT_CACHING_1H_BEDROCK`); near-zero on standard API |

Claude Code hardcodes `cache_control: {"type": "ephemeral"}` for all cache writes on the standard Anthropic API. The `{"ttl": "1h"}` variant is conditionally added only on Bedrock. In practice, `-w-cache-1h` columns will be zero for standard API usage and are retained in the schema solely for Bedrock compatibility. The dollar model for standard deployments is effectively four types: `input`, `output`, `r-cache`, `w-cache`.

Each token-type column appears in two variants: `-n` (token count) and `-usd` (dollar cost).

#### Record Type `i` — Instance Wide Row

**One line per session per interval.** All token types appear as columns on the same row. Since each session runs one model, tok_type columns are not model-prefixed; the `model` field carries the model identity.

Time fields for promotion detection: `hr_et` (0–23, hour in US Eastern time at `t0`) and `dow` (0=Mon … 6=Sun). Together with `pk` these allow grouping intervals by peak/off-peak without parsing timestamps, and computing the `usd_per_pct` ratio across time-of-day buckets to validate whether the 2× promotion is being applied.

```
{"r":"i","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","sess":"needle-claude-anthropic-sonnet-alpha","sid":"ad5b2e01","model":"claude-sonnet-4-6","pk":0,"hr_et":10,"dow":2,"input-n":15410,"input-usd":0.0462,"output-n":2830,"output-usd":0.0425,"r-cache-n":104200,"r-cache-usd":0.0313,"w-cache-n":4100,"w-cache-usd":0.0154,"w-cache-1h-n":0,"w-cache-1h-usd":0,"total-usd":0.1621,"p5h":null,"p7d":null,"p7ds":null}
```

Two workers in the same interval produce two `i` lines, directly comparable column-for-column:

```
{"r":"i",...,"sess":"...alpha","hr_et":10,"dow":2,"pk":0,"input-n":15410,"input-usd":0.0462,"output-n":2830,"output-usd":0.0425,"r-cache-n":104200,"r-cache-usd":0.0313,"w-cache-n":4100,"w-cache-usd":0.0154,"w-cache-1h-n":0,"w-cache-1h-usd":0,"total-usd":0.1621,"p5h":null,"p7d":null,"p7ds":null}
{"r":"i",...,"sess":"...bravo","hr_et":10,"dow":2,"pk":0,"input-n":14430,"input-usd":0.0433,"output-n":2680,"output-usd":0.0402,"r-cache-n":94100,"r-cache-usd":0.0282,"w-cache-n":3800,"w-cache-usd":0.0143,"w-cache-1h-n":0,"w-cache-1h-usd":0,"total-usd":0.1260,"p5h":null,"p7d":null,"p7ds":null}
```

`p5h`, `p7d`, `p7ds` are `null` at write time. The governor annotates them in the SQLite mirror by apportioning each window's observed percentage delta across concurrent sessions, weighted by `total-usd`.

#### Record Type `f` — Fleet Wide Row

**One line per interval.** All models × all token types appear as columns, prefixed by model name. Zero-filled for models with no activity. Ends with fleet totals, per-worker variance, percentage deltas, and `usd-per-pct` ratios for promotion validation.

`hr_et` and `dow` appear here too, matching the `i` records, so peak/off-peak grouping is possible on either table without a join. `usd-per-pct-7ds` is the key promotion signal: divide `total-usd` by `p7ds` to get the dollar cost of 1% of the Sonnet weekly window. During active off-peak promotion this ratio should be ~2× its peak value, because the same dollar spend moves the percentage half as much.

```
{"r":"f","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","pk":0,"hr_et":10,"dow":2,"workers":2,"claude-sonnet-4-6-input-n":29840,"claude-sonnet-4-6-input-usd":0.0924,"claude-sonnet-4-6-output-n":5510,"claude-sonnet-4-6-output-usd":0.0828,"claude-sonnet-4-6-r-cache-n":198300,"claude-sonnet-4-6-r-cache-usd":0.0595,"claude-sonnet-4-6-w-cache-n":7900,"claude-sonnet-4-6-w-cache-usd":0.0296,"claude-sonnet-4-6-w-cache-1h-n":0,"claude-sonnet-4-6-w-cache-1h-usd":0,"claude-opus-4-6-input-n":0,"claude-opus-4-6-input-usd":0,"claude-opus-4-6-output-n":0,"claude-opus-4-6-output-usd":0,"claude-opus-4-6-r-cache-n":0,"claude-opus-4-6-r-cache-usd":0,"claude-opus-4-6-w-cache-n":0,"claude-opus-4-6-w-cache-usd":0,"claude-opus-4-6-w-cache-1h-n":0,"claude-opus-4-6-w-cache-1h-usd":0,"total-usd":0.2643,"p75-usd-hr":2.147,"std-usd-hr":0.312,"p5h":0.66,"p7d":0.54,"p7ds":0.75,"usd-per-pct-7ds":0.3524}
```

The column set is fixed at startup from the pricing config — all configured models appear in every `f` row, zero-filled when inactive. This keeps the schema stable and rows directly comparable across time.

#### Record Type `w` — Window Forecast Row

**Three lines per interval** (one per window), written last. These are the **primary output** of the entire measurement pipeline: they answer "will workers be stopped before this window resets?"

Key fields:
- `ceil` — configured target ceiling for this window (e.g. `90.0` when `target_utilization=0.90`)
- `snap` — raw platform utilization % at time of snapshot
- `remain` — headroom to `ceil`, not to 100% (`ceil - snap`); this is the usable budget
- `exh_hrs` — hours until `ceil` is reached at current fleet burn rate (`remain / fleet_pct_hr`)
- `hrs_left` — hours until window resets
- `cutoff_risk` — `1` if `exh_hrs < hrs_left` (fleet will hit the ceiling before window resets)
- `margin_hrs` — `hrs_left - exh_hrs`; positive = safe, negative = will exceed ceiling
- `bind` — `1` if this is the most constrained window (smallest `margin_hrs`)
- `safe_w` — max worker count where `exh_hrs >= hrs_left` (only present on binding window)

```
{"r":"w","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","win":"five_hour","ceil":85.0,"snap":36.4,"reset":"2026-03-18T15:59:59Z","delta":0.66,"remain":48.6,"hrs_left":1.50,"fleet_pct_hr":7.92,"exh_hrs":6.14,"cutoff_risk":0,"margin_hrs":4.64,"bind":0,"safe_w":null,"pk":0}
{"r":"w","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","win":"seven_day","ceil":90.0,"snap":72.6,"reset":"2026-03-20T03:00:00Z","delta":0.54,"remain":17.4,"hrs_left":37.5,"fleet_pct_hr":6.48,"exh_hrs":2.69,"cutoff_risk":1,"margin_hrs":-34.81,"bind":0,"safe_w":null,"pk":0}
{"r":"w","ts":"2026-03-18T14:30:00Z","t0":"2026-03-18T14:25:00Z","t1":"2026-03-18T14:30:00Z","win":"seven_day_sonnet","ceil":90.0,"snap":63.5,"reset":"2026-03-20T03:59:59Z","delta":0.75,"remain":26.5,"hrs_left":37.5,"fleet_pct_hr":9.00,"exh_hrs":2.94,"cutoff_risk":1,"margin_hrs":-34.56,"bind":1,"safe_w":2,"pk":0}
```

---

**Fast-query SQLite mirror** at `~/.needle/state/token-history.db` (JSONL is authoritative; DB rebuilt on corruption). Table schemas are wide to match the records:

```sql
-- Type "i": one row per session per interval
CREATE TABLE i (
    ts TEXT, t0 TEXT, t1 TEXT, sess TEXT, sid TEXT, model TEXT,
    pk INTEGER,       -- 1 = peak hours (8-14 ET weekdays), 0 = off-peak
    hr_et INTEGER,    -- hour of day in US Eastern (0-23) at t0
    dow INTEGER,      -- day of week at t0 (0=Mon … 6=Sun)
    "input-n" INTEGER,    "input-usd" REAL,
    "output-n" INTEGER,   "output-usd" REAL,
    "r-cache-n" INTEGER,  "r-cache-usd" REAL,
    "w-cache-n" INTEGER,  "w-cache-usd" REAL,    -- 5-min TTL; standard API only
    "w-cache-1h-n" INTEGER,"w-cache-1h-usd" REAL, -- Bedrock only; near-zero on standard API
    "total-usd" REAL,
    p5h REAL, p7d REAL, p7ds REAL   -- null until governor annotates
);
CREATE INDEX i_t0_sess  ON i(t0, sess);
CREATE INDEX i_model_t0 ON i(model, t0);
CREATE INDEX i_pk_t0    ON i(pk, t0);  -- fast peak vs off-peak queries

-- Type "f": one wide row per interval; columns generated from pricing config
-- Example with claude-sonnet-4-6 and claude-opus-4-6 configured:
CREATE TABLE f (
    ts TEXT, t0 TEXT, t1 TEXT,
    pk INTEGER, hr_et INTEGER, dow INTEGER,
    workers INTEGER,
    "claude-sonnet-4-6-input-n" INTEGER,     "claude-sonnet-4-6-input-usd" REAL,
    "claude-sonnet-4-6-output-n" INTEGER,    "claude-sonnet-4-6-output-usd" REAL,
    "claude-sonnet-4-6-r-cache-n" INTEGER,   "claude-sonnet-4-6-r-cache-usd" REAL,
    "claude-sonnet-4-6-w-cache-n" INTEGER,   "claude-sonnet-4-6-w-cache-usd" REAL,
    "claude-sonnet-4-6-w-cache-1h-n" INTEGER,"claude-sonnet-4-6-w-cache-1h-usd" REAL,
    "claude-opus-4-6-input-n" INTEGER,       "claude-opus-4-6-input-usd" REAL,
    "claude-opus-4-6-output-n" INTEGER,      "claude-opus-4-6-output-usd" REAL,
    "claude-opus-4-6-r-cache-n" INTEGER,     "claude-opus-4-6-r-cache-usd" REAL,
    "claude-opus-4-6-w-cache-n" INTEGER,     "claude-opus-4-6-w-cache-usd" REAL,
    "claude-opus-4-6-w-cache-1h-n" INTEGER,  "claude-opus-4-6-w-cache-1h-usd" REAL,
    "total-usd" REAL,
    "p75-usd-hr" REAL, "std-usd-hr" REAL,
    p5h REAL, p7d REAL, p7ds REAL,
    "usd-per-pct-7ds" REAL   -- promotion signal: should be ~2x higher when pk=0 during promo
);
CREATE INDEX f_t0    ON f(t0);
CREATE INDEX f_pk_t0 ON f(pk, t0);  -- fast peak vs off-peak queries

-- Type "w": one row per window per interval
-- Primary output: cutoff_risk=1 means the fleet will hit the target ceiling before this window resets
CREATE TABLE w (
    ts TEXT, t0 TEXT, t1 TEXT, win TEXT,
    ceil REAL,            -- configured target ceiling (e.g. 90.0); stored per-record so history reflects config at the time
    snap REAL, reset TEXT, delta REAL,
    remain REAL,          -- ceil - snap (headroom to target ceiling, not to 100%)
    hrs_left REAL,
    fleet_pct_hr REAL, exh_hrs REAL,
    cutoff_risk INTEGER,  -- 1 if exh_hrs < hrs_left (fleet will hit ceiling before reset)
    margin_hrs REAL,      -- hrs_left - exh_hrs; negative means over-budget relative to target
    bind INTEGER, safe_w INTEGER, pk INTEGER
);
CREATE INDEX w_win_t0        ON w(win, t0);
CREATE INDEX w_cutoff_risk   ON w(cutoff_risk, t0);  -- fast query: "when were we at risk?"

-- Cross-instance comparison: all sessions side by side for a given interval
CREATE VIEW instance_compare AS
SELECT t0, hr_et, dow, pk, sess, model,
    "total-usd", "input-usd", "output-usd",
    "r-cache-usd", "w-cache-usd", "w-cache-1h-usd",
    "total-usd" / ((julianday(t1)-julianday(t0))*24) AS usd_per_hour,
    p7ds,
    CASE WHEN p7ds > 0 THEN "total-usd" / p7ds END AS usd_per_pct_7ds
FROM i ORDER BY t0 DESC, "total-usd" DESC;

-- Promotion validation: compare usd_per_pct_7ds across peak vs off-peak
-- If 2x promotion is applying: AVG(usd_per_pct_7ds) WHERE pk=0 ≈ 2× WHERE pk=1
CREATE VIEW promo_check AS
SELECT pk, hr_et,
    AVG("usd-per-pct-7ds") AS avg_usd_per_pct,
    COUNT(*)               AS samples
FROM f
WHERE p7ds > 0 AND "usd-per-pct-7ds" IS NOT NULL
GROUP BY pk, hr_et
ORDER BY hr_et;
```

**Standalone CLI subcommands** (all built into `cgov` binary):
```
cgov collect                          # one collection pass; write i+f+w lines
cgov collect --daemon                 # loop every N minutes
cgov token-history [--last N]         # recent w rows (window forecasts)
cgov token-history --compare [--at TS]  # instance_compare view for given interval
cgov token-history --fleet [--last N]   # recent f rows showing all model×tok_type columns
cgov token-history --rebuild-db       # reconstruct SQLite from JSONL
```

---

### 3. State Store

**File:** `~/.needle/state/governor-state.json`

**Note:** The sample below is illustrative, with values drawn from the historical March 2026 Off-Peak 2x promotion window (hence `"is_promo_active": true` and `"promo_multiplier": 2.0`). Under the shipped configuration — `config/promotions.json` containing an empty array `[]` — `is_promo_active` is `false` and `promo_multiplier` is `1.0`.

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
    // PRIMARY OUTPUT: per-window cutoff prediction.
    // cutoff_risk=true means the fleet will hit the target ceiling before this window resets.
    // target_ceiling = target_utilization * 100 (e.g. 90.0 when target_utilization=0.90).
    // remaining_pct = target_ceiling - current_utilization (headroom to ceiling, not to 100%).
    "five_hour": {
      "target_ceiling":             85.0,   // target_utilization=0.85 for session window
      "current_utilization":        36.4,
      "remaining_pct":              48.6,   // 85.0 - 36.4
      "hours_remaining":            1.50,
      "fleet_pct_per_hour":         7.92,
      "predicted_exhaustion_hours": 6.14,
      "cutoff_risk":                false,  // exh_hrs (6.14) > hrs_left (1.50) → safe
      "margin_hrs":                 4.64,
      "binding":                    false
    },
    "seven_day": {
      "target_ceiling":             90.0,
      "current_utilization":        72.6,
      "remaining_pct":              17.4,   // 90.0 - 72.6
      "hours_remaining":            37.5,
      "fleet_pct_per_hour":         6.48,
      "predicted_exhaustion_hours": 2.69,
      "cutoff_risk":                true,   // exh_hrs (2.69) < hrs_left (37.5) → will exceed target
      "margin_hrs":                 -34.81,
      "binding":                    false
    },
    "seven_day_sonnet": {
      "target_ceiling":             90.0,
      "current_utilization":        63.5,
      "remaining_pct":              26.5,   // 90.0 - 63.5
      "hours_remaining":            37.5,
      "fleet_pct_per_hour":         9.00,
      "predicted_exhaustion_hours": 2.94,
      "cutoff_risk":                true,   // exh_hrs (2.94) < hrs_left (37.5) → will exceed target
      "margin_hrs":                 -34.56, // BINDING: most constrained window
      "binding":                    true,
      "safe_worker_count":          2       // max workers where exh_hrs >= hrs_left
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

### 4. Promotion and Schedule Awareness

**Peak window:** `[08:00, 14:00)` US Eastern Time (weekdays only) — half-open interval. 08:00:00 is the first peak second; 13:59:59 is the last. 14:00:00 is off-peak. All boundary comparisons use `>=` for start and `<` for end.
**Off-peak:** Everything else (all weekends, weekday evenings/nights)

**Promotion detection:**
- Governor reads a `promotions.json` config file listing active promotions.
- When no active promotion is defined, `promotions.json` contains an empty array `[]`, which results in `offpeak_multiplier = 1.0` (flat model — this is the correct default when no promotion is running).
- The schedule calculator returns the current effective multiplier at any moment.

**Historical promotion entry (for reference only):**
The following configuration was used during the March 2026 Off-Peak 2x promotion, which has now expired. This is provided as a reference example for defining future promotions; it is not an active configuration.

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

**Default configuration (no active promotion):**

```json
[]
```

When no promotion is running, `promotions.json` should contain an empty array `[]`. This results in `offpeak_multiplier = 1.0` (flat model — this is the correct default when no promotion is running). The empty array is the shipped configuration and the normal operating state.

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
# target_utilization caps effective consumption; default 1.0 = use everything
target_ceiling  = target_utilization * 100      # e.g. 90.0 when target_utilization=0.90
remaining_capacity = target_ceiling - sonnet_pct
remaining_capacity = max(0, remaining_capacity)  # clamp if already past target

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

#### Per-Instance Burn Rate (from `i` records)

```
# For each i record with annotated window_pct_deltas:
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
# EMA update after each f (fleet) interval:
ema_pct_per_hour[model][window] = α * fleet_pct_per_hour[window] + (1-α) * prev_ema
α = 0.2

# Capacity forecast per window:
# target_utilization is configured per-window (default 1.0 = 100%).
# E.g. target_utilization=0.90 treats the window as exhausted at 90%,
# leaving a 10% reserve that workers will never consume.
target_ceiling[W]  = target_utilization[W] * 100         # e.g. 90.0
effective_used[W]  = min(snapshot_utilization[W], target_ceiling[W])
remaining_pct[W]   = target_ceiling[W] - effective_used[W]  # headroom to target, not to 100%
hours_remaining[W] = (resets_at[W] - now).total_seconds() / 3600

fleet_pct_per_hour[W]         = ema_pct_per_hour[model][W] * workers_active
predicted_exhaustion_hours[W] = remaining_pct[W] / fleet_pct_per_hour[W]

# PRIMARY CUTOFF PREDICTION — the core question the governor answers:
# With target_utilization < 1.0, "cutoff" means hitting the configured ceiling,
# not the hard platform limit. This gives a configurable safety reserve.
cutoff_risk[W]  = predicted_exhaustion_hours[W] < hours_remaining[W]
margin_hrs[W]   = hours_remaining[W] - predicted_exhaustion_hours[W]
# margin_hrs > 0: safe (workers will idle before hitting target ceiling)
# margin_hrs < 0: at risk (workers WILL exceed the target ceiling before window resets)

# Safe worker count: max workers where margin_hrs[W] >= 0, using conservative rate
safe_worker_count[W] = floor(remaining_pct[W] / hours_remaining[W] / p75_rate_per_worker)

# Binding window: the most constrained window (smallest / most negative margin_hrs)
binding_window = argmin(margin_hrs[W] for W in windows)
```

The governor's `compute_target_workers()` uses `safe_worker_count[binding_window]` as its ceiling. Any window where `cutoff_risk=True` immediately triggers a scale-down toward `safe_worker_count`. When the 5h and 7d windows give contradictory ceilings, the lower (more conservative) `safe_worker_count` wins.

**When `safe_worker_count[binding_window]` is `None`** (no burn-rate samples yet — e.g. immediately after a daemon restart, or while the token collector is offline/recovering), the governor holds at `current_total` and takes no scaling action, rather than guessing a number. See ADR-002 — this replaced an earlier fallback that used `max_workers` as the ceiling whenever data was missing, which meant a fresh restart with `current_total=0` launched the fleet at full configured capacity with zero usage awareness.

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
- When a window resets mid-interval, discard that interval's pct_delta for that window only (it spans two separate quota periods). **Reset detection:** a window has reset when `current_utilization < previous_utilization - 1.0` for that window (the 1.0% margin absorbs API rounding noise)

**Token collector offline — graceful degradation:**

The governor must function when the Token Collector is not running or its data is stale. **As of ADR-002 (2026-07-22), the policy is: no fresh burn-rate data → hold at `current_total`, take no scaling action.** This superseded an earlier three-tier staleness design (below, kept for history) that was never actually wired up end-to-end — the shipped code instead fell back to `max_workers` whenever `safe_worker_count` was `None`, which is the opposite of graceful: it scaled a subscription-billed fleet to full configured capacity with no idea how much quota was left. The incident that surfaced this: `claude-token-collector.service` was silently failing every cycle for hours (`Failed to load cursors: JSON error`, itself caused by a concurrent-write race in `CursorStore::save()` — also fixed by ADR-002), so the `None`-fallback ran continuously and scaled the `polish-opus` pool (Anthropic subscription billing) from 0 to 4 concurrent Opus workers with zero usage awareness while weekly usage was already at ~51%.

Originally-planned three-tier design (not implemented as such; superseded by the hold-at-current policy above):

| Collector data age | Behavior |
|---|---|
| < 10 minutes | Use normally — collector may be between cycles |
| 10–30 minutes | Use last EMA values; log `[governor] WARN: collector data stale ({age}s)` |
| > 30 minutes | Fall back to `baseline_burn_rate` from `governor.yaml`; create HUMAN alert bead: `"Token collector offline — burn rate using baseline fallback"` |

In fallback mode, dollar-equivalent burn rates and cache efficiency metrics are unavailable. The governor continues scaling using percentage-based burn rate only. When the collector resumes, the governor returns to EMA-based rates within one interval.

The independent emergency brake (any window's `current_utilization >= 98%`, checked from the last polled snapshot before the EMA/forecast pipeline runs at all) is unaffected by data staleness and still fires regardless — so a real cutoff is caught even while the fleet is held at `current_total` for lack of forecast data.

**Claude Code cache behavior note:** Cache reads dominate token counts (cheap, 0.1× input) while 1h cache writes are the most expensive token type per unit (2.0× input). Dollar-equivalent burn rate is the most accurate single measure of plan consumption rate, more so than raw token count or even pct/hr alone.

---

### 6. Worker Manager

#### Scale-Up
```
for _ in range(target - current):
    shell_exec(agent.launch_cmd)   # configured per agent in governor.yaml
    sleep(1)                        # stagger to avoid tmux naming collisions
```

**Separation of concerns — capacity vs. orchestration:**

The governor manages **how many** workers run. The worker system (NEEDLE, or any alternative) manages **what** workers do. These are deliberately separate:

| Responsibility | Governor (`cgov`) | Worker system (`needle`) |
|---|---|---|
| Decide worker count | Yes — exhaustion prediction, scaling | No |
| Launch a worker | Executes `launch_cmd` via shell | Handles all setup (tmux session, workspace discovery, bead claiming, prompt construction, agent dispatch) |
| Monitor worker state | Reads heartbeat JSON files | Writes heartbeat JSON files |
| Stop a worker | Sends SIGINT / kills tmux session | Receives signal, exits cleanly |
| Claim and process beads | No | Yes |
| Build prompts | No | Yes |
| Invoke Claude CLI | No | Yes |

**Call chain for `launch_cmd: "needle run --agent=claude-anthropic-sonnet --force"`:**

```
cgov (Rust binary)
 └─ sh -c "needle run --agent=claude-anthropic-sonnet --force"
     └─ needle (bash) — validates agent, auto-discovers workspace, generates session name
         └─ tmux new-session -d -s "needle-claude-anthropic-sonnet-alpha" "needle _run_worker ..."
             └─ needle _run_worker — infinite loop:
                 ├─ claim bead from br
                 ├─ build prompt (3-tier project context, type-specific instructions)
                 ├─ render agent invoke template
                 ├─ execute claude CLI with prompt
                 ├─ process result (commit, close bead, or quarantine)
                 ├─ emit heartbeat to ~/.needle/state/heartbeats/{session}.json
                 └─ loop
```

The governor's `launch_cmd` returns immediately after the tmux session is created — it does not block on worker execution. The governor detects the new worker on its next cycle via the heartbeat file.

**Why not absorb NEEDLE into cgov?** NEEDLE is a 44K-line worker orchestration system with bead lifecycle management, multi-agent prompt templating, stream parsing, watchdog recovery, and hook execution. Rewriting this in Rust would be massive scope creep with no capacity-management benefit. The shell-out interface is the correct boundary: `cgov` decides "launch one worker," NEEDLE handles everything else.

**Custom worker systems:** Any system that (a) can be launched with a shell command, (b) runs in a tmux session matching `session_pattern`, and (c) writes heartbeat JSON to `heartbeat_dir` is compatible with the governor. The heartbeat format is specified in the Heartbeat File Format section below.

#### Scale-Down (Graceful Only)
```
# Find idle workers from heartbeat files (path from agent.heartbeat_dir)
idle_sessions = []
for hb_path in glob(agent.heartbeat_dir / agent.session_pattern + ".json"):
    heartbeat = read_json(hb_path)
    if heartbeat.status == "idle":
        idle_sessions.push(heartbeat.session)

# Kill idle, unattached workers (most recent first)
killed = 0
for session in idle_sessions.reversed():
    attached = tmux("display-message", session, "#{session_attached}")
    if attached > 0:
        continue  # human is watching
    tmux("send-keys", session, "C-c")   # graceful SIGINT
    sleep(2)
    tmux("kill-session", session)        # force after timeout
    killed += 1
    if killed >= current - target:
        break
```

**Key principle:** Never kill an `executing` worker. Only kill `idle` workers. If no idle workers are available but current > target, wait until the next cycle.

**Heartbeat file format:**

NEEDLE workers write heartbeat files to `~/.needle/state/heartbeats/{session}.json` every 30 seconds:

```json
{
  "session": "needle-claude-anthropic-sonnet-alpha",
  "agent": "claude-anthropic-sonnet",
  "status": "idle",
  "bead_id": null,
  "workspace": "/home/coding/kalshi-trading",
  "updated_at": "2026-03-18T14:29:45Z",
  "launched_at": "2026-03-18T12:00:00Z"
}
```

`status` values: `idle` (between beads, safe to kill), `executing` (working on a bead, never kill), `starting` (launched but not yet claimed a bead, treat as executing). Heartbeat files older than 60 seconds are treated as stale — the worker may have crashed without cleanup. Stale heartbeats are verified against `tmux list-sessions` before any action is taken; if the tmux session no longer exists, the heartbeat file is removed.

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
| Any window `cutoff_risk=1` with `margin_hrs < -2` | `cutoff_imminent` | Create HUMAN bead: "Window `{win}` will exhaust in `{exh_hrs:.1f}h`, resets in `{hrs_left:.1f}h` — workers will be stopped" |
| `seven_day_sonnet.cutoff_risk=1` with `margin_hrs < 0` | `sonnet_cutoff_risk` | Scale to `safe_worker_count`; log prediction |
| `five_hour.cutoff_risk=1` with `margin_hrs < 0` | `session_cutoff_risk` | Scale to `safe_worker_count` for session window; log prediction |
| `burn_rate_sample > baseline * 2` | `burn_rate_spike` | Log anomaly; increase polling rate to recalibrate faster |
| All windows `margin_hrs > hrs_left * 0.5` | `underutilization` | Scale up toward max_workers; headroom is ample |

**Deduplication:** Per-*episode*, not per-cooldown-window. An episode is one continuous stretch during which a condition holds, keyed by alert type plus scope (`cutoff_imminent:five_hour`, `subscription_billing_drift:needle-sonnet`). The first cycle a condition is true creates exactly one bead and records its id in the state file under `open_alert_beads`; while the condition persists no further beads are created (the existing one is refreshed at most once per `alerts.cooldown_minutes`); the first cycle it is no longer detected, the bead is auto-closed and the entry dropped. `alerts.cooldown_minutes` remains only as an anti-flap floor on *opening* a new episode after one resolves, with timestamps stored per episode key under `alert_cooldown.last_fired`. The earlier pure-cooldown scheme minted a fresh bead every hour for as long as a condition held and never closed one on recovery — the source of the 226 `sonnet_cutoff_risk` / 160 `cutoff_imminent` beads in this repo's own workspace. See `docs/research/alerts.md` for the full lifecycle.

---

### 9. Emergency Brake

If any window `utilization >= 98%`, immediately scale all workers to 0 regardless of hysteresis or idle state. Log `EMERGENCY BRAKE APPLIED — {win} at {pct}%`. This is the hard stop that guarantees workers are not running when the limit is about to be hit. It fires regardless of whether `cutoff_risk` was predicted — it acts on observed utilization, not forecasts.

---

### 10. CLI Output Layer (`cgov`)

A single `cgov` entry point provides both human and machine interfaces to all governor state. It reads `governor-state.json` and the token-history DB directly — it does not need the daemon running to show current data.

**Dual-mode design:** every subcommand detects whether stdout is a TTY:
- TTY → colorized human-readable table
- non-TTY or `--json` → raw JSON to stdout, errors to stderr

**Exit codes carry semantic meaning** so scripts can branch without parsing JSON:
- `0` = all windows safe
- `1` = general error / daemon not running
- `2` = cutoff risk active (one or more windows)
- `3` = emergency brake engaged

**`cgov status` human output:**
```
Claude Governor — 2026-03-18 14:30 ET
──────────────────────────────────────────────────────
Window          Used    Ceiling  Remain  Resets    Risk
five_hour        36%      85%     49%    in 1h30   OK
seven_day        73%      90%     17%    in 37h    ⚠ CUTOFF (2.7h at current rate)
seven_day_sonnet 64%      90%     26%    in 37h    ⚠ CUTOFF (2.9h) ← BINDING

Workers:  2 active  (target: 2 · safe ceiling: 2)
Burn:     $2.15/hr  (p75 per worker)
Peak:     no  (promo 2x active, off-peak until 08:00 ET)
──────────────────────────────────────────────────────
Last cycle: 14s ago  |  Next: in 4m46s
```

The `Peak:` line above is illustrative output captured during the historical March 2026 Off-Peak 2x promotion. With the shipped empty `promotions.json` (`[]`), no promotion is in effect and the line carries no promo annotation.

**Daemon management via `cgov`:** `cgov enable/disable/start/stop/restart` abstract over systemd user services (Linux) and tmux sessions (fallback), so callers never need to know the underlying mechanism.

---

### 11. Trajectory Simulator (`cgov simulate`)

**Purpose:** Project future capacity utilization under configurable worker scenarios, accounting for all promotion boundaries and window resets.

**Usage:**
```bash
cgov simulate --workers 4 --hours 12           # 4 workers for 12 hours
cgov simulate --workers "4:6h,2:6h" --hours 12 # 4 for 6h, then 2 for 6h
cgov simulate --hours 24 --json                 # current worker count, JSON output
```

**Algorithm:** Walk forward from `now` in 1-minute steps, applying:
- Current per-model EMA burn rate (from `governor-state.json`)
- Promotion multiplier transitions from `promotions.json` (e.g., 2x→1x at 08:00 ET)
- Window resets (utilization drops to 0 when `resets_at` is crossed)
- Configured `target_utilization` ceilings per window

At each step, compute per-window utilization, remaining headroom, and whether the ceiling would be breached. Output is a trajectory at configurable resolution (default: 15-minute intervals).

**Human output:**
```
Simulating: 4 workers for 12h starting 2026-03-18 14:30 ET
Using burn rate: 4.5 %/hr/worker (p75 EMA, seven_day_sonnet)

Time         5h%   7d%   7ds%   Promo   Workers   Event
14:30        36    73    64     2x      4
15:00        40    74    67     2x      4
...
17:30        62    79    82     2x      4
18:00        ──    80    85     2x      4         ← 5h resets
18:30         4    81    88     2x      4
19:00         8    82    ██ 91  2x      4         ← 7ds CEILING BREACH at 18:47
```

**JSON output:** Array of `{"t": "...", "five_hour": 36.2, "seven_day": 73.1, "seven_day_sonnet": 64.0, "promo": 2.0, "workers": 4, "events": []}` objects. Includes a `breach` object identifying the first window/time that exceeds its ceiling, or `null` if all windows stay safe.

The simulator is read-only — it reads current state but makes no changes.

---

### 12. Prediction Accuracy Self-Calibration

**Purpose:** Track how accurate past exhaustion predictions were, score them, and auto-tune governor parameters to improve over time.

**Mechanism:** After each window reset, compare the prediction made at the *start* of that window period against actual outcome:

```
prediction_error = actual_final_pct - predicted_final_pct
# negative = conservative (left capacity on table), positive = aggressive (cut it too close)
```

**Scoring:** Each prediction scored to `~/.needle/state/prediction-accuracy.jsonl`:

```json
{"ts":"2026-03-20T04:00:00Z","win":"seven_day_sonnet","pred_pct":87.2,"actual_pct":82.1,"error":-5.1,"pred_exh":false,"actual_exh":false,"correct":true,"workers_avg":2.8}
```

**Auto-tuning rules** (activated after ≥10 scored predictions per window):

| Signal | Adjustment | Rationale |
|---|---|---|
| Median error < -5 (conservative) | Increase `burn_rate_alpha` +0.02, widen `hysteresis_band` +0.5 | Leaving capacity on the table |
| Median error > +5 (aggressive) | Decrease `burn_rate_alpha` -0.02, tighten `hysteresis_band` -0.5 | Cutting it too close |
| Stddev > 10 (unpredictable) | Lower `target_utilization` -0.02 | Need larger safety buffer |

Adjustments are clamped to prevent runaway drift: `alpha ∈ [0.05, 0.5]`, `hysteresis ∈ [0, 3]`, `target_util ∈ [0.70, 0.98]`.

**State:** `burn_rate.calibration` in `governor-state.json`:

```json
{
  "calibration": {
    "predictions_scored": 24,
    "median_error_7ds": -3.2,
    "auto_tuned_alpha": 0.22,
    "auto_tuned_hysteresis": 1.0,
    "last_tuned_at": "2026-03-20T04:00:00Z"
  }
}
```

Auto-tuning is opt-in via `calibration.auto_calibrate: true` in `governor.yaml`. When disabled, accuracy is still tracked and reported via `cgov status` but parameters are not modified.

---

### 13. Promotion Boundary Pre-Scaling

**Purpose:** Anticipate upcoming promotion multiplier transitions and begin scaling changes *before* the boundary hits, preventing the burst of over-consumption when effective burn rate changes abruptly.

**Problem (historical example):** During the March 2026 Off-Peak 2x promotion, at 08:00 ET on a weekday the multiplier dropped from 2x to 1x. If the governor is running 5 workers at an effective burn rate sustainable only at 2x, the burn rate against quota effectively doubles instantly. By the time the next governor cycle detects this (up to 5 minutes later), significant quota has been consumed at the higher effective rate. Pre-scaling mitigates this by beginning the scale-down before the boundary is reached.

**Mechanism:** The schedule calculator gains a `next_transition()` function:

```python
def next_transition(promotions) -> (datetime, old_multiplier, new_multiplier):
    """Return the next upcoming multiplier change."""
    # e.g., (2026-03-19T08:00:00-04:00, 2.0, 1.0)
```

The governor loop checks: if a transition is within `pre_scale_minutes` (default: 30), compute the target worker count using the *post-transition* multiplier. If the post-transition target is lower than current workers, begin scaling down immediately — one worker per cycle:

```python
transition_in = next_transition() - now
if transition_in < pre_scale_minutes:
    post_target = compute_target_workers(multiplier=new_multiplier)
    if post_target < current_workers:
        effective_target = max(post_target, current_workers - 1)
```

Pre-scaling is **conservative-only**: it scales down before losing a multiplier bonus, but does **not** scale up before gaining one. Never speculate on cheaper capacity that hasn't started yet.

**Logging:** `[governor] PRE-SCALE: off-peak→peak in 22min — scaling 4→3 (post-transition safe: 2)`

---

### 14. End-of-Window Capacity Sprint

**Purpose:** Capture throughput from capacity that would otherwise expire unused at window reset.

**Problem:** If the `five_hour` window is at 55% with 45 minutes to reset, 30% of headroom (to the 85% ceiling) will evaporate at reset. Those tokens are already paid for — not using them is waste.

**Trigger conditions** (all must be true):
- Window resets in ≤ `sprint.horizon_minutes` (default: 90)
- Remaining headroom > `sprint.min_headroom_pct` (default: 15%)
- Bead backlog exists (workers have work to do)
- No other window is at `cutoff_risk`

**Behavior:** Temporarily raise `max_workers` by `sprint.max_workers_boost` (default: 3) for the affected agent. The sprint flag is set in state so `cgov status` shows the reason. Sprint ends when the window resets or headroom drops below 5%.

```python
if sprint_eligible(window):
    effective_max = min(
        max_workers + sprint_max_workers_boost,
        safe_worker_count_other_windows  # never violate other windows
    )
    target = min(compute_target(), effective_max)
    state.sprint_active = True
    state.sprint_window = window.name
```

**Guard:** Sprint never violates other windows. `effective_max` is capped at the minimum `safe_worker_count` across non-sprinting windows. Additionally, sprint is inhibited when the forecast confidence cone (Component 21) is wide (`cone_ratio > 2.0`) — high prediction uncertainty makes aggressive scaling dangerous. Sprint is also disabled while safe mode (Component 20) is active.

**Status output during sprint:**
```
Workers:  5 active  (target: 5 · normal max: 3 · SPRINT on five_hour, resets in 0h42m)
```

**Most useful for:** The `five_hour` window, which resets frequently and often has significant unused headroom.

---

### 15. Worker Capacity Awareness

**Purpose:** Give workers visibility into the fleet's capacity state so they can adapt their behavior to budget constraints.

Workers are Claude Code instances that can run shell commands. The approach: `cgov status --json` is the data source, and NEEDLE injects a capacity summary into the worker's operating context at launch.

**Mechanism — Prompt Injection via NEEDLE:**

When NEEDLE launches a worker, it runs `cgov status --json` and injects a capacity summary block into the worker's CLAUDE.md:

```markdown
## Fleet Capacity (auto-injected by governor)

- Binding window: seven_day_sonnet — 26% headroom, resets in 37h
- Capacity pressure: HIGH (cutoff risk active)
- Recommendation: prefer lighter approaches, minimize unnecessary exploration,
  use Haiku subagents where possible, avoid speculative multi-file reads
```

Three pressure levels drive the recommendation:
- **LOW** (`margin_hrs > hrs_left * 0.5`): no constraints — work normally
- **MEDIUM** (`margin_hrs > 0` but < `hrs_left * 0.5`): be efficient but don't compromise quality
- **HIGH** (`cutoff_risk` active): actively conserve — prefer Haiku subagents, skip optional steps

**Active Checking:** Workers can run `cgov status --json` mid-task. The exit code (0=safe, 2=cutoff_risk, 3=emergency) enables simple conditionals in hooks:

```bash
# Claude Code hook (pre-tool):
cgov status --json > /dev/null 2>&1
[ $? -eq 3 ] && echo "Emergency brake — pausing" && exit 1
```

**NEEDLE integration:** Capacity injection happens in NEEDLE's worker launch function. If `cgov` is not installed or the daemon is not running, the block is omitted — non-breaking enhancement.

---

### 16. Decision Narration & Audit Log (`cgov explain`)

**Purpose:** Make every scaling decision transparent and auditable with a plain-English explanation and persistent decision log.

**Decision log:** `~/.needle/state/governor-decisions.jsonl`, one line per governor cycle that resulted in a scaling action or notable state change:

```json
{"ts":"2026-03-18T14:30:00Z","action":"scale_down","from":3,"to":2,"reason":"seven_day_sonnet binding at 72.3% with 24.1h to reset. 3 workers exhaust ceiling in 5.9h; 2 workers extend to 8.8h.","trigger":"cutoff_risk","binding_window":"seven_day_sonnet","margin_before":-18.2,"margin_after":8.8}
```

**Notable state changes that generate entries:**
- Scale up or down (with full reasoning)
- Cutoff risk transitions (safe→risk, risk→safe)
- Sprint activation/deactivation
- Pre-scale activation
- Emergency brake engagement/release
- Promotion multiplier transitions
- Prediction accuracy scores (on window reset)

**`cgov explain` output:**

```
Latest decision — 2026-03-18 14:30 ET

Action:  scale_down 3 → 2 workers
Reason:  seven_day_sonnet binding at 72.3% with 24.1h to reset.
         At p75 burn of 4.5%/hr/worker, 3 workers exhaust the 90%
         ceiling in 5.9h. Reducing to 2 extends exhaustion to 8.8h.
Trigger: cutoff_risk on seven_day_sonnet
```

`cgov explain --last 5` shows the 5 most recent decisions. `cgov explain --json` emits raw JSONL records.

**Implementation:** A template-based text generator in `src/narrator.rs` that takes the governor state diff (before/after) and produces the explanation. Templates cover each action type. No LLM — pure string formatting from structured data.

---

### 17. Cross-Window Capacity Optimization

**Purpose:** Optimize worker count across all three windows simultaneously, weighting by reset horizon, rather than governing solely to the binding window.

**Problem:** The binding-window approach is overly conservative when a short-horizon window is the constraint. If `five_hour` is binding at 72% with 45 minutes to reset, the governor scales down — even though the 5h window regenerates in 45 minutes while `seven_day` windows have ample room. The "cost" of consuming 5h capacity is low (regenerates soon); the "cost" of consuming 7d_sonnet capacity is high (gone for days).

**Marginal window cost:**

```python
def window_cost(window):
    """Higher cost = more scarce capacity to consume."""
    if window.remaining_pct <= 0:
        return float('inf')
    scarcity = (1.0 / window.remaining_pct) * window.hrs_left
    return scarcity
```

A window with 30% remaining and 2h to reset: cost = 0.067. Same 30% with 36h to reset: cost = 1.2 — 18× more expensive. This correctly captures that short-horizon capacity is cheap.

**Optimized target:**

```python
def composite_risk(N, windows, burn_rate):
    """Total risk score for running N workers."""
    risk = 0
    for w in windows:
        exhaust_hrs = w.remaining_pct / (burn_rate * N)
        if exhaust_hrs < w.hrs_left:
            risk += window_cost(w) * (w.hrs_left - exhaust_hrs)
    return risk

# Max N where composite_risk is acceptable
optimal = max(N for N in range(max_workers + 1)
              if composite_risk(N, windows, burn_rate) < cost_threshold)
```

`cost_threshold` defaults to `0.0` (never breach any window — equivalent to current binding-window behavior). Setting it positive allows breaching cheap (short-horizon) windows when long-horizon windows have ample room.

**Interaction with sprint:** Cross-window optimization and sprint are complementary. Sprint captures end-of-window surplus by adding workers temporarily; cross-window optimization allows running more workers *persistently* by recognizing that short-horizon breaches are low-cost.

**State schema when enabled:** When `cross_window.enabled` is true:
- `binding_window` — still populated (the window with smallest `margin_hrs`) for display and logging
- `safe_worker_count` — computed by the composite risk function rather than the binding-window-only formula
- `cross_window_optimal` — new field: the worker count where `composite_risk < cost_threshold`
- When `cross_window.enabled` is false or safe mode is active, `safe_worker_count` reverts to the strict binding-window formula and `cross_window_optimal` is `null`

---

### 18. Cache Efficiency Monitor

**Purpose:** Track per-worker and per-workspace cache hit ratios to identify inefficient capacity consumption.

**Metric:**

```
cache_efficiency = cache_read_tokens / (cache_read_tokens + input_tokens)
```

When both `cache_read_tokens` and `input_tokens` are zero for an interval (worker produced only output or was idle), `cache_eff` is `null` — the metric is undefined and excluded from aggregation and alerting. This avoids division-by-zero and prevents idle intervals from skewing statistics.

Cache reads cost 0.1× input rate (30× cheaper). A worker with 90% cache efficiency burns ~3× less quota per useful token than one with 50% efficiency. This is the single largest lever on burn rate the governor doesn't currently observe.

**Tracking:** The Token Collector adds `cache_eff` to each `i` record and `fleet_cache_eff` + `cache_eff_p25` to `f` records.

**SQLite view:**

```sql
CREATE VIEW workspace_cache_eff AS
SELECT
    sess,
    AVG("cache_eff") AS avg_eff,
    MIN("cache_eff") AS min_eff,
    COUNT(*) AS samples
FROM i
WHERE t0 > datetime('now', '-24 hours')
GROUP BY sess
ORDER BY avg_eff ASC;
```

**Alerting:** When a worker's cache efficiency drops below `cache_efficiency.warn_threshold` (default: 0.60) for `consecutive_intervals` (default: 3) consecutive intervals:

```
[governor] CACHE: needle-claude-anthropic-sonnet-bravo efficiency 0.43 (fleet avg 0.84)
```

**`cgov status` integration:**

```
Burn:     $2.15/hr  (p75 per worker)  ·  Cache: 84% (fleet avg)
```

**Common causes of low cache efficiency:**
- Worker recently restarted (cold cache) — expected, transient
- Workspace has many small files (poor cache locality) — structural
- Context exceeded 200k tokens and was compressed — causes cache invalidation burst

The governor does not automatically act on cache efficiency (causes are too varied), but surfacing the metric makes the operator aware of a key cost driver.

---

### 19. System Health Diagnostic (`cgov doctor`)

**Purpose:** Validate the entire governor stack in one command with specific remediation steps.

**Checks:**

| Check | Pass | Warn | Fail |
|---|---|---|---|
| OAuth credentials | Valid, >1h to expiry | <30min to expiry | Expired or missing |
| API reachability | 200 OK in <2s | Slow (>2s) | Unreachable or auth error |
| Token collector | Running, cursors advancing | Cursors stale >10min | Not running |
| State file freshness | Updated <2× interval | Stale 2–10× interval | Missing or stale >10× interval |
| Heartbeat consistency | Worker count matches tmux sessions | Minor mismatch | Major mismatch or corruption |
| Burn rate samples | ≥5 per window | 3–4 samples | <3 (using baseline fallback) |
| Pricing config | All detected models have entries | — | Unknown model in token records |
| Model generation | Rates match known generation | — | Opus 4.6 priced at $15/$75 (legacy) |
| Promotion dates | Active or future promo configured, or no promotion configured | Expires in <48h | Expired, still in config |
| SQLite integrity | `PRAGMA integrity_check` passes | — | Corruption detected |
| JSONL/DB sync | Row counts within 1% | Diverge >1% | DB missing or empty |
| Config parseable | Valid YAML, all required fields present | — | Syntax error or missing required fields |
| Tmux available | `tmux` command works | — | Not installed (worker management requires tmux) |
| Alert cooldown | Cooldown timestamps valid | — | Invalid timestamps |
| Disk space | >1GB free on state partition | <500MB free | <100MB free |
| Daemon status | Running, last cycle <2× interval | Last cycle >2× interval | Not running |
| Log file | Exists, <100MB | >100MB | Missing or not writable |
| Prediction accuracy | Median error <5% | 5–10% | >10% or insufficient data |
| Claude binary installed | `claude` in PATH | — | Not found (API polling requires Claude CLI) |
| Claude print installed | `claude print` command available | — | Not found (older Claude CLI version) |
| Subscription session | Valid session file exists | — | No active Claude Code subscription session |

**Output:**

```
cgov doctor — 2026-03-18 14:30 ET
──────────────────────────────────────────
✓ OAuth credentials        valid, expires in 3h12m
✓ API reachability         200 OK (142ms)
✓ Token collector          running, cursors current
✓ State file freshness     updated 14s ago
✓ Heartbeat consistency    2 workers, 2 sessions
✓ Burn rate samples        12 samples (seven_day_sonnet)
✓ Pricing config           all models matched
✓ Model generation         rates consistent
⚠ Promotion dates          expires in 10 days
✓ SQLite integrity         OK
✓ JSONL/DB sync            4,201 / 4,198 rows (99.9%)
✓ Config parseable         valid YAML
✓ Tmux available           /usr/bin/tmux found
✓ Alert cooldown           valid timestamps
✓ Disk space               8.2GB free
✓ Daemon status            running, last cycle 42s ago
✓ Log file                 12.4 MB
✓ Prediction accuracy      median error -2.1% (24 scored)
✓ Claude binary installed  /usr/local/bin/claude found
✓ Claude print installed   v2.1.78
✓ Subscription session     active (claude-code-glm47-charlie)
──────────────────────────────────────────
20 passed · 1 warning · 0 failed
```

The `⚠ Promotion dates` line above is illustrative, from the historical March 2026 Off-Peak 2x promotion window. With the shipped empty `promotions.json` (`[]`) this check passes with `No promotions configured`.

**Note:** The implementation includes ~20 health checks covering OAuth, API, collector, state files, heartbeats, burn rates, pricing, promotions, database integrity, configuration, tmux, alerts, disk space, daemon status, logs, prediction accuracy, Claude CLI installation, and subscription session presence. The table above shows all implemented checks.

**Exit codes:** `0` = all pass (warnings OK), `1` = any fail.

**Remediation hints:** Each failing check includes a specific fix command:
- Credentials expired → `"Run: claude login"`
- SQLite corrupt → `"Run: cgov token-history --rebuild-db"`
- Pricing mismatch → `"Update pricing in ~/.config/claude-governor/governor.yaml"`
- Collector not running → `"Run: cgov start"`

---

### 20. Safe Mode

**Purpose:** When prediction accuracy degrades beyond a threshold, the governor automatically enters a conservative fallback mode rather than continuing to make confident-but-wrong scaling decisions.

**Trigger:** The calibrator (Component 12) tracks prediction accuracy over scored window resets. Safe mode activates when:
- Median prediction error > `safe_mode.enter_median_error` (default: 10.0%), OR
- Prediction error stddev > `safe_mode.enter_stddev` (default: 15.0%)

**Effects while active:**
- `target_utilization` reduced by `safe_mode.util_reduction` (default: 0.10) — e.g., 0.90 → 0.80
- `hysteresis_band` widened by `safe_mode.hysteresis_boost` (default: 1) — less reactive
- Sprint (Component 14) disabled — no aggressive end-of-window scaling
- Cross-window optimization (Component 17) disabled — fall back to strict binding-window behavior
- Logging: `[governor] SAFE MODE ENTERED — prediction accuracy degraded (median error 14.2%, stddev 18.1%)`

**Exit conditions** (all must be true):
- Median error drops below `safe_mode.exit_median_error` (default: 7.0%)
- Stddev drops below `safe_mode.exit_stddev` (default: 12.0%)
- At least 3 new predictions scored since safe mode was entered

Uses hysteresis between entry and exit thresholds to prevent toggling. Logging: `[governor] SAFE MODE EXITED — prediction accuracy recovered (median error 4.8%, stddev 9.2%)`

**State:** `safe_mode` in `governor-state.json`:

```json
{
  "safe_mode": {
    "active": true,
    "entered_at": "2026-03-19T10:00:00Z",
    "trigger": "median_error",
    "median_error_at_entry": 14.2,
    "predictions_since_entry": 1
  }
}
```

**`cgov status` integration:**
```
Workers:  2 active  (target: 2 · safe ceiling: 2)  ⚠ SAFE MODE (prediction accuracy degraded)
```

**Interaction with calibrator:** Safe mode and auto-tuning (Component 12) are complementary. Auto-tuning gradually adjusts parameters to improve accuracy; safe mode is the immediate defensive response when accuracy is too poor for tuning alone to fix. Safe mode buys time for the auto-tuner to converge.

**Interaction with sprint (Component 14):** If safe mode activates while a sprint is running, the sprint terminates at the end of the current governor cycle. Sprint cannot be re-entered while safe mode is active.

**Interaction with confidence cone (Component 21):** While safe mode is active, the governor always acts on the p75 (worst-case) estimate regardless of cone width. This is the most conservative posture.

**Manual override:** `cgov scale N` overrides the safe-mode-adjusted target for one cycle but logs `[governor] WARN: manual scale override during safe mode`. Safe mode remains active and will reassert its target on the next cycle unless overridden again.

**`predictions_since_entry` semantics:** Incremented once per window reset event (regardless of how many windows reset at the same time). Three distinct reset events must occur before safe mode can exit, ensuring enough fresh data to judge whether accuracy has genuinely recovered.

---

### 21. Forecast Confidence Cone

**Purpose:** Replace single-point exhaustion predictions with range estimates that convey prediction certainty, using per-worker variance data already collected.

**Mechanism:** The per-worker variance data in `f` records (`p75-usd-hr`, `std-usd-hr`) feeds three prediction scenarios per window:

```python
# Per-worker burn rate distribution from recent fleet aggregates:
rate_p25 = ema_rate - 0.675 * stddev   # optimistic (25th percentile)
rate_p50 = ema_rate                     # expected (median)
rate_p75 = ema_rate + 0.675 * stddev   # conservative (75th percentile)

# Per-window exhaustion prediction cone:
exh_hrs_p25 = remaining_pct / (rate_p25 * workers)  # best case (slow burn)
exh_hrs_p50 = remaining_pct / (rate_p50 * workers)  # expected
exh_hrs_p75 = remaining_pct / (rate_p75 * workers)  # worst case (fast burn)
```

**Decision logic uses the cone width:**
- **Narrow cone** (p75/p25 ratio < 1.5): high confidence — act on p50 (expected case)
- **Wide cone** (p75/p25 ratio > 2.0): uncertain — act on p75 (worst case), which is more conservative
- **Between**: blend linearly between p50 and p75 as the action threshold

This means the governor is automatically more conservative when workload is heterogeneous (high variance) and more aggressive when workload is uniform (low variance), without any explicit configuration.

**Extended `w` record fields:**

```json
{"r":"w",...,"exh_hrs":2.94,"exh_hrs_p25":3.81,"exh_hrs_p50":2.94,"exh_hrs_p75":2.02,"cone_ratio":1.89,...}
```

**`cgov status` integration:**

```
Window          Used    Ceiling  Remain  Resets    Risk
seven_day_sonnet 64%      90%     26%    in 37h    ⚠ CUTOFF 2.0–3.8h ← BINDING
```

The range `2.0–3.8h` replaces the single `2.9h`, immediately communicating uncertainty to the operator.

**`cgov simulate` integration:** The simulator outputs three trajectory lines (best/expected/worst) instead of one, showing the cone diverge over time. Useful for seeing when a decision that looks safe in the expected case is risky in the worst case.

**Interaction with safe mode:** When safe mode is active (Component 20), the governor always uses p75 regardless of cone width — maximum conservatism during periods of known unreliability.

---

## Configuration File

The canonical default is `~/.config/claude-governor/governor.yaml`. When
`XDG_CONFIG_HOME` is set, the implementation checks
`$XDG_CONFIG_HOME/claude-governor/governor.yaml` first, then falls back to the
canonical default path below.

`~/.config/claude-governor/governor.yaml`:

```yaml
# Governor configuration
loop_interval: 300          # seconds between cycles (5 minutes)
hysteresis_band: 1          # workers deviation before acting
log_file: ~/.local/share/claude-governor/governor.log
log_level: INFO             # DEBUG, INFO, WARN, ERROR
log_max_bytes: 104857600    # 100 MB — rotate when exceeded
log_backup_count: 3         # keep 3 rotated log files (.1, .2, .3)
state_file: ~/.needle/state/governor-state.json

# Target utilization — governs how much of each window the fleet is allowed to consume.
# 1.0 = use all available capacity; 0.9 = reserve 10%; 0.8 = reserve 20%.
# Applies to the remaining headroom calculation: remaining = (target * 100) - current_pct
# Can be overridden per-window under `windows:` below.
target_utilization: 0.90    # default: reserve 10% across all windows

# Per-window overrides (optional — inherit target_utilization if absent)
windows:
  five_hour:
    target_utilization: 0.85  # session window: tighter reserve to avoid mid-task cutoff
  seven_day:
    target_utilization: 0.90
  seven_day_sonnet:
    target_utilization: 0.90

# Managed agents
agents:
  claude-anthropic-sonnet:
    enabled: true
    min_workers: 1
    max_workers: 5
    baseline_burn_rate: 1.2    # % per worker per hour (initial estimate)
    burn_rate_alpha: 0.2       # EMA smoothing factor
    launch_cmd: "needle run --agent=claude-anthropic-sonnet --force"
    session_pattern: "needle-claude-anthropic-sonnet-*"  # tmux session glob
    heartbeat_dir: ~/.needle/state/heartbeats            # JSON heartbeat files

  claude-anthropic-opus:
    enabled: false             # not managed by default
    min_workers: 0
    max_workers: 2
    baseline_burn_rate: 4.0    # Opus consumes ~3-4x more quota than Sonnet
    launch_cmd: "needle run --agent=claude-anthropic-opus --force"
    session_pattern: "needle-claude-anthropic-opus-*"
    heartbeat_dir: ~/.needle/state/heartbeats

# Multi-agent orchestration:
# All agents share the same usage windows (five_hour, seven_day, seven_day_sonnet).
# Each agent has independent min/max/target worker counts and burn rate EMA.
# The governor's capacity forecast aggregates burn across all active agents:
#   fleet_pct_per_hour = sum(ema_rate[agent] * workers[agent] for agent in enabled_agents)
# When scaling down under cutoff_risk, the governor reduces the agent with the
# highest per-worker dollar cost first (Opus before Sonnet before Haiku).
# When scaling up, the governor adds workers to the agent with the lowest
# per-worker cost that has capacity (current < max_workers).

# Promotion definitions
promotions_file: ~/.config/claude-governor/promotions.json

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
  cooldown_minutes: 60        # suppress duplicate alerts for this period

# Promotion validation
promotion_validation:
  tolerance_pct: 10           # observed ratio must be within ±10% of declared multiplier
  min_samples: 5              # minimum peak + off-peak samples before validating
  fallback_multiplier: 1.0    # use this when validation fails (conservative)

# Trajectory simulator
simulator:
  default_resolution_minutes: 15  # output interval granularity
  max_horizon_hours: 168          # maximum simulation window (1 week)

# Prediction accuracy self-calibration
calibration:
  auto_calibrate: true            # auto-tune parameters based on prediction accuracy
  min_predictions_to_tune: 10     # minimum scored predictions before tuning activates
  alpha_range: [0.05, 0.5]        # clamp range for auto-tuned EMA alpha
  hysteresis_range: [0, 3]        # clamp range for auto-tuned hysteresis band
  target_util_range: [0.70, 0.98] # clamp range for auto-tuned target utilization

# Promotion boundary pre-scaling
pre_scale_minutes: 30             # look-ahead for multiplier transitions (0 = disabled)

# End-of-window capacity sprint
sprint:
  enabled: true
  horizon_minutes: 90             # how close to reset before sprint activates
  min_headroom_pct: 15            # minimum remaining headroom to trigger sprint
  max_workers_boost: 3            # workers added to max_workers during sprint

# Cross-window capacity optimization
cross_window:
  enabled: true                   # composite risk scoring vs binding-window-only
  cost_threshold: 0.0             # 0 = strict (never breach); >0 = allow cheap-window breach

# Cache efficiency monitoring
cache_efficiency:
  enabled: true
  warn_threshold: 0.60            # warn when worker cache efficiency drops below this
  consecutive_intervals: 3        # consecutive intervals below threshold before warning

# Safe mode — automatic conservatism when predictions degrade
safe_mode:
  enabled: true
  enter_median_error: 10.0        # enter safe mode when median prediction error exceeds this %
  enter_stddev: 15.0              # or when prediction error stddev exceeds this %
  exit_median_error: 7.0          # exit when median error drops below this %
  exit_stddev: 12.0               # and stddev drops below this %
  util_reduction: 0.10            # reduce target_utilization by this amount in safe mode
  hysteresis_boost: 1             # widen hysteresis_band by this in safe mode
```

---

## Implementation Plan

### Phase 1: Usage Poller (Foundation)

**Goal:** Replace `claude-status.sh` with reliable direct API polling.

1. Implement `src/poller.rs`:
   - Reads `~/.claude/.credentials.json` for OAuth token
   - Checks token expiry; refreshes via HTTPS (ureq + rustls) if needed
   - Calls `/api/oauth/usage` via HTTPS
   - Parses response, computes `hours_remaining` from `resets_at`
   - Handles errors (network, invalid token, refresh failure with 3-cycle escalation)

2. Test: Run `cgov poll` every minute for 30 minutes, verify output matches TUI `/status` values.

**Deliverable:** `cgov poll` subcommand — standalone, usable without the daemon running.

---

### Phase 1b: Token Collector (Independent Data Capture)

**Goal:** Independently capture model-specific, token-type-specific consumption data. Runs as a separate daemon; the governor reads from its output but does not depend on it being active to function.

1. Implement `src/collector.rs`:
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
   - **Shrinkage guard:** Before seeking, compare current file size to stored offset. If file is smaller (log rotation replaced it), reset that file's cursor to 0 and re-scan from the beginning. Log `[collector] cursor reset for {filepath} (file shrunk: {stored} → {actual} bytes)`

3. `window_pct_deltas` annotation: on each governor poll cycle, the governor joins the interval's instance records to the concurrent API percentage snapshots to fill in `pct_delta_5h`, `pct_delta_7d`, `pct_delta_7d_s`. Apportionment across instances uses each instance's `dollar_equiv.total` as the weight (heavier-spending workers are attributed proportionally more of the observed percentage movement). This is the only field the collector cannot self-populate.

4. Write one `f` (fleet) record per interval after all `i` (instance) records, containing aggregated stats, `per_worker_stats` (mean/p75/stddev), `window_snapshots` from the API, and the full `capacity_forecast` block.

5. Test independently:
   - Verify dollar computation against known API pricing (unit test each tok_type × model)
   - Verify delta (not cumulative) — run twice, confirm second pass emits zero-n records
   - Verify correct model attribution when multiple models active in same session
   - Verify `f` record variance stats with synthetic data: 3 sessions, divergent `usd` values
   - Verify `w` record `bind` and `safe_w` selection when 5h is more constraining than 7d
   - Verify Python queryability: `python3 -c "import json,sys;[print(l,end='') for l in open('token-history.jsonl') if json.loads(l).get('r')=='i' and json.loads(l).get('w-cache-1h-n',0)>0]"` returns only records with 1-hour cache write activity

**Deliverable:** `cgov collect` subcommand — fully standalone, can be queried without the daemon running.

---

### Phase 2: Schedule and Promotion Calculator

**Goal:** Accurate effective-hours calculation with off-peak awareness.

1. Implement `src/schedule.rs`:
   - `is_peak_now()` → bool
   - `current_multiplier()` → float (1.0 or 2.0 during promo)
   - `effective_hours_remaining(reset_time)` → float
   - Reads from `promotions.json`

2. Create `promotions.json` with an entry defining the promotion window when a promotion is active (see Component 4 for the format and the historical March 2026 Off-Peak 2x example). The historical example is not a current configuration: when no promotion is running, `promotions.json` should contain an empty array `[]` (flat 1.0 multiplier — this is the default shipped configuration and the correct normal operating state).

3. Unit tests:
   - Peak hour boundaries (7:59 AM ET → 1x, 8:00 AM ET → 1x peak, 2:01 PM ET → 2x)
   - Weekend classification
   - Past-promo-end returns 1x
   - Effective hours: 40h reset with 30h off-peak should be > 40

4. **Promotion validation against measured consumption:**

   The `offpeak_multiplier: 2.0` in the historical promotion example came from the official announcement — an active future promotion must be confirmed against observed data before the governor trusts it for scheduling. With the shipped empty `promotions.json`, there is no declared multiplier to validate and scheduling remains at the flat 1.0 default.

   **Validation approach:** Once the poller has accumulated ≥5 peak-hour samples and ≥5 off-peak samples with the same worker count, compute:
   ```
   observed_ratio = median(tokens_per_pct_offpeak_samples) / median(tokens_per_pct_peak_samples)
   ```
   - If `observed_ratio` is within 10% of `offpeak_multiplier` (e.g., 1.8–2.2 for a declared 2.0): **validated**, log confirmation.
   - If `observed_ratio < 1.2`: promotion may not be applying — log warning, fall back to 1x multiplier until re-validated.
   - If `observed_ratio > 2.5`: unexpected — log anomaly, use observed ratio instead of declared.

   Write validation result to `burn_rate.promotion_validated` in state file. The scheduler reads this flag: if `false`, it uses `offpeak_multiplier: 1.0` (conservative) rather than the declared 2.0.

   This guards against: the promotion ending early, the multiplier applying to some limit buckets but not others, or a future promotion with a different multiplier being misconfigured.

**Deliverable:** `src/schedule.rs` + `config/promotions.json` (empty array by default) + promotion validation logic in burn_rate module

---

### Phase 3: Adaptive Burn Rate Estimator

**Goal:** Empirically calibrate per-model burn rates in %/hr, tokens/hr, and $/hr.

1. Extend state store with `burn_rate.by_model`, `last_fleet_aggregate`, and `capacity_forecast` as specified in the State Store schema.

2. Implement `src/burn_rate.rs`:
   - Reads `i` (instance) records from `token-history.db` for the most recent complete interval
   - Computes per-instance `dollar_burn` and `pct_burn` per window from annotated `window_pct_deltas`
   - Computes fleet-level `per_worker_stats` (mean, p75, stddev) across active sessions
   - Maintains per-(model, window) EMA with `alpha = 0.2`
   - Generates `capacity_forecast` per window: `fleet_pct_per_hour`, `predicted_exhaustion_hours`, `will_exhaust_before_reset`, `safe_worker_count`
   - Identifies `binding_window` — the window that will exhaust soonest
   - Stores separate peak/off-peak `tokens_per_pct` per window for promotion validation
   - Guard conditions: skip short intervals, changed worker counts, window resets mid-interval, zero-delta API responses
   - Falls back to `baseline_burn_rate` from `governor.yaml` until 3 valid samples exist **per window** (each window independently transitions from baseline to EMA as it accumulates samples; a window with 2 samples uses baseline even if another window has 10)

3. Update `compute_target_workers()` in governor to use `safe_worker_count[binding_window]` as the ceiling rather than the single-window formula.

4. Log per-window capacity forecast each cycle:
   ```
   [governor] 5h: 63.6% remaining, resets in 1.5h — OK (exhausts in 8h at current rate)
   [governor] 7d: 27.4% remaining, resets in 37.5h — OK (not binding)
   [governor] 7d-sonnet: 36.5% remaining, resets in 37.5h — BINDING (exhausts in 4.1h at 2 workers)
   [governor] → target: 2 workers (safe_worker_count from binding window)
   ```

**Deliverable:** `src/burn_rate.rs` + updated state schema + per-window capacity forecast

---

### Phase 4: Core Governor Loop

**Goal:** Replace `capacity-governor.sh` with the full governor.

1. Implement `src/governor.rs` (main daemon):
   ```python
   while True:
       usage = poll_usage()
       schedule = compute_schedule()
       burn_rate = compute_burn_rate()
       target = compute_target_workers(usage, schedule, burn_rate)
       current = count_workers()
       apply_scaling(current, target)
       check_alerts(usage, schedule)
       write_state()
       time.sleep(LOOP_INTERVAL)
   ```

2. Implement `compute_target_workers()` using corrected formula with effective hours.

3. Implement `apply_scaling()` with hysteresis band and graceful scale-down.

4. Implement emergency brake (>= 98% → scale to 0).

5. Implement `src/worker.rs`:
   - `scale_up(n)`: execute `agent.launch_cmd` n times (one per tick) via shell
   - `scale_down_graceful(n)`: read heartbeat JSON files from `agent.heartbeat_dir`, find idle workers, send SIGINT via tmux, fall back to kill after timeout
   - `count_workers()`: read heartbeat files + verify against `tmux list-sessions`

6. `--dry-run` mode: compute and log everything but do not modify workers.

**Deliverable:** `cgov daemon` subcommand — replaces `capacity-governor.sh`

---

### Phase 5: Alert Manager

**Goal:** Surfacing important state transitions to human attention.

1. Implement `src/alerts.rs`:
   - Check each alert condition against thresholds
   - Check if alert already fired this period (dedup by cooldown in state file)
   - Create HUMAN-type bead via configured alert command (default: `bf create --type human "..."`)
   - Log alert to `governor.log`

2. Add `last_fired` per-type tracking to state file.

3. Add "underutilization sprint" logic: if < 50% used and < 2h to reset, boost to max_workers.

**Deliverable:** `src/alerts.rs` module

---

### Phase 6: CLI, Packaging, and Deployment

**Goal:** Ship a single `cgov` CLI entry point that works for both humans and scripts, with a one-command install that persists across reboots.

#### 6.1 — `cgov` CLI Entry Point

A single static binary at `~/.local/bin/cgov` that implements all subcommands directly. Every subcommand supports `--json` for machine-readable output.

**Subcommand surface:**

```
cgov status [--json] [--watch]             # Windows, workers, burn rate, cutoff risk
cgov forecast [--json]                     # Window forecast only (latest w records)
cgov workers [--json]                      # Worker count, targets, heartbeat status
cgov scale <N> [--dry-run]                 # Manually override target worker count
cgov start                                 # Start the governor daemon
cgov stop                                  # Stop the governor daemon (graceful)
cgov restart                               # Restart daemon
cgov enable                                # Install + enable systemd service (survives reboot)
cgov disable                               # Disable systemd service
cgov logs [--follow] [--lines N]           # Tail governor log
cgov token-history [--json] [--last N] [--window W]  # Query w records
cgov config [--edit]                       # Print active config; --edit opens $EDITOR
cgov version                               # Print version and component status
cgov simulate [--workers N|SCHEDULE] [--hours H] [--json]  # Project capacity trajectory
cgov explain [--json] [--last N]                            # Scaling decision reasoning
cgov doctor [--json]                                        # System health diagnostic
```

**Human output (`cgov status`):**

```
Claude Governor — 2026-03-18 14:30 ET
──────────────────────────────────────────────────────
Window          Used    Ceiling  Remain  Resets    Risk
five_hour        36%      85%     49%    in 1h30   OK
seven_day        73%      90%     17%    in 37h    ⚠ CUTOFF (2.7h at current rate)
seven_day_sonnet 64%      90%     26%    in 37h    ⚠ CUTOFF (2.9h) ← BINDING

Workers:  2 active  (target: 2 · safe ceiling: 2)
Burn:     $2.15/hr  (p75 per worker)
Peak:     no  (promo 2x active, off-peak until 08:00 ET)
──────────────────────────────────────────────────────
Last cycle: 14s ago  |  Next: in 4m46s
```

- As in Component 10, the `Peak:` line is illustrative output from the historical March 2026 Off-Peak 2x promotion; with the shipped empty `promotions.json` (`[]`) it carries no promo annotation
- Color coding: green = OK, yellow = approaching ceiling, red = cutoff risk
- Falls back to plain ASCII when `NO_COLOR` is set or stdout is not a TTY
- `--watch` clears terminal and re-renders on a 30s interval

**Robot output (`cgov status --json`):**

Emits the full `governor-state.json` to stdout. Exit codes signal actionable states without requiring JSON parsing:

| Exit code | Meaning |
|---|---|
| `0` | All windows safe |
| `1` | General error (daemon not running, parse failure) |
| `2` | Cutoff risk active on one or more windows |
| `3` | Emergency brake engaged (a window ≥ 98%) |

```bash
# Branch on exit code without parsing JSON:
cgov status --json > state.json
case $? in
  2) echo "Cutoff risk — pausing new submissions" ;;
  3) echo "Emergency brake — halt all workers" ;;
esac
```

**Non-TTY auto-detection:** When stdout is not a TTY and `--json` is omitted, `cgov status` emits JSON automatically — assumes pipeline. Human formatting only when writing to a terminal.

#### 6.2 — Daemon Management

Two modes, selected at install time based on what's available:

**Mode A: systemd user service (Linux default)**

```ini
# ~/.config/systemd/user/claude-governor-observe.service
[Service]
Type=simple
ExecStart=%h/.local/bin/cgov _observe
Restart=always
RestartSec=10
```

```ini
# ~/.config/systemd/user/claude-governor-act.service
[Service]
Type=simple
ExecStart=%h/.local/bin/cgov _act
Restart=always
RestartSec=10
```

```ini
# ~/.config/systemd/user/claude-token-collector.service
[Unit]
Description=Claude Governor — token collector
After=default.target

[Service]
Type=simple
ExecStart=%h/.local/bin/cgov _token-collector
Restart=on-failure
RestartSec=30
```

```bash
cgov enable         # installs units → daemon-reload → enable + start all
cgov disable       # stop + disable all units
cgov start observe # start telemetry without enabling act
cgov stop act      # pause scaling and alerting, keep telemetry running
cgov restart       # restart all units
```

`claude-governor-observe.service` and `claude-governor-act.service` are
independently supervised. `cgov init`/`enable` remove the pre-ADR combined
`claude-governor.service` and `cgov.service` units from an existing install.

**Mode B: tmux sessions (fallback — no systemd or macOS)**

```bash
# cgov enable (tmux fallback path)
tmux new-session -d -s "cgov-observe" "cgov _observe"
tmux new-session -d -s "cgov-act" "cgov _act"
tmux new-session -d -s "claude-token-collector" "cgov _token-collector"
```

`cgov start` auto-detects whether a systemd user session is available and picks the appropriate mode. The chosen mode is written to `governor.yaml` as `daemon_mode: systemd|tmux`. `cgov stop` uses the same detection to know how to stop it.

`cgov logs --follow` works regardless of mode — it tails the log file directly, not the service journal.

#### 6.3 — Install Script

**Pre-built binary (recommended):**
```bash
curl -fsSL https://github.com/jedarden/claude-governor/releases/latest/download/cgov-linux-amd64 \
    -o ~/.local/bin/cgov && chmod +x ~/.local/bin/cgov
cgov init   # writes default config + systemd units
```

**Or build from source:**
```bash
git clone https://github.com/jedarden/claude-governor
cd claude-governor && cargo build --release
cp target/release/cgov ~/.local/bin/
cgov init
```

**`cgov init` steps:**
1. No prerequisites to check — `cgov` is a static binary with everything compiled in. Warn if `tmux` absent (needed for worker management)
2. Copy default configs → `~/.config/claude-governor/` (skip existing files — never clobber user config)
3. Create `~/.local/share/claude-governor/` (log directory) and `~/.needle/state/` if absent
4. Detect systemd availability; install units if present, print tmux fallback note otherwise
5. Print quickstart message:
   ```
   cgov v0.1.0 initialized
     cgov enable    → start daemon (survives reboot)
     cgov start     → start now, this session only
     cgov status    → check state
   Config: ~/.config/claude-governor/governor.yaml
   ```
6. Migration note: if `capacity-governor.sh` is detected, print the key config differences

**Upgrade:**
```bash
curl -fsSL https://github.com/jedarden/claude-governor/releases/latest/download/cgov-linux-amd64 \
    -o ~/.local/bin/cgov && chmod +x ~/.local/bin/cgov
```
Binary replacement is atomic — config files are never touched.

**Uninstall:**
```bash
cgov disable
rm ~/.local/bin/cgov
```
Removes the binary and systemd units. Does **not** touch `~/.needle/state/` (collected data) or `~/.config/claude-governor/governor.yaml` (configuration).

**Deliverable:** `cgov` binary, systemd unit files, `README.md` quickstart

---

### Phase 7: Advanced Capacity Intelligence

**Goal:** Add predictive, self-tuning, and diagnostic capabilities that make the governor increasingly accurate and transparent over time.

1. Implement `src/simulator.rs` (Component 11):
   - Reads current state + burn rates + promotions config
   - Walks forward in 1-minute steps with configurable worker schedule
   - Outputs trajectory as JSON array or ASCII table
   - Handles window resets (utilization drops to 0) and promotion transitions mid-simulation
   - Test: simulate 24h with known burn rate, verify window reset handling and promotion transitions

2. Implement `src/calibrator.rs` (Component 12):
   - Hooks into window reset events to score past predictions vs actual outcomes
   - Maintains `prediction-accuracy.jsonl` time series
   - Auto-tunes alpha, hysteresis, target_utilization within clamped ranges
   - Test: feed synthetic prediction/actual pairs, verify tuning direction and clamp enforcement

3. Add pre-scaling logic to `src/schedule.rs` (Component 13):
   - `next_transition()` returns upcoming multiplier change time and magnitude
   - Governor loop pre-scales down when transition is within `pre_scale_minutes`
   - Conservative-only: pre-scale down before losing bonus, never pre-scale up
   - Test: mock clock at 07:35 ET during promo, verify scale-down triggers before 08:00

4. Add sprint logic to `src/worker.rs` (Component 14):
   - Sprint eligibility check per window (headroom, time-to-reset, backlog, cross-window safety)
   - Temporary max_workers override with other-window guard
   - Sprint flag in state with auto-expiry at window reset
   - Test: verify sprint does not activate when a non-sprinting window has cutoff_risk

5. Add capacity injection to NEEDLE worker launch (Component 15):
   - Run `cgov status --json` at launch, format summary into pressure-level block
   - Inject into worker's CLAUDE.md under `## Fleet Capacity`
   - Three levels: LOW / MEDIUM / HIGH with behavioral guidance
   - Graceful degradation: omit block silently when cgov not installed
   - Test: verify injection content at each pressure level; verify omission when cgov absent

6. Implement `src/narrator.rs` (Component 16):
   - Template-based decision explanation generator (no LLM — pure string formatting)
   - Append to `governor-decisions.jsonl` on every scaling action or state transition
   - `cgov explain` reads and formats recent entries
   - Test: verify narration covers scale_up, scale_down, sprint, pre_scale, emergency_brake

7. Add cross-window optimization to `src/burn_rate.rs` (Component 17):
   - `window_cost()` and `composite_risk()` functions
   - Replace binding-window ceiling with composite-risk optimal worker count when enabled
   - Configurable `cost_threshold` (default 0.0 = strict, equivalent to binding-window behavior)
   - Test: 5h near-reset with 7d ample — optimizer should allow more workers than binding approach

8. Add cache efficiency tracking to `src/collector.rs` (Component 18):
   - Compute `cache_eff` per instance interval, add to `i` records
   - Add `fleet_cache_eff` and `cache_eff_p25` to `f` records
   - Create `workspace_cache_eff` view in SQLite
   - Warn on sustained low efficiency (below threshold for N consecutive intervals)
   - Test: synthetic token records with known cache ratios, verify metric and alert logic

9. Implement `src/doctor.rs` (Component 19):
   - Health checks with pass/warn/fail thresholds (the implementation includes ~20 checks covering OAuth, API, collector, burn rates, pricing, promotions, database integrity, configuration, tmux, alerts, disk space, daemon status, logs, prediction accuracy, Claude CLI installation, and subscription session presence)
   - Structured JSON output (`--json`) + human-readable table (TTY)
   - Specific remediation commands per failure type
   - Test: break each check condition, verify detection and remediation text

10. Add safe mode logic to `src/governor.rs` (Component 20):
    - Check calibrator's prediction accuracy on each cycle
    - Enter safe mode when thresholds exceeded: reduce target_util, widen hysteresis, disable sprint + cross-window
    - Exit when accuracy recovers with hysteresis between entry/exit thresholds
    - Write `safe_mode` block to state; surface in `cgov status`
    - Test: feed poor-accuracy prediction history, verify entry; feed good accuracy, verify exit with minimum-scored-since-entry guard

11. Add confidence cone to `src/burn_rate.rs` and `w` records (Component 21):
    - Compute `exh_hrs_p25`, `exh_hrs_p50`, `exh_hrs_p75` per window using per-worker stddev
    - Compute `cone_ratio` = p75/p25; use to modulate scaling aggressiveness
    - Narrow cone → act on p50; wide cone → act on p75; blend between
    - Extend `w` record and `cgov status` / `cgov simulate` output with range display
    - Test: synthetic fleet data with low vs high stddev, verify cone width affects which percentile governs decisions

**Deliverable:** `src/simulator.rs`, `src/calibrator.rs`, `src/narrator.rs`, `src/doctor.rs` + extensions to existing modules

---

## File Layout

```
claude-governor/
├── Cargo.toml                # Rust project manifest
├── Makefile                   # Build automation (linux-amd64, linux-arm64)
├── install.sh                 # Download binary + write default config (supports linux-amd64, linux-arm64)
├── src/
│   ├── main.rs                # CLI entry point + subcommand dispatch
│   ├── lib.rs                 # Library exports
│   ├── config.rs              # Configuration loader and path resolution
│   ├── db.rs                  # Database layer (SQLite wrapper)
│   ├── poller.rs              # Usage API poller (Phase 1) — ureq + rustls
│   ├── collector.rs           # Token collector (Phase 1b)
│   ├── governor.rs            # Main daemon loop (Phase 4)
│   ├── worker.rs              # Worker manager — launch/stop via configured commands (Phase 4)
│   ├── alerts.rs              # Alert creation (Phase 5)
│   ├── schedule.rs            # Peak/off-peak calculator (Phase 2)
│   ├── burn_rate.rs           # Model-specific burn rate EMA (Phase 3)
│   ├── pricing.rs             # Pricing model and dollar-equivalent calculation
│   ├── simulator.rs           # Trajectory projection (Phase 7)
│   ├── calibrator.rs          # Prediction accuracy self-tuning (Phase 7)
│   ├── narrator.rs            # Decision explanation generator (Phase 7)
│   ├── doctor.rs              # Health diagnostic (Phase 7)
│   ├── state.rs               # State store — JSON read/write + rusqlite
│   ├── capacity_summary.rs    # Capacity forecast aggregation
│   ├── status_display.rs      # Human-readable status output
│   └── snapshot_fixtures.rs   # Shared snapshot-test fixtures
├── tests/
│   ├── fixtures.rs            # Test fixtures and helpers
│   └── governor_cycle_snapshot_test.rs  # Integration test fixtures
├── config/
│   ├── governor.yaml          # Main configuration (incl. pricing table)
│   ├── promotions.json        # Promotion window definitions (empty by default when no active promotion)
│   ├── claude-governor-observe.service # Systemd user service — observe loop
│   ├── claude-governor-act.service # Systemd user service — act loop
│   └── claude-token-collector.service # Systemd user service — token collector
├── docs/
│   ├── research/
│   │   ├── usage-tracking.md
│   │   ├── off-hours-promotion.md
│   │   └── needle-architecture.md
│   └── plan/
│       └── plan.md           # This document
└── README.md
```

Service unit source files live in `config/`, not in a repository `systemd/`
directory. The current implementation ships separate observe, act, and token
collector units. The former combined `claude-governor.service` and
`cgov.service` names are legacy installed-unit names that `cgov init`/`enable`
remove when encountered; they are not current source artifacts.

**Build output:** `cargo build --release` produces a single statically-linked binary `target/release/cgov` (~5–10 MB). This is the only artifact that needs to be deployed.

**Runtime state files** (written to `~/.needle/state/`):

| File | Written by | Purpose |
|---|---|---|
| `governor-state.json` | governor | Current scaling state, burn rates, capacity estimate |
| `governor-state.prev.json` | governor | Previous cycle snapshot for delta calculation |
| `token-history.jsonl` | token-collector | Append-only per-interval token delta records |
| `token-history.db` | token-collector | SQLite mirror for fast queries |
| `collector-cursors.json` | token-collector | File byte offsets to avoid re-processing |
| `prediction-accuracy.jsonl` | calibrator | Scored prediction vs actual for self-tuning |
| `governor-decisions.jsonl` | narrator | Plain-English scaling decision audit log |
| `heartbeats/{session}.json` | NEEDLE workers | Per-worker status (idle/executing), refreshed every 30s |

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

5. **Promotion end date:** When a promotion period ends, the governor must correctly revert to 1x flat model. Test the `promotions.json` cutoff logic explicitly. The March 2026 Off-Peak 2x promotion is a historical example that ended on 2026-03-28; after that date `promotions.json` was reverted to an empty array `[]`. That is the current shipped state — `config/promotions.json` in this repository contains `[]` — and it is the correct default whenever no promotion is running (flat 1.0 multiplier). An expired promotion left in the file is a configuration error the `promotion_dates` doctor check flags.

6. **Multiple accounts / credential rotation:** The poller assumes a single `~/.claude/.credentials.json`. If multiple accounts are used, parameterize the credentials path.

7. **Token collector lag:** The collector reads JSONL files that may be flushed with a short delay after each API call. For burn rate samples, use intervals ≥ 2 minutes to ensure all requests in the window have been written. The `pct_delta` annotation from the API snapshot is the authoritative signal; token deltas enrich it.

8. **Pricing staleness and model generation confusion:** The `pricing` block in `governor.yaml` must be manually updated when Anthropic changes API rates. Critically, Opus 4.6/4.5 pricing ($5/$25) is dramatically different from Opus 4.1/4 ($15/$75) — a stale config using the wrong generation will produce 3× dollar-equivalent errors. Log the pricing source URL and snapshot date in `governor.yaml`. When a new model version is detected in token records but not found in the pricing config, log a warning and fall back to the nearest known model's pricing rather than silently using wrong rates.

9. **Single vs. two-tier cache write fallback:** Not all API responses include the `cache_creation` sub-object (it may be absent in older response formats or certain service tiers). When absent, attribute all `cache_creation_input_tokens` to the 5-minute tier (1.25× rate) as the conservative fallback — this slightly underestimates cost rather than overestimating.

10. **Model attribution in multi-model sessions:** A single Claude Code session can make calls to multiple models (e.g., a tool-call routing to Haiku while the main conversation uses Sonnet). Token records must be attributed per-model from the `model` field in each response, not inferred from the session path alone.

---

## ADR-001: 2026-07-20 — Split the governor daemon into an always-on Observe loop and a separately-gated Act loop

**Status:** Implemented

### Context

This ADR was written during a fleet-wide artifact-improvement pass, prompted by inspecting the actual live deployment on the `ex44` host rather than just the plan.

Live findings (2026-07-20):

- `claude-token-collector.service` is `enabled`/`active`, has been running continuously, and is still appending `i`/`f` records to `token-history.jsonl` (visible in `journalctl --user -u claude-token-collector`).
- `claude-governor.service` is `disabled`/`inactive`. `~/.config/claude-governor/governor-state.json` has not been updated since `2026-06-28T19:51:09Z` — **21+ days stale** as of this writing.
- `cgov doctor` on the live host reports 4 hard failures directly caused by this: `daemon_running` (stopped), `state_freshness` (stale), `burn_rate_samples` (insufficient — no fresh EMA data), `prediction_accuracy` (10 stale scored predictions, 14% median error).
- `governor.yaml`'s `alerts.auto_bead` is `false`, with the comment `"Disabled: alert predicates have 100% FP rate (docs-878a) ... Re-enable only after FP rate < 5% over 100-alert window."` `docs-878a` (closed) and `notes/bf-h7toj.md` show the false-positive rate was later driven to 0% across 9 samples by an `is_cutoff_alert_consistent()` guard — but `auto_bead` was never turned back on, and the FP telemetry counter (`alert_fp_telemetry.total_recorded`) is stuck at effectively zero precisely because the daemon that would grow it towards the 100-sample re-enablement threshold is the same daemon that got shut off.
- Git history (`docs/notes/bf-48qtz.md`, 2026-07-09) shows this is not a one-time incident: the daemon has previously been confirmed running, then gone stopped again, at least once before in the last two weeks, with no record of *why* it was stopped either time.

The root cause is architectural, not incidental: `src/governor.rs`'s main loop (`cgov _daemon`) bundles three things that have very different risk profiles into one process behind one on/off switch:

1. **Observe** — poll `/api/oauth/usage`, read the token collector's output, compute per-window EMA burn rates, capacity forecasts, the confidence cone (Component 21), calibrator scoring (Component 12), and safe-mode evaluation (Component 20). This is read-only with respect to the outside world — it only ever writes `governor-state.json`.
2. **Act (scale)** — launch/stop NEEDLE workers via `launch_cmd` / graceful SIGINT (Component 6).
3. **Act (alert)** — shell out to create HUMAN-type beads (Component 8).

Because these three are one process, the only way to stop *acting* on distrusted predictions (which is exactly what happened around `docs-878a`, and is presumably why `auto_bead: false` was set) is to kill the *whole* daemon — which also stops the harmless, valuable Observe half. That, in turn, starves the calibrator of the very data (`prediction-accuracy.jsonl` samples, `alert_fp_telemetry` samples) needed to decide when it is safe to re-enable Act. It is a self-perpetuating blackout: nothing is currently accumulating the evidence that would justify turning the governor back on. The token collector, which the plan already correctly identifies as "an independent daemon... can be started, stopped, and queried independently" (Component 2), is the existing proof that this separation is both desirable and already half-built in practice — it just needs to be extended to the forecasting/calibration logic that currently lives only inside the coupled daemon.

### Decision

Split `cgov _daemon` into two independently-supervised processes:

- **`cgov _observe`** (new hidden subcommand, replaces the observation portion of `_daemon`): runs the poll → burn-rate EMA → capacity-forecast → confidence-cone → calibrator → safe-mode pipeline every `loop_interval` seconds and writes `governor-state.json` / `governor-state.prev.json` / `prediction-accuracy.jsonl`. It **never** shells out to `launch_cmd`, never sends SIGINT/kill to a worker session, and never executes the alert command. It is installed as its own systemd unit (`claude-governor-observe.service`) with `Restart=always`; `cgov enable` enables and starts it alongside the act and collector units.
- **`cgov _act`** (new hidden subcommand, replaces the scaling+alerting portion of `_daemon`): reads the state that `_observe` last wrote, and *only* performs `apply_scaling()` (Component 6/7/9/13/14/17) and `check_alerts()` / `fire_alert()` (Component 8). It is installed as a separate systemd unit (`claude-governor-act.service`) that an operator can `cgov stop act` / `cgov disable act` independently, without losing forecasting, calibration, or `cgov status` freshness.

`governor-state.json` gains an explicit ownership split so the two processes don't race on the same file: `_observe` owns `usage`, `capacity_forecast`, `burn_rate`, `schedule`, `calibration`, and `safe_mode`; `_act` owns `workers.*.target`, `alert_cooldown`, `open_alert_beads`, `alert_fp_telemetry`, and appends to `alerts`. `_act` reads the file, computes its decision, and writes back only its own subtree; both halves serialize their merge transaction so a concurrent write cannot revert the other half.

`cgov status`, `cgov doctor`, `cgov explain`, and `cgov simulate` are unaffected — they already read state from disk and don't care which process wrote it. `cgov enable` installs and starts both units; `cgov disable`/`cgov stop` gain a target argument (`observe`, `act`, or `all`, default `all`) so `cgov stop act` becomes the documented, single command for "pause automated scaling and alerting without losing telemetry," replacing the undocumented `systemctl --user disable claude-governor.service` that appears to be what actually happened here.

`doctor` also gains a distinction it currently lacks: today `daemon_running: stopped` reads as an unqualified failure. Post-split, `doctor` checks `_observe` and `_act` independently, and a deliberately-stopped `_act` (an intentional, tracked posture) is a `warn`, not a `fail` — while a stopped `_observe` remains a hard `fail`, because there is no valid reason for the read-only half to ever be off.

### Alternatives Considered

1. **Do nothing; just document "remember to `cgov restart` after disabling."** Rejected — this is what the previous incident (`bf-48qtz`, then regression by 2026-06-28) already tried implicitly, and it silently regressed within three weeks. Documentation doesn't fix a coupling.
2. **Add a `--dry-run` / `--observe-only` flag to the single `_daemon` process instead of splitting into two processes.** Simpler (one binary invocation, one unit file), and would fix the "forecast keeps flowing while paused" problem. Rejected as the primary fix because it's still one process / one systemd unit / one failure domain: a crash, an unhandled panic in the scaling code path, or a future risky feature added to the same loop still takes down forecasting with it. A flag also doesn't give operators a way to stop *only* alerting while keeping scaling (or vice versa) without a third flag. Two independently-supervised units, mirroring the token collector's already-proven pattern, is more robust for the same implementation cost and generalizes better.
3. **Add a watchdog process that automatically restarts `claude-governor.service` if state goes stale.** Treats the symptom (daemon is down) rather than the cause (the only way to stop untrusted actions is to stop everything, so operators are incentivized to leave it down). A watchdog would have fought the operator's actual intent in this incident — repeatedly restarting a daemon whose *acting* half was correctly distrusted — rather than giving them a precise tool to keep observing while pausing acting.
4. **Keep one process, but gate `apply_scaling()`/`fire_alert()` behind the existing `auto_bead`-style config flags instead of a process split.** This is close to what already exists for alerts (`auto_bead: false`) but doesn't exist at all for scaling, and — critically — doesn't change *which systemd unit* the operator has to stop to get "no actions." An operator who wants to pause scaling still has only the single-daemon on/off switch available, so they still stop the whole thing. The config-flag approach and the process split are complementary, not mutually exclusive; this ADR chooses the process split because it's the one that actually removes the "stopping actions blinds you to state" failure mode.

### Consequences

**Positive:**
- `governor-state.json`, the confidence cone, and the calibrator's `prediction-accuracy.jsonl` keep accumulating fresh data even while scaling/alerting is paused — which is precisely the data the calibrator/FP-telemetry re-enablement criteria need in order for anyone to ever have evidence it's safe to turn `_act` back on. Breaks the self-perpetuating blackout described in Context.
- `cgov status`/`cgov doctor` stop going stale as a side effect of an operator's decision to distrust actions — the two concerns are now independently observable.
- Mirrors the token collector's already-successful independent-unit pattern instead of introducing a new one.
- `doctor`'s `daemon_running` check becomes more precise (distinguishes "observe is down" — always bad — from "act is intentionally paused" — a normal, trackable posture).

**Negative / costs:**
- Two systemd units to install/manage instead of one (`cgov init`/`cgov enable`/`cgov disable` all need updating; `install.sh` and `config/*.service` need a third unit file).
- `governor-state.json` needs a documented ownership split (above) to avoid a torn-write race between two processes; this is a small but real increase in state-management complexity versus the current single-writer model.
- Slightly higher baseline resource usage (a second always-on process, though `_observe`'s workload — HTTP poll + arithmetic — is lighter than `_act`'s tmux/process-exec work).

### Implementation

The split is implemented in `src/governor.rs`: `run_observe_cycle` owns polling,
collection, forecasting, calibration, and observe-owned state; `run_act_cycle`
owns worker lifecycle and alert episodes. The hidden `_observe` and `_act`
commands run those halves as independently supervised loops. `cgov init` installs
`claude-governor-observe.service`, `claude-governor-act.service`, and the token
collector; `cgov enable` enables and starts them. Lifecycle commands accept
`observe`, `act`, `collector`, `governor`, or `all` targets, so `cgov stop act`
pauses scaling and alerting while observation continues.

State subtree merges are serialized with a scoped lock and use process-specific
temporary files for atomic writes, so observe and act cannot overwrite each
other's owned fields or splice partial JSON. `cgov doctor` reports
`observe_running` as the required telemetry loop and `act_running` as a warning
when acting is intentionally paused.

---

## ADR-002: 2026-07-22 — Missing burn-rate data must hold at current worker count, never fall back to max_workers

**Status:** Implemented

### Context

Live incident on the `lab` host (2026-07-22): the operator (jose) noticed `claude-governor.service` had scaled the `polish-opus` agent pool (Anthropic subscription billing, `claude-print-opus`) from 0 to 4 concurrently-running Opus workers, and was actively trying to scale further toward a `target workers: 8` — while the account was already at ~51% of its weekly usage limit with 5 days remaining until reset. This was caught and stopped (all 4 workers killed, `claude-governor.service` stopped) before it escalated further, but it was a real, live over-provisioning of the most expensive model tier against a constrained weekly budget, not a theoretical risk.

Root-caused to two compounding bugs:

1. **The decision bug.** `governor.rs::safe_worker_count_or_max()` — the function `compute_target_workers()` calls whenever the binding window's `safe_worker_count` is `None` (no usable burn-rate EMA data) — returned `max_workers` in that case, by explicit original design: the doc comment read *"no burn rate data yet; use the configured ceiling so the fleet stays at full capacity rather than freezing at current_total"*. On a fresh daemon restart, `current_total` is `0` and there are zero EMA samples (`insufficient burn rate data, using max_workers as ceiling` was logged every single cycle), so the very first thing a restarted governor did — every time, unconditionally — was launch workers up to `max_workers` with no idea what fraction of quota was actually left. This is the opposite of what an "automated capacity *governor* for subscription usage" should do when it cannot see the meter.
2. **The data bug that made (1) fire continuously instead of only briefly.** `claude-token-collector.service` had been failing every collection pass for an extended period (`collection pass failed: Failed to load cursors: JSON error: expected ',' or '}' at line 9576 column 105`) — `~/.needle/state/collector-cursors.json` was corrupted. Root cause: `CursorStore::save()` wrote to a *shared* fixed temp filename (`<path>.tmp`) with no per-writer uniqueness or locking; when multiple `cgov`/collector processes saved concurrently (which repeated `claude-governor.service` restarts that same day produced), two writers could each open/truncate/write the same temp path independently, splicing their output together at the byte level before either atomic rename ran. `CursorStore::load()` then hard-errored the entire collection pass on any parse failure, so the corruption was permanent until manually fixed — there was no self-healing path, and no scaling decision ever again had real data to work with once this happened, hence why the `None → max_workers` bug wasn't a one-cycle blip but a standing, repeated over-provision every 5-minute cycle for hours.

### Decision

Three changes, all in this same pass:

1. **`safe_worker_count_or_max` → `safe_worker_count_or_hold`** (`src/governor.rs`): the `None` case now returns `current_total` instead of `max_workers`, and logs accordingly. Concretely: a fresh restart with `current_total=0` and no samples now computes `target=0` and takes **no scaling action** — matching the operator's explicit instruction that in the absence of tracked usage, the governor should do nothing. `max_workers` is only ever reached by genuine data-driven scale-up, one hysteresis-limited step per cycle, once real EMA samples confirm it's affordable. The pre-existing, independent emergency brake (any window's `current_utilization >= 98%` from the last polled snapshot, checked *before* this fallback runs) is untouched and still fires regardless of burn-rate data availability — a real cutoff is still caught even while the fleet is held for lack of forecast data.
2. **`CursorStore::save()` uses a per-process temp filename** (`<path>.tmp.<pid>` instead of `<path>.tmp`): `fs::rename` is atomic at the filesystem level, so as long as no two writers ever share a temp path, the final file is always a complete write from exactly one writer — whichever rename lands last simply wins outright, never a byte-level splice. This removes the actual corruption mechanism, not just its symptom.
3. **`CursorStore::load()` degrades gracefully on corrupt JSON** instead of returning `Err` and aborting the whole collection pass: it now logs a warning and returns an empty `CursorStore`, so a corrupted file (from this bug, a disk issue, or anything else) costs at most one pass of re-reading already-seen JSONL bytes — never a multi-hour (or multi-day) blackout of usage tracking. Cursors are read-position bookkeeping, not the usage data itself, so this is a safe boundary to be defensive at.

Verified live on `lab` post-deploy: the collector recovered on its first pass with the new binary (`collector recovered — last record 0s old, clearing offline alert cooldown`), and the governor's first post-restart cycle logged `insufficient burn rate data, holding at current worker count (0) — no scaling action` / `target workers: 0` / `decision: NoChange` — zero workers launched, as intended.

### Alternatives Considered

1. **Keep `None → max_workers`, but gate it on `current_total > 0`** (i.e. only "stay at full capacity" if some workers were already running, don't launch fresh ones from zero). Rejected: still guesses a number (`max_workers`) with no data behind it whenever the fleet happens to already be nonzero when data drops out mid-run, which is exactly the scenario a data-availability blip during active operation would hit. `current_total` (hold, no guess in either direction) dominates this for the "no data" case specifically.
2. **`None → 0`** (treat missing data as maximally unsafe, scale everything to zero immediately). Rejected as the primary behavior because it's not what "do nothing" means — it's an active kill of in-flight, already-supervised work over a transient collector hiccup (a single missed cycle, a brief restart window), which is disruptive and wastes partial task progress for a problem that resolves itself within one interval most of the time. The existing emergency brake already owns the "actively force to zero" response, gated on a real signal (>=98% utilization) rather than mere data absence. `current_total` reuses the parameter that was already present-but-unused in the function signature for exactly this purpose.
3. **Implement the plan's originally-specified three-tier staleness fallback** (< 10min normal, 10-30min stale-EMA + warn, > 30min baseline-rate + alert bead) verbatim. Rejected for this pass as larger in scope than the incident required and only partially specified (`baseline_burn_rate` exists as a struct in `burn_rate.rs` but was never wired into the decision path, and `alerts.auto_bead` is currently disabled repo-wide for an unrelated reason — see ADR-001 context). `current_total`-hold is a strict subset/simplification: it's the same "don't guess" spirit without introducing a new baseline-rate code path that would itself need its own verification. Left as documented follow-up if finer-grained staleness tiers are wanted later.

### Consequences

**Positive:**
- The specific incident (0→4→trending-to-8 concurrent Opus workers with a corrupted usage-data pipeline) cannot recur via this code path: a restart with no data now launches nothing.
- The corruption vector (concurrent writers on a shared temp filename) is fixed at the root, not papered over.
- A future corruption (from any cause) self-heals within one collection pass instead of requiring someone to notice `journalctl` errors and manually repair a JSON file, as happened here.

**Negative / costs:**
- On a genuinely fresh install or a long-stale restart, the fleet now stays at 0 workers until the collector accumulates enough fresh samples to produce a `safe_worker_count`, rather than immediately running at full capacity. This trades a startup-latency cost (bounded by the collector's poll interval, currently 120s, times however many samples the EMA needs) for never running blind. Given the governor's stated purpose is subscription-usage safety, this tradeoff is intentional and correct.
- `CursorStore::load()` silently discarding a corrupt file means whatever incremental read-position progress was in that file is lost on the one pass where corruption is detected — the next pass re-reads already-seen JSONL bytes once. Cheap in practice (bounded by file sizes under `~/.claude/projects`), but not literally free.

### Follow-up

- Consider re-introducing the plan's staleness-tiered fallback (Alternative 3) as a deliberate enhancement — `current_total`-hold is safe but coarse; a 10-30min "trust the last known EMA" tier would reduce startup latency for short-lived collector blips without reintroducing the max_workers guess. Not filed as a bead yet; revisit if hold-at-current proves too conservative in practice.
- The `polish-opus` agent's `max_workers: 4` in `governor.yaml` predates this incident and was not itself changed by this ADR — it remains the ceiling that data-driven scale-up can reach once real samples justify it. Whether 4 is still the right ceiling given current weekly-usage patterns is a separate, operator-facing config question, not a code-correctness one.
