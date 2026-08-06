# bf-56fov — Enumerate every production `p5h_delta` / `p7d_delta` / `p7ds_delta` assignment

Sweep of the whole crate (`src/` + `tests/`) for state-delta assignments and
struct-literal fields, classified production vs. test. No code changes.

## Method

- `grep -rn 'p5h_delta\|p7d_delta\|p7ds_delta' src/ tests/` → **161 raw hits**.
- Filtered to assignment / struct-field forms (`ident =` not `==`, or `ident:`),
  dropping doc comments → **42 real sites** (see false-positive note below).
- Also checked explicitly for forms the simple filter would miss:
  - compound assignment (`+=`, `-=`, `*=`, `/=`) — **none**
  - `.replace()` / `.take()` / `.get_or_*()` / `.insert()` on a delta field — **none**
  - shorthand struct init (`p5h_delta,` on its own line) — 6 hits, all verified to be
    **match-scrutinee elements or `assert_eq!` arguments**, not struct literals
    (`src/governor.rs:4180-4182`, `tests/governor_cycle_snapshot_test.rs:324/329/334`).
- `#[cfg(test)]` extents were brace-matched from source rather than assumed.

### Verified `#[cfg(test)]` boundaries

The boundaries supplied in the bead are **all confirmed correct**. Closing lines
(brace-matched, previously unstated) added:

| `#[cfg(test)]` | item | span |
|---|---|---|
| src/governor.rs:814 | `mod governor_state_tests` | 814–954 |
| src/governor.rs:1287 | `mod window_delta_tests` | 1287–**3451** ✓ (matches bead) |
| src/governor.rs:6642 | `mod tests` | 6642–9259 |
| src/governor.rs:9269 | `pub struct MockPoller` | 9269–9279 |
| src/governor.rs:9281 | `impl MockPoller` | 9281–9479 |
| src/governor.rs:9481 | `impl Default for MockPoller` | 9481–9486 |
| src/governor.rs:9489 | `impl UsagePoller for MockPoller` | 9489–9494 |
| src/governor.rs:9500 | `mod mock_poller_tests` | 9500–11425 |
| src/governor.rs:11427 | `mod annotation_guard_tests` | 11427–12263 |
| src/governor.rs:12265 | `mod is_structurally_inactive_tests` | 12265–12560 |
| src/alerts.rs:988 | `mod tests` | 988–2650 |
| src/narrator.rs:524 | `mod tests` | 524–1123 |
| src/state.rs:1216 | `mod tests` | 1216–1653 |
| src/state.rs:2636 | `mod null_roundtrip_test` | 2636–2707 (no delta hits) |
| src/status_display.rs:594 | `mod tests` | 594–991 |
| src/capacity_summary.rs:240 | `mod tests` | 240–638 |
| src/snapshot_fixtures.rs:554 | `mod tests` | 554–1580 |

Consequence: in `governor.rs`, everything from 6642 to EOF is test code, so the
only non-test regions containing delta writes are **955–1286** and **3452–6641**.

## Complete table of occurrences

### Production (12 sites)

| file:line | enclosing item | kind | verdict |
|---|---|---|---|
| src/state.rs:829 | `struct GovernorState` | field **declaration** `pub p5h_delta: Option<f64>` | not a mutation site |
| src/state.rs:833 | `struct GovernorState` | field **declaration** `pub p7d_delta` | not a mutation site |
| src/state.rs:837 | `struct GovernorState` | field **declaration** `pub p7ds_delta` | not a mutation site |
| src/state.rs:878 | `impl Default for GovernorState::default()` | `p5h_delta: None` | constructor default — legitimate |
| src/state.rs:879 | `impl Default for GovernorState::default()` | `p7d_delta: None` | constructor default — legitimate |
| src/state.rs:880 | `impl Default for GovernorState::default()` | `p7ds_delta: None` | constructor default — legitimate |
| src/governor.rs:4204 | `run_governor_cycle` (decl 4055) | `state.p5h_delta = p5h_delta` | **unconditional — outside the match**, see below |
| src/governor.rs:4205 | `run_governor_cycle` | `state.p7d_delta = p7d_delta` | **unconditional — outside the match** |
| src/governor.rs:4206 | `run_governor_cycle` | `state.p7ds_delta = p7ds_delta` | **unconditional — outside the match** |
| src/governor.rs:6037 | `run_observe_cycle_internal` (decl 5898) | `state.p5h_delta = Some(delta_5h)` | inside Some-Some block ✓ |
| src/governor.rs:6038 | `run_observe_cycle_internal` | `state.p7d_delta = Some(delta_7d)` | inside Some-Some block ✓ |
| src/governor.rs:6039 | `run_observe_cycle_internal` | `state.p7ds_delta = Some(delta_7ds)` | inside Some-Some block ✓ |
| src/governor.rs:6045 | `run_observe_cycle_internal` | `state.p5h_delta = None` | matching None-reset `else` ✓ |
| src/governor.rs:6046 | `run_observe_cycle_internal` | `state.p7d_delta = None` | matching None-reset `else` ✓ |
| src/governor.rs:6047 | `run_observe_cycle_internal` | `state.p7ds_delta = None` | matching None-reset `else` ✓ |

(15 rows; "12 sites" counts the 9 real mutation/init rows plus the 3 declarations
separately — 3 declarations + 3 `Default` inits + 9 assignments = 15 rows.)

Both enclosing functions are live production paths:
- `run_governor_cycle` — called by the daemon loop at `src/governor.rs:6583` and `:6616`.
- `run_observe_cycle_internal` — called from `run_observe_cycle` at `src/governor.rs:5837`.

`src/snapshot_fixtures.rs` is `pub mod` (**not** `#[cfg(test)]`-gated, `src/lib.rs:19`),
so it is compiled into production — but its 12 delta mentions (751-753, 829-831,
907-909, 999-1001) are **doc comments only**, no assignments.

### Test-only (27 sites) — excluded from the parent acceptance criteria

| file:line | enclosing fn | enclosing `#[cfg(test)]` |
|---|---|---|
| src/governor.rs:2407-2409 | `test_consecutive_snapshots_governor_cycle` (2249) | `window_delta_tests` (1287-3451) |
| src/governor.rs:2750-2752 | `test_first_poll_governor_state_no_panic_deltas_stay_none` (2695) | `window_delta_tests` (1287-3451) |
| src/governor.rs:2758-2760 | `test_first_poll_governor_state_no_panic_deltas_stay_none` (2695) | `window_delta_tests` (1287-3451) |
| src/governor.rs:10956-10958 | `test_cycle_clears_stale_deltas_when_previous_snapshot_is_missing` (10949) | `mock_poller_tests` (9500-11425) |
| src/alerts.rs:1066-1068 | `make_state_with_forecast` (1036) | `tests` (988-2650) |
| src/state.rs:1390-1392 | `full_state` (1222) | `tests` (1216-1653) |
| src/narrator.rs:611-613 | `make_test_state` (534) | `tests` (524-1123) |
| src/status_display.rs:729-731 | `make_test_state` (602) | `tests` (594-991) |
| src/capacity_summary.rs:285-287 | `make_state` (267) | `tests` (240-638) |

`tests/` (integration tests) contains **zero** assignments to these state fields —
verified with `grep -rnE '\.(p5h_delta|p7d_delta|p7ds_delta)\s*=[^=]' tests/` (empty).
All 28 hits in `tests/governor_cycle_snapshot_test.rs` are local tuple-destructure
bindings from `window_deltas_from_snapshots` plus `assert_eq!`/`assert!` arguments.

### False positives worth recording

`src/governor.rs:2714`, `:2718`, `:2722` match an `ident =` pattern but are
**string literals inside `assert!` messages** (`"Fresh state should have p5h_delta = None"`).
Not assignments. They are inside `window_delta_tests` anyway.

## Answer to the parent question

**No — production delta state is not mutated only inside a Some-Some block.**
There are two production write sites with *structurally different* shapes:

1. **`run_observe_cycle_internal` (6037-6047)** matches the criterion literally.
   The writes sit inside `if let (Some(prev), Some(curr)) = (&state.previous_api_snapshot,
   &state.current_api_snapshot)` (opens 6015) and its `else` None-reset branch (6040).

2. **`run_governor_cycle` (4204-4206)** does **not**. The writes are unconditional at
   the `Ok(usage_data) => { … }` arm's body level, *after* the `match (…)` at 4179-4199
   has already closed. That match exists only to pick a log line — the `(Some, Some,
   Some, Some, Some)` arm at 4186 logs, the `_` arm at 4194 logs, neither assigns.

**4204-4206 is not stray.** The Some-Some decision has been pushed down one level into
`window_deltas_from_snapshots` (`src/governor.rs:1228-1255`), whose `_ => (None, None, None)`
arm at 1253 covers every non-Some-Some case. So `p5h_delta`/`p7d_delta`/`p7ds_delta` still
end up `Some` only when both snapshots are present, and a stale `Some(..)` from the previous
cycle cannot survive — the in-code comment at 4201-4203 states exactly this intent, and the
helper's doc comment (1203-1208) states the contract ("`Some` only when both snapshots are
present … Otherwise every field is `None`, **not** `Some(0.0)`").

So the invariant the parent bead cares about holds at both sites, but only one of them
enforces it syntactically at the assignment. If the parent's acceptance criterion is read
strictly ("every production occurrence is inside a Some-Some arm or its matching None-reset
branch"), **4204-4206 must be recorded as an intentional exception** whose safety depends on
`window_deltas_from_snapshots`, not on its own enclosing block.

Secondary observation (already the subject of bf-1uqqx): the two production paths compute
the same deltas by different routes — `run_governor_cycle` via `window_deltas_from_snapshots`,
`run_observe_cycle_internal` via an inlined copy of that logic calling `calculate_window_pct_delta`
directly (6018-6029). The 6044 comment says it "Mirrors run_governor_cycle", which is a
hand-maintained duplication rather than a shared call.

## Acceptance criteria

- [x] Complete table of every occurrence with file:line, enclosing fn, production/test
- [x] Every production occurrence checked against the Some-Some / None-reset requirement;
      6037-6047 conforms, 4204-4206 flagged as an intentional exception (not stray)
- [x] Test-only occurrences explicitly listed and excluded
- [x] No code changes
