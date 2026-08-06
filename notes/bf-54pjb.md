# bf-54pjb — Verify `calculate_window_pct_delta` call is inside the Some/Some block

**Date:** 2026-08-05
**Scope:** read-only verification, no code changes
**File under review:** `src/governor.rs` (12560 lines)

## 1. In-block call — CONFIRMED

`src/governor.rs:1244`

```rust
1228  pub fn window_deltas_from_snapshots(
1229      previous: Option<&crate::state::PrevUsageSnapshot>,
1230      current: Option<&crate::state::PrevUsageSnapshot>,
1231  ) -> (Option<f64>, Option<f64>, Option<f64>) {
1232      match (previous, current) {
1233          (Some(prev), Some(curr)) => {
1234              let prev_pct = crate::db::WindowPctSnapshot { ... };   // 1234–1238
1239              let curr_pct = crate::db::WindowPctSnapshot { ... };   // 1239–1243
1244              let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
1245              (Some(delta_5h), Some(delta_7d), Some(delta_7ds))
1246          }
1253          _ => (None, None, None),
1254      }
1255  }
```

The call statement at line 1244 matches the target text verbatim and sits inside the
`(Some(prev), Some(curr))` match arm body (lines 1233–1246), after `prev_pct` (1234–1238)
and `curr_pct` (1239–1243) are constructed from the two unwrapped bindings. Both operands
are locals of that arm, so the call cannot be hoisted out of the guard. The catch-all arm
at 1253 returns `(None, None, None)` without calling the function.

## 2. All call sites enumerated (49 occurrences of the identifier)

Test-module boundaries used for classification (top-level `#[cfg(test)]` blocks in
`src/governor.rs`):

| range | module |
|---|---|
| 814–954 | `governor_state_tests` |
| 1287–3451 | `window_delta_tests` |
| 6642–9259 | `tests` |
| 9269–9279, 9281–9479, 9481–9486, 9489–9494 | small `#[cfg(test)]` items |
| 9500–11425 | `mock_poller_tests` |
| 11427–12263 | `annotation_guard_tests` |
| 12265–12560 | `is_structurally_inactive_tests` |

### Non-call occurrences (outside test modules)

| line | kind |
|---|---|
| 1173, 1176 | rustdoc example on `calculate_window_pct_delta` (compiled as a doctest → test, not production) |
| 1181 | the `pub fn` definition itself |

### Production call sites — 4 total

| lines | enclosing fn | guard | verdict |
|---|---|---|---|
| **1244** | `window_deltas_from_snapshots` | `match (previous, current)` → arm `(Some(prev), Some(curr))` @ 1233 | ✅ Some/Some |
| **6028–6029** | `run_observe_cycle_internal` (5898–6534) | `if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot)` @ 6015–6017 | ✅ Some/Some |
| **4509–4510** | `run_governor_cycle` (4055–5813) | `if let Some(snap) = old_snapshot.clone()` @ 4493 | ⚠️ single-`Some` — see §3 |
| **6245–6246** | `run_observe_cycle_internal` | `if let Some(snap) = old_snapshot.clone()` @ 6227 | ⚠️ single-`Some` — see §3 |

### Test call sites — 42 total

All in `#[cfg(test)]` modules, none in production paths:

- `window_delta_tests` (1287–3451): 1303, 1321, 1339, 1427, 1478, 1570, 1622, 1678, 1712,
  2130, 2164, 2404, 2814, 2870, 2916, 2950, 2987, 3013, 3040, 3093, 3161, 3187, 3279, 3390.
  Plus name-only mentions at 1292 / 1310 / 1328 (`fn test_calculate_window_pct_delta_*`)
  and a comment at 2484.
- `tests` (6642–9259): 8691, 8828, 8957.
- `mock_poller_tests` (9500–11425): 10392, 10438, 10465, 10515, 10590, 10616, 10654, 10682,
  10739. Plus comments at 11039, 11094.
- Doctest at 1176 (in the rustdoc block above the definition).

No call site exists in any file other than `src/governor.rs` (`grep -rn --include="*.rs"`
over the repo, including `tests/`, returns hits only in `src/governor.rs`).

## 3. Reported explicitly: the two non-Some/Some production call sites

`src/governor.rs:4509–4510` and `src/governor.rs:6245–6246` are **not** behind a literal
`Some`/`Some` guard. Both are burn-rate/EMA paths with this shape:

```rust
let old_snapshot = state.burn_rate.prev_usage_snapshot.clone();   // Option<PrevUsageSnapshot>
if !state.usage.stale {
    let new_five_hour    = state.usage.five_hour_pct;      // f64, not Option
    let new_seven_day    = state.usage.all_models_pct;     // f64, not Option
    let new_weekly_scoped = state.usage.weekly_scoped_pct; // f64, not Option
    if let Some(snap) = old_snapshot.clone() {             // 4493 / 6227
        if elapsed_secs >= MIN_ELAPSED_SECS && elapsed_secs <= MAX_ELAPSED_SECS {
            let old_pct = WindowPctSnapshot { ...snap... };
            let new_pct = WindowPctSnapshot { ...new_*... };
            let (delta_5h, delta_7d, delta_7ds) =
                calculate_window_pct_delta(&old_pct, &new_pct);   // 4510 / 6246
```

Assessment: these are **semantically equivalent** to a Some/Some guard even though the
pattern is a single `Some`. Only the *previous* side is optional
(`state.burn_rate.prev_usage_snapshot: Option<PrevUsageSnapshot>`, `src/state.rs:596`);
the *current* side reads plain `f64` fields off `state.usage`
(`five_hour_pct`, `all_models_pct`, `weekly_scoped_pct` — `src/state.rs:63–64,100`), which
are never `Option`, so there is no second `Option` to match on. Guarding is in fact
stricter here than at 1244: the call additionally requires `!state.usage.stale` and
`60s <= elapsed <= 1800s`.

These two sites also serve a different consumer — EMA / burn-rate forecasting — not the
window-delta reporting path that 1244 and 6028 feed. No `None` can reach
`calculate_window_pct_delta` at either site.

## Acceptance criteria

- [x] In-block call confirmed with line number → `src/governor.rs:1244`, inside the
      `(Some(prev), Some(curr))` arm at 1233–1246.
- [x] All other call sites enumerated and classified → 4 production, 42 test (+ definition
      at 1181 and rustdoc at 1173/1176).
- [x] No production call site outside an equivalent Some/Some guard; the two single-`Some`
      sites (4509–4510, 6245–6246) are reported explicitly in §3 and shown to be equivalent
      — their "current" operand is non-`Option` by type, so a two-arm `Some`/`Some` pattern
      is not expressible there.

**Result: PASS.** No code changes required.
