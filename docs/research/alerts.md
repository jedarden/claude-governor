# Claude Governor Alert System — Research

## Overview

The governor creates HUMAN-type beads via NEEDLE when specific conditions are detected. Alerts are deduplicated by *episode*: one bead per continuous stretch during which a condition holds, refreshed while it persists and auto-closed when it clears.

## Alert Types

### Critical Severity

#### `cutoff_imminent`

Any window has `cutoff_risk=1` **and** either:

1. **High utilization risk:** `hard_limit_margin_hrs < -2` **and** `utilization >= 95%`, OR
2. **Deep margin risk:** `hard_limit_margin_hrs < -24` **and** `utilization >= 90%`

- **Trigger:** Window at cutoff risk with tiered thresholds
- **Severity:** Critical
- **Message:** `Window {name} at cutoff risk: hard_limit_margin_hrs={:.1}h, utilization={:.1}%, hrs_left={:.1}h, remaining_to_100={:.1}%`
- **Action:** Immediate manual intervention — scale down workers immediately

**Why tiered thresholds:** A low-utilization window with small negative margin is a transient burn rate spike. However, a moderate/high-utilization window with deeply negative margin is a real crisis. The two-tier system catches both patterns while preventing false positives.

**Why hard_limit_margin_hrs:** Measures margin against the 100% platform hard limit (not the 90% target ceiling). This prevents false positives when utilization exceeds the target ceiling but is far from actual cutoff risk.

**Consistency guard:** Alerts are suppressed when `hard_limit_remaining_pct > 5%` because burn-rate extrapolation beyond that range is unreliable (observed 100% FP rate). The fleet is far enough from 100% that negative margins don't correspond to actual cutoffs.

**False-positive patterns discovered (Apr 16-23, 2026):**
- **Transient burn-rate spikes:** 60-65% utilization with -35 to -48h margin on seven_day — 10+ instances, all resolved without incident
- **Low-utilization FPs:** 1-33% utilization with deeply negative margins — 45+ instances, all false positives (consistency guard now suppresses these)
- **Stale EMA during rollover:** 50-86% utilization with moderate negative margins while hrs_left remained high — 200+ instances, all resolved without incident
- **At ceiling with workers halted:** 88-100% utilization with -8 to -23h margin but workers already at 0 — 25+ instances, all historical artifacts

#### `emergency_brake_activated`

Emergency brake was triggered (98%+ utilization detected).

- **Trigger:** `safe_mode.active=true` with trigger="emergency_brake"
- **Severity:** Critical (log-only — bead creation disabled as of 2026-04-23)
- **Message:** `Emergency brake active since {timestamp}`
- **Action:** Workers have been scaled to minimum; investigate why prediction failed

**Why log-only:** The 98% threshold is purely reactive and doesn't account for time remaining until reset. During a single high-utilization event, the brake re-fires every cooldown period (60 min) despite workers already being at 0. This produced 15+ false-positive beads in a single event (Apr 23, 2026). The governor log now records brake application; no human-actionable bead is needed.

#### `token_refresh_failing`

OAuth token refresh failing — governor is using stale cached usage data.

- **Trigger:** `token_refresh_failing=true` in state
- **Severity:** Critical
- **Message:** `OAuth token refresh failing — Claude Code sessions may be unable to make API calls. Run: claude login`
- **Action:** Re-authenticate with `claude login`

**False positive prevention:** The flag is cleared on non-auth errors (e.g., 429 rate limits). Only auth-related errors preserve the flag across cycles.

**Transient failures:** 5 transient HTTP 400 failures (Apr 18-22, 2026) where the token self-recovered within 10 minutes. All resolved without intervention.

### Warning Severity

#### `sonnet_cutoff_risk`

Seven-day Sonnet window at cutoff risk.

- **Trigger:** `seven_day_sonnet.cutoff_risk=true` **and** `hard_limit_margin_hrs < 0` **and** `utilization >= 85%`
- **Severity:** Warning
- **Message:** `Seven-day Sonnet window at cutoff risk: {:.1}% utilized, {:.1}h remaining, hard_limit_margin_hrs={:.1}h`
- **Action:** Consider scaling down Sonnet workers; monitor seven_day all-models window

**False-positive patterns:** 150+ instances at 0-78% utilization with deeply negative margins (Apr 16-23, 2026). All were stale EMA artifacts. The 85% threshold now suppresses these automatically.

#### `session_cutoff_risk`

Five-hour session window at cutoff risk.

- **Trigger:** `five_hour.cutoff_risk=true` **and** `hard_limit_margin_hrs < 0` **and** `utilization >= 50%`
- **Severity:** Warning
- **Message:** `Five-hour session window at cutoff risk: {:.1}% utilized, {:.1}h remaining, hard_limit_margin_hrs={:.1}h`
- **Action:** Reduce worker count or pause work until session resets

**False-positive patterns:** 20+ instances at 0-71% utilization with small negative margins (Apr 19-23, 2026). All were transient spikes. The consistency guard (hard_limit_remaining_pct > 5%) suppresses these automatically.

#### `burn_rate_spike`

Burn rate significantly higher than baseline (not yet implemented).

- **Trigger:** `burn_rate_sample > baseline * 2`
- **Severity:** Warning
- **Status:** Placeholder — requires baseline tracking

#### `promotion_not_applying`

Off-peak promotion active but not validated during off-peak hours.

- **Trigger:** `is_promo_active=true`, `is_peak_hour=false`, `!promotion_validated`
- **Severity:** Warning
- **Message:** `Off-peak promotion not applying: observed ratio {:.2} vs expected {:.2}`

#### `collector_offline`

Token collector has stopped reporting (last update > 5 minutes ago).

- **Trigger:** `now - last_fleet_aggregate.t1 > 300` seconds
- **Severity:** Warning
- **Message:** `Token collector offline: last update {N} minutes ago`

#### `low_cache_efficiency`

Fleet cache efficiency below threshold for N consecutive intervals.

- **Trigger:** `fleet_cache_eff < threshold` for `low_cache_eff_intervals` consecutive polls
- **Severity:** Warning

#### `promotion_ratio_anomaly`

Observed off-peak ratio outside expected range [0.8, 2.5].

- **Trigger:** `offpeak_ratio_observed > 2.5` OR `< 0.8`
- **Severity:** Warning

### Info Severity

#### `underutilization`

All windows have abundant capacity — safe to increase worker count.

- **Trigger:** All windows have `margin_hrs > hrs_left * 0.5`
- **Severity:** Info
- **Message:** `All windows have abundant capacity: safe to increase worker count`

## Alert Configuration

Alerts are configured in `~/.config/claude-governor/config.yaml`:

```yaml
alerts:
  enabled: true
  min_severity: warning          # info | warning | critical
  cooldown_minutes: 60           # anti-flap floor + bead refresh throttle
  command:                       # message is appended as the final argument
    - bf
    - create
    - --json
    - --type
    - human
    - --title
  close_command: [bf, close]     # <close_command> <bead_id> --reason <reason>
  update_command: [bf, update]   # <update_command> <bead_id> --notes <notes>
  low_cache_eff_threshold: 0.30  # 30%
  low_cache_eff_intervals: 5     # 5 consecutive polls (~25 min)
```

## Episode Deduplication

Dedup is stateful, not timer-based. An **episode** is one continuous stretch during which a
condition is true, identified by an *episode key* of `alert_type` plus an optional scope —
the window name for cutoff alerts (`cutoff_imminent:five_hour`), the agent name for
`subscription_billing_drift`. Two windows at cutoff risk are two incidents and get two beads;
one window at cutoff risk for a week is one incident and gets one bead.

| Cycle | Condition | Action |
|-------|-----------|--------|
| First true | not tracked | Create one bead, store its id in `open_alert_beads[key]` |
| Still true | tracked | No new bead. Record the sighting; refresh the bead's notes at most once per `cooldown_minutes` |
| No longer reported | tracked | Close the bead with an auto-close reason, drop the entry |

State lives in `governor-state.json`:

```json
"open_alert_beads": {
  "sonnet_cutoff_risk:weekly_scoped": {
    "bead_id": "bf-abc12",
    "alert_type": "sonnet_cutoff_risk",
    "scope": "weekly_scoped",
    "opened_at": "2026-08-01T12:00:00Z",
    "last_seen": "2026-08-03T09:05:00Z",
    "observations": 573,
    "last_message": "Seven-day Sonnet window at cutoff risk: 96.2% utilized ...",
    "last_refreshed_at": "2026-08-03T09:00:00Z"
  }
}
```

`cooldown_minutes` survives only as an **anti-flap floor**: after an episode resolves, the same
key cannot open a new episode (and so cannot create a new bead) until the cooldown elapses. It is
not a repeat interval — a condition that stays true never produces a second bead, however long it
lasts.

### Why this replaced pure-cooldown dedup

The old scheme re-fired the configured command every time the cooldown elapsed while a condition
held, so a long-lived condition minted a bead per hour indefinitely. This repo's own workspace
accumulated 226 closed `[WARNING] sonnet_cutoff_risk` beads and 160 `[CRITICAL] cutoff_imminent`
beads (517 alert beads total) that way — far more beads than distinct incidents. It also meant
nothing ever closed a bead when the condition recovered, so open alert beads outlived the problem
they described.

## Sprint Triggers (Underutilization)

When capacity is abundant and time is limited, the governor can trigger a "sprint" — automatically scaling workers to max to burn remaining budget before reset.

**Sprint conditions:**
- Utilization < threshold (default 50%)
- Hours remaining < limit (default 2 hours)
- No window has `cutoff_risk` (safety check)
- Safe mode is not active

**Sprint behavior:** Selects worker with most headroom, scales to max_workers, logs reason.

## Alert Severity Thresholds

Only alerts at or above `min_severity` fire:

| Setting          | Info | Warning | Critical |
|------------------|------|---------|----------|
| `info`           | ✓    | ✓       | ✓        |
| `warning` (default) | ✗  | ✓       | ✓        |
| `critical`       | ✗    | ✗       | ✓        |

## Alert Command

The default alert command creates NEEDLE beads:
```bash
bf create --type human "[WARNING] sonnet_cutoff_risk: ..."
```

This integrates with the existing task tracking system — alerts appear as HUMAN-type beads requiring attention.
