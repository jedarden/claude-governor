# Human-headroom reserve policy (claudego-fac322a2)

Validation outcome for the Sonnet-fleet resurrection. Decides how cgov reserves
weekly capacity so the human's interactive Claude Code use is never blocked
while Sonnet workers drive the weekly limit toward full consumption.

## Live usage windows (verified against poller/src, 2026-09-07)

The Anthropic usage API this account exposes produces exactly three windows —
there is **no `seven_day_sonnet` window** (the plan's goal-1 naming is
aspirational; the code uses these keys):

| Key             | Meaning                                   | Sonnet workers consume? |
|-----------------|-------------------------------------------|-------------------------|
| `five_hour`     | rolling 5h, all models                    | **yes**                 |
| `seven_day`     | 7-day weekly, all models                  | **yes**                 |
| `weekly_scoped` | 7-day weekly scoped to the premium model  | **no** (premium only)   |

`weekly_scoped` is currently scoped to **Fable** (the model this operator
session runs); it sat at ~23% remaining / cutoff-risk on 2026-09-07. Because
Sonnet usage does **not** count against `weekly_scoped`, saturating a Sonnet
worker fleet does not touch the human's premium (Fable/Opus) weekly budget.

**Consequence:** the binding window for the Sonnet fleet is **`seven_day`**
(with `five_hour` as the short-burst guard). The reserve ceiling must be set on
those, not on `weekly_scoped`.

> Empirical confirmation owed: that Sonnet worker load moves `seven_day`/
> `five_hour` and not `weekly_scoped` is inferred from API semantics + the
> poller mapping, not yet observed here (the collector is stale). The canary
> (claudego-37ef2441) and collector-repair (claudego-4f38f04e) beads must
> confirm it from live records before enforcement is trusted.
>
> **Partially discharged 2026-09-19.** The usage payload labels the scoped
> window itself: `limits[].kind == "weekly_scoped"` carries
> `scope.model.display_name == "Fable"`, and `seven_day_sonnet` is `null`. So
> "the premium weekly window is scoped to a model the Sonnet pool does not run"
> is now read off the API rather than inferred from it. What is still owed is
> the *burn* half — observing Sonnet worker load move `seven_day`/`five_hour`
> and leave `weekly_scoped` flat across a live run.

## Enforcement: pool window affinity (claudego-ec6d3ae3)

The table above is a fact about billing. Until 2026-09-19 cgov did not know it.
The binding window was the globally highest-`risk_score` window across all
three, and every pool's target derived from that one window. With
`weekly_scoped` at cutoff risk because the **operator's own** session burns
Fable, the log read `safe_worker_count: 0 workers from binding window
weekly_scoped`, and the Sonnet executor pool sat at 0 for as long as a Fable
window takes to reset — held there by a window Sonnet cannot touch. The
saturation objective silently never happened.

A pool now declares what it actually draws down:

```yaml
agents:
  needle-sonnet:
    windows: [five_hour, seven_day]   # never weekly_scoped (premium-only)
```

That declaration drives two independent gates:

1. **Binding candidacy** (`fleet_binding_affinity`, observe cycle) — the binding
   window drives the *aggregate* target for the whole fleet, so a window may
   bind only if **every enabled pool** consumes it. This composes with the
   existing data-presence gate rather than replacing it.
2. **Per-pool caps** (`pool_window_safe_cap`, act cycle) — a pool is never
   allocated past the minimum `safe_worker_count` across **its own** windows,
   whichever window binds the aggregate. A premium pool therefore keeps its
   `weekly_scoped` bound even when `seven_day` binds.

Four properties keep this from weakening the reserve above:

- **Absent means all windows.** An undeclared pool is assumed to consume
  everything, so a missing annotation can only over-constrain, never
  under-constrain. An unknown name or `windows: []` normalizes the same way, so
  a typo cannot leave a pool unbounded.
- **A disabled pool (`max_workers: 0`) restricts nothing** — leftover config on
  a retired pool cannot make a window bindable again.
- **Disjoint declarations fall back to unrestricted** aggregate selection; the
  per-pool caps still bound each pool by its own windows.
- **The 98% emergency brake is deliberately NOT affinity-gated.** Any window at
  the brake threshold — `weekly_scoped` included — still zeroes the whole fleet.

`min_workers` floors still win over the cap, matching how the floor pass treats
floors elsewhere: an explicit floor is a stated intent to run through tight
windows.

> If Anthropic ever re-scopes the premium weekly window to a model a pool
> actually runs, that pool's `windows:` must gain `weekly_scoped` again. The
> declaration is an assertion about billing, and nothing in cgov can verify it.

## Reserve mechanism (verified in burn_rate.rs)

`remaining_pct = target_ceiling − current_utilization`, where
`current_utilization` is **account-wide (includes the human's usage so far)**,
and `safe_worker_count = remaining_pct / (per_worker_rate × hours_remaining)`
uses **worker-only** burn. The human is therefore protected two ways:

1. **Reactive** — as the human consumes, `current_utilization` rises,
   `remaining_pct` shrinks, and `safe_worker_count` drops, so workers scale
   down (graceful, idle-only). The reserve is already semi-dynamic.
2. **Static buffer** — the `target_ceiling → 100%` gap absorbs a human burst
   that arrives faster than the 300s control loop can drain workers.

## Decision

- A **static per-window ceiling is sufficient**; a predictive human-burn model
  is **not** needed for v1. Rationale: graceful scale-down is idle-only, so the
  reserve gap must cover a human burst over one worker-task drain window. A 15%
  reserve of a *weekly* budget is an enormous absolute amount relative to any
  plausible interactive burst over ~one task duration, so it holds comfortably.
- Set `seven_day` **target_ceiling = 85%** (15% weekly reserve for the human).
- Set `five_hour` **target_ceiling = 85%** (short-burst guard).
- Leave `weekly_scoped` at the default/high ceiling — Sonnet workers don't
  consume it; it only bounds the human's own premium use.
- Keep `max_scale_down_per_cycle` and loop interval as-is; the static gap, not
  drain speed, is the guarantee. If future data shows human bursts breaching
  the gap, revisit with a dynamic reserve — do not build it preemptively.

Consumed by: the config example (claudego-24f77066) bakes these values; the
saturation tuning (claudego-f1ceaf96) ramps up to these ceilings; the canary
(claudego-37ef2441) confirms the human is never blocked in practice.
