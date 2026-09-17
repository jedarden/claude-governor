# Burn attribution semantics — fleet vs exogenous

cgov measures burn **account-wide** (the usage API reports one percentage per
window for the whole account) but governs only **its own pool**. Everything
else sharing the account — the operator's interactive Claude Code sessions,
workers of some *other* governor instance — is **exogenous**. Conflating the
two is the bug behind claudego-0ccbae3c: with zero NEEDLE workers and the
operator's own Opus session burning 12–14%/hr, every window read as fleet
CUTOFF_RISK with margins of −26.4h/−27.8h, and the pool was held at 0 even
though the 7d window could sustain ~0.97%/hr to reset.

This note records the semantics that untangled them (claudego-a542d686,
claudego-892a82b1) and where each half lives, so future workers don't
re-merge the models.

## Classification: fleet vs exogenous

Each instance record the collector writes (`InstanceRecord` in
`src/burn_rate.rs`, the `i` table in token-history.db) carries a `worker`
column: the needle worker session name (`needle-{agent}-{worker_id}`) that
dispatched the CC session, resolved per collection pass by
`src/worker_attribution.rs` from `/proc` fd scanning + heartbeat names
(claudego-a542d686). `None` means "not attributable": an operator session,
or a worker whose claude process exited before the pass scanned.

`record_is_fleet(worker, fleet_session_patterns)` is the single
classification point:

- **FLEET** — `worker` is `Some(name)` and `name` matches one of the
  agents' `session_pattern` globs from governor.yaml.
- **EXOGENOUS** — everything else: `worker == None` (operator sessions,
  unattributed tail usage), and matched-but-foreign names (a needle worker
  belonging to another governor instance is real burn, but it is not ours to
  scale).

An unparsable glob never matches, so a config typo degrades to "everything is
exogenous" (under-scaling) rather than adopting foreign burn as fleet burn
(over-scaling).

**At zero workers every record classifies exogenous by construction** — there
is no needle process to resolve to, so `worker` is always `None`. The
zero-worker moment therefore remains the fallback that keeps the exogenous
baseline calibrated whenever attribution is unavailable.

## The two models the rates feed

`estimate_burn_rates` splits per-instance rates by attribution and feeds two
never-interchanging models:

### Fleet side — sizes and risks the pool

- `fleet_pct_hr` per window is computed from **fleet-attributable records
  only** (`compute_fleet_stats`). Before attribution this read every session
  on the account, so an operator burning 12%/hr sat inside `mean_pct_hr` and
  rode into the forecast as if the pool were burning it.
- The per-(model, window) `pct_per_worker_per_hour` EMA is likewise updated
  from fleet records only. It is a **per-worker** figure; an operator's rate
  divided by the pool's worker count is exactly the category error the split
  removes.
- The fleet side keeps its per-session **mean** as the fleet rate (the
  fleet-only forecast math is unchanged by the split; pinned by
  `fleet_only_math_is_unchanged`-style pins).

### Exogenous side — a constant budget offset

Exogenous burn enters `generate_window_forecast_with_exogenous` as a
**CONSTANT BUDGET RESERVATION**, never as fleet burn:

- Measured directly from records classified exogenous, **summed (not
  averaged)** per window: each record carries its spend-weighted share of the
  window's delta, so the sum is the total non-fleet burn on the account, and
  the offset must reserve all of it, whoever produced it.
- EMA-smoothed under the reserved model key `<exogenous>` (one entry per
  window). The key is bookkeeping: it never surfaces in `by_model`, never as
  a model row anywhere.
- Over `hours_remaining`, the offset reserves `exogenous_pct_hr ×
  hours_remaining` percent as already spent: `effective_utilization =
  current_utilization + reserve`, and everything downstream (`remaining_pct`,
  margins, exhaustion headroom, `safe_worker_count`, `hard_limit_*`) plans
  against the **net** budget.
- The offset **never multiplies into `pct_per_worker_per_hour`** and **never
  scales with worker count**. Adding a worker does not burn the operator's
  share faster; removing one does not free it.
- An interval with **no positive exogenous observation leaves the baseline
  holding** — an idle operator must not decay a baseline the attribution
  scan simply had nothing to say about this pass.
- A window reset clears fleet EMA calibration for that window but
  **deliberately preserves the exogenous baseline**: those percentage units do
  not change when a window rolls.
- The reported `current_utilization` stays the measured account fact; only
  the *budget* is net of the reserve.

Consequence to internalize: an operator-only account shows reduced (possibly
zero) remaining budget on every window **while `fleet_pct_per_hour` stays 0
and `cutoff_risk` stays false** — there is no fleet burn to govern, so the
fleet can pose no cutoff risk. Exhaustion at the hands of the operator is not
fleet exhaustion and must not scale the pool; the fleet simply yields the net
budget (often 0 on long windows) without alarming.

## Where each half runs

Two seams carry these semantics; both are covered by
`tests/operator_session_no_scale_to_zero.rs`:

1. **The attribution pipeline** — `burn_rate::estimate_burn_rates`: record
   classification, fleet stats, per-worker EMA, exogenous baseline, and the
   offset-aware forecasts
   (`generate_window_forecast_with_exogenous`). The plain
   `generate_window_forecast` delegates with an offset of 0.0, so fleet-only
   forecast math is byte-identical to pre-split behaviour.
2. **The daemon's per-cycle seams** — `governor::effective_fleet_pct_rate`
   pins the fleet rate to **0.0 whenever `current_total == 0`** (the
   claudego-1942b4ea zero-worker guard: the API reading is account-wide, so
   an idle fleet must report zero *fleet* burn no matter what the account is
   doing), and `governor::per_worker_pct_for_sizing` falls back to the
   configured baseline at zero workers so the pool can still start. Those two
   functions are what `cgov forecast`'s CUTOFF_RISK / safe-worker output
   actually flows through while the fleet is idle.

## The verification clause this pins

With zero NEEDLE workers and an active operator Claude Code session:

- `cgov forecast` must not report fleet CUTOFF_RISK, and
- `safe_worker_count` for a window must be >= 1 whenever **real headroom** —
  net of the operator's own reservation — supports one worker for the
  remaining window.

The clause is conditional on real headroom on purpose: when the operator's
own measured rate already exceeds a window (e.g. 1.3%/hr against a 7d window
with 90h left), the net budget floors to 0 and authorizing 0 workers is the
*correct* outcome — it must simply never be reported as fleet CUTOFF_RISK.

Companion invariant: genuine fleet-attributable burn must still trip
CUTOFF_RISK, so the split cannot over-correct into ignoring the pool's own
overspend.
