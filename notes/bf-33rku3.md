# Bead bf-33rku3: Replace sonnet_pct with weekly_scoped_pct in EMA calculation

## Task
Replace the hardcoded `sonnet_pct` reference with the model-agnostic `weekly_scoped_pct` in the exponential moving average calculation code.

## Finding
**The fix was already implemented.** The EMA calculation code in `src/governor.rs` is already using the model-agnostic `weekly_scoped_pct` field instead of the legacy `sonnet_pct` field.

## Evidence

### 1. Code at governor.rs:4166
```rust
let new_weekly_scoped = state.usage.weekly_scoped_pct;
```
The code uses `weekly_scoped_pct` from the `UsageState` structure.

### 2. Inline documentation (governor.rs:4162-4165)
```rust
// NOTE: weekly_scoped_pct is the model-agnostic field for weekly_scoped utilization.
// The legacy sonnet_pct field is kept for backward compatibility but should not be used
// in new code. When model identity changes, reset logic above ensures stale samples
// are cleared.
```

### 3. WindowPctSnapshot creation (governor.rs:4180-4189)
```rust
let old_pct = crate::db::WindowPctSnapshot {
    five_hour: snap.five_hour_pct,
    seven_day: snap.seven_day_pct,
    weekly_scoped: snap.weekly_scoped_pct,  // ← correct field
};
let new_pct = crate::db::WindowPctSnapshot {
    five_hour: new_five_hour,
    seven_day: new_seven_day,
    weekly_scoped: new_weekly_scoped,  // ← model-agnostic value
};
```

### 4. Verification logging (governor.rs:4168-4173)
The code includes verification logging that confirms the EMA is using the rotated model's actual pct:
```rust
log::info!(
    "[governor] EMA input: weekly_scoped_model={:?}, weekly_scoped_pct={:.2}% (this is the actual pct from the rotated model)",
    state.usage.weekly_scoped_model,
    new_weekly_scoped
);
```

## Acceptance Criteria Met
- ✅ EMA calculation no longer references `sonnet_pct` for weekly_scoped
- ✅ Uses `weekly_scoped_pct` field from input structure
- ✅ Code compiles without errors (verified with `cargo build --release`)
- ✅ cargo build passes (exit code 0)

## Related Work
Parent bead bf-3g686t (commit 3a0b95f) verified this with:
> "Task verification confirms that weekly_scoped field already exists in the EMA input structure (WindowPctDeltas) and is correctly sourced from the model-agnostic state.usage.weekly_scoped_pct field. No code changes required - feature already implemented."

## Conclusion
The task was already completed as part of the weekly_scoped_pct implementation work. The EMA calculation correctly uses the model-agnostic `weekly_scoped_pct` field, making it work properly when the weekly_scoped model rotates between different Claude models (Sonnet, Opus, Fable, etc.).
