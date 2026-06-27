# Safe-Mode Warning Implementation - Code Documentation

## Overview
Safe mode is a protective state in cgov that activates when prediction accuracy degrades or emergency conditions occur. This document traces the complete code flow for safe mode detection, warning generation, and reassertion behavior.

## Safe Mode Detection and Entry

### File: `src/governor.rs`
### Function: `update_safe_mode_from_calibration()` (lines 1098-1156)

**Location:** `/home/coding/claude-governor/src/governor.rs:1098-1156`

**Entry Conditions:**
- Median absolute error > `SAFE_MODE_ENTRY_ERROR_THRESHOLD` (15.0 pct points)
- Minimum samples >= `SAFE_MODE_MIN_SAMPLES` (5 samples)
- Checked in governor daemon loop at line 1871-1880

**Entry Code:**
```rust
// Line 1134-1143
if median_error_abs > SAFE_MODE_ENTRY_ERROR_THRESHOLD
    && stats.total_samples >= SAFE_MODE_MIN_SAMPLES
{
    log::warn!(
        "[governor] safe_mode enter: median_error={:.2} > entry_threshold={:.1}, \
         samples={}",
        median_error_abs,
        SAFE_MODE_ENTRY_ERROR_THRESHOLD,
        stats.total_samples,
    );
    *safe_mode = state::SafeModeState {
        active: true,
        entered_at: Some(now),
        trigger: Some("median_error".to_string()),
        median_error_at_entry: Some(median_error_abs),
        predictions_since_entry: 0,
        scored_at_entry: stats.total_samples,
    };
}
```

**Exit Conditions:**
- Median absolute error < `SAFE_MODE_EXIT_ERROR_THRESHOLD` (8.0 pct points)
- Predictions since entry >= `SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT` (3 predictions)
- Hysteresis gap: 15.0 entry vs 8.0 exit prevents rapid toggling

**Exit Code:**
```rust
// Line 1117-1128
if median_error_abs < SAFE_MODE_EXIT_ERROR_THRESHOLD
    && safe_mode.predictions_since_entry >= SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT
    && stats.total_samples >= SAFE_MODE_MIN_SAMPLES
{
    log::info!(
        "[governor] safe_mode exit: median_error={:.2} < exit_threshold={:.1}, \
         predictions_since_entry={}",
        median_error_abs,
        SAFE_MODE_EXIT_ERROR_THRESHOLD,
        safe_mode.predictions_since_entry,
    );
    *safe_mode = state::SafeModeState::default();
}
```

### Emergency Brake Safe Mode Entry

**Location:** `/home/coding/claude-governor/src/governor.rs:2374-2376`

When any window hits 98%+ utilization (emergency brake):
```rust
state.safe_mode.active = true;
state.safe_mode.trigger = Some("emergency_brake".to_string());
state.safe_mode.entered_at = Some(now);
```

## Safe Mode Warning Log Message

### File: `src/main.rs`
### Function: `run_scale_command()` (lines 541-603)

**Warning Generation Location:** `/home/coding/claude-governor/src/main.rs:549-564`

**Code:**
```rust
// Check if safe mode is active
if state.safe_mode.active {
    log::warn!("[governor] WARN: manual scale override during safe mode");

    // Also write directly to log file for persistence
    let log_path = default_log_path();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let log_line = format!(
            "{} [governor] WARN: manual scale override during safe mode\n",
            Utc::now().to_rfc3339()
        );
        let _ = file.write_all(log_line.as_bytes());
    }
}
```

**Two Warning Methods:**
1. **Logger (line 550):** Uses `log::warn!()` macro - appears in structured logs
2. **Direct file write (lines 554-563):** Writes to `governor.log` with timestamp for persistence

## Stdout Notification About Safe Mode Reassertion

**Location:** `/home/coding/claude-governor/src/main.rs:599-600`

**Code:**
```rust
// Warn user that safe mode will reassert on next cycle
if safe_mode_was_active {
    println!("NOTE: Safe mode remains active and will reassert its target on the next cycle");
}
```

**Context:** This message appears AFTER the manual scale is applied, informing the user that their change is temporary.

## Safe Mode Reassertion Mechanism

### How Safe Mode Overrides Manual Scale

**Location:** `/home/coding/claude-governor/src/governor.rs:2433-2446`

When `cgov scale` sets a worker target during safe mode:
1. The manual target is saved to state (`state.workers.target = count`)
2. Safe mode remains active (`state.safe_mode.active = true`)
3. On the next daemon cycle, the governor computes a new target based on safe mode parameters
4. The computed target overrides the manual setting

**Reassertion Code:**
```rust
// Lines 2433-2446 - Even with NoChange decision, targets are updated
ScalingDecision::NoChange => {
    // Still update target to reflect current desired state using priority distribution
    let target_distribution = distribute_workers_by_cost_priority(
        agents,
        &current_workers_map,
        effective_target,  // ← Uses safe-mode-adjusted target
        &state.burn_rate.by_model,
        pricing_config,
        cutoff_risk,
    );
    for (agent_name, ws) in state.workers.iter_mut() {
        ws.target = *target_distribution.get(agent_name).unwrap_or(&ws.current);
    }
}
```

## Safe Mode Effects on Governor Decisions

**Location:** `/home/coding/claude-governor/src/governor.rs:1888-1935`

When safe mode is active, the governor applies conservative overrides:

### 1. Reduced Target Ceiling
```rust
// Lines 1888-1898
let effective_target_ceiling = if state.safe_mode.active {
    let reduced = target_ceiling - SAFE_MODE_CEILING_REDUCTION;  // -5 pct points
    log::info!(
        "[governor] safe_mode active: target_ceiling {:.0}% → {:.0}%",
        target_ceiling,
        reduced
    );
    reduced.max(50.0)  // never below 50%
} else {
    target_ceiling
};
```

### 2. Widened Hysteresis Band
```rust
// Lines 1900-1910
let effective_hysteresis = if state.safe_mode.active {
    let widened = hysteresis_band * SAFE_MODE_HYSTERESIS_MULTIPLIER;  // 2x
    log::info!(
        "[governor] safe_mode active: hysteresis_band {:.1} → {:.1}",
        hysteresis_band,
        widened
    );
    widened.min(10.0)  // cap at 10 pct points
} else {
    hysteresis_band
};
```

### 3. Disabled Composite Risk
```rust
// Lines 1915-1923
let effective_composite_risk: &CompositeRiskConfig = if state.safe_mode.active {
    safe_composite_risk = CompositeRiskConfig {
        enabled: false,  // ← Disables cross-window optimization
        ..composite_risk_config.clone()
    };
    &safe_composite_risk
} else {
    composite_risk_config
};
```

### 4. Conservative Cone Scaling
```rust
// Lines 1927-1935
let effective_cone_scaling: &ConeScalingConfig = if state.safe_mode.active {
    safe_cone_scaling = ConeScalingConfig {
        narrow_threshold: 0.0,  // Always uses p75 (conservative)
    };
    &safe_cone_scaling
} else {
    cone_scaling_config
};
```

## Code Path Summary: `cgov scale` During Safe Mode

1. **Entry:** `cgov scale 10` (main.rs:947)
2. **Load state:** `state::load_state()` (main.rs:543)
3. **Check safe mode:** `if state.safe_mode.active` (main.rs:549)
4. **Log warning:** `log::warn!("[governor] WARN: manual scale override during safe mode")` (main.rs:550)
5. **Write to log file:** Direct append to governor.log (main.rs:554-563)
6. **Validate count:** Check against worker min/max (main.rs:568-578)
7. **Apply scale:** `state.workers.values_mut().target = count` (main.rs:589-591)
8. **Save state:** `state::save_state()` (main.rs:594)
9. **Print confirmation:** "Target worker count set to 10 for all agents" (main.rs:596)
10. **Warn about reassertion:** "NOTE: Safe mode remains active and will reassert its target on the next cycle" (main.rs:600)

## Safe Mode Constants

**Location:** `/home/coding/claude-governor/src/governor.rs:40-56`

```rust
const SAFE_MODE_ENTRY_ERROR_THRESHOLD: f64 = 15.0;
const SAFE_MODE_EXIT_ERROR_THRESHOLD: f64 = 8.0;
const SAFE_MODE_MIN_SAMPLES: u32 = 5;
const SAFE_MODE_MIN_PREDICTIONS_FOR_EXIT: u32 = 3;
const SAFE_MODE_CEILING_REDUCTION: f64 = 5.0;
const SAFE_MODE_HYSTERESIS_MULTIPLIER: f64 = 2.0;
```

## Safe Mode State Structure

**Location:** `/home/coding/claude-governor/src/state.rs:475-498`

```rust
pub struct SafeModeState {
    pub active: bool,
    pub entered_at: Option<DateTime<Utc>>,
    pub trigger: Option<String>,  // "median_error" or "emergency_brake"
    pub median_error_at_entry: Option<f64>,
    pub predictions_since_entry: u32,
    pub scored_at_entry: u32,
}
```

## Test Implications

The following code paths should be tested:

1. **Entry trigger:** Median error exceeds 15.0 with ≥5 samples
2. **Exit condition:** Median error drops below 8.0 with ≥3 new predictions
3. **Warning log:** `cgov scale` during safe mode generates warning
4. **Log file persistence:** Direct write to governor.log succeeds
5. **Stdout notification:** Reassertion message displayed
6. **Target reassertion:** Manual target overridden on next daemon cycle
7. **Conservative parameters:** Ceiling reduced, hysteresis widened, composite risk disabled
8. **Emergency brake entry:** 98%+ utilization triggers safe mode

## File Reference Summary

| Component | File | Lines |
|-----------|------|-------|
| Entry/exit detection | `src/governor.rs` | 1098-1156 |
| Warning log generation | `src/main.rs` | 549-564 |
| Stdout reassertion warning | `src/main.rs` | 599-600 |
| Target reassertion logic | `src/governor.rs` | 2433-2446 |
| Conservative ceiling | `src/governor.rs` | 1888-1898 |
| Widened hysteresis | `src/governor.rs` | 1900-1910 |
| Disabled composite risk | `src/governor.rs` | 1915-1923 |
| Conservative cone scaling | `src/governor.rs` | 1927-1935 |
| Emergency brake entry | `src/governor.rs` | 2374-2376 |
| SafeModeState struct | `src/state.rs` | 475-498 |
| Safe mode constants | `src/governor.rs` | 40-56 |
