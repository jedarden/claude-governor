# Production Governor Path for Window EMA Updates

**Bead:** bf-14umd (Child 1 of 4 from bf-2kexl split)  
**Date:** 2026-07-27  
**Status:** INVESTIGATION ONLY — No code changes

## Executive Summary

This document verifies and documents the ACTUAL production code paths for window EMA (Exponential Moving Average) updates in the Claude Governor. It confirms the location of per-window EMA updates, forecast generation, and identifies dead code.

## 1. Production Code Paths

### 1.1 Per-Window EMA Updates (Inline)

**Location:** `src/governor.rs:4023-4141`

The governor updates per-window EMA values **inline during the reconcile cycle** by computing percentage deltas between consecutive API readings. This is the production path — NOT through `estimate_burn_rates`.

**Key function:** Inline EMA update logic in `governor.rs::reconcile_workers`

```rust
// Lines 4024-4032: Documentation explaining the approach
// 5-pre. Update fleet_pct_hr_ema from consecutive API reading deltas.
//
// The fleet record's p5h/p7d/p7ds fields are always null (the collector writes them
// null and never fills them in), so dividing them by elapsed_hours always yields 0.
// Instead we compute pct_hr from the delta between consecutive poller readings,
// applying an EMA that is only updated on positive deltas — zero-delta cycles
// (when the API percentage hasn't moved in the past N seconds) are skipped so
// they can't drive the EMA down to zero.
```

**Process:**
1. Save old snapshot from previous cycle (line 4034)
2. Compute elapsed hours between snapshots (lines 4047-4048)
3. Calculate window deltas via `calculate_window_pct_delta` (line 4063)
4. For each window with positive delta:
   - Compute rate = delta / elapsed_hours
   - If samples == 0: initialize EMA directly (lines 4078-4079, 4098-4099, 4118-4119)
   - If samples > 0: apply EMA formula (lines 4081-4082, 4101-4102, 4121-4123)
   - Update USD-per-pct ratio in parallel (lines 4084-4092, 4104-4112, 4125-4134)
5. Increment sample counter when any window updated (lines 4139-4141)

**EMA Formula:**
```rust
EMA_ALPHA = 0.2
new_ema = EMA_ALPHA * rate + (1.0 - EMA_ALPHA) * old_ema
```

### 1.2 Forecast Generation

**Location:** `src/burn_rate.rs:1169-1260`  
**Called from:** `src/governor.rs:4607`

**Function:** `generate_window_forecast`

**Purpose:** Convert EMA burn rate into a capacity forecast (predicted exhaustion time, safe worker counts, confidence cone)

**Inputs:**
- `window`: "five_hour" | "seven_day" | "weekly_scoped"
- `fleet_pct_hr`: Fleet burn rate (%/hr) from EMA or fallback
- `current_utilization`: Current % used from API
- `target_ceiling`: Cap (usually 100%)
- `hours_remaining`: Time until reset
- `mean_rate_per_worker`: Per-worker burn rate (pct/hr)
- `std_pct_hr`: Standard deviation for confidence cone

**Outputs:** `WindowForecast` struct with:
- `predicted_exhaustion_hours`: When the window will exhaust
- `safe_worker_count`: p50 estimate (mean rate)
- `safe_worker_count_p75`: p75 estimate (fast-burn, more conservative)
- `cone_ratio`: Uncertainty metric (p75/p25 hours ratio)

**Key Formula (burn_rate.rs:1180-1184):**
```rust
predicted_exhaustion_hours = if fleet_pct_hr > 0.0 {
    remaining_pct / fleet_pct_hr
} else {
    f64::INFINITY  // Zero burn = infinite headroom
};
```

### 1.3 Dead Code: `estimate_burn_rates`

**Location:** `src/burn_rate.rs:1276-1461`

**Status:** **DEAD CODE — Test only**

**Evidence:**
- No calls in `governor.rs` (grep returns zero results)
- All call sites in `burn_rate.rs` are AFTER line 1739: `#[cfg(test)]`
- Call sites at lines: 2475, 2523, 2561, 2607, 2681, 2779, 2854, 2893, 2951, 3013, 3068

**Conclusion:** `estimate_burn_rates` is **never called in production**. It's a legacy function used only in unit tests. The production governor uses the inline EMA update path (section 1.1) instead.

---

## 2. Cold-Start Behavior

### 2.1 What is Cold Start?

A window is in **cold start** when:
- `state.burn_rate.fleet_pct_ema_samples == 0` (no EMA samples yet)
- No fresh per-instance rate this interval
- The window may have been newly created or just appeared in the API

### 2.2 Burn Rate on Cold Start (0 Samples)

**Tiered Fallback Chain (governor.rs:4369-4422):**

The governor uses a priority-ordered fallback system:

1. **(A) API EMA** — when `samples >= MIN_SAMPLES_FOR_EMA` (3)
   - Use `state.burn_rate.fleet_pct_hr_ema.{five_hour|seven_day|weekly_scoped}`
   - Most accurate after calibration

2. **(B) Dollar fallback with learned ratio** — when `samples < 3` but `usd_per_pct_ema > 0`
   - `pct_per_hour = fleet_usd_hr / usd_per_pct_ema`
   - Uses learned USD-per-pct ratio from collector data

3. **(C) Dollar fallback with baseline ratio** — when `samples < 3` and no learned ratio
   - `pct_per_hour = fleet_usd_hr / baseline_usd_per_pct`
   - Falls back to baseline ratio: `baseline.dollars_per_worker_per_hour / baseline.pct_per_worker_per_hour`

4. **(D) Cold-start seeding** — when `samples == 0` AND no dollar data
   - `fleet_pct_per_hour = baseline.pct_per_worker_per_hour * worker_count`
   - **Default baseline:** `1.5 pct/hr per worker` (src/config.rs:95-96)
   - Prevents treating "no data" as "definitely empty"

**Implementation (governor.rs:4408-4420):**
```rust
} else {
    // (D) Cold-start: no samples yet, seed from per-agent baseline_burn_rate config.
    // Use baseline.pct_per_worker_per_hour (default: 1.5 pct/hr) scaled by worker count.
    let fleet_baseline_pct_hr = baseline.pct_per_worker_per_hour * current_total as f64;
    fleet_baseline_pct_hr
}
```

### 2.3 Uncertainty Cone on Cold Start

**StdDev Calculation (governor.rs:4592-4605):**
```rust
let baseline_usd_per_pct = baseline.dollars_per_worker_per_hour / baseline.pct_per_worker_per_hour;
let usd_per_pct = match *window {
    "five_hour" => state.burn_rate.usd_per_pct_ema_five_hour,
    "seven_day" => state.burn_rate.usd_per_pct_ema_seven_day,
    "weekly_scoped" => state.burn_rate.usd_per_pct_ema_weekly_scoped,
    _ => 0.0,
};
let effective_usd_per_pct = if usd_per_pct > 0.0 {
    usd_per_pct
} else {
    baseline_usd_per_pct
};
let std_pct_hr = state.last_fleet_aggregate.sonnet_std_usd_hr / effective_usd_per_pct;
```

**Cone Ratio (burn_rate.rs:1219-1235):**
```rust
let (exh_hrs_p25, exh_hrs_p50, exh_hrs_p75, cone_ratio) = if fleet_pct_hr > 0.0 {
    let rate_fast = (fleet_pct_hr + 0.675 * std_pct_hr).max(MIN_RATE);
    let rate_slow = (fleet_pct_hr - 0.675 * std_pct_hr).max(MIN_RATE);
    
    let p25 = remaining_pct / rate_fast;  // Pessimistic (fast burn)
    let p75 = remaining_pct / rate_slow;  // Optimistic (slow burn)
    
    let ratio = if p25 > 0.0 { p75 / p25 } else { 1.0 };
    (p25, p50, p75, ratio)
} else {
    // Zero burn rate → infinite headroom, cone_ratio = 1.0 (no uncertainty)
    (predicted_exhaustion_hours, predicted_exhaustion_hours, predicted_exhaustion_hours, 1.0)
};
```

**Cold Start Cone Behavior:**
- When `std_pct_hr == 0` (no fleet spread data yet): `cone_ratio = 1.0` (no uncertainty cone)
- When `std_pct_hr > 0`: cone widens proportionally to fleet volatility
- The `rate_fast` / `rate_slow` bounds use ±0.675σ (25th/75th percentiles of normal distribution)

### 2.4 Existing Cold-Start Signal

**Location:** `src/governor.rs:4643-4656`

**EstimateQuality Markers:**
```rust
// Mark estimate quality based on EMA sample count (MIN_SAMPLES_FOR_EMA = 3)
if state.burn_rate.fleet_pct_ema_samples >= 3 && ema_val > 0.0 {
    // Calibrated: enough samples to trust the EMA
    forecast.estimate_quality = state::EstimateQuality::Calibrated;
} else if state.burn_rate.fleet_pct_ema_samples == 0 {
    // ColdStart: no samples yet, rate is seeded from baseline
    forecast.estimate_quality = state::EstimateQuality::ColdStart;
} else {
    // InsufficientSamples: have some data (1-2 samples) but not enough to trust the EMA
    forecast.estimate_quality = state::EstimateQuality::InsufficientSamples;
}
```

**Enum Definition (inferred from usage):**
- `Calibrated` — ≥3 samples, EMA is trusted
- `ColdStart` — 0 samples, rate is seeded from baseline
- `InsufficientSamples` — 1-2 samples, not enough to trust EMA

**Current Usage:**
- The field is plumbed through `WindowForecast` (added in bead bf-n21u3 child 1)
- Governor reads the field but behavior is **not yet divergent** — all paths treated equally
- Future work: use this signal to engage more conservative scaling heuristics

---

## 3. Configuration Constants

### 3.1 EMA Parameters

**Location:** Inline in governor.rs

```rust
const EMA_ALPHA: f64 = 0.2;                    // Line 4036
const MIN_ELAPSED_SECS: f64 = 60.0;           // Line 4038 — min time between delta samples
const MAX_ELAPSED_SECS: f64 = 1800.0;         // Line 4040 — max staleness before snapshot invalid
const MIN_SAMPLES_FOR_EMA: u32 = 3;           // burn_rate.rs:148 — threshold for calibrated estimates
```

### 3.2 Baseline Defaults

**Location:** `src/config.rs:95-100`

```rust
pub fn default_baseline_pct() -> f64 {
    1.5  // pct/hr per worker
}

pub fn default_baseline_dollars() -> f64 {
    5.0  // dollars/hr per worker
}
```

**Derived Baseline Ratio:**
```rust
baseline_usd_per_pct = 5.0 / 1.5 = 3.333... $/pct
```

---

## 4. Data Flow Summary

```
┌─────────────────────────────────────────────────────────────────┐
│ 1. POLLER: Consecutive API Readings                              │
│    - Reads current: five_hour_pct, seven_day_pct, sonnet_pct    │
│    - Compares to prev_usage_snapshot                             │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│ 2. INLINE EMA UPDATE (governor.rs:4023-4141)                    │
│    - calculate_window_pct_delta(old, new) → (d5h, d7d, d7ds)    │
│    - For each positive delta:                                    │
│      rate = delta / elapsed_hours                                │
│      if samples == 0: fleet_pct_hr_ema[window] = rate           │
│      else: fleet_pct_hr_ema[window] = 0.2*rate + 0.8*old_ema    │
│      - Update usd_per_pct_ema in parallel                       │
│      - Increment fleet_pct_ema_samples                          │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│ 3. TIERED FALLBACK (governor.rs:4369-4422)                       │
│    - if samples >= 3: use EMA directly                           │
│    - else if usd_per_pct_ema > 0: use dollar fallback (learned)  │
│    - else if fleet_usd_hr > 0: use dollar fallback (baseline)   │
│    - else: seed from baseline.pct_per_worker_per_hour × workers │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│ 4. FORECAST GENERATION (burn_rate.rs:1169)                     │
│    generate_window_forecast(                                    │
│      window, fleet_pct_hr, util, ceiling, hrs_left,             │
│      pct_per_worker, std_pct_hr                                  │
│    ) → WindowForecast {                                         │
│      predicted_exhaustion_hours,                                 │
│      safe_worker_count (p50),                                    │
│      safe_worker_count_p75 (p75),                                │
│      cone_ratio (uncertainty metric)                            │
│    }                                                             │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│ 5. ESTIMATE QUALITY MARKING (governor.rs:4643-4656)             │
│    - if samples >= 3 && ema_val > 0: Calibrated                 │
│    - else if samples == 0: ColdStart                            │
│    - else: InsufficientSamples                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## 5. Verification Summary

### 5.1 Confirmed Production Paths

| Component | Location | Status |
|-----------|----------|--------|
| Per-window EMA update | `governor.rs:4023-4141` | ✅ Active (inline) |
| Forecast generation | `burn_rate.rs:1169` → `governor.rs:4607` | ✅ Active |
| `estimate_burn_rates` | `burn_rate.rs:1276` | ❌ Dead code (test-only) |

### 5.2 Cold-Start Behavior Confirmed

| Aspect | Behavior | Location |
|--------|----------|----------|
| Burn rate (0 samples) | Tiered fallback → baseline seeding | `governor.rs:4369-4422` |
| Default baseline | `1.5 pct/hr per worker` | `config.rs:95-96` |
| Uncertainty cone | `cone_ratio = 1.0` when `std_pct_hr = 0` | `burn_rate.rs:1229-1235` |
| Cold-start signal | `EstimateQuality::ColdStart` when samples == 0 | `governor.rs:4647-4651` |

### 5.3 Key Findings

1. **Production uses inline EMA updates, NOT `estimate_burn_rates`**
   - `estimate_burn_rates` is only called in test code (all 10 references are in `#[cfg(test)]` module)
   - Governor reconciles directly via `calculate_window_pct_delta` and inline EMA logic

2. **Cold-start has explicit handling**
   - Tiered fallback chain ensures windows always have a burn rate (even at 0 samples)
   - Baseline seeding (1.5 pct/hr per worker) prevents "no data = definitely empty" bug
   - `EstimateQuality::ColdStart` signal exists but not yet behaviorally divergent

3. **Uncertainty cone is conservative on cold start**
   - When `std_pct_hr = 0` (no fleet spread data), `cone_ratio = 1.0` (no uncertainty)
   - As samples accumulate, cone widens proportionally to fleet volatility

---

## 6. Next Steps (Subsequent Children)

This child (bf-14umd) documents the current state. Subsequent children will implement improvements:

- **Child 2 (bf-14ume):** Enhance cold-start uncertainty signaling
- **Child 3 (bf-14umf):** Implement conservative scaling on ColdStart estimates
- **Child 4 (bf-14umg):** Verification and testing

---

## Appendix A: Function Signatures

### `calculate_window_pct_delta`
```rust
// governor.rs (search for function definition)
pub fn calculate_window_pct_delta(
    previous_snapshot: &crate::db::WindowPctSnapshot,
    current_snapshot: &crate::db::WindowPctSnapshot,
) -> (f64, f64, f64)  // (delta_5h, delta_7d, delta_7ds)
```

### `generate_window_forecast`
```rust
// burn_rate.rs:1169
pub fn generate_window_forecast(
    window: &str,
    fleet_pct_hr: f64,
    current_utilization: f64,
    target_ceiling: f64,
    hours_remaining: f64,
    mean_rate_per_worker: f64,
    std_pct_hr: f64,
) -> crate::state::WindowForecast
```

### `estimate_burn_rates` (DEAD CODE)
```rust
// burn_rate.rs:1276
#[allow(clippy::too_many_arguments)]
pub fn estimate_burn_rates(
    instance_records: &[InstanceRecord],
    elapsed_hours: f64,
    current_workers: u32,
    prev_workers: u32,
    ema_state: &mut HashMap<(String, String), ModelWindowEma>,
    baseline: &BaselineBurnRates,
    current_utilization: &HashMap<String, f64>,
    target_ceiling: f64,
    hours_remaining: &HashMap<String, f64>,
) -> (BurnRateEstimate, crate::state::CapacityForecast)
```

---

**End of Document**
