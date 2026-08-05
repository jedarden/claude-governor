# bf-4bce1 — Explicit Option pattern matching for snapshot handling

**Date:** 2026-08-05
**Outcome:** No code change required. The requested pattern is already implemented
and covered by tests; this note records the verification.

## What the bead asked for

An `if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot, &state.current_api_snapshot)`
guard around delta computation, with an `else` that handles the first-poll case by
explicitly initializing the delta fields.

## What the code already does

That exact shape landed in commit `946220a` (bf-9mtsa) at both production cycle
sites:

- `src/governor.rs:4510` — `run_governor_cycle`
- `src/governor.rs:6365` — `run_observe_cycle_internal`

Both read:

```rust
if let (Some(prev), Some(curr)) =
    (&state.previous_api_snapshot, &state.current_api_snapshot)
{
    // ... calculate_window_pct_delta(&prev_pct, &curr_pct)
    state.p5h_delta = Some(delta_5h);
    state.p7d_delta = Some(delta_7d);
    state.p7ds_delta = Some(delta_7ds);
} else {
    state.p5h_delta = None;
    state.p7d_delta = None;
    state.p7ds_delta = None;
    log::debug!("[governor] no previous API snapshot; window deltas cleared ...");
}
```

Field types confirm the match is over genuine `Option`s, not a stand-in
(`src/state.rs:821-837`): `previous_api_snapshot` and `current_api_snapshot` are
`Option<PrevUsageSnapshot>`; `p5h_delta` / `p7d_delta` / `p7ds_delta` are
`Option<f64>`.

## On "early return or skip logic"

The bead offered "early return **or** skip logic". Skip is what is implemented and
it is the correct one of the two: this block sits mid-cycle, inside the
`Ok(usage_data)` arm. An early `return` would abandon forecasting, calibration,
scaling, and the state write for the whole first cycle — the governor would do
nothing at all on its first poll. Skipping just the delta computation is the
narrower and correct behavior.

`None` rather than `Some(0.0)` for the acceptance criterion's "None or 0.0": the
fields are `Option<f64>`, and `0.0` would assert the window did not move, which is
a different claim from having no interval to measure. Reasoning carried over from
bf-9mtsa and bf-3z0vo.

## Acceptance criteria

- **Pattern matches on Option types correctly** — yes, both sites, over real
  `Option<PrevUsageSnapshot>` fields.
- **Code compiles without errors** — `cargo build` exits 0, no warnings.
- **First poll case handled gracefully** — yes; verified by test, below.

## Test evidence

Run against the tree as-is (no modifications):

- `cargo build` → exit 0
- `cargo test --lib governor::` → 187 passed, 0 failed. Includes
  `test_first_cycle_does_not_fabricate_deltas_without_a_previous_snapshot`
  (`src/governor.rs:11025`) and
  `test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing`
  (`src/governor.rs:11077`), which drive a real `run_governor_cycle` and assert the
  three delta fields come back `None`.
- `cargo test --test governor_cycle_snapshot_test --test governor_cycle_behavior_test`
  → 14 passed, 0 failed. Includes `test_first_poll_no_previous_snapshot` and
  `test_poll_failure_current_snapshot_remains_none`.

## Gap found, not closed here

The `else` branch in `run_observe_cycle_internal` has **no direct test**. It cannot
have one today: that function takes a concrete `&mut Poller` rather than
`&mut impl UsagePoller`, so no mock can be injected. `run_governor_cycle` is generic
and is the one the tests exercise. Making the observe path testable means changing
its signature to match — a refactor outside this bead. The branch itself is a
character-for-character mirror of the tested one, so the risk is low, but it is
unverified by execution and worth a follow-up bead.

Separately, the `Err` arm on the *failing* cycle still leaves stale `Some(..)`
deltas for that one cycle. That half of the bf-3z0vo staleness gap was explicitly
left open by bf-9mtsa and is likewise out of scope here — this bead is about the
`if let` pattern, not the error path.
