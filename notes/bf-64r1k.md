# bf-64r1k — state delta assignments are inside the Some-Some block

**Verdict: all three acceptance criteria pass.** `state.p5h_delta`,
`state.p7d_delta`, and `state.p7ds_delta` are each assigned exactly once inside
the `if let (Some(prev), Some(curr))` body, and no delta mutation exists anywhere
else in the enclosing test.

Verified against `src/governor.rs` at HEAD `7805749` (2026-08-05).

---

## 0. Line range

The bead text cites `governor.rs:2585-2609`. That range is stale — it holds the
tail of the fleet-aggregate assertions and the serialization loop, and contains
no `Some`/`Some` destructuring. See [bf-1row2](bf-1row2-some-some-containment.md)
§0. The real construct, re-confirmed at HEAD:

| Line | Content |
|------|---------|
| 2391 | `if let (Some(prev), Some(curr)) =` |
| 2392 | `(&state.previous_api_snapshot, &state.current_api_snapshot)` |
| 2393 | `{` — opens the body |
| 2516 | `} else {` — the `}` closes the body |
| 2517 | `panic!("Both snapshots should be Some after consecutive polls");` |
| 2518 | `}` — closes the construct |

Enclosing item: `fn test_consecutive_snapshots_governor_cycle()`, **2249-2682**
(both boundaries computed by brace walk, not assumed).

## 1. Containment of the three assignments

```rust
2407    state.p5h_delta = Some(delta_5h);
2408    state.p7d_delta = Some(delta_7d);
2409    state.p7ds_delta = Some(delta_7ds);
```

Proved by a brace-depth walk over 2391-2518 with line comments, string literals,
and char literals stripped first — indentation was not used as evidence:

```
2391 depth=0    2404 depth=1  (the calculate_window_pct_delta call)
2392 depth=0    2407 depth=1  state.p5h_delta  = Some(delta_5h)
2393 depth=1    2408 depth=1  state.p7d_delta  = Some(delta_7d)
               2409 depth=1  state.p7ds_delta = Some(delta_7ds)
2516 depth=1  (`} else {` is net-neutral)
2518 depth=0  <- first return to 0
```

Depth is continuously 1 from 2393 through 2409 and never returns to 0 anywhere
before 2518, so no intervening `}` closes the body ahead of the assignments. All
three satisfy `2393 < 2407..2409 < 2516`.

| Criterion | Line | Result |
|---|---|---|
| `p5h_delta` assignment inside the Some-Some block | 2407 | pass |
| `p7d_delta` assignment inside the Some-Some block | 2408 | pass |
| `p7ds_delta` assignment inside the Some-Some block | 2409 | pass |

## 2. No delta mutation outside the block

This was the open item bf-1row2 §4 left for this bead. Every occurrence of the
three field names within the enclosing test (2249-2682) was classified
mechanically — a *mutation* is the field appearing on the left of a top-level
`=` (excluding `==`/`!=`/`>=`/`<=`); everything else is a read, a comment, or a
string literal.

| Lines | In block | Kind | Notes |
|---|---|---|---|
| 2267, 2271, 2275 | no | read | `is_none()` preconditions before the block |
| **2407-2409** | **yes** | **MUTATION** | the only three |
| 2413, 2417, 2421 | yes | read | `is_some()` |
| 2431, 2438, 2445 | yes | read | `unwrap()` in assert messages |
| 2486-2488 | yes | read | bound to `computed_*_delta` |
| 2521 | no | comment | prose naming the fields |
| 2548-2550 | no | read | see below |
| 2601-2603 | no | string | field-name keys in the serialization loop |

**Exactly three mutations, all inside the block.** Total occurrences: 22.

The one line worth reading twice is 2548-2550:

```rust
state.last_fleet_aggregate.window_pct_deltas.five_hour = state.p5h_delta.unwrap();
```

An `=` appears and a delta field appears, but the delta field is on the *right*.
The assignment target is `last_fleet_aggregate.window_pct_deltas.*`, a different
field. These are reads of `p5h_delta`/`p7d_delta`/`p7ds_delta`, not mutations, so
their position outside the block (2548 > 2516) is not a violation of the
criteria. A naive `grep 'p5h_delta.*='` would misclassify them.

## 3. Production sites — for the record

Neither is the tracked block, but both were checked so a future reader does not
file the second one as a violation.

- **`run_observe_cycle_internal`, 6015-6052** — same shape as the tracked block.
  Assignments at 6037-6039 sit inside the `if let (Some(prev), Some(curr))` body;
  the `else` arm at 6045-6047 sets all three to `None`. Consistent with the
  criteria.
- **`run_governor_cycle`, 4174-4206** — **does not use an `if let Some`-`Some`
  guard around its assignments, by design.** `window_deltas_from_snapshots`
  (4174-4177) returns three `Option`s; the `match` at 4179-4199 only picks a log
  line; the assignments at 4204-4206 are then *deliberately unconditional*, with
  the comment at 4201-4203 explaining why: on the no-baseline path the values are
  already `None`, so assigning unconditionally is what prevents a stale `Some(..)`
  from a previous cycle surviving into an interval it does not describe. Guarding
  these three lines behind a `Some`-`Some` block would reintroduce that staleness
  bug — the pattern this bead verifies must not be propagated here.

## 4. `cargo check --tests`

```
errors=0  warnings=46  build_success=true
```

Matches the counts in bf-1row2 §5 and bf-r3093 exactly. No warning mentions
`delta_5h`, `delta_7d`, or `delta_7ds`; the absence of any
`unused variable: delta_*` independently confirms all three bindings are consumed
before their block ends.

---

## Sources

- [bf-1row2](bf-1row2-some-some-containment.md) — consolidated Some-Some findings; supplied the corrected range and flagged the stale one
- [bf-1row2 bead note](bf-1row2.md)
