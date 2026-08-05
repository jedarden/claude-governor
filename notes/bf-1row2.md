# bf-1row2 — `calculate_window_pct_delta` call is inside the Some-Some block

Verification record for bf-1row2. The four children (bf-65oue, bf-6c7td,
bf-4mdeh, bf-r3093) were consolidated by bf-kcc78 into
[bf-1row2-some-some-containment.md](bf-1row2-some-some-containment.md); this note
re-derives the parent's own acceptance criteria directly against `src/governor.rs`
rather than inheriting them.

## 0. The bead's line range is stale

The bead text cites `governor.rs:2585-2609`. That range contains **no**
`Some`/`Some` destructuring. At HEAD it holds the tail of the fleet-aggregate
delta assertions and the serialization block of the same test:

- 2585-2590 — `assert!` on `aggregate_deltas.weekly_scoped`
- 2592-2597 — `// === Verify the delta fields survive serialization ===`
- 2598 — `serde_json::to_value(&state)`
- 2600-2618 — the `for (field, expected)` loop over `p5h_delta` / `p7d_delta` / `p7ds_delta`

Correct range: **`src/governor.rs:2391-2516`** (block body), or 2391-2518 with the
`else` arm.

`src/governor.rs` is byte-identical to the state the consolidated note was written
against — `git log 55b8c96..HEAD -- src/governor.rs` is empty, so every line
number below matches that note.

## 1. The block

Enclosing item: `mod window_delta_tests` → `#[test] fn
test_consecutive_snapshots_governor_cycle()` (declared at 2249, body through 2682).

| Line | Content |
|------|---------|
| 2390 | `// This simulates the delta computation that happens in run_governor_cycle` |
| 2391 | `if let (Some(prev), Some(curr)) =` |
| 2392 | `(&state.previous_api_snapshot, &state.current_api_snapshot)` |
| 2393 | `{` — opens the body |
| 2516 | `} else {` |
| 2517 | `panic!("Both snapshots should be Some after consecutive polls");` |
| 2518 | `}` |

## 2. The call, and its containment

`src/governor.rs:2404`:

```rust
let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
```

Containment proved by brace-depth walk from the opening brace, not by
indentation (re-run for this note):

| Line | Depth after line | Content |
|------|------|---------|
| 2393 | 1 | `{` |
| 2394 | 2 | `let prev_pct = crate::db::WindowPctSnapshot {` |
| 2398 | 1 | `};` |
| 2399 | 2 | `let curr_pct = crate::db::WindowPctSnapshot {` |
| 2403 | 1 | `};` |
| 2404 | 1 | the `calculate_window_pct_delta` call |

Depth is ≥ 1 on every line from 2393 to 2404 — it never returns to 0 — so no
intervening `}` closes the body before the call. `2393 < 2404 < 2516`: the call is
inside the Some-Some block. ✅

The two `WindowPctSnapshot` struct literals are the only nesting in between, and
both open and close within the body.

## 3. The delta computation happens within the `if let` pattern

Both arguments are `let`-bound inside the body, from the pattern bindings
themselves:

- `prev_pct` (2394-2398) — built from `prev.*`, the `Some(prev)` binding
- `curr_pct` (2399-2403) — built from `curr.*`, the `Some(curr)` binding

So the computation is not merely lexically inside the block; it is *data*-dependent
on the pattern bindings and could not be hoisted out of it. Argument order matches
the signature at 1181-1184 (`previous_snapshot`, `current_snapshot`) — both
parameters share one type, so a swap would compile and silently sign-flip, which
is why order is read rather than inferred.

Exactly one call site in the enclosing test (2249-2682): line 2404. The only other
grep hit inside the test, at 2484, is the function name in prose inside a comment.
Nearest real calls outside the test are 2130 and 2814, in different tests.

## 4. State of the next bead

The three assignments at 2407-2409 (`state.p5h_delta` / `p7d_delta` / `p7ds_delta`)
are likewise inside the body — see §4 of the consolidated note for the
binding→field bijection. What remains for bf-64r1k is confirming no *other*
mutation of those three fields in the same test sits outside 2393-2516.

## Sources

- [bf-1row2-some-some-containment.md](bf-1row2-some-some-containment.md) — consolidated children
- `src/governor.rs:1181-1188`, `2249`, `2386-2409`, `2510-2518`, `2585-2618`
