# NEEDLE Capacity Governor — Architecture Research

## 1. Existing `capacity-governor.sh` — Full Algorithm Analysis

**File:** `/home/coding/NEEDLE/scripts/capacity-governor.sh`

### Purpose
A standalone Bash script that monitors Claude Code subscription quota and adjusts the number of active `claude-anthropic-sonnet` workers to pace consumption toward the weekly reset without hitting the limit or wasting unused capacity.

### Constants and Configuration

| Constant | Value | Meaning |
|---|---|---|
| `LOOP_INTERVAL` | 900s | 15-minute polling cycle |
| `SONNET_MIN` | 1 | Always keep at least one sonnet worker alive |
| `SONNET_MAX` | 5 | Hard ceiling; matches config.yaml `limits.models.claude-anthropic-sonnet.max_concurrent` |
| `SONNET_AGENT` | `claude-anthropic-sonnet` | Agent name string used to match sessions/workers |
| `PROMO_START/END` | 2026-03-13 to 2026-03-27 | Anthropic 2x off-peak promotion window |
| `PEAK_START_HOUR` | 8 (8 AM ET) | Start of peak billing window |
| `PEAK_END_HOUR` | 14 (2 PM ET) | End of peak billing window |
| `GOVERNOR_STATE` | `~/.needle/state/capacity-governor.json` | JSON state persistence |
| `STATUS_SCRIPT` | `~/.claude/skills/claude-status/scripts/claude-status.sh` | Usage data source |

### Pacing Algorithm — Linear Rate Model

The governor uses a **linear pacing model** in `compute_target_workers()`:

1. Fetch usage → parse `sonnet_pct` (weekly Sonnet budget consumed %) and `reset_date`
2. Compute remaining: `remaining = 100 - sonnet_pct`
3. Hours until reset: parse reset date from stored state, fall back to 48h if unknown
4. Promo-aware effective hours: `effective_hours = peak_hours * 1.0 + offpeak_hours * 2.0`
5. Target rate: `target_rate_per_hour = remaining / hours_remaining`
6. Rate per worker: empirical constant **1.2% per hour per sonnet worker**
7. Target workers: `floor(target_rate / rate_per_worker)`, clamped to [SONNET_MIN, SONNET_MAX]

**Known bug:** `effective_hours` is computed but `target_rate_per_hour` uses `hours_remaining` (raw), not `effective_hours`. Off-peak math is partially implemented.

### Scale-Up Logic
```bash
needle run --workspace=/home/coding/kalshi-trading \
    --agent="claude-anthropic-sonnet" --force
```
- Hard-coded workspace (not auto-discovered)
- `--force` bypasses NEEDLE's built-in concurrency limit checks
- Launches one worker per iteration per additional worker needed

### Scale-Down Logic
```bash
tmux list-sessions | grep "needle-claude-anthropic-sonnet" | sort -t: -k1 -r | head -N
tmux kill-session -t "$session"
```
- Kills most-recently-named sessions first (reverse NATO alphabetical order)
- **No graceful drain** — kills mid-task

### Worker Counting
```bash
needle list 2>/dev/null | grep -c "claude-anthropic-sonnet"
```

### State Persistence
Written to `~/.needle/state/capacity-governor.json` after every cycle:
```json
{
    "timestamp": "2026-03-18T12:26:31Z",
    "sonnet_pct": 72,
    "all_models_pct": 81,
    "reset_date": "Mar 20",
    "target_workers": 1,
    "current_workers": 0,
    "multiplier": "1.0",
    "is_peak": true
}
```

### CLI Modes

| Flag | Behavior |
|---|---|
| _(none)_ | Run once: fetch → compute → scale |
| `--loop` | Run every 900s forever |
| `--dry-run` | Print what would change, do nothing |
| `--status` | Print current state and exit |
| `--interval N` | Override loop interval |

---

## 2. Worker Tracking Mechanisms

NEEDLE uses **three parallel tracking systems**:

### A. tmux Sessions (Primary — ground truth)
Pattern: `needle-{runner}-{provider}-{model}-{identifier}`

```bash
# List all needle sessions:
tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^needle-'

# Count sonnet workers:
tmux list-sessions -F '#{session_name}' 2>/dev/null | grep -c '^needle-claude-anthropic-sonnet-'
```

### B. Worker Registry JSON (Secondary — concurrency enforcement)
**File:** `~/.needle/state/workers.json`

Atomic flock-based JSON registry:
```json
{
  "workers": [{
    "session": "needle-claude-anthropic-sonnet-alpha",
    "pid": 12345,
    "workspace": "/home/coding/kalshi-trading",
    "started": "2026-03-18T08:26:32Z"
  }]
}
```

### C. Heartbeat Files (Tertiary — health/status)
**Directory:** `~/.needle/state/heartbeats/{session_name}.json`

Contains: `status` (idle/executing/draining/starting), `current_bead`, `last_heartbeat`, `pid`, `workspace`.

---

## 3. Worker Launch Architecture

### Invocation Chain
```
needle run --agent=claude-anthropic-sonnet --workspace=...
  → validate args, check concurrency limits (skipped with --force)
  → tmux new-session -d -s "needle-claude-anthropic-sonnet-alpha" "needle run ... --_tmux"
    → _needle_worker_loop()    # runs inside tmux
      → while true:
          find work via strand engine
          execute bead via agent
          sleep $polling_interval (2s default)
```

### Key Parameters
```bash
needle run \
    --workspace=<path>     # Which .beads/ directory to pull from
    --agent=<name>         # Agent YAML config name (runner-provider-model)
    --id=<identifier>      # NATO alphabet ID (alpha, bravo...)
    --count=N              # Spawn N workers
    --force                # Bypass concurrency limits
    --no-tmux              # Debug: run in foreground
```

---

## 4. All Configured Agent Types

| Agent Name | Runner | Provider | Model | Max Concurrent | Cost Type |
|---|---|---|---|---|---|
| `claude-anthropic-sonnet` | claude | anthropic | sonnet | 5 | **subscription quota** |
| `claude-anthropic-opus` | claude | anthropic | opus | 2 | **subscription quota** |
| `claude-code-glm-5` | claude | zai | glm-5 | 10 | pay-per-token |
| `claude-code-glm-5-turbo` | claude | zai | glm-5-turbo | 10 | pay-per-token |
| `codex-openai-gpt4` | codex | openai | gpt4 | 3 | pay-per-token |
| `aider-ollama-deepseek` | aider | ollama | deepseek | 2 | local (free) |
| `opencode-ollama-deepseek` | opencode | ollama | deepseek | 1 | local (free) |

### Per-Provider Concurrency Limits (config.yaml)
```yaml
limits:
  global_max_concurrent: 45
  providers:
    anthropic:
      max_concurrent: 5
      requests_per_minute: 60
```

---

## 5. NEEDLE Built-in Scaling (Queue-Depth Based)

The NEEDLE config has a `scaling` block distinct from the quota-pacing governor:
```yaml
scaling:
  spawn_threshold: 3         # Spawn a new worker when open bead count exceeds N
  max_workers_per_agent: 10
  cooldown_seconds: 30       # Min seconds between consecutive spawn attempts
```

This is a **queue-depth autoscaler** — fire more workers when the backlog exceeds a threshold. It is complementary to, not a replacement for, quota-pacing.

---

## 6. Graceful Shutdown Mechanism

NEEDLE workers handle SIGTERM/INT/HUP via `_needle_loop_setup_signals()`:
- Sets `_NEEDLE_LOOP_SHUTDOWN=true`
- Worker enters `draining` state
- Finishes current bead, then exits cleanly

The current `capacity-governor.sh` uses `tmux kill-session` (bypasses this). Better approach:
```bash
# Graceful: send Ctrl-C into the session
tmux send-keys -t "$session" "C-c" 2>/dev/null

# Check if human is watching before killing:
attached=$(tmux display-message -t "$session" -p '#{session_attached}')
[[ "$attached" -gt 0 ]] && echo "Skipping — session has human attached"
```

---

## 7. State Files Summary

| File | Purpose | Format |
|---|---|---|
| `~/.needle/state/capacity-governor.json` | Governor last-run state | JSON |
| `~/.needle/state/workers.json` | Active worker registry | JSON |
| `~/.needle/state/workers.json.lock` | flock mutex for workers.json | empty |
| `~/.needle/state/heartbeats/{session}.json` | Per-worker liveness heartbeat | JSON |
| `~/.needle/state/rate_limits/{provider}.json` | Sliding-window request counter | JSON |
| `~/.local/share/claude-governor/governor.log` | Governor execution log | line-oriented |

---

## 8. Known Design Gaps in Existing Governor

### Gap 1: No Hysteresis
Each cycle recomputes target from scratch. Workers frequently exit between 15-minute cycles due to `idle_timeout`, causing repeated relaunch churn. Observed pattern:
```
08:26 Scaling UP sonnet: 0 → 1
08:41 Scaling UP sonnet: 0 → 1  (same worker exited, relaunched)
08:56 Scaling UP sonnet: 0 → 1
```

### Gap 2: Forceful Scale-Down
`tmux kill-session` ignores in-flight tasks. Beads are left in `IN_PROGRESS` until the stale claim threshold (3600s) triggers their release. Only idle workers should be killed.

### Gap 3: Hardcoded Burn Rate
The `1.2% per worker per hour` constant is never validated against actual observed burn. A governor that measures `delta_pct / delta_time` per active worker would auto-calibrate.

### Gap 4: Hard-Coded Workspace
Always launches into `/home/coding/kalshi-trading`. Multi-project workspaces are not considered.

### Gap 5: Off-Peak Logic Incomplete
`effective_hours` is computed but the rate formula uses raw `hours_remaining`. Workers are not scaled up during off-peak to take advantage of the 2x window.

### Gap 6: No Alerting
Near-capacity conditions generate no notification. The `_needle_alert_crash_loop()` pattern (create a HUMAN-type bead) is the right mechanism to reuse.

### Gap 7: Fragile Status Source
`claude-status.sh` is a TUI screen-scraper that takes ~10s. The direct API call to `/api/oauth/usage` is more reliable, faster (~200ms), and not dependent on TUI layout.

---

## 9. Governor Signal Inputs by Worker Type

Different worker types require different governor inputs:

| Worker Type | Primary Signal | Scale-Down Trigger | Scale-Up Trigger |
|---|---|---|---|
| `claude-anthropic-sonnet` | Weekly quota % remaining | Burn rate too high | Quota + time remaining |
| `claude-anthropic-opus` | Weekly quota % remaining (heavier weight) | Same, lower target | Quota + time remaining |
| `claude-code-glm-5-turbo` | Daily spend USD | Daily budget threshold | Open beads > N |
| `aider-ollama-deepseek` | CPU/GPU utilization | Load > threshold | Queue depth > N |

The quota-pacing model only applies to Anthropic subscription-based agents. A general governor needs pluggable signal sources per provider.
