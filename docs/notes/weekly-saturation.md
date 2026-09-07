# Weekly saturation tuning (claudego-f1ceaf96)

How cgov drives the weekly Sonnet window (`seven_day`) to full consumption so
the limit is hit every week, paired with the human reserve
(docs/notes/human-reserve-policy.md).

## Mechanism (verified in governor.rs / alerts.rs) — complete, no code change

Each cycle `compute_target_workers` sets
`target = safe_worker_count(binding).min(max_workers).max(min_workers)`, where
`safe_worker_count = remaining_pct / (per_worker_rate × hours_remaining)` — the
most workers that consume up to the ceiling by reset. So the governor already
*aims to saturate* to the ceiling, bounded only by `max_workers`.

Leftover-budget guards exist so the window isn't left unused at reset:
- **Underutilization sprint** (alerts.rs): fires when utilization < 50% and
  hours_remaining < 2h → boosts the pool to `max_workers`.
- **End-of-window sprint** (config `sprint`): within `horizon_minutes` (90) of
  reset with headroom > `min_headroom_pct` (15%), temporarily raises the cap by
  `max_workers_boost` (+3), blocked if the confidence cone ratio > 2.0.

Scale-up throttle (`max_scale_up_per_cycle=1` per 300s loop) reaches 8 workers
from 0 in ~40 min — negligible against a 7-day window. No change needed.

## The one binding tuning parameter: max_workers

Whether the weekly window is actually filled reduces to **max_workers vs. the
empirical per-worker Sonnet burn rate**. If the whole pool's weekly burn is
below the weekly budget, the window never reaches the ceiling no matter how the
governor behaves. Size it as:

    max_workers ≈ ceil( (weekly_sonnet_budget × 0.85) / per_worker_weekly_burn )

with a modest safety margin (the end-of-window sprint's +3 boost covers the
final 90 min, not steady-state). The current value is 8, chosen before any
Sonnet burn was measured.

**Deferred, not unknown:** `per_worker_weekly_burn` requires fresh collector
records for the Sonnet adapter, which do not exist yet (collector is 17 days
stale — claudego-4f38f04e). So:

1. Repair the collector (claudego-4f38f04e).
2. Run the canary (claudego-37ef2441) long enough to measure real Sonnet
   per-worker burn on `seven_day`.
3. Set `max_workers` from the formula above; raise it past 8 if 8 workers'
   measured weekly burn cannot reach 85% of the weekly window.

Until then max_workers stays 8 as a safe starting point — it cannot *over*-consume
(the ceiling still binds), it can only *under*-fill, which the canary will detect.
