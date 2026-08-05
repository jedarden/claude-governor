# bf-4mdeh — Verify the delta call consumes the in-block `prev_pct` / `curr_pct`

Verification only. No code changes.

## Target

`window_deltas_from_snapshots` in `src/governor.rs`, the `(Some(prev), Some(curr))`
match arm (`src/governor.rs:1233`–`1246`).

## Signature under test

`src/governor.rs:1181`–`1184`:

```rust
pub fn calculate_window_pct_delta(
    previous_snapshot: &crate::db::WindowPctSnapshot,
    current_snapshot: &crate::db::WindowPctSnapshot,
) -> (f64, f64, f64)
```

Parameter order is **previous first, current second**. The body
(`src/governor.rs:1185`–`1187`) computes `current − previous` for each of the
three windows, so swapping the arguments would silently sign-flip every delta —
it would not be a compile error, since both parameters share the type
`&crate::db::WindowPctSnapshot`. That is why the argument order has to be checked
by reading, not by trusting the type checker.

## Findings

### 1. Both arguments are let-bound inside the block, above the call

| Binding    | Declared at            | Built from                              |
|------------|------------------------|-----------------------------------------|
| `prev_pct` | `src/governor.rs:1234` | `prev.*` — the arm's `previous` binding  |
| `curr_pct` | `src/governor.rs:1239` | `curr.*` — the arm's `current` binding   |

Both `let` statements sit inside the `(Some(prev), Some(curr))` arm body and both
precede the call at `src/governor.rs:1244`. (Their field-by-field construction was
verified separately in bf-19b7h; this note only confirms the call consumes them.)

### 2. Argument order matches the signature

`src/governor.rs:1244`:

```rust
let (delta_5h, delta_7d, delta_7ds) = calculate_window_pct_delta(&prev_pct, &curr_pct);
```

| Position | Argument     | Parameter           | Correct |
|----------|--------------|---------------------|---------|
| 1st      | `&prev_pct`  | `previous_snapshot` | yes     |
| 2nd      | `&curr_pct`  | `current_snapshot`  | yes     |

Previous first, current second — matches `src/governor.rs:1181`. The deltas
therefore carry the intended `current − previous` sign, consistent with the
doctest at `src/governor.rs:1176`–`1179` (`12.5 − 10.0 == 2.5`, positive when
usage grows).

### 3. No shadowing, no outer-scope leakage

The enclosing scope is the body of `window_deltas_from_snapshots`, whose only
bindings are the two parameters `previous` and `current`
(`src/governor.rs:1229`–`1230`). Neither is named `prev_pct` or `curr_pct`, and
the function body introduces no other bindings before the `match` — the `match`
at `src/governor.rs:1232` is the first and only statement. So the names
`prev_pct` and `curr_pct` at the call site can only resolve to the `let`s at
`src/governor.rs:1234` and `1239`; there is nothing above them for those names to
resolve to, and nothing shadowing them between the `let`s and the call.

The arm's own pattern bindings `prev` / `curr` are distinct names
(`&crate::state::PrevUsageSnapshot`, not `WindowPctSnapshot`), so they cannot be
confused with `prev_pct` / `curr_pct`; passing either to
`calculate_window_pct_delta` would be a type error.

## Result

All three acceptance criteria met:

- Argument order confirmed against the `calculate_window_pct_delta` signature at
  `src/governor.rs:1181`.
- Both arguments traced to `let` bindings inside the `Some`-`Some` block
  (`src/governor.rs:1234`, `1239`).
- No shadowing or outer-scope leakage in the arguments.
