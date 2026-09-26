# Hysteresis and Smooth Scaling Transitions in Claude Governor

## Overview

This document describes the hysteresis implementation and scaling behavior in Claude Governor (cgov): current behavior, the convergence guarantees, and the improvements that remain open — each of which is tracked as a bead (see [Remaining Improvements](#remaining-improvements-not-implemented)).

## Current Implementation

### Asymmetric Hysteresis Band

**Location**: `src/governor.rs`, function `apply_scaling()`

The hysteresis band prevents oscillation by damping **scale-down only**. A deficit (target above current) of any size — including 1 worker — is always closed:

```rust
pub fn apply_scaling(
    target: u32,
    current: u32,
    hysteresis_band: f64,
    max_up_per_cycle: u32,
    max_down_per_cycle: u32,
    emergency_brake_active: bool,
) -> ScalingDecision {
    // Emergency brake: a real >= 98% usage window drove the target to 0. A
    // computed Some(0) verdict with no such window falls through to the
    // graceful scale-down below — that is a duty-cycle withdrawal, not an
    // emergency (claudego-1138ab78).
    if emergency_brake_active && target == 0 && current > 0 {
        return ScalingDecision::EmergencyBrake;
    }

    let delta = target as i32 - current as i32;

    // At target — nothing to do.
    if delta == 0 {
        return ScalingDecision::NoChange;
    }

    if delta < 0 {
        let hysteresis = hysteresis_band as i32;

        // The band applies here and only here: a target within the band
        // below current holds, so forecast noise cannot shed workers.
        // (Manual overrides pass bypass_scale_down_hysteresis = true.)
        if delta.abs() <= hysteresis {
            return ScalingDecision::NoChange;
        }

        let scale = delta.unsigned_abs().min(max_down_per_cycle);
        return ScalingDecision::ScaleDown(scale);
    }

    // delta > 0: a deficit of any size closes — the band never damps scale-up.
    let scale = (delta as u32).min(max_up_per_cycle);
    ScalingDecision::ScaleUp(scale)
}
```

This is the decision flow of `apply_scaling_with_policy` (in `src/governor.rs`), which `apply_scaling` delegates to with `bypass_scale_down_hysteresis = false`.

**Why asymmetric**: cgov is a use-or-lose subscription governor. Capacity below target is capacity that resets unused, so every deficit closes immediately — a symmetric band turned a 1-worker deficit into a permanent strand one worker short of target (with band 1.0 and integer worker counts, `|delta| == 1` was never actionable). A surplus above the soft target, by contrast, is tolerated up to the band: the forecast jitters, an extra worker burns quota productively, and the hard protection (the emergency brake, a window actually at/above 98%) still overrides regardless — including a computed `safe_worker_count = Some(0)`, which without such a window is an ordinary band-damped withdrawal (claudego-1138ab78). The scale-down band is the noise cushion; the scale-up path is the convergence guarantee.

**Configuration**: the live file is `~/.config/claude-governor/governor.yaml` (or
`$XDG_CONFIG_HOME/claude-governor/governor.yaml`) — machine-specific, not in the
repo. `config/governor.yaml` in this repo is only the checked-in **seed template**:
`src/config.rs` bakes it into the binary with `include_str!` and copies it to the
live path when no config exists (`GovernorConfig::config_paths`,
`GovernorConfig::create_default_config`). Hosts edit tuning keys in the live file,
so it drifts from the template by design — treat the values below as **template
defaults, not running values**. Read what is actually running with `cgov config`,
which prints the config file it loaded and every `daemon` key.

Template defaults (`config/governor.yaml`):
```yaml
daemon:
  hysteresis_band: 1.0          # Scale-down damping band
  max_scale_up_per_cycle: 1     # Maximum workers to add per cycle
  max_scale_down_per_cycle: 1   # Maximum workers to remove per cycle
  progressive_scaling: false    # Widen per-cycle caps with the gap (see below)
  min_scale_interval_secs: 60   # Minimum time between scale operations
  loop_interval_secs: 300       # 5-minute polling cycle
```

### Safe Mode Hysteresis

Safe mode activation also uses hysteresis to prevent flapping:

**Entry threshold**: 15% median absolute error
**Exit threshold**: 8% median absolute error (hysteresis gap)
**Hysteresis multiplier**: 2.0x (widens the hysteresis band during safe mode)

```rust
// Lines 46-59 in governor.rs
const SAFE_MODE_ENTRY_ERROR_THRESHOLD: f64 = 15.0;
const SAFE_MODE_EXIT_ERROR_THRESHOLD: f64 = 8.0;  // Hysteresis gap
const SAFE_MODE_HYSTERESIS_MULTIPLIER: f64 = 2.0;
```

The widened band also damps scale-down only; safe mode never holds back a deficit.

### Target Worker Computation

**Location**: `src/governor.rs`, function `compute_target_workers()`

The target worker count is computed from:

1. **Emergency brake check**: Any window ≥ 98% → target = 0
2. **Binding window selection**: Five-hour, seven-day, or weekly-scoped
3. **Cone-based scaling**:
   - Narrow cone (low uncertainty) → use p50 median estimate
   - Wide cone (high uncertainty) → use p75 conservative estimate
4. **Composite risk optimization** (optional): Balances risk across all windows
5. **Min/max bounds**: Respects per-agent configured limits

## Current Behavior Analysis

### Scaling Rate Limits

Per-cycle rate limiting is binary by default (1 worker up, 1 down per 5-minute cycle), or **progressive** when `progressive_scaling: true` (see below).

### Example: Large Scale-Up (converges)

**Scenario**: Current = 5 workers, Target = 10 workers, default caps

| Cycle | Workers | Delta | Action |
|-------|---------|-------|--------|
| 0     | 5       | +5    | Scale up 1 → 6 |
| 1     | 6       | +4    | Scale up 1 → 7 |
| 2     | 7       | +3    | Scale up 1 → 8 |
| 3     | 8       | +2    | Scale up 1 → 9 |
| 4     | 9       | +1    | Scale up 1 → 10 |
| 5     | 10      | 0     | At target → NoChange |

**Result**: Takes 25 minutes and **ends at 10**. The old symmetric band stopped the sequence at 9 forever (`|10 - 9| = 1 <= 1` was never actionable) — that was the convergence bug fixed in claudego-44b1f4f5.

### Example: Gradual Scale-Down (holds one above the soft target)

**Scenario**: Current = 10 workers, Target = 2 workers, band 1.0, default caps

| Cycle | Workers | Delta | Action |
|-------|---------|-------|--------|
| 0     | 10      | -8    | Scale down 1 → 9 (gap 8 > band) |
| 1     | 9       | -7    | Scale down 1 → 8 |
| ...   | ...     | ...   | ... |
| 7     | 3       | -1    | Within band → NoChange |
| 8     | 3       | -1    | NoChange (band) |

**Result**: Takes 40 minutes to reach 3, then **holds one above the soft target of 2 by design**. The last worker of surplus is the down-side cushion: a target sitting one below current is within forecast noise, and shedding a worker on that signal is exactly what the band exists to prevent. If a window genuinely hits ≥98%, the emergency brake forces the fleet down regardless of the band; a computed `safe_worker_count` of 0 without one is a duty-cycle withdrawal that respects the band and the per-cycle cap like any other surplus (claudego-1138ab78).

### Example: Progressive Scale-Up

**Scenario**: Current = 5, Target = 15, `progressive_scaling: true`, base cap 3

| Cycle | Gap | Tier | Effective cap (min(3×tier, gap)) | Action |
|-------|-----|------|----------------------------------|--------|
| 0     | 10  | 3x (gap > 5)  | min(9, 10) = 9 | Scale up 9 → 14 |
| 1     | 1   | 1x            | min(3, 1) = 1  | Scale up 1 → 15 |
| 2     | 0   | —             | —              | At target → NoChange |

**Result**: 2 cycles (10 minutes) instead of 10. `progressive_scale_cap` widens the configured cap by the remaining gap — 3x when the gap exceeds 5, 2x when it exceeds 3, 1x otherwise — always clamped to the gap itself, so a cycle never overshoots the target and the operator's cap still bounds the base rate.

## Design Notes

### The Band Is a Down-Side Cushion, Not a Dead Zone

The band must never create a gap at which the fleet stops converging. With integer worker counts, any symmetric band ≥ 1.0 makes the smallest meaningful correction (1 worker) permanently invisible — that is the failure this design removes. Invariants:

1. **Every deficit closes.** `target > current` ⇒ `ScaleUp` in the next act cycle (bounded by the per-cycle cap).
2. **Surplus up to `hysteresis_band` is held.** Forecast noise of one worker must not shed workers.
3. **No overshoot.** Scale amount is `min(|delta|, cap)` — never past the target.
4. **Hard overrides still fire.** `EmergencyBrake` (target 0) bypasses band and caps; per-agent min/max bounds are applied by the executor.

### Anti-Oscillation Under the Asymmetric Band

A target jittering ±1 around the fleet produces no churn: the first deficit closes (fleet at 6, say), after which a target of 5 sits inside the down-band and holds. Churn requires the target itself to swing by more than the band — the same condition the symmetric band required.

## Remaining Improvements (Not Implemented)

Both open items are tracked as beads. They are serialized (`claudego-71a82180` blocked by `claudego-b9195f5a`) because both touch `src/governor.rs` and `src/config.rs` and extend the same test file.

### Option B: Exponential Decay — [claudego-b9195f5a]

Close a fixed fraction of the remaining gap per cycle (e.g. 30%), for a smooth asymptotic approach. The progressive tier function already approximates this for large gaps; true exponential scaling remains a proposal.

### Option C: Adaptive Timing — [claudego-71a82180]

Shorten the polling interval while far from target (e.g. 1/3 interval when gap > 5). Faster convergence without touching per-cycle limits. Not implemented — the 300s loop interval is also the burn-rate sampling granularity (see `DaemonConfig::loop_interval_secs`), so the design must keep observation/sampling on the fixed cadence or explicitly test the changed semantics.

## Configuration Examples

These examples are edits to the live `~/.config/claude-governor/governor.yaml`
(machine-specific). The repo's `config/governor.yaml` is only the seed template
the binary is built with; loader precedence lives in `src/config.rs`
(`GovernorConfig::config_paths`). Verify applied values with `cgov config`.

### Default (binary per-cycle caps)

```yaml
daemon:
  hysteresis_band: 1.0
  max_scale_up_per_cycle: 1
  max_scale_down_per_cycle: 1
  progressive_scaling: false
  loop_interval_secs: 300  # 5 minutes
```

### Progressive Scaling

```yaml
daemon:
  hysteresis_band: 1.0
  max_scale_up_per_cycle: 3      # base rate; widened by the gap when enabled
  max_scale_down_per_cycle: 3
  progressive_scaling: true       # 3x cap when gap > 5, 2x when gap > 3, never past target
  loop_interval_secs: 300
```

## Testing Strategy

**Test file**: `tests/hysteresis_smooth_scaling_test.rs` — the unit/integration floor:

- Band edge cases, both directions (`test_hysteresis_exact_threshold[_scale_down]`, `test_hysteresis_wide_band`, `test_very_large_hysteresis_band`)
- Convergence to target (`test_smooth_scale_up_sequence` — the 5→10 table ends at 10; `test_scale_up_converges_from_one_short` — the regression in one line)
- Down-side damping (`test_hysteresis_scale_down_within_band`, `test_smooth_scale_down_sequence`)
- Progressive caps: tiers, gap clamp, disabled cap (`test_progressive_scale_cap_tiers_and_clamps`), convergence speed (`test_progressive_scaling_converges_faster_and_exactly`)
- Anti-oscillation under jitter (`test_hysteresis_prevents_oscillation`)
- Emergency brake override (`test_emergency_brake_bypasses_hysteresis`)

End-to-end wiring through a real act cycle (decision-log entries for a closed deficit and a suppressed at-target hold) lives in `tests/explain_decisions_test.rs` (`act_cycle_closes_one_worker_deficit_despite_band`, `act_cycle_at_target_records_nothing`).

### The CLAUDE.md §4 allocation guarantees — named tests at each boundary

The four allocation guarantees this design rests on are each pinned by a named
test that fails on revert; `tests/governor_scaling_fixes.rs` documents the
mutation-checked one-to-one mapping back to CLAUDE.md §4, and
`tests/scaling_invariants_test.rs` sweeps the parameter space each invariant
quantifies over. This section is the doc→test pointer so the linkage stays
bidirectional (the test files cite this doc as their source of truth):

| Guarantee (CLAUDE.md §4) | Regression pin | Boundary / sweep |
|---|---|---|
| `distribute_workers_by_cost_priority` guarantees each agent's `min_workers` floor before cost-distributing the remainder | `governor_scaling_fixes.rs::min_workers_floor_expensive_pool_wins_slot_regression` | `src/governor.rs` `distribute_enforces_min_workers_for_expensive_pool` (unit); `scaling_invariants_test.rs::invariant_4_min_workers_floor_survives_cost_priority` (floor funded out of the cheap pool's cost-priority share) |
| The `NoChange` arm still reconciles the per-agent allocation at an unchanged aggregate total — a pinned pool launches at a steady total | `governor_scaling_fixes.rs::no_change_still_reconciles_per_agent_allocation_regression` (live executor: the below-floor pool launches while the total holds) | `scaling_invariants_test.rs::invariant_6_nochange_cycle_still_rewrites_per_agent_targets` (persisted per-agent targets; the executor half rides on the same distribution call pinned above) |
| `safe_worker_count_or_hold` (named `safe_worker_count_or_max` in §4) maps `Some(0)` → 0, so an unaffordable binding window actually scales to 0 | `governor_scaling_fixes.rs::safe_worker_count_some_zero_scales_to_zero_regression` | `src/governor.rs` `safe_worker_count_some_zero_scales_to_zero` and the `None`-holds siblings (unit); `scaling_invariants_test.rs::invariant_5_safe_count_zero_targets_zero_workers` (incl. the configured-floor clamp) |
| A computed `Some(0)` verdict without a real ≥98% window is a band-damped duty-cycle withdrawal, not an `EmergencyBrake` | `src/governor.rs` `computed_zero_without_brake_window_takes_graceful_scale_down` / `computed_zero_with_brake_window_still_takes_emergency_brake` | `emergency_brake_distinction_test.rs` (98.0% threshold boundary: `brake_engages_at_exactly_98_percent` / `brake_stays_off_just_below_98_percent`; classification under a real cycle: `computed_zero_without_a_brake_window_is_a_graceful_scale_down` / `same_zero_target_with_a_real_98_window_takes_the_brake`); `scaling_invariants_test.rs::invariant_2_computed_zero_without_brake_window_scales_down_gracefully` |

## Safety Considerations

### Hysteresis Placement

The band is applied to scale-down only, inside `apply_scaling`:

```rust
if delta < 0 {
    if delta.abs() <= hysteresis {
        return ScalingDecision::NoChange;   // down-side cushion
    }
    return ScalingDecision::ScaleDown(...);
}
// delta > 0 always scales up
```

### Emergency Brake Override

The emergency brake — a usage window at/above 98% on the last polled snapshot — always bypasses hysteresis and rate limits. A computed target of 0 *without* such a window is an ordinary graceful scale-down (claudego-1138ab78):

```rust
if emergency_brake_active && target == 0 && current > 0 {
    return ScalingDecision::EmergencyBrake;  // Immediate scale to 0
}
```

### Min/Max Bounds

Per-agent min/max bounds are always respected by the executor:

```rust
let new_count = (current as i32 + scale_delta)
    .max(agent.min as i32)
    .min(agent.max as i32) as u32;
```

## References

- **Source code**: `src/governor.rs` (`apply_scaling`, `progressive_scale_cap`, `run_act_cycle` step 5)
- **Configuration**: live `~/.config/claude-governor/governor.yaml` (machine-specific; verify with `cgov config`), seeded from the checked-in template `config/governor.yaml` (`daemon.progressive_scaling`); loader: `src/config.rs` (`DaemonConfig`, `GovernorConfig::config_paths`)
- **Tests**: `tests/hysteresis_smooth_scaling_test.rs`, `tests/explain_decisions_test.rs`, `tests/governor_cycle_snapshot_test.rs`; the CLAUDE.md §4 guarantee layer — `tests/governor_scaling_fixes.rs` (one named test per §4 fix), `tests/scaling_invariants_test.rs` (one enumerative test per invariant), `tests/emergency_brake_distinction_test.rs` (brake threshold and computed-zero classification), and the `#[cfg(test)]` module in `src/governor.rs` (see the table above)
- **Related modules**: `src/burn_rate.rs`, `src/worker.rs`, `src/calibrator.rs`

## Version History

- **2026-08-23**: Initial documentation of hysteresis and scaling behavior
- Identified gaps in smooth scaling transitions
- Proposed progressive, exponential, and adaptive improvements
- **2026-09-16** (claudego-44b1f4f5): Hysteresis made **asymmetric** — the band damps scale-down only, so every deficit closes and the fleet converges exactly to target (the 5→10 example no longer stops at 9). Added opt-in **progressive scaling** (`daemon.progressive_scaling` + `progressive_scale_cap`): per-cycle caps widen with the remaining gap (3x/2x/1x tiers), clamped to the gap. Decision-log trigger text updated to match; exponential and adaptive-timing options remain open proposals.
- **2026-09-24** (claudego-4c0f2ae9): Tracked the two open proposals as beads — Option B → claudego-b9195f5a, Option C → claudego-71a82180 (blocked by B; shared `governor.rs`/`config.rs`/test-file footprint). Synced the "Current Implementation" snippet with the live `apply_scaling` signature (`emergency_brake_active` param; the brake fires only on a real ≥98% window, not on a computed zero).
- **2026-09-25** (claudego-3648483e): Resolved the canonical config-path reference. This doc previously headed its tuning keys with `config/governor.yaml` as if that were the configuration; that file is the checked-in seed template (`include_str!`-baked in `src/config.rs`), while the live machine config (`~/.config/claude-governor/governor.yaml`) is authoritative and drifts from the template by design. The Configuration block now names both roles and points live-value verification at `cgov config`; CLAUDE.md §1 says the same.
- **2026-09-26** (claudego-e76bcced): Linked the CLAUDE.md §4 allocation-guarantee test layer into this doc. The four §4 guarantees were already pinned (one regression test per fix in `tests/governor_scaling_fixes.rs`, one enumerative test per invariant in `tests/scaling_invariants_test.rs`, the 98%-threshold boundary in `tests/emergency_brake_distinction_test.rs`, unit boundaries in `src/governor.rs`) — the Testing Strategy section now maps each guarantee to its pins so the doc↔test linkage is bidirectional, and the References list names the three suites. CLAUDE.md §4's `safe_worker_count_or_max` bullet now notes the function's current name, `safe_worker_count_or_hold`.
