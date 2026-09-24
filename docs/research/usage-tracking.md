# Claude Code Usage Tracking — Research

## 1. `claude status` CLI Command

The `claude status` command is **authentication status only**, not usage:

```bash
claude status [--json | --text]
```

JSON output:
```json
{
  "loggedIn": true,
  "authMethod": "claude.ai",
  "apiProvider": "firstParty",
  "email": "...",
  "subscriptionType": "max",
  "rateLimitTier": "default_claude_max_20x"
}
```

There is **no** `claude status` command that outputs usage percentages. That lives only in the interactive TUI at `/status` → Usage tab.

---

## 2. The Primary Usage API: `/api/oauth/usage`

This is the **canonical programmatic source** for session and weekly usage data.

### Endpoint

```
GET https://api.anthropic.com/api/oauth/usage
```

### Required Headers

```http
Authorization: Bearer <accessToken>
anthropic-beta: oauth-2025-04-20
User-Agent: claude-code/2.1.78
```

The `anthropic-beta: oauth-2025-04-20` header is mandatory — requests without it return a 401 authentication error even with a valid OAuth token.

### Response Structure

```json
{
  "five_hour": {
    "utilization": 14.0,
    "resets_at": "2026-03-18T13:59:59.918852+00:00"
  },
  "seven_day": {
    "utilization": 82.0,
    "resets_at": "2026-03-20T03:00:00.918880+00:00"
  },
  "seven_day_oauth_apps": null,
  "seven_day_opus": null,
  "seven_day_sonnet": {
    "utilization": 72.0,
    "resets_at": "2026-03-20T03:59:59.918891+00:00"
  },
  "seven_day_cowork": null,
  "extra_usage": {
    "is_enabled": false,
    "monthly_limit": null,
    "used_credits": null,
    "utilization": null
  }
}
```

### Field Mapping to `/status` UI Labels

| API Field | UI Label |
|---|---|
| `five_hour` | "Current session" |
| `seven_day` | "Current week (all models)" |
| `seven_day_sonnet` | "Current week (Sonnet only)" |
| `seven_day_opus` | "Opus limit" |
| `extra_usage` | "Extra usage" |

### Parsing

- `utilization`: float, 0–100, representing percentage used
- `resets_at`: ISO 8601 datetime string with timezone offset
- Fields are `null` when not applicable for the current plan

**Note:** The endpoint is self-rate-limited. Calling it too frequently returns:
```json
{"error": {"type": "rate_limit_error", "message": "Rate limited. Please try again later."}}
```

---

## 3. The `/claude-status` Skill (Screen-Scraping Approach)

Located at `~/.claude/skills/claude-status/scripts/claude-status.sh`.

### How it Works

1. Creates a detached tmux session
2. Launches `claude` interactively inside it
3. Navigates to `/status` → Right → Right (Usage tab)
4. Captures pane output, strips ANSI codes, greps for usage lines
5. Kills the session on exit

**This is a screen-scraping approach** — fragile, version-dependent, takes ~10 seconds.

---

## 4. Rate Limit Headers in API Responses

These headers appear when rate limits are being approached or exceeded:

| Header | Description |
|---|---|
| `anthropic-ratelimit-unified-status` | `allowed`, `allowed_warning`, `rejected` |
| `anthropic-ratelimit-unified-reset` | Unix timestamp when limit resets |
| `anthropic-ratelimit-unified-{type}-utilization` | Float 0–1 for a specific limit type |
| `anthropic-ratelimit-unified-{type}-reset` | Unix timestamp for that limit type |
| `anthropic-ratelimit-unified-representative-claim` | Rate limit type hit (e.g., `five_hour`, `seven_day`) |

`{type}` is one of: `five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`, `overage`.

Warning thresholds in Claude Code binary:
- `five_hour`: warn at 90% utilization
- `seven_day`: warn at 75%, 50%, 25% utilization

---

## 5. Credentials and Token Refresh

### `~/.claude/.credentials.json`

The primary auth state file:

```json
{
  "claudeAiOauth": {
    "accessToken": "sk-ant-oat01-...",
    "refreshToken": "sk-ant-ort01-...",
    "expiresAt": 1773844535295,
    "subscriptionType": "max",
    "rateLimitTier": "default_claude_max_20x"
  }
}
```

- `subscriptionType`: `"free"`, `"pro"`, `"max"`, `"team"`, `"enterprise"`
- `rateLimitTier`: `default_claude_max_20x` = 20x base limits
- `expiresAt`: milliseconds since epoch; tokens expire after ~2 hours

**Token refresh** (POST when `Date.now() + 300000 >= expiresAt`):
```bash
curl -s -X POST \
    -H "Content-Type: application/json" \
    -d '{"grant_type":"refresh_token","refresh_token":"<token>","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","scope":"user:profile user:inference user:mcp_servers user:sessions:claude_code"}' \
    "https://platform.claude.com/v1/oauth/token"
```

---

## 6. Calculating Hours Until Reset

```python
import datetime, json

now = datetime.datetime.now(datetime.timezone.utc)
for key in ['five_hour', 'seven_day', 'seven_day_sonnet']:
    item = response.get(key)
    if not item:
        continue
    resets_at = datetime.datetime.fromisoformat(item['resets_at'])
    hours = (resets_at - now).total_seconds() / 3600
    print(f"{key}: {item['utilization']:.0f}% used, resets in {hours:.1f}h")
```

---

## 7. Local State Files

| File | Contents | Useful For |
|---|---|---|
| `~/.claude/.credentials.json` | OAuth tokens, plan type, tier | API auth |
| `~/.claude/settings.json` | User config, model, hooks | Configuration |
| `~/.claude/sessions/<pid>.json` | PID-to-session mapping | Active session detection |
| `~/.claude/projects/**/*.jsonl` | Per-request token usage (raw) | Historical token data |
| `~/.ccdash/tokens.db` | SQLite aggregate token counts | Local token totals |
| `~/.ccdash/sessions/<uuid>.json` | Hook-populated session status | Worker state |

---

## 8. Complete Working Script for Programmatic Usage Polling

```bash
#!/usr/bin/env bash
# Direct API polling — most reliable approach

ACCESS_TOKEN=$(python3 -c "
import json, os
with open(os.path.expanduser('~/.claude/.credentials.json')) as f:
    d = json.load(f)
    print(d['claudeAiOauth']['accessToken'])
")

curl -s \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "anthropic-beta: oauth-2025-04-20" \
    -H "User-Agent: claude-code/2.1.78" \
    "https://api.anthropic.com/api/oauth/usage" | python3 -c "
import json, sys, datetime

data = json.load(sys.stdin)
if 'error' in data:
    print('Error:', data['error']['message'])
    sys.exit(1)

now = datetime.datetime.now(datetime.timezone.utc)
labels = {
    'five_hour': 'Current session (5h)',
    'seven_day': 'Current week (all models)',
    'seven_day_sonnet': 'Current week (Sonnet)',
    'seven_day_opus': 'Current week (Opus)',
}
for key, label in labels.items():
    item = data.get(key)
    if not item:
        continue
    util = item['utilization']
    resets_at = datetime.datetime.fromisoformat(item['resets_at'])
    hours = (resets_at - now).total_seconds() / 3600
    print(f'{label}: {util:.0f}% used, resets in {hours:.1f}h')
"
```

---

## 9. Summary of Approaches Ranked by Reliability

| Approach | Reliability | Latency | Notes |
|---|---|---|---|
| `GET /api/oauth/usage` (direct API) | **High** | ~200ms | Requires valid OAuth token; self-rate-limits |
| Rate limit headers on API responses | Medium | inline | Only present when near/over limits |
| `~/.ccdash/tokens.db` | Medium | instant | Local aggregate; no weekly %; good for token counts |
| `~/.claude/projects/**/*.jsonl` | Medium | varies | Raw token data per request; no plan %s |
| `/claude-status` skill (tmux scraping) | Low | ~10s | Fragile, requires tmux, version-dependent |
| `console.anthropic.com` web scraping | Very Low | varies | Cloudflare-protected; requires browser session |

**The `/api/oauth/usage` endpoint is the only direct programmatic source** for subscription-level usage percentages.

---

## 10. cgov Poller Contract — Retries, Rate Limits, and Safe Fallback

How cgov's poller (`src/poller.rs`) consumes the endpoint. This is the
client-side behavior pinned by `tests/usage_polling_contract.rs`.

### Authentication flow (per poll)

1. Read the credentials file (§5). A missing file raises `CredentialsNotFound`;
   unparseable JSON raises `InvalidCredentials`; a file carrying an empty
   `accessToken`, an empty `refreshToken`, or a zero `expiresAt` is rejected as
   corrupted **before any network call**.
2. If `now + 300s >= expiresAt` (the 5-minute refresh threshold), POST the
   refresh endpoint (§5) and persist the rotated credentials; the usage call
   then carries the new bearer.
3. `GET /api/oauth/usage` with the §2 headers.

### Retries

| Request | Retry policy |
|---|---|
| `GET /api/oauth/usage` | **Never retried client-side.** The endpoint self-rate-limits (§2), so hammering it from a retry loop only extends the lockout. Any non-200 (`ApiError`), transport failure (`ApiRequestFailed`), or unparseable body (`ParseError`) surfaces to the caller after exactly one request. |
| `POST /v1/oauth/token` (refresh) | **Exactly one retry**, 5s after the first attempt fails. Both attempts failing increments a process-global consecutive-failure counter. |

The failure counter resets to zero on any successful refresh. When it reaches
3 consecutive failed refresh cycles (`MAX_REFRESH_FAILURES`), `attempt_refresh`
stops retrying and returns `MaxRefreshFailures`, and
`Poller::should_alert()` turns true — the observe path then prints
`WARNING: OAuth token refresh failing - run: claude login` (the HUMAN
escalation surface).

### Safe fallback

- **Auth-path failures degrade to stale data.** If refreshing or reading the
  credentials fails and the poller already holds a successful reading, it
  returns that reading with `stale: true` and its **original** reading
  timestamp preserved — consumers can see its age, and the governor cycle
  keeps running on old data instead of failing.
- **No cached reading → the error propagates.** A poll that fails before any
  successful reading exists (cold start during an outage) fails; there is
  nothing safe to fall back to.
- **Usage-endpoint failures do not fall back.** A failure from
  `GET /api/oauth/usage` itself — 429 rate limit, 401, malformed body,
  transport error — propagates to the caller **even when a cached reading
  exists**. Only the auth path degrades to stale data; a stale reading is
  never manufactured to paper over a fetch failure.
