# Claude Governor Alert System — Research

## Overview

The governor creates HUMAN-type beads via NEEDLE when specific conditions are detected. Alerts are deduplicated via per-type cooldowns to prevent spam while ensuring persistent conditions generate fresh notifications after the cooldown period.

## Alert Types

### Critical Severity

#### `cutoff_imminent`

Any window has `cutoff_risk=1` **and** either:

1. **High utilization risk:** `margin_hrs < -2` **and** `utilization >= 80%`, OR
2. **Deep margin risk:** `margin_hrs < -24` **and** `utilization >= 50%`

- **Trigger:** Window is at cutoff risk with tiered thresholds
  - High utilization risk: Exhaustion predicted >2 hours before reset AND utilization at 80%+ OR
  - Deep margin risk: Exhaustion predicted >24 hours before reset AND utilization at 50%+
- **Severity:** Critical
- **Message:** `Window {name} at cutoff risk: margin_hrs={:.1}h, utilization={:.1}%, hrs_left={:.1}h`
- **Action:** Immediate manual intervention required — scale down workers immediately
- **Why tiered thresholds:** A low-utilization window (e.g., 52%) with small negative margin (-3h) is a transient burn rate spike. However, a moderate-utilization window (50-60%) with deeply negative margin (<-24h) is a real crisis — exhaustion is predicted in hours despite modest utilization. The two-tier system catches both patterns:
  - The 80% threshold prevents false positives from transient spikes with small negative margins
  - The 50% threshold with -24h margin catches genuine crises where moderate utilization masks imminent exhaustion
- **Deep margin risk examples:** seven_day at 56-60% utilization with margin_hrs=-47 to -55h predicts exhaustion in ~2-3 hours despite 34-40% headroom to the 90% ceiling. This IS a capacity crisis — the deep negative margin indicates sustained elevated burn rate that will exhaust the window before reset.
- **Resolved false positives (docs-uqq8, docs-i592, docs-nqa6):** Recurring seven_day alerts at 60% utilization, margin_hrs=-43.1 to -48.1h, hrs_left=45.5 to 50.5h. Same transient burn rate spike pattern (~12.5%/hr implied burn rate: 30% remaining / 2.4h predicted exhaustion). Alerts created 2026-04-16; all instances saw the seven_day window reset without incident, confirming the spike was transient. The 60%/-48h operating point is a known false positive for deep_margin_risk.

#### `emergency_brake_activated`

Emergency brake was triggered (98%+ utilization detected).

- **Trigger:** `safe_mode.active=true` with trigger="emergency_brake"
- **Severity:** Critical
- **Message:** `Emergency brake active since {timestamp}`
- **Action:** Workers have been scaled to minimum; investigate why prediction failed

#### `token_refresh_failing`

OAuth token refresh failing — governor is using stale cached usage data because live API polling cannot authenticate.

- **Trigger:** `token_refresh_failing=true` in state (set when poller returns stale data due to token refresh failure)
- **Severity:** Critical
- **Message:** `OAuth token refresh failing — Claude Code sessions may be unable to make API calls. Run: claude login`
- **Action:** Re-authenticate with `claude login`
- **False positive prevention:** The flag is cleared when `poll()` returns `Err` from non-auth errors (e.g., 429 rate limits from `fetch_usage`). Only auth-related errors (token refresh, credentials) preserve the flag across cycles. This prevents the alert from persisting when the token is valid but the API is temporarily rate-limiting.

### Warning Severity

#### `sonnet_cutoff_risk`

Seven-day Sonnet window at cutoff risk (`cutoff_risk=1`).

- **Trigger:** `seven_day_sonnet.cutoff_risk=true` **and** `margin_hrs < 0` **and** `current_utilization >= 50%` (negative margin indicates exhaustion before reset; utilization guard prevents false positives from stale EMA burn rates)
- **Severity:** Warning
- **Message:** `Seven-day Sonnet window at cutoff risk: {:.1}% utilized, {:.1}h remaining, margin_hrs={:.1}h`
- **Action:** Consider scaling down Sonnet workers; monitor seven_day all-models window
- **Why both conditions:** The `margin_hrs < 0` guard prevents false positives when `cutoff_risk=true` but the margin is actually positive (safe). Positive margin means exhaustion will occur **after** reset, so no alert should fire. This catches corrupted state or sign convention mismatches between modules. The `utilization >= 50%` guard prevents false positives from stale EMA burn rates — the fleet_pct_hr EMA only updates on positive deltas, so during seven-day window rollover periods (when old high-usage data drops off), the EMA can stay inflated while actual utilization is declining. At 40% utilization with 50% headroom to the 90% ceiling, a stale EMA predicting imminent exhaustion is not a real crisis.
- **Resolved false positive (docs-amvn):** seven_day_sonnet at 40% utilized, margin_hrs=-108h, hrs_left=112h. The EMA was stuck at 12.47%/hr (from prior heavy usage) while actual burn was ~0.47%/hr. During window rollover, net deltas went negative (old data dropping off faster than new usage accumulating), preventing the EMA from updating. The 50% utilization threshold now suppresses this pattern automatically.

#### `session_cutoff_risk`

Five-hour session window at cutoff risk (`cutoff_risk=1`).

- **Trigger:** `five_hour.cutoff_risk=true` **and** `margin_hrs < 0` **and** `current_utilization >= 50%` (negative margin indicates exhaustion before reset; utilization guard prevents false positives from transient burn rate spikes)
- **Severity:** Warning
- **Message:** `Five-hour session window at cutoff risk: {:.1}% utilized, {:.1}h remaining, margin_hrs={:.1}h`
- **Action:** Reduce worker count or pause work until session resets
- **Why both conditions:** The `margin_hrs < 0` guard prevents false positives when `cutoff_risk=true` but the margin is actually positive (safe). The `utilization >= 50%` guard prevents false positives from transient spikes in `fleet_pct_per_hour` — with low utilization (e.g., 26%), the governor has ample headroom to scale down workers before exhaustion. A negative margin at low utilization indicates a temporary burn rate spike, not an actual capacity crisis.

**False positive example (docs-e0rm):** An alert fired with 0.0% utilization, 4.4h remaining, and margin_hrs=-1.1h. This is a false positive — if utilization is truly 0%, the window cannot be at cutoff risk. The negative margin at near-zero utilization indicates a measurement anomaly or corrupted state, not a real capacity crisis. The `>= 50%` utilization guard was added to prevent these false positives.

#### `burn_rate_spike`

Burn rate significantly higher than baseline (not yet implemented).

- **Trigger:** `burn_rate_sample > baseline * 2`
- **Severity:** Warning
- **Status:** Placeholder — requires baseline tracking

#### `promotion_not_applying`

Off-peak promotion active but not validated during off-peak hours.

- **Trigger:** `is_promo_active=true`, `is_peak_hour=false`, `!promotion_validated`, sufficient samples
- **Severity:** Warning
- **Message:** `Off-peak promotion not applying: observed ratio {:.2} vs expected {:.2}`
- **Action:** Check if promotion period is actually active; verify schedule configuration

#### `collector_offline`

Token collector has stopped reporting (last update > 5 minutes ago).

- **Trigger:** `now - last_fleet_aggregate.t1 > 300` seconds
- **Severity:** Warning
- **Message:** `Token collector offline: last update {N} minutes ago`
- **Context:** The collector writes a "heartbeat" fleet record every 2 minutes (120s interval) even when idle (no new token usage), so this alert should only fire when the collector daemon has actually stopped or cannot write to the database. The collector service is independent of the governor (no `PartOf`), so governor restarts should not trigger this alert.
- **Action:**
  1. Check if the collector daemon is running: `ps aux | grep cgov`
  2. Check for collection errors in governor logs: `tail -100 ~/.needle/logs/governor.log | grep collector`
  3. Verify database is writable: `ls -la ~/.needle/state/token-history.*`
  4. If collector is not running, restart it; if running but failing, check disk space or database corruption
  5. After recovery, the alert cooldown is automatically cleared to enable immediate re-notification if the issue recurs

#### `low_cache_efficiency`

Fleet cache efficiency below threshold for N consecutive intervals.

- **Trigger:** `fleet_cache_eff < threshold` for `low_cache_eff_intervals` consecutive polls
- **Severity:** Warning
- **Message:** `Fleet cache efficiency {:.1}% below threshold {:.0}% for {N} consecutive intervals (~{min} min)`
- **Action:** Investigate why cache hit rate is low; may indicate inefficient workloads

#### `promotion_ratio_anomaly`

Observed off-peak ratio outside expected range [0.8, 2.5].

- **Trigger:** `offpeak_ratio_observed > 2.5` OR `< 0.8`
- **Severity:** Warning
- **Message:** `Promotion ratio anomaly: observed ratio {:.2} exceeds/below threshold (expected {:.2})`
- **Action:** Possible miscalibration or inverse anomaly detected

### Info Severity

#### `underutilization`

All windows have abundant capacity — safe to increase worker count.

- **Trigger:** All windows have `margin_hrs > hrs_left * 0.5`
- **Severity:** Info
- **Message:** `All windows have abundant capacity: safe to increase worker count`
- **Action:** Consider scaling up workers to utilize remaining budget

## Alert Configuration

Alerts are configured in `~/.config/claude-governor/config.yaml`:

```yaml
alerts:
  enabled: true
  min_severity: warning          # info | warning | critical
  cooldown_minutes: 60           # suppress duplicate alerts
  command:
    - br
    - create
    - --type
    - human
  low_cache_eff_threshold: 0.30  # 30%
  low_cache_eff_intervals: 5     # 5 consecutive polls (~25 min)
```

## Cooldown Deduplication

Each alert type has an independent cooldown timer. When an alert fires:
1. A bead is created via the configured command
2. The alert type is recorded with a timestamp in `alert_cooldown`
3. Subsequent detections of the same alert type are suppressed until cooldown expires
4. If the condition clears and re-triggers after cooldown, a new alert fires

**Cooldown clearing:** When an alert condition is no longer detected, the cooldown is cleared immediately, allowing re-notification if the condition returns.

## Alert Logs

All fired alerts are logged to `~/.needle/logs/governor.log` with format:
```
2026-03-20T10:00:00Z [WARNING] sonnet_cutoff_risk: Seven-day Sonnet window at cutoff risk: 75.0% utilized, 45.2h remaining, margin_hrs=-5.8h
```

## Sprint Triggers (Underutilization)

When capacity is abundant and time is limited, the governor can trigger a "sprint" — automatically scaling workers to max to burn remaining budget before reset.

**Sprint conditions:**
- Utilization < threshold (default 50%)
- Hours remaining < limit (default 2 hours)
- No window has `cutoff_risk` (safety check)
- Safe mode is not active

**Sprint behavior:**
- Selects worker with most headroom (max - current)
- Scales selected worker to max_workers
- Logs sprint reason with window, utilization, and hours remaining

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
br create --type human "[WARNING] sonnet_cutoff_risk: ..."
```

This integrates with the existing task tracking system — alerts appear as HUMAN-type beads requiring attention.
