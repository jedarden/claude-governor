# bf-3t7xa — Verify delta computation location

**Date:** 2026-08-05
**Scope:** verification only. No production code changed.
**Verified against:** working tree at `6aa8d99`, `~/.cargo/bin/cargo check` → exit 0, 15 warnings, 0 errors.

## Headline

The bead's cited anchor, `governor.rs:2585-2609`, **is stale and does not point at delta
computation.** It lands inside `fn test_consecutive_snapshots_governor_cycle`, within
`#[cfg(test)] mod window_delta_tests` (`1287`–`3450`): lines `2585-2590` are an `assert!`
on `aggregate_deltas.weekly_scoped`, and `2592-2618` check the three delta field names
survive `serde_json` round-tripping. There is no Some-Some block there to audit.

Re-run against the real code, the four checks pass at **both** production sites that write
the state delta fields — but there are two such sites, not one.

## The four checks

| Check | `window_deltas_from_snapshots` (canonical) | `run_observe_cycle_internal` (inline copy) |
|---|---|---|
| `prev_pct` inside Some-Some | ✅ `1234-1238` | ✅ `6018-6022` |
| `curr_pct` inside Some-Some | ✅ `1239-1243` | ✅ `6023-6027` |
| `calculate_window_pct_delta` inside Some-Some | ✅ `1244` | ✅ `6028-6029` |
| `p5h_delta` / `p7d_delta` / `p7ds_delta` assigned inside Some-Some | ⚠️ see below | ✅ `6037-6039` |

**Site 1 — `window_deltas_from_snapshots`, `src/governor.rs:1228-1255`.**
Everything lives in the `(Some(prev), Some(curr))` arm at `1233-1246`; every other input
shape falls to `_ => (None, None, None)` at `1253`. Nothing leaks out of the match.

The caveat on the fourth check: the caller `run_governor_cycle` assigns the three state
fields **unconditionally** at `4204-4206`, outside any if-let. The `match` above it
(`4179-4199`) picks a log line and assigns nothing. This is deliberate, and the comment at
`4201-4203` says so — the Some-Some decision has been pushed down into the helper, whose
`_` arm makes the fields `Some` only when both snapshots exist. Read literally, `4204-4206`
violates "no delta logic outside the if-let"; read by behaviour, the invariant holds and no
stale `Some(..)` can survive a cycle. Strengthening, not a leak.

**Site 2 — `run_observe_cycle_internal`, `src/governor.rs:6014-6052`.**
Never calls the helper; re-implements it inline. Passes all four checks literally
(`if let` at `6015-6017`, `else` resetting to `None` at `6045-6047`), but does so by
hand-maintained duplication. Both paths are live: `run_governor_cycle` from the daemon loop
at `6583`/`6616`; `run_observe_cycle_internal` via `run_observe` at `5837`, dispatched from
`src/main.rs:1203` → `1655`.

**So: all delta computation *is* inside a Some-Some block — at each of two sites.** The
acceptance criteria are met per-site; they are not met in the singular sense the bead
implies.

## No stray delta logic outside a guard

Exhaustive grep for `p5h_delta =` / `p7d_delta =` / `p7ds_delta =` across `src/`. Only five
assignment clusters exist; every one outside the two production sites is test code:

| Location | Verdict |
|---|---|
| `governor.rs:4204-4206` | production — unconditional by design (see above) |
| `governor.rs:6037-6039`, `6045-6047` | production — inside if-let / else ✓ |
| `governor.rs:2407-2409`, `2750-2760` | test — `mod window_delta_tests` (`1287`–`3450`) |
| `governor.rs:10956-10958` | test — `mod mock_poller_tests` (from `9501`) |
| `snapshot_fixtures.rs:751-1001` | doc comments only, no assignment |

## Three further production `calculate_window_pct_delta` call sites

Not covered by the earlier enumeration in `notes/bf-56fov.md`, which scoped itself to state
delta *assignments*. These compute window deltas but feed burn-rate EMA and DB annotation —
they never touch `state.p5h_delta` / `p7d_delta` / `p7ds_delta`, so they are out of scope
for the bead's checks. Each is nonetheless guarded:

| Site | Guard | Consumes |
|---|---|---|
| `governor.rs:4499-4510` | `if let Some(snap) = old_snapshot.clone()` + elapsed-window bounds `4497` | burn-rate EMA |
| `governor.rs:4644-4655` | `if !state.usage.stale` + `if let (Some(ref prev_snap), Ok(conn))` `4638` | DB window-delta annotation |
| `governor.rs:6235-6246` | `if !state.usage.stale` `6222` + `if let Some(snap)` `6227` + elapsed bounds `6233` | burn-rate EMA |

No unguarded delta computation exists anywhere in production code.

## Acceptance criteria

- [x] `WindowPctSnapshot` creation for `prev_pct` inside the Some-Some block — both sites
- [x] `WindowPctSnapshot` creation for `curr_pct` inside the Some-Some block — both sites
- [x] `calculate_window_pct_delta` call inside the Some-Some block — both sites
- [x] State delta assignments inside the Some-Some block — literally at `6037-6039`;
      at `4204-4206` via the helper's `_` arm, an intentional and documented exception
- [x] No delta logic outside a guard — exhaustive grep, only test code remains
- [x] Code structure matches the bead requirements — with the two corrections recorded here:
      the cited line range is stale, and the computation is confined per-site but duplicated

## Residual risk (not fixed here)

The inline copy at `6014-6052` has **zero** test coverage — no test in `tests/` calls
`run_observe` or `run_observe_cycle_internal` — while ~12 unit tests and 7 integration
assertions cover the helper. Its `Mirrors run_governor_cycle` comment at `6044` is already
partly stale: it claims to mirror code that has since moved into the helper and to
unconditional assignment. Recommendation carried forward: delegate `6014-6052` to
`window_deltas_from_snapshots`. That is a separate bead.

## Related

- `notes/bf-3t7xa-verdict.md` — child bf-3btuv's verdict; this note confirms it and adds
  the three burn-rate/annotation call sites
- `notes/bf-56fov.md` — enumeration of production window-delta assignments
- `notes/bf-1uqqx.md` — field-by-field equivalence audit of the duplicate computation
