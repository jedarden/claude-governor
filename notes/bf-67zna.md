# bf-67zna — Document completion and verify tests

**Date:** 2026-08-05
**Scope:** verification and documentation only. No production code changed.
**Verified against:** working tree at `aa1178e`, toolchain `~/.cargo/bin/cargo`
(the PATH `cargo` wrapper swallows stderr and exits 0 on failure — do not trust it).

## Verdict

All four acceptance criteria pass. The full test suite is green: **838 tests, 0 failures**
across the lib, the binary, 14 integration test files, and doc-tests. One criterion needs a
wording correction and one residual gap is carried forward; both are recorded below.

## Acceptance criteria

### 1. Pattern matches on `Option<PrevUsageSnapshot>` and `Option<PrevUsageSnapshot>` — ✅ with a correction

`src/governor.rs:1228-1232`:

```rust
pub fn window_deltas_from_snapshots(
    previous: Option<&crate::state::PrevUsageSnapshot>,
    current: Option<&crate::state::PrevUsageSnapshot>,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    match (previous, current) {
```

The two parameters are `Option<&PrevUsageSnapshot>`, **not** `Option<PrevUsageSnapshot>` as
the bead text says. The match is on the tuple of both, which is the structure the criterion
is really asking for. The borrow is the correct choice — the helper only reads four `f64`
fields, so taking ownership would force the callers to clone a snapshot they still need for
the next cycle's baseline. Recording the discrepancy so the criterion is not read later as
evidence that an owned signature was ever intended.

### 2. Code compiles successfully — ✅

`~/.cargo/bin/cargo check` → exit 0, 15 warnings, 0 errors. All 15 are pre-existing
`unused_imports` / `unused_variables` warnings unrelated to the delta path.

### 3. Delta computation is ONLY inside the Some-Some block — ✅

Confirmed independently against the working tree, not inherited from the parent's note.
In `window_deltas_from_snapshots` every computation sits in the `(Some(prev), Some(curr))`
arm at `1233-1246`: `prev_pct` (`1234-1238`), `curr_pct` (`1239-1243`), the
`calculate_window_pct_delta` call (`1244`), and the `Some(..)`-wrapping return (`1245`).
Every other input shape falls to `_ => (None, None, None)` at `1253`. Nothing leaks out.

The two known qualifications from `notes/bf-3t7xa.md` still stand and are unchanged by this
bead:

- `run_governor_cycle:4204-4206` assigns the three state fields unconditionally, outside any
  `if let`. Deliberate — the Some-Some decision was pushed down into the helper, whose `_`
  arm makes the fields `Some` only when both snapshots exist, so no stale `Some(..)` can
  survive a cycle.
- `run_observe_cycle_internal:6014-6052` re-implements the helper inline. It passes all four
  checks literally (`if let` at `6015-6017`, `else` resetting to `None` at `6045-6047`), but
  by hand-maintained duplication rather than delegation.

### 4. Tests pass (cargo test for governor module) — ✅

`~/.cargo/bin/cargo test --lib governor::` → **188 passed, 0 failed, 0 ignored**
(550 filtered out), 12.94s. Covers `governor::window_delta_tests`,
`governor::mock_poller_tests`, and `governor::tests`.

Full-suite regression check, `~/.cargo/bin/cargo test`:

| Target | Result |
|---|---|
| `unittests src/lib.rs` | 738 passed, 0 failed |
| `unittests src/main.rs` | 10 passed, 0 failed |
| 14 integration files under `tests/` | 71 passed, 0 failed, 3 ignored |
| doc-tests | 14 passed, 0 failed, 5 ignored |

Zero failures anywhere. The helper's own doc-test is live, not ignored:
`src/governor.rs - governor::window_deltas_from_snapshots (line 1211) ... ok` — it asserts
the first-poll case `window_deltas_from_snapshots(None, Some(&curr)) == (None, None, None)`,
so criterion 3's `_` arm is executable documentation.

## Residual gap carried forward

Re-verified here, not just quoted: `grep -rn "run_observe" tests/` returns **nothing**, and
in `src/` the only references to `run_observe_cycle_internal` are its definition (`5898`)
and its single call from `run_observe` (`5837`). The inline copy at `6014-6052` therefore
still has **zero** test coverage in either unit or integration tests, while the helper it
duplicates has ~12 unit tests, 7 integration assertions, and a doc-test.

Nothing in this bead's scope changes that. The standing recommendation — delegate
`6014-6052` to `window_deltas_from_snapshots` and delete the duplicate — remains open work
for a separate bead. Until then the two paths can drift, and only one of them would fail a
test if it did.

## Related

- `notes/bf-3t7xa.md` — the containment verification this bead documents completion of
- `notes/bf-56fov.md` — enumeration of production window-delta assignments
- `notes/bf-1uqqx.md` — field-by-field equivalence audit of the duplicate computation
