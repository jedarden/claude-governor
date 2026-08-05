# bf-65oue — current location of the Some-Some snapshot block in `src/governor.rs`

Re-located as of commit `bb46a0b` (2026-08-05). Line numbers below supersede the
parent bead's citation.

## The stale range

**`governor.rs:2585-2609` is stale — do not re-read it.**

That range now holds the tail of the fleet-aggregate delta assertions plus the
start of the serialization block inside `test_consecutive_snapshots_governor_cycle`:

- 2585-2590 — `assert!` on `aggregate_deltas.weekly_scoped`
- 2592-2597 — `// === Verify the delta fields survive serialization ===` comment
- 2598 — `serde_json::to_value(&state)`
- 2600-2610 — the `for (field, expected)` loop over `p5h_delta` / `p7d_delta` / `p7ds_delta`

No `Some`/`Some` destructuring appears anywhere in it.

## The test block the bead chain means

`src/governor.rs:2391-2518` — the `if let (Some(prev), Some(curr))` block that
simulates the delta computation `run_governor_cycle` performs.

- Enclosing test: `mod window_delta_tests` (1288) → `#[test] fn test_consecutive_snapshots_governor_cycle()` (2249)
- 2390 — lead-in comment: `// This simulates the delta computation that happens in run_governor_cycle`
- 2391 — `if let (Some(prev), Some(curr)) =`
- 2392 — `(&state.previous_api_snapshot, &state.current_api_snapshot)`
- 2393 — `{` opens the block
- 2516 — `} else {` closes the `if let` body
- 2517 — `panic!("Both snapshots should be Some after consecutive polls");`
- 2518 — `}` closes the whole construct

So: body is 2391-2516; the full `if let … else` construct is 2391-2518. Cite
**2391-2518** when the else-arm matters, **2391-2516** for the Some-Some body alone.

Naming note: the parent chain calls this "the first-poll test". The function is
actually named `test_consecutive_snapshots_governor_cycle` and exercises the
*second* poll (two snapshots present). There is a separate
`test_first_poll_governor_state_no_panic_default_deltas`, referenced from a
comment at 2426, which is not this block.

## The production counterpart

`src/governor.rs:1233-1246` — the real match arm, so later beads do not confuse
the two.

- `pub fn window_deltas_from_snapshots(...)` — 1228
- `match (previous, current) {` — 1232
- `(Some(prev), Some(curr)) => {` — 1233
- arm closes — 1246

The arm body is near-identical to the test block's opening (same
`WindowPctSnapshot` construction, same `calculate_window_pct_delta` call), which
is exactly why a bare grep confuses them. Discriminator: production is a `match`
arm returning `(Some(delta_5h), Some(delta_7d), Some(delta_7ds))` at 1245; the
test is an `if let` that *assigns* into `state.p5h_delta` etc. at 2407-2409.

## Every other Some-Some site (for disambiguation)

| Line | Form | Enclosing item | Kind |
|------|------|----------------|------|
| 1233 | `match` arm | `window_deltas_from_snapshots` (1228) | **production** |
| 2391 | `if let` | `test_consecutive_snapshots_governor_cycle` (2249) | **the block this bead tracks** |
| 3371 | commented-out `if let` | `test_second_poll_with_both_snapshots` (3307) | comment only |
| 3373 | `match` arm | `test_second_poll_with_both_snapshots` (3307) | test |
| 6015 | `if let` | `run_observe_cycle_internal` (5898) | production |
| 10312 | `match` arm | `test_first_poll_and_second_poll_complete_flow` (10151) | test |
| 10887 | doc comment | `test_cycle_polls_once_and_persists_polled_usage` (10833) | prose |

Note 6015: `run_observe_cycle_internal` carries its *own* production `if let`
Some-Some, distinct from the `window_deltas_from_snapshots` match arm. Two
production sites, not one.
