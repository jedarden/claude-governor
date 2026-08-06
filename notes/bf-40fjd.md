# bf-40fjd — Corrected line anchors for the window-delta verification

Fact-finding only. No code changes. All line numbers verified by reading
`src/governor.rs` at commit-time state (file is **12560 lines**).

## The stale anchor: governor.rs:2585-2609 is TEST CODE

**2585-2609 is not production delta computation.** It sits inside:

- `#[cfg(test)] mod window_delta_tests` — **1287-3451** (confirmed: `#[cfg(test)]`
  at 1287, `mod window_delta_tests {` at 1288, closing `}` at 3451)
- `fn test_consecutive_snapshots_governor_cycle()` — starts at **2249**
  (`#[test]` at 2248)

The specific content of 2585-2609:

- 2585-2590 — `assert!` that `aggregate_deltas.weekly_scoped` matches the
  expected 7ds delta
- 2592-2597 — comment block "Verify the delta fields survive serialization"
- 2598 — `serde_json::to_value(&state)`
- 2600-2618 — loop asserting the serialized `p5h_delta` / `p7d_delta` /
  `p7ds_delta` fields carry the computed values

So the block is a serde round-trip assertion on already-computed deltas. Any
bead that "fixes the Some-Some delta block" by editing 2585-2609 is editing a
test's assertions, not the governor's behavior. That is why the parent
(bf-3t7xa) keeps failing.

## Verification of each anchor given in the parent

| Claimed | Verdict | Actual |
|---|---|---|
| `window_deltas_from_snapshots` at 1228-1255 | **confirmed** | signature at 1228, body closes at 1255 |
| Some-Some match arm at 1232-1246 | **confirmed** | `match (previous, current)` at 1232; `(Some(prev), Some(curr)) =>` at 1233; arm closes at 1246 |
| catch-all `_ => (None, None, None)` at 1253 | **confirmed** | exactly at 1253 (explanatory comment 1247-1252) |
| `calculate_window_pct_delta` defined at 1181 | **confirmed** | signature at 1181, body 1184-1189; doc comment starts ~1160 |
| `run_observe_cycle_internal` starts 5898 | **confirmed** | spans **5898-6534** |
| second site "inlines the same logic around 6010-6050" | **corrected** | the inlined delta block is **6014-6052**. 6006-6012 is the `current_api_snapshot` assignment, not delta logic. |

## Production delta-computation sites (current line numbers)

Test modules in this file start at 814, 1287, 6642, 9269, 9281, 9481, 9489,
9500, 11427, 12265 — everything below is outside them.

1. **`calculate_window_pct_delta` — 1181-1189.**
   The arithmetic primitive: `current - previous` for each of the three windows.
   Returns bare `(f64, f64, f64)`.

2. **`window_deltas_from_snapshots` — 1228-1255.**
   The pure helper holding the first-poll contract. Some-Some arm 1233-1246
   (delegates to `calculate_window_pct_delta` at 1244); catch-all
   `_ => (None, None, None)` at 1253.

3. **`run_governor_cycle` — 4174-4206.** (fn starts 4055)
   The good site. Calls `window_deltas_from_snapshots(previous_api_snapshot,
   current_api_snapshot)` at 4174-4177, logs via a `match` at 4179-4199, then
   assigns unconditionally into state at 4204-4206.

4. **`run_governor_cycle` burn-rate EMA — 4503-4510.** (inside 4055)
   Separate concern: builds `old_pct`/`new_pct` and calls
   `calculate_window_pct_delta` at 4509-4510 to feed `fleet_pct_hr_ema` /
   `usd_per_pct_ema_*`. Does **not** touch `state.p5h_delta` et al.

5. **`run_observe_cycle_internal` — 6014-6052.** (fn spans 5898-6534)
   **The duplicated logic.** Hand-rolled `if let (Some(prev), Some(curr))` at
   6015-6017, builds the two `WindowPctSnapshot`s at 6018-6027, calls
   `calculate_window_pct_delta` at 6028-6029, assigns `Some(..)` at 6037-6039;
   `else` branch clears to `None` at 6045-6047. This is a copy of
   `window_deltas_from_snapshots` rather than a call to it — the comment at 6044
   even says "Mirrors run_governor_cycle".

6. **`run_observe_cycle_internal` burn-rate EMA — 6240-6246.** (inside 5898-6534)
   Mirror of site 4. Feeds EMAs only; does not write the `p*_delta` state fields.

### Sites that write `state.p5h_delta` / `p7d_delta` / `p7ds_delta`

Only **site 3** (4204-4206) and **site 5** (6037-6039 / 6045-6047). Those are
the two production paths a delta-behavior change must cover. Sites 4 and 6 are
burn-rate math and are out of scope for delta-field work.

### Non-governor.rs callers

`tests/governor_cycle_snapshot_test.rs` calls `window_deltas_from_snapshots`
at 317, 371, 426, 463, 530, 612 — all integration-test assertions, no
production code outside `src/governor.rs` uses either function.
