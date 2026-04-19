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
- **Resolved false positives (docs-uqq8, docs-i592, docs-nqa6, docs-wv4g, docs-eya0, docs-y6nc, docs-f1rx, docs-prhv, docs-psy1, docs-ai7o):** Recurring seven_day alerts at 60-65% utilization, margin_hrs=-35.4 to -48.1h, hrs_left=37.5 to 50.5h. Same transient burn rate spike pattern (~12.5%/hr implied burn rate: 25-30% remaining / 2.0-2.4h predicted exhaustion). Alerts created 2026-04-16 to 2026-04-19; all instances saw the seven_day window reset without incident, confirming the spike was transient. The 60-65%/-35 to -48h operating point is a known false positive for deep_margin_risk.
- **Resolved false positives (docs-2pxg, docs-05pk, docs-qsl8, docs-t9wb, docs-hqsw, docs-4qg8, docs-0ycp, docs-ibmi, docs-rp5k, docs-hl92, docs-hiwr, docs-cu0r, docs-gga8, docs-ozcn, docs-pwsf, docs-18qf, docs-eax1, docs-9is7, docs-2xo9, docs-pqoa, docs-n2uw, docs-za7t, docs-2ip5, docs-rkwz, docs-hrm0, docs-jar2, docs-7mda):** seven_day at 1-16% utilization, margin_hrs=-159.1 to -132.4h, hrs_left=138.4 to 166.5h. Extreme false positives — at 1-16% utilization with 84-99% headroom to the 90% ceiling, the window cannot be at cutoff risk. Alerts created 2026-04-16 to 2026-04-18; the deeply negative margin at near-zero utilization indicates a corrupted state or measurement anomaly, not an actual capacity crisis. The 50% utilization threshold should have suppressed these alerts automatically.
- **Resolved false positive (docs-vfu6):** five_hour at 27% utilization, margin_hrs=-2.3h, hrs_left=3.4h. False positive for the high_utilization_risk tier — margin_hrs=-2.3h marginally passes the < -2 threshold, but at only 27% utilization with 73% headroom to the 90% ceiling, the window cannot be at cutoff risk. The negative margin at low utilization indicates a transient burn rate spike or stale EMA, not an actual capacity crisis. The 80% utilization threshold should have suppressed this alert automatically. Alert created 2026-04-19.

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
- **Resolved transient failure (docs-az7r):** Token refresh failed with HTTP 400 at 2026-04-18 15:16 EDT on two consecutive attempts, triggering the alert. The token self-recovered within ~10 minutes — successful (non-stale) polls resumed at 15:26 and continued uninterrupted. Subsequent poll failures were all HTTP 429 rate limits on the usage endpoint (not auth errors), confirming the token remained valid. The HTTP 400 was a transient OAuth platform error, not an expired or revoked token. No `claude login` was needed.

### Warning Severity

#### `sonnet_cutoff_risk`

Seven-day Sonnet window at cutoff risk (`cutoff_risk=1`).

- **Trigger:** `seven_day_sonnet.cutoff_risk=true` **and** `margin_hrs < 0` **and** `current_utilization >= 50%` (negative margin indicates exhaustion before reset; utilization guard prevents false positives from stale EMA burn rates)
- **Severity:** Warning
- **Message:** `Seven-day Sonnet window at cutoff risk: {:.1}% utilized, {:.1}h remaining, margin_hrs={:.1}h`
- **Action:** Consider scaling down Sonnet workers; monitor seven_day all-models window
- **Why both conditions:** The `margin_hrs < 0` guard prevents false positives when `cutoff_risk=true` but the margin is actually positive (safe). Positive margin means exhaustion will occur **after** reset, so no alert should fire. This catches corrupted state or sign convention mismatches between modules. The `utilization >= 50%` guard prevents false positives from stale EMA burn rates — the fleet_pct_hr EMA only updates on positive deltas, so during seven-day window rollover periods (when old high-usage data drops off), the EMA can stay inflated while actual utilization is declining. At 40% utilization with 50% headroom to the 90% ceiling, a stale EMA predicting imminent exhaustion is not a real crisis.
- **Resolved false positive (docs-amvn):** seven_day_sonnet at 40% utilized, margin_hrs=-108h, hrs_left=112h. The EMA was stuck at 12.47%/hr (from prior heavy usage) while actual burn was ~0.47%/hr. During window rollover, net deltas went negative (old data dropping off faster than new usage accumulating), preventing the EMA from updating. The 50% utilization threshold now suppresses this pattern automatically.
- **Resolved false positive (docs-mbt4):** seven_day_sonnet at 64% utilized, margin_hrs=-42.6h, hrs_left=44.3h. Same transient burn rate spike pattern as cutoff_imminent's seven_day false positives (~12.5%/hr implied burn rate: 36% remaining / 2.9h predicted exhaustion). Alert created 2026-04-16; the seven_day_sonnet window reset without incident, confirming the spike was transient. The 60-64%/-42h operating point is a known false positive pattern for both cutoff_imminent and sonnet_cutoff_risk.
- **Resolved false positive (docs-1pyp):** seven_day_sonnet at 65% utilized, margin_hrs=-40.6h, hrs_left=42.3h. Same transient burn rate spike pattern (~12.5%/hr implied burn rate: 35% remaining / 2.8h predicted exhaustion). Alert created 2026-04-16; confirms the 60-65%/-40 to -42h operating point is a recurring false positive pattern for sonnet_cutoff_risk.
- **Resolved false positive (docs-u9ad):** seven_day_sonnet at 64% utilized, margin_hrs=-41.6h, hrs_left=43.3h. Same transient burn rate spike pattern as docs-mbt4 and docs-1pyp (~12.5%/hr implied burn rate: 36% remaining / 2.9h predicted exhaustion). Alert created 2026-04-16; confirms the 60-65%/-40 to -42h operating point is a recurring false positive pattern for sonnet_cutoff_risk.
- **Resolved false positive (docs-oaek):** seven_day_sonnet at 66% utilized, margin_hrs=-38.6h, hrs_left=40.3h. Same transient burn rate spike pattern (~12.5%/hr implied burn rate: 34% remaining / 2.7h predicted exhaustion). Alert created 2026-04-19; confirms the 60-66%/-38 to -42h operating point is a recurring false positive pattern for sonnet_cutoff_risk.
- **Resolved false positive (docs-8oj9):** seven_day_sonnet at 65% utilized, margin_hrs=-39.6h, hrs_left=41.3h. Same transient burn rate spike pattern as docs-mbt4, docs-1pyp, docs-u9ad, and docs-oaek (~12.5%/hr implied burn rate: 25% remaining / 1.7h predicted exhaustion). Alert created 2026-04-16; the seven_day_sonnet window reset without incident, confirming the spike was transient. The 60-66%/-38 to -42h operating point continues to be a recurring false positive pattern for sonnet_cutoff_risk.
- **Resolved false positive (docs-8kr7):** seven_day_sonnet at 66% utilized, margin_hrs=-37.6h, hrs_left=39.3h. Same transient burn rate spike pattern (~12.5%/hr implied burn rate: 24% remaining / 1.9h predicted exhaustion). Alert created 2026-04-16; confirms the 60-66%/-37 to -42h operating point is a recurring false positive pattern for sonnet_cutoff_risk.
- **Resolved false positive (docs-yy1q):** seven_day_sonnet at 67% utilized, margin_hrs=-36.6h, hrs_left=38.3h. Same transient burn rate spike pattern (~13.5%/hr implied burn rate: 23% remaining / 1.7h predicted exhaustion). Alert created 2026-04-16; confirms the 60-67%/-36 to -42h operating point is a recurring false positive pattern for sonnet_cutoff_risk.
- **Resolved false positive (docs-nvs0):** seven_day_sonnet at 0.0% utilized, margin_hrs=-160.6h, hrs_left=167.3h. Extreme false positive — 0% utilization indicates an idle or reset window, yet the deeply negative margin_hrs suggests imminent exhaustion. This pattern occurs when the EMA burn rate remains stuck from prior heavy usage while the actual utilization has dropped to near-zero (likely during a window rollover or idle period). At 0% utilization with 167.3 hours remaining, there is effectively no capacity risk. The 50% utilization threshold now suppresses this pattern automatically.
- **Resolved false positive (docs-y124):** seven_day_sonnet at 2.0% utilized, margin_hrs=-158.5h, hrs_left=165.3h. Extreme false positive — at 2% utilization with 98% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a corrupted state or measurement anomaly, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-vd2s):** seven_day_sonnet at 2.0% utilized, margin_hrs=-157.5h, hrs_left=164.3h. Extreme false positive — at 2% utilization with 98% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a corrupted state or measurement anomaly, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-ipi7):** seven_day_sonnet at 2.0% utilized, margin_hrs=-156.5h, hrs_left=163.3h. Extreme false positive — at 2% utilization with 98% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a corrupted state or measurement anomaly, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-ar2x):** seven_day_sonnet at 2.0% utilized, margin_hrs=-155.5h, hrs_left=162.3h. Extreme false positive — at 2% utilization with 98% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a corrupted state or measurement anomaly, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-02iv):** seven_day_sonnet at 2.0% utilized, margin_hrs=-159.5h, hrs_left=166.3h. Same extreme false positive pattern as docs-nvs0, docs-y124, docs-vd2s, docs-ipi7, and docs-ar2x — at 2% utilization with 166.3h remaining, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-lb1o):** seven_day_sonnet at 2.0% utilized, margin_hrs=-154.5h, hrs_left=161.3h. Same extreme false positive pattern as docs-nvs0, docs-y124, docs-vd2s, docs-ipi7, docs-ar2x, and docs-02iv — at 2% utilization with 161.3h remaining, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-4ist):** seven_day_sonnet at 3.0% utilized, margin_hrs=-153.5h, hrs_left=160.3h. Extreme false positive — at 3% utilization with 97% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-wimh):** seven_day_sonnet at 5.0% utilized, margin_hrs=-151.5h, hrs_left=158.3h. Extreme false positive — at 5% utilization with 95% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-h1wk):** seven_day_sonnet at 5.0% utilized, margin_hrs=-150.5h, hrs_left=157.3h. Same extreme false positive pattern as docs-wimh — at 5% utilization with 95% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-wrbt):** seven_day_sonnet at 4.0% utilized, margin_hrs=-152.5h, hrs_left=159.3h. Same extreme false positive pattern as docs-nvs0, docs-y124, docs-vd2s, docs-ipi7, docs-ar2x, docs-02iv, docs-lb1o, docs-4ist, docs-wimh, and docs-h1wk — at 4% utilization with 159.3h remaining, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-y1yl):** seven_day_sonnet at 5.0% utilized, margin_hrs=-149.5h, hrs_left=156.3h. Same extreme false positive pattern as docs-wimh and docs-h1wk — at 5% utilization with 95% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-onfb):** seven_day_sonnet at 5.0% utilized, margin_hrs=-148.5h, hrs_left=155.3h. Same extreme false positive pattern as docs-y1yl, docs-wimh, and docs-h1wk — at 5% utilization with 95% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-uyri):** seven_day_sonnet at 5.0% utilized, margin_hrs=-145.4h, hrs_left=152.2h. Same extreme false positive pattern as docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 5% utilization with 95% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-yccn):** seven_day_sonnet at 5.0% utilized, margin_hrs=-146.4h, hrs_left=153.2h. Same extreme false positive pattern as docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 5% utilization with 95% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-iewa):** seven_day_sonnet at 5.0% utilized, margin_hrs=-147.4h, hrs_left=154.2h. Same extreme false positive pattern as docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 5% utilization with 95% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-c4jz):** seven_day_sonnet at 6.0% utilized, margin_hrs=-144.5h, hrs_left=151.2h. Same extreme false positive pattern as docs-iewa, docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 6% utilization with 94% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-ibwa):** seven_day_sonnet at 6.0% utilized, margin_hrs=-143.5h, hrs_left=150.2h. Same extreme false positive pattern as docs-c4jz, docs-iewa, docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 6% utilization with 94% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-ddvb):** seven_day_sonnet at 7.0% utilized, margin_hrs=-142.5h, hrs_left=149.2h. Same extreme false positive pattern as docs-ibwa, docs-c4jz, docs-iewa, docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 7% utilization with 93% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-8tx7):** seven_day_sonnet at 8.0% utilized, margin_hrs=-141.5h, hrs_left=148.2h. Same extreme false positive pattern as docs-ddvb, docs-ibwa, docs-c4jz, docs-iewa, docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 8% utilization with 92% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at near-zero utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-5muj):** seven_day_sonnet at 10.0% utilized, margin_hrs=-141.7h, hrs_left=147.2h. Same extreme false positive pattern as docs-8tx7, docs-ddvb, docs-ibwa, docs-c4jz, docs-iewa, docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 10% utilization with 90% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-g5ur):** seven_day_sonnet at 13.0% utilized, margin_hrs=-142.1h, hrs_left=146.2h. Same extreme false positive pattern as docs-5muj, docs-8tx7, docs-ddvb, docs-ibwa, docs-c4jz, docs-iewa, docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 13% utilization with 87% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-4101):** seven_day_sonnet at 16.0% utilized, margin_hrs=-139.4h, hrs_left=144.2h. Same extreme false positive pattern as docs-g5ur and predecessors — at 16% utilization with 84% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.

- **Resolved false positive (docs-863n):** seven_day_sonnet at 15.0% utilized, margin_hrs=-140.6h, hrs_left=145.2h. Same extreme false positive pattern as docs-g5ur, docs-5muj, docs-8tx7, docs-ddvb, docs-ibwa, docs-c4jz, docs-iewa, docs-yccn, docs-uyri, docs-onfb, docs-y1yl, docs-wimh, and docs-h1wk — at 15% utilization with 85% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-hj7y):** seven_day_sonnet at 17.0% utilized, margin_hrs=-138.3h, hrs_left=143.2h. Same extreme false positive pattern as docs-863n, docs-g5ur, docs-4101, and predecessors — at 17% utilization with 83% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-66rk):** seven_day_sonnet at 17.0% utilized, margin_hrs=-137.3h, hrs_left=142.2h. Same extreme false positive pattern as docs-hj7y, docs-863n, docs-g5ur, docs-4101, and predecessors — at 17% utilization with 83% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-ldl4):** seven_day_sonnet at 20.0% utilized, margin_hrs=-135.9h, hrs_left=140.2h. Same extreme false positive pattern as docs-66rk and predecessors — at 20% utilization with 80% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-8f4b):** seven_day_sonnet at 18.0% utilized, margin_hrs=-136.2h, hrs_left=141.2h. Same extreme false positive pattern as docs-66rk, docs-hj7y, docs-863n, docs-g5ur, docs-4101, and predecessors — at 18% utilization with 82% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-4k1z):** seven_day_sonnet at 23.0% utilized, margin_hrs=-134.2h, hrs_left=138.2h. Same extreme false positive pattern as docs-ldl4, docs-66rk, and predecessors — at 23% utilization with 77% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
- **Resolved false positive (docs-x4u8):** seven_day_sonnet at 22.0% utilized, margin_hrs=-135.4h, hrs_left=139.2h. Same extreme false positive pattern as docs-8f4b, docs-ldl4, docs-66rk, and predecessors — at 22% utilization with 78% headroom to the 90% ceiling, the window cannot be at cutoff risk. The deeply negative margin at low utilization indicates a stale EMA burn rate persisting from prior heavy usage during a window rollover or idle period, not an actual capacity crisis. The 50% utilization threshold should have suppressed this alert automatically.
#### `session_cutoff_risk`

Five-hour session window at cutoff risk (`cutoff_risk=1`).

- **Trigger:** `five_hour.cutoff_risk=true` **and** `margin_hrs < 0` **and** `current_utilization >= 50%` (negative margin indicates exhaustion before reset; utilization guard prevents false positives from transient burn rate spikes)
- **Severity:** Warning
- **Message:** `Five-hour session window at cutoff risk: {:.1}% utilized, {:.1}h remaining, margin_hrs={:.1}h`
- **Action:** Reduce worker count or pause work until session resets
- **Why both conditions:** The `margin_hrs < 0` guard prevents false positives when `cutoff_risk=true` but the margin is actually positive (safe). The `utilization >= 50%` guard prevents false positives from transient spikes in `fleet_pct_per_hour` — with low utilization (e.g., 26%), the governor has ample headroom to scale down workers before exhaustion. A negative margin at low utilization indicates a temporary burn rate spike, not an actual capacity crisis.

**False positive example (docs-e0rm):** An alert fired with 0.0% utilization, 4.4h remaining, and margin_hrs=-1.1h. This is a false positive — if utilization is truly 0%, the window cannot be at cutoff risk. The negative margin at near-zero utilization indicates a measurement anomaly or corrupted state, not a real capacity crisis. The `>= 50%` utilization guard was added to prevent these false positives.

**False positive example (docs-d3wx):** An alert fired with 1.0% utilization, 4.9h remaining, and margin_hrs=-0.1h. At only 1% utilization with 4.9 hours remaining, the window has 99% headroom to the 90% ceiling — this cannot be a real cutoff risk. The negative margin at minimal utilization indicates a transient measurement anomaly, not an actual capacity crisis. The `>= 50%` utilization guard suppresses this pattern automatically.

**False positive (docs-upiz):** An alert fired with 6.0% utilization, 4.7h remaining, and margin_hrs=-1.5h. At only 6% utilization with 4.7 hours remaining, the window has 94% headroom to the 90% ceiling — this cannot be a real cutoff risk. The negative margin at low utilization indicates a transient measurement anomaly or stale burn rate, not an actual capacity crisis. The `>= 50%` utilization guard should have suppressed this alert automatically.

**False positive (docs-h4c8):** An alert fired with 6.0% utilization, 3.7h remaining, and margin_hrs=-0.5h. At only 6% utilization with 3.7 hours remaining, the window has 94% headroom to the 90% ceiling — this cannot be a real cutoff risk. The negative margin at low utilization indicates a transient measurement anomaly or stale burn rate, not an actual capacity crisis. The `>= 50%` utilization guard should have suppressed this alert automatically.

**False positive (docs-nv8d):** An alert fired with 37.0% utilization, 2.7h remaining, and margin_hrs=-1.4h. At 37% utilization with 2.7 hours remaining, the window has 63% headroom to the 90% ceiling — this is not a real cutoff risk. The negative margin indicates a transient burn rate spike, not an actual capacity crisis. The `>= 50%` utilization guard should have suppressed this alert automatically.

**False positive (docs-7bre):** An alert fired with 0.0% utilization, 5.0h remaining, and margin_hrs=-0.9h. This is a false positive — if utilization is truly 0%, the window cannot be at cutoff risk. The negative margin at zero utilization indicates a measurement anomaly or corrupted state, not a real capacity crisis. The `>= 50%` utilization guard was added to prevent these false positives.

**False positive (docs-8ysk):** An alert fired with 47.0% utilization, 1.7h remaining, and margin_hrs=-0.6h. At 47% utilization with 1.7 hours remaining, the window has 43% headroom to the 90% ceiling — this is not a real cutoff risk. The negative margin indicates a transient burn rate spike, not an actual capacity crisis. The `>= 50%` utilization guard should have suppressed this alert automatically.

**False positive (docs-78hv):** An alert fired with 15.0% utilization, 2.8h remaining, and margin_hrs=-0.6h. At 15% utilization with 2.8 hours remaining, the window has 85% headroom to the 90% ceiling — this cannot be a real cutoff risk. The negative margin at low utilization indicates a transient measurement anomaly or stale burn rate, not an actual capacity crisis. The `>= 50%` utilization guard should have suppressed this alert automatically.

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
