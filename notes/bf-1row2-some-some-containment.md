# bf-1row2 — Some-Some containment: consolidated findings

Consolidates the four verification children of bf-1row2 (bf-65oue, bf-6c7td,
bf-4mdeh, bf-r3093) into one record so the next bead in the chain — bf-64r1k,
"Verify state delta assignments are inside the Some-Some block" — starts from
facts instead of re-deriving them.

All line numbers below were re-confirmed against `src/governor.rs` at HEAD
(`55b8c96`, 2026-08-05) while writing this note.

---

## 0. The stale line range — read this first

**`governor.rs:2585-2609`, cited in the bf-1row2 bead text (and copied verbatim
into bf-64r1k), is stale. Do not read it.**

That range no longer contains any `Some`/`Some` destructuring. It now holds the
tail of the fleet-aggregate delta assertions plus the serialization block inside
the same test:

- 2585-2590 — `assert!` on `aggregate_deltas.weekly_scoped`
- 2592-2597 — `// === Verify the delta fields survive serialization ===`
- 2598 — `serde_json::to_value(&state)`
- 2600-2610 — the `for (field, expected)` loop over `p5h_delta` / `p7d_delta` / `p7ds_delta`

**Replacement: `src/governor.rs:2391-2516`** (the `Some`-`Some` body), or
**2391-2518** when the `else` arm matters. Cite these, not 2585-2609.

Any remaining bead in this chain that still says 2585-2609 is quoting the stale
text; substitute the range above.

## 1. Corrected line range

`src/governor.rs:2391-2518` — the `if let (Some(prev), Some(curr))` construct
this chain tracks.

| Line | Content |
|------|---------|
| 2390 | `// This simulates the delta computation that happens in run_governor_cycle` |
| 2391 | `if let (Some(prev), Some(curr)) =` |
| 2392 | `(&state.previous_api_snapshot, &state.current_api_snapshot)` |
| 2393 | `{` — opens the body |
| 2516 | `} else {` — the `}` closes the body |
| 2517 | `panic!("Both snapshots should be Some after consecutive polls");` |
| 2518 | `}` — closes the whole construct |

Enclosing item: `mod window_delta_tests` (1288) →
`#[test] fn test_consecutive_snapshots_governor_cycle()` (2249-2682).

Naming note carried over from bf-65oue: the chain calls this "the first-poll
test", but the function exercises the *second* poll (both snapshots present).
`test_first_poll_governor_state_no_panic_default_deltas` is a different test.

Two near-identical sites exist elsewhere and are easy to grep into by mistake:

| Line | Form | Enclosing item | Kind |
|------|------|----------------|------|
| 1233-1246 | `match` arm | `window_deltas_from_snapshots` (1228) | production |
| **2391-2516** | `if let` | `test_consecutive_snapshots_governor_cycle` (2249) | **this chain's block** |
| 6015-6052 | `if let` | `run_observe_cycle_internal` (5898) | production (second site) |

Discriminator: the production match arm *returns*
`(Some(delta_5h), Some(delta_7d), Some(delta_7ds))` at 1245; the tracked block
*assigns* into `state.p5h_delta` etc.

## 2. The call's line number

**`src/governor.rs:2404`**

```rust
let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
```

Containment: `2393 < 2404 < 2516`, proved by a brace-depth walk (bf-6c7td), not
by indentation. Depth stays at 1 continuously from 2393 through 2404, so no
intervening `}` closes the body first. `} else {` at 2516 is net-neutral, which
is why the raw counter does not reach 0 until 2518.

Exactly one call in the enclosing test (2249-2682). The other grep hit at 2484
is prose inside a comment. Nearest real calls outside the test are 2130 and
2814, both in different tests.

## 3. Argument order

Signature — `src/governor.rs:1181-1184`:

```rust
pub fn calculate_window_pct_delta(
    previous_snapshot: &crate::db::WindowPctSnapshot,
    current_snapshot: &crate::db::WindowPctSnapshot,
) -> (f64, f64, f64)
```

Previous first, current second; the body (1185-1187) computes
`current − previous`. Both parameters share one type, so a swap would **not** be
a compile error — it would silently sign-flip every delta. Order has to be read,
not inferred from the type checker.

| Site | Call | 1st arg | 2nd arg | Correct |
|------|------|---------|---------|---------|
| tracked block | 2404 | `&prev_pct` → `previous_snapshot` | `&curr_pct` → `current_snapshot` | yes |
| production arm | 1244 | `&prev_pct` | `&curr_pct` | yes (bf-4mdeh) |
| production observe | 6029 | `&prev_pct` | `&curr_pct` | yes (bf-r3093) |

Both arguments at 2404 are `let`-bound above the call, inside the block:
`prev_pct` at 2394 (from `prev.*`), `curr_pct` at 2399 (from `curr.*`). bf-4mdeh
additionally ruled out shadowing and outer-scope leakage at the production arm —
the names can only resolve to the in-arm `let`s at 1234/1239.

Sign check against the doctest at 1176-1179: `12.5 − 10.0 == 2.5`, positive when
usage grows.

## 4. Field-to-field assignment mapping

The return tuple's position → name mapping at the destructuring site is the
identity (returns `(delta_5h, delta_7d, delta_7ds)` at 1188 in that order):

| Tuple position | Returned expression (1185-1187) | Bound name |
|---|---|---|
| `.0` | `current.five_hour - previous.five_hour` | `delta_5h` |
| `.1` | `current.seven_day - previous.seven_day` | `delta_7d` |
| `.2` | `current.weekly_scoped - previous.weekly_scoped` | `delta_7ds` |

Assignments in the tracked block, `src/governor.rs:2407-2409` — all three inside
the body (`2393 < 2407..2409 < 2516`):

```rust
2407    state.p5h_delta = Some(delta_5h);
2408    state.p7d_delta = Some(delta_7d);
2409    state.p7ds_delta = Some(delta_7ds);
```

| Binding | Assigned to | Window | Crossover? |
|---|---|---|---|
| `delta_5h` | `state.p5h_delta` | 5-hour | no |
| `delta_7d` | `state.p7d_delta` | 7-day | no |
| `delta_7ds` | `state.p7ds_delta` | 7-day scoped | no |

Each binding appears exactly once on a right-hand side and the three left-hand
sides are three distinct fields, so the mapping is a bijection — no value can be
routed to the wrong window.

bf-r3093 established the same bijection at the `run_observe_cycle_internal` site
(assignments 6037-6039, `else` arm at 6045-6047 setting all three to `None`), and
proved the bindings do not escape: no occurrence of `delta_5h` / `delta_7d` /
`delta_7ds` between 6040 and 6244; the next occurrence at 6245 is a fresh `let`
fed by `old_pct`/`new_pct`. Escape is compiler-enforced anyway — a post-block
reference would be `error[E0425]`.

**This is the finding bf-64r1k needs.** Its three acceptance criteria are already
satisfied for the tracked block by lines 2407-2409 above; what remains for that
bead is to confirm no *other* `p5h_delta` / `p7d_delta` / `p7ds_delta` mutation
in the same test sits outside 2393-2516.

## 5. `cargo check --tests`

Re-run at HEAD for this note:

```
$ cargo check --tests --message-format=json
EXIT=0
compiler-artifact records:     215
"level":"error" occurrences:     0
"level":"warning" occurrences:  46
build-finished:                 "success":true
```

Counts match bf-r3093's run exactly. None of the 46 warnings mentions
`delta_5h`, `delta_7d`, or `delta_7ds`, and the absence of any
`unused variable: delta_*` warning independently confirms all three bindings are
consumed before their block ends.

One correction to bf-r3093's §4: it reports that no warning mentions
`governor.rs`. That is not right — 23 of the 46 have a primary span in
`src/governor.rs`. None of them, however, falls inside 2391-2516 or 6015-6052,
and none concerns the delta bindings, so the conclusion is unaffected. They are
pre-existing and unrelated: unnecessary parentheses (6314, 6320, 6326), a
needless `mut` (5147), unused variables (`composite_risk_config` 5904,
`cone_scaling_config` 5905, `total_tmux_count` 6198/6201, `target_ceiling` 6216,
`std_pct_hr_seeded` 7600, `baseline` 7637, `weekly_scoped_model_at_startup` 7883,
`first_poll_model` 7888), and a never-used `is_structurally_inactive` (128). The
remaining warnings are unused imports and test helpers in other modules.

---

## Scope note

The four children did not all examine the same site. bf-65oue and bf-6c7td
worked the tracked test block (2391-2516); bf-4mdeh worked the production match
arm (1233-1246); bf-r3093 worked the second production site (6015-6052). The
argument-order and assignment-mapping facts for the **tracked block** at 2404 and
2407-2409 were therefore re-derived directly for this note (§3, §4) rather than
inherited, so every point above is stated about 2391-2516 itself.

## Sources

- [bf-65oue](bf-65oue-some-some-line-ranges.md) — re-located the block, flagged the stale range
- [bf-6c7td](bf-6c7td.md) — brace-depth containment proof for the call at 2404
- [bf-4mdeh](bf-4mdeh.md) — argument order, no shadowing (production arm)
- [bf-r3093](bf-r3093.md) — assignment bijection, no escape, `cargo check`
