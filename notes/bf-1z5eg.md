# bf-1z5eg — Verify the Some-Some arm of `window_deltas_from_snapshots` holds all delta logic

Verification only. **No defect found; no code changed.**

Authoritative block: `fn window_deltas_from_snapshots` at `src/governor.rs:1228-1255`
(signature 1228-1231, `match` 1232-1254, closing brace 1255).

## Required items — all inside the `(Some(prev), Some(curr))` arm

The arm opens at `src/governor.rs:1233` and closes at `src/governor.rs:1246`.

| Item | Lines | Status |
| --- | --- | --- |
| `prev_pct` constructed as `crate::db::WindowPctSnapshot` | 1234-1238 | inside the arm |
| `curr_pct` constructed as `crate::db::WindowPctSnapshot` | 1239-1243 | inside the arm |
| Call to `calculate_window_pct_delta(&prev_pct, &curr_pct)` | 1244 | inside the arm |
| `Some`-wrapping of the three deltas | 1245 | inside the arm |

Nowhere else in the function: the function body is exactly lines 1232-1254, and the
only other arm is the catch-all. There is no computation before the `match`, no
computation after it, and no shared prelude — the `match` is the sole expression in
the body, so lines 1233-1246 are the only place these four items can and do appear.

Field mapping checked against the struct definitions:
- `crate::state::PrevUsageSnapshot` (`src/state.rs:246-252`): `five_hour_pct`,
  `seven_day_pct`, `weekly_scoped_pct`.
- `crate::db::WindowPctSnapshot` (`src/db.rs:690-697`): `five_hour`, `seven_day`,
  `weekly_scoped`.

Both constructions map `*_pct -> *` one-to-one with no crossed fields, and `prev_pct`
is built from `prev` while `curr_pct` is built from `curr` — no swap. The call at 1244
passes `(&prev_pct, &curr_pct)` in that order, matching
`calculate_window_pct_delta(previous_snapshot, current_snapshot)` at
`src/governor.rs:1181-1184`, so the sign convention (`current - previous`) is preserved.

## Catch-all arm

`_ => (None, None, None)` at `src/governor.rs:1253` (preceded by the explanatory
comment at 1247-1252). The arm body is a single tuple literal of three `None`s: no
snapshot construction, no delta call, no arithmetic, no side effects. It covers
`(None, Some)`, `(Some, None)`, and `(None, None)` — every case lacking a real
interval — and returns all-`None` rather than `Some(0.0)`, matching the `# Returns`
contract documented at lines 1203-1208.

## Doc comment vs. real behaviour

The `# Example` at `src/governor.rs:1210-1227` asserts
`window_deltas_from_snapshots(None, Some(&curr)) == (None, None, None)`.
`(None, Some(..))` does not match `(Some(prev), Some(curr))`, so it falls to the
catch-all at 1253 and yields all-`None`. The doc matches the code.

Verified by execution, not by reading:

```
$ ~/.cargo/bin/cargo test --doc governor::window_deltas_from_snapshots
test src/governor.rs - governor::window_deltas_from_snapshots (line 1211) ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out
```

(Per the standing note in memory, `~/.cargo/bin/cargo` was used directly because the
wrapper on `PATH` discards stderr and exits 0 on failure.)

## Deviations

None. No item was found outside the `Some`-`Some` arm, the catch-all computes nothing,
and the doc example's claim about the first poll is accurate.

## Incidental confirmation (outside the required scope)

The doc comment's claim that `run_governor_cycle` calls this function is accurate:
`src/governor.rs:4174-4177`, inside `run_governor_cycle` (`src/governor.rs:4055`),
calls `window_deltas_from_snapshots(state.previous_api_snapshot.as_ref(),
state.current_api_snapshot.as_ref())` and assigns straight into
`p5h_delta` / `p7d_delta` / `p7ds_delta`. The many other
`calculate_window_pct_delta` call sites in the file are in `#[cfg(test)]` code or in
unrelated annotation paths; none of them are a second copy of this function's match.
