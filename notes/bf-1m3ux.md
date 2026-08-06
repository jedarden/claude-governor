# bf-1m3ux — `prev_pct` / `curr_pct` are constructed inside the Some/Some block

**Provenance:** `src/governor.rs` @ 12560 lines, last touched by `13eb0db`
("test(bf-1gscj): make the first-poll tests test the governor"). Working tree clean
for this file at verification time, so every line number below is current.

**Authoritative block** (per bf-4xxa6): the `(Some(prev), Some(curr))` decision lives in
`window_deltas_from_snapshots` (`src/governor.rs:1228-1255`), which
`run_governor_cycle` calls at `4174-4177`. The `if let (Some(prev), Some(curr))` idiom
is *not* present in `run_governor_cycle` itself — the match arm in the helper is the
production site.

## Verdict: CONFIRMED

Both snapshots are constructed inside the arm body, and both read from the matched
bindings.

Arm braces: opening `{` on **1233** (`(Some(prev), Some(curr)) => {`), closing `}` on
**1246**. Everything below is strictly interior to `1233..1246`.

| Binding | Line | Inside `1233..1246`? | Source |
| --- | --- | --- | --- |
| `prev_pct` | **1234** (fields 1235–1237, closes 1238) | yes | matched `prev` |
| `curr_pct` | **1239** (fields 1240–1242, closes 1243) | yes | matched `curr` |

Field-by-field sourcing — every field reads through the matched binding; no outer
`Option`, no `.unwrap()`, no field lifted from a variable bound outside the arm:

```rust
1233        (Some(prev), Some(curr)) => {
1234            let prev_pct = crate::db::WindowPctSnapshot {
1235                five_hour: prev.five_hour_pct,
1236                seven_day: prev.seven_day_pct,
1237                weekly_scoped: prev.weekly_scoped_pct,
1238            };
1239            let curr_pct = crate::db::WindowPctSnapshot {
1240                five_hour: curr.five_hour_pct,
1241                seven_day: curr.seven_day_pct,
1242                weekly_scoped: curr.weekly_scoped_pct,
1243            };
1244            let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
1245            (Some(delta_5h), Some(delta_7d), Some(delta_7ds))
1246        }
```

`prev` / `curr` are `&crate::state::PrevUsageSnapshot` produced by the match on
`(previous, current)` at 1232; they have no binding outside the arm, so the constructions
could not compile anywhere else in this function. The `_ => (None, None, None)` arm
(1253) constructs no snapshot at all.

## Constructions found outside the block

Reported as required. `src/governor.rs` holds 113 `WindowPctSnapshot { .. }`
constructions; all but the sites below are inside `#[cfg(test)]` modules
(`governor_state_tests` 814–954, `window_delta_tests` 1287–3451, `tests` 6642–9268,
`mock_poller_tests` 9500–11425, `annotation_guard_tests` 11427–12264) or in rustdoc
examples (281, 282, 287, 1174, 1175).

### Same-shape duplicate — the one that matters

| Lines | Fn | Assessment |
| --- | --- | --- |
| **6018 / 6023** | `run_observe_cycle_internal` | `prev_pct` / `curr_pct`, identical field-for-field to 1234/1239, inside its own `if let (Some(prev), Some(curr))` at 6015–6017 (block closes 6052). Also correctly scoped and also sourced from its matched `prev` / `curr`. |

This is the pre-refactor inline copy bf-4xxa6 flagged: the observe path builds both
snapshots by hand and writes `Some(..)`/`None` in an if/else (6037–6039 / 6045–6047)
whose comment reads "Mirrors run_governor_cycle." **Containment is correct there too**,
so this bead's check passes for both paths — but it remains a second place to change,
since it does not route through `window_deltas_from_snapshots`.

### Unrelated production constructions (not this delta path)

Named `old_pct` / `new_pct`, feeding burn-rate attribution and record annotation rather
than `state.p5h_delta` / `p7d_delta` / `p7ds_delta`. Listed so a future grep does not
mistake them for the delta site:

| Lines | Fn | Purpose |
| --- | --- | --- |
| 4499 / 4504 | `run_governor_cycle` | per-window delta for fleet USD attribution, gated on `MIN/MAX_ELAPSED_SECS`; `new_pct` reads loose `new_five_hour`-style locals, not a matched binding |
| 4644 / 4649 | `run_governor_cycle` | window-delta record annotation, under `if let (Some(ref prev_snap), Ok(conn))` |
| 6235 / 6240 | `run_observe_cycle_internal` | observe-path mirror of 4499/4504 |

**No construction of `prev_pct` or `curr_pct` was found outside a Some/Some block, and
none was found reading from an `Option` unwrapped elsewhere.**
