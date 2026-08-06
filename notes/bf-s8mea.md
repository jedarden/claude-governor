# bf-s8mea — verifying delta logging end to end

Write-up file for the umbrella bead **bf-1fwf5** (*Verify delta logging end to
end with a manual governor run*). Each child bead appends its own section.

---

## bf-2m9rt — Runtime emit sites for the delta log lines

Scope-setting only: this section locates every runtime site that emits the log
lines added by bf-omuq9 (`format_window_deltas`), bf-4yj6o (wiring both cycle
paths), and bf-nr365 (the no-previous-snapshot message), and picks the
credential-free way to drive two poll cycles through one of them.

No production code was changed for this bead.

### The two formatters

Both are pure `String` builders in `src/governor.rs`; neither logs or does I/O,
so the log level and target are entirely the caller's choice.

| Function | Definition | Renders |
| --- | --- | --- |
| `format_window_deltas` | `src/governor.rs:1228` | `window deltas: 5h=±X.XX% (a→b), 7d=…, 7ds=… [prev_ts → curr_ts]` |
| `format_no_previous_snapshot` | `src/governor.rs:1288` | `no previous snapshot yet (first poll or poll following a failure); window deltas unavailable this poll. current: 5h=…, 7d=…, 7ds=… [curr_ts]` |

### Emit sites

There are exactly **four** `format_*` emit sites, in two clusters of two, plus a
`debug!` fallback per cluster. Every one prefixes the rendered line with
`[governor] ` and emits through the `log` facade, so the target is the module
path **`claude_governor::governor`** in both the daemon binary and the test
harness (`src/main.rs` consumes the library crate — `use claude_governor::…` —
so there is no separate binary-crate module path to worry about).

#### Cluster A — `run_governor_cycle` (the scaling daemon)

Enclosing function: `pub fn run_governor_cycle` (`src/governor.rs:4220`),
generic over `poller: &mut impl UsagePoller`.

| file:line | Emits | Level | Target | Reached when |
| --- | --- | --- | --- | --- |
| `src/governor.rs:4365` (call at `:4367`) | `format_window_deltas` | `INFO` | `claude_governor::governor` | `window_deltas_from_snapshots` returned all three deltas **and** both `previous_api_snapshot` and `current_api_snapshot` are `Some` — i.e. the second and later successful polls |
| `src/governor.rs:4379` (call at `:4381`) | `format_no_previous_snapshot` | `INFO` | `claude_governor::governor` | No baseline, but `current_api_snapshot` is `Some` — the first successful poll after start or a state clear, and the poll following a failed one |
| `src/governor.rs:4385` | literal `no current API snapshot; window deltas cleared for this poll` | `DEBUG` | `claude_governor::governor` | `current_api_snapshot` is `None` (defensive; unreachable in practice because `:4328` assigns it unconditionally on the `Ok` arm) |

What drives it: `run_governor_cycle` is only ever called from
`run_daemon` (`src/governor.rs:6748`) — once for the initial cycle at
`:6791` and once per `loop_interval` tick at `:6824`. `run_daemon` is reached
from `main.rs:1229` via `run_daemon_command`, i.e. the `daemon` subcommand and
the hidden systemd `_daemon` path (`main.rs:1165`, `main.rs:1647`).

Ordering note that matters for the harness: the emit at `:4365` sits **before**
the collector pass (`:4442`), the SQLite fleet-aggregate read (`:4460`) and the
tmux worker count. So a cycle reaches the delta line as long as
`poller.poll_usage()` returns `Ok`; nothing downstream can suppress it.

#### Cluster B — `run_observe_cycle_internal` (the `_observe` one-shot)

Enclosing function: `fn run_observe_cycle_internal` (`src/governor.rs:6088`),
private, taking a **concrete** `poller: &mut Poller`.

| file:line | Emits | Level | Target | Reached when |
| --- | --- | --- | --- | --- |
| `src/governor.rs:6221` (call at `:6223`) | `format_window_deltas` | `INFO` | `claude_governor::governor` | `if let (Some(prev), Some(curr))` over `previous_api_snapshot` / `current_api_snapshot` matches — second and later successful observe polls |
| `src/governor.rs:6249` (call at `:6251`) | `format_no_previous_snapshot` | `INFO` | `claude_governor::governor` | `else` branch of that guard, with `current_api_snapshot` `Some` — first observe poll after start or a state clear |
| `src/governor.rs:6255` | literal `no current API snapshot; window deltas cleared for this poll` | `DEBUG` | `claude_governor::governor` | `current_api_snapshot` is `None` (same defensive case) |

What drives it: `run_observe_cycle_internal` has exactly one caller,
`pub fn run_observe` (`src/governor.rs:6005`), which is called only from
`run_internal_observe_command` (`src/main.rs:1664`) behind the hidden
`_observe` subcommand (`main.rs:435`, dispatched at `main.rs:1202`).

Both clusters share the same snapshot rotation — `previous = current.take()` at
the top of the cycle (`:4249` for A, `:6106` for B) — so "first poll emits the
no-baseline line, second poll emits deltas" holds identically on both paths.

### Chosen credential-free approach for driving two cycles

**A new integration test binary under `tests/` that defines its own
`UsagePoller` implementation and calls `run_governor_cycle` twice** — Cluster A,
the `:4379` line on cycle 1 and the `:4365` line on cycle 2.

Why this and not the alternatives:

- **`run_governor_cycle` is the only path a fake poller can reach.** It is
  `pub` and generic over `impl UsagePoller`. `run_observe_cycle_internal` is
  private and takes a concrete `&mut Poller`, and its only public entry
  (`run_observe`, `:6019`) constructs a real `Poller` from
  `pricing_config.credentials_path` internally. Cluster B cannot be driven
  without credentials at all short of a production signature change — out of
  scope here, and unnecessary since bf-4yj6o made both clusters render through
  the same two formatters, so verifying A verifies the rendered text of B.
- **The harness must own the process-global logger**, because the acceptance
  criterion for bf-1fwf5 is a *captured log excerpt*, and these lines only exist
  as `log::info!` records. A separate integration-test binary can call
  `log::set_logger` exactly once and assert on the captured records — the
  pattern already proven in `tests/heartbeat_orphan_cleanup_test.rs:18-43`. The
  in-crate unit tests cannot: they share one test binary (and therefore one
  global logger) with every other `#[cfg(test)]` module in the crate.
- **`MockPoller` is unusable from `tests/`**, being `#[cfg(test)]`
  (`src/governor.rs:9477`, impl at `:9698`) — a limitation already documented in
  `tests/governor_cycle_behavior_test.rs:6-8`. The fix is *not* to un-gate it
  (that would be a production change). `UsagePoller` is `pub`
  (`src/poller.rs:709`) and every field of `UsageData` (`src/poller.rs:236-260`)
  is `pub`, so a five-line fake poller in the test file gets the same result
  with zero production churn.
- **`src/simulator.rs` is the wrong tool.** It projects a forward trajectory
  from an existing `GovernorState` (`simulate(&state, &config, promotions)`); it
  contains no reference to `poll`, `UsagePoller`, or `run_governor_cycle`, and
  never enters a cycle function. It cannot emit these lines.
- **`src/snapshot_fixtures.rs` cannot drive cycles either** — it only builds
  `PrevUsageSnapshot` values. It is still *useful* to the harness as the source
  of the two readings: `snapshot_pair_5h()` (`:288`) gives a realistic
  prev/curr pair whose percentages the fake poller can return on poll 1 and
  poll 2, so the logged numbers are checkable against known inputs.

Sketch of the harness the follow-up bead should build (not implemented here):

1. New file `tests/delta_logging_runtime_test.rs`, owning the global logger via
   the `TestLogger` / `OnceLock<Mutex<Vec<(Level, String)>>>` pattern at
   `tests/heartbeat_orphan_cleanup_test.rs:18-43`, with
   `log::set_max_level(LevelFilter::Info)`.
2. A local `struct FakePoller { readings: Vec<UsageData>, n: usize }` with
   `impl claude_governor::poller::UsagePoller`, returning reading 1 then
   reading 2, sourced from `snapshot_fixtures::snapshot_pair_5h()`.
3. `TempDir` state path; call `run_governor_cycle(&mut fake, &state_path,
   /* dry_run */ true, …)` twice, exactly as the existing two-cycle test at
   `src/governor.rs:10414`/`:10468` does — except that test uses the **real**
   `Poller` and therefore silently no-ops the delta lines when credentials are
   absent, which is precisely the gap bf-1fwf5 exists to close.
4. Assert cycle 1 produced an `INFO` record containing `no previous snapshot`
   with the poll-1 percentages, and cycle 2 an `INFO` record containing
   `window deltas:` with both timestamps and signed deltas matching the two
   readings.

`dry_run = true` keeps the cycle off the tmux scaling path
(`src/governor.rs:5827`). The collector pass, the SQLite read and the worker
count are all fault-tolerant (`match … Err => warn!`, `if let Ok(conn)`) and,
as noted above, all run *after* the emit, so a machine without `~/.claude` data
or a fleet DB still reaches both log lines.

---

## bf-3i9t2 — Two-cycle harness that captures the delta log output

Built the harness bf-2m9rt specified, as `tests/delta_logging_runtime_test.rs`
(one test: `two_cycles_emit_the_delta_log_lines`). No production code changed.

Shape, as sketched in the section above:

- `TestLogger` over `OnceLock<Mutex<Vec<(Level, String)>>>` (the pattern from
  `tests/heartbeat_orphan_cleanup_test.rs:18-43`), installed once at
  `LevelFilter::Info` — this binary owns the process-global logger.
- `FakePoller`, a local `impl claude_governor::poller::UsagePoller` that returns
  a scripted `Vec<UsageData>`, one reading per cycle. No credentials, no
  network, no un-gating of `MockPoller`.
- Readings built from `snapshot_fixtures::snapshot_pair_5h()` — 5h 12.5→18.2%,
  7d 45.2→46.8%, 7ds 38.7→40.3%. `limits` is empty so `scoped_weekly()` is
  `None` and the cycle falls back to `weekly_scoped_utilization`;
  `weekly_scoped_model` is held at `None` across both polls so the
  model-rotation EMA reset never fires.
- Two `run_governor_cycle` calls against a `TempDir` state path with
  `dry_run = true`, cycle 1 starting with no state file on disk.

Assertions are sliced by log index (`log_len()` before each cycle), so a cycle-2
assertion cannot be satisfied by a cycle-1 record. Per cycle: the expected line
appears exactly once, at `Level::Info`, and the *other* line does not appear at
all. `poller.polls == 2` at the end confirms both cycles actually polled.

Negative check (the criterion that the test fails if the emit is removed), run
by editing `src/governor.rs` and restoring it:

| Emit removed | Result |
| --- | --- |
| both (`:4365` and `:4379`) | FAILED at the cycle-1 no-baseline assertion |
| `format_window_deltas` only (`:4365`) | FAILED at the cycle-2 window-deltas assertion |
| neither (restored) | ok |

So each of the two emits is independently load-bearing for this test.

Confirmed on the way through: the delta lines really do precede the collector
pass and the worker count, and those later stages ran and logged on a host with
no fleet DB configured without failing the cycle — `run_governor_cycle` returned
`Ok` both times.

`cargo test` (via `~/.cargo/bin/cargo`): 740 lib tests plus all integration
binaries pass, including the new one.

Scope kept to presence, level and ordering of the two lines. Checking the
numbers they carry against the fixture inputs is the next bead — note that the
timestamps in the rendered line come from `Utc::now()` at cycle time, not from
the fixtures' `taken_at`, since the cycle stamps its own snapshots.

---

## bf-4o9bk — The real cycle 1 / cycle 2 output, and the arithmetic checked by hand

> **Re-captured after bf-3r0is.** The excerpts and the hand-check below were
> refreshed from a run made *after* the timestamp fix bf-3r0is applied, so this
> section shows the final output rather than the format it found. The findings
> at the end are as bf-4o9bk wrote them; each one's resolution is in the
> bf-3r0is section that follows.

### Which path produced these

**The integration test** — `tests/delta_logging_runtime_test.rs`, the harness
bf-3i9t2 built. Not the simulator (it never enters a cycle function) and not a
live run (that needs credentials; `run_observe` builds a real `Poller`
internally, and the daemon path is the same poll).

Command, run from the repo root:

```
RUST_LOG=info ~/.cargo/bin/cargo test --test delta_logging_runtime_test -- --nocapture
```

`RUST_LOG` is inert here and is recorded only because the bead asked for it: the
harness owns the process-global logger itself and pins
`log::set_max_level(LevelFilter::Info)`
(`tests/delta_logging_runtime_test.rs:64-72`), so the level is INFO regardless of
the environment. The only change this bead made to the harness is `dump_cycle`
(`:85-99`), which prints each cycle's captured records verbatim between banners;
the assertions are untouched.

### The inputs that produced the excerpts

Both readings come from `snapshot_fixtures::snapshot_pair_5h()` =
`(baseline_snapshot(), snapshot_after_5h())`, converted to `UsageData` by
`usage_data_from` (`tests/delta_logging_runtime_test.rs:134`).

| Fixture | `taken_at` | `five_hour_pct` | `seven_day_pct` | `weekly_scoped_pct` |
| --- | --- | --- | --- | --- |
| `baseline_snapshot()` (`src/snapshot_fixtures.rs:82`) — cycle 1's poll | `2026-03-18T10:00:00Z` | 12.5 | 45.2 | 38.7 |
| `snapshot_after_5h()` (`src/snapshot_fixtures.rs:120`) — cycle 2's poll | `2026-03-18T15:00:00Z` | 18.2 | 46.8 | 40.3 |

The `taken_at` column is listed because the bead asked for it, but see
finding **F1** — those two timestamps do **not** appear in the output, by
design. The timestamps that do appear are the cycle wall-clock instants:

| | Cycle wall clock (`let now = Utc::now()`, `src/governor.rs:4237`) |
| --- | --- |
| cycle 1 | `2026-08-06T06:50:51.731050204+00:00` |
| cycle 2 | `2026-08-06T06:50:53.187327079+00:00` |

The delta lines render these to milliseconds (`…51.731Z`, `…53.187Z`) — see the
bf-3r0is section; the `=== cycle start ===` lines quoted below are a different
log site and still print nanoseconds.

### Cycle 1 — no previous snapshot

Verbatim, the full set of records the harness captured during the first
`run_governor_cycle` call. `[LEVEL] ` is the harness's own prefix from
`dump_cycle`; everything after it is the record as emitted.

```
===== BEGIN CYCLE 1 =====
[INFO] [governor] === cycle start at 2026-08-06T06:50:51.731050204+00:00 ===
[INFO] [governor] polled usage: weekly_scoped=38.7%, all_models=45.2%, 5h=12.5%
[INFO] [governor] weekly_scoped model change detection: prev_model=None, new_model=None, new_weekly_scoped_pct=38.70%
[INFO] [governor] no previous snapshot yet (first poll or poll following a failure); window deltas unavailable this poll. current: 5h=12.50%, 7d=45.20%, 7ds=38.70% [2026-08-06T06:50:51.731Z]
[WARN] Unknown model 'claude-opus-5', falling back to 'claude-opus-4-7' for pricing — add it to governor.yaml
[INFO] [collector] pass complete: 18 lines, 1 instances, $0.4362 total
[INFO] [governor] collector pass: 18 lines, 1 instances, $0.4362 total
[INFO] [governor] workers: 0 active (0 heartbeats, 0 tmux sessions, consistent=true, agents=1)
[INFO] [governor] EMA input: weekly_scoped_model=None, weekly_scoped_pct=38.70% (this is the actual pct from the rotated model)
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[INFO] [governor] 5h: 77.5% remaining, resets in 4.0h — exhausts in 49.4h at 12 workers
[INFO] [governor] 7d: 44.8% remaining, resets in 120.0h CUTOFF_RISK — exhausts in 28.5h at 0 workers
[INFO] [governor] weekly_scoped: 51.3% remaining, resets in 120.0h BINDING CUTOFF_RISK — exhausts in 32.7h at 0 workers
[INFO] [governor] → safe_worker_count: 0 workers from binding window weekly_scoped
[INFO] [governor] target workers: 0 (ceiling: 90%)
[INFO] [governor] no scaling action this cycle (dry-run)
[INFO] [governor] === cycle complete (decision: NoChange, next in 60s) ===
===== END CYCLE 1 =====
```

The line under test is the fourth one. No `window deltas:` line appears anywhere
in the cycle, which is the contract: cycle 1 has no baseline and prints no
deltas, not even zeros.

### Cycle 2 — computed deltas

```
===== BEGIN CYCLE 2 =====
[INFO] [governor] === cycle start at 2026-08-06T06:50:53.187327079+00:00 ===
[INFO] [governor] polled usage: weekly_scoped=40.3%, all_models=46.8%, 5h=18.2%
[INFO] [governor] weekly_scoped model change detection: prev_model=None, new_model=None, new_weekly_scoped_pct=40.30%
[INFO] [governor] window deltas: 5h=+5.70% (12.50%→18.20%), 7d=+1.60% (45.20%→46.80%), 7ds=+1.60% (38.70%→40.30%) [2026-08-06T06:50:51.731Z → 2026-08-06T06:50:53.187Z, Δt=1.5s]
[WARN] Unknown model 'claude-opus-5', falling back to 'claude-opus-4-20250514' for pricing — add it to governor.yaml
[INFO] [collector] pass complete: 2 lines, 1 instances, $0.0952 total
[INFO] [governor] collector pass: 2 lines, 1 instances, $0.0952 total
[INFO] [governor] workers: 0 active (0 heartbeats, 0 tmux sessions, consistent=true, agents=1)
[INFO] [governor] EMA input: weekly_scoped_model=None, weekly_scoped_pct=40.30% (this is the actual pct from the rotated model)
[WARN] [governor] skipping window delta annotation: worker count changed mid-interval (1 -> 0)
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[WARN] [governor] no subscription agents configured, using default baseline for dollar staleness checks
[INFO] [governor] 5h: 71.8% remaining, resets in 4.0h — exhausts in 209.6h at 52 workers
[INFO] [governor] 7d: 43.2% remaining, resets in 120.0h — exhausts in 126.1h at 1 workers
[INFO] [governor] weekly_scoped: 49.7% remaining, resets in 120.0h BINDING — exhausts in 145.1h at 1 workers
[INFO] [governor] → safe_worker_count: 1 workers from binding window weekly_scoped
[INFO] [governor] target workers: 0 (ceiling: 90%)
[INFO] [governor] no scaling action this cycle (dry-run)
[INFO] [governor] === cycle complete (decision: NoChange, next in 60s) ===
===== END CYCLE 2 =====
```

No `no previous snapshot` line appears in cycle 2 — the baseline rotated in, so
the cycle stops reporting one missing.

### Hand-check of the cycle 2 numbers

Isolating the line:

```
window deltas: 5h=+5.70% (12.50%→18.20%), 7d=+1.60% (45.20%→46.80%), 7ds=+1.60% (38.70%→40.30%) [2026-08-06T06:50:51.731Z → 2026-08-06T06:50:53.187Z, Δt=1.5s]
```

**Percentages against the fixtures.** Each window's `(a→b)` pair must be
`(baseline, after_5h)` for that window:

| Window | printed prev | fixture `baseline_snapshot()` | printed curr | fixture `snapshot_after_5h()` | match |
| --- | --- | --- | --- | --- | --- |
| `5h` | 12.50% | `five_hour_pct: 12.5` | 18.20% | `five_hour_pct: 18.2` | yes |
| `7d` | 45.20% | `seven_day_pct: 45.2` | 46.80% | `seven_day_pct: 46.8` | yes |
| `7ds` | 38.70% | `weekly_scoped_pct: 38.7` | 40.30% | `weekly_scoped_pct: 40.3` | yes |

So `7ds` is fed by `weekly_scoped_pct`, not by any separate Sonnet field — the
label is a legacy name for the weekly-scoped window. Confirmed independently by
the `polled usage:` line of cycle 2, which reports the same value as
`weekly_scoped=40.3%`.

**Each signed delta = current − previous, for its own window.** Arithmetic done
by hand from the table above:

| Window | subtraction | exact result | printed (`{:+.2}`) | match |
| --- | --- | --- | --- | --- |
| `5h` | 18.2 − 12.5 | 5.7 | `+5.70%` | yes |
| `7d` | 46.8 − 45.2 | 1.6 | `+1.60%` | yes |
| `7ds` | 40.3 − 38.7 | 1.6 | `+1.60%` | yes |

No cross-wiring: the `5h` delta uses only the `5h` pair, and each of `7d` / `7ds`
likewise — visible from the fact that `7d` and `7ds` both print `+1.60%` while
their operand pairs differ (45.20→46.80 vs 38.70→40.30), so neither is a copy of
the other. Sign is `+` in all three, matching current > previous everywhere.

Float note: none of these subtractions is exact in binary f64 (e.g. 18.2 − 12.5
evaluates to 5.699999999999999), but `{:+.2}` rounds to the same two decimals as
the exact decimal arithmetic, so the printed values agree with the hand
computation. This is why the check is stated to two decimals and not more.

**Both snapshot timestamps appear.** The line ends with two RFC 3339 instants,
`prev.taken_at → curr.taken_at`, rendered to milliseconds. Cross-checking them
against the rest of the capture:

- the second, `2026-08-06T06:50:53.187Z`, is cycle 2's `=== cycle start at
  2026-08-06T06:50:53.187327079+00:00 ===` truncated to milliseconds;
- the first, `2026-08-06T06:50:51.731Z`, is **cycle 1's** `cycle start`
  (`…51.731050204+00:00`) truncated the same way, *and* is byte-identical to the
  single timestamp in cycle 1's no-baseline line — that one is rendered by the
  same helper, so the two agree exactly.

That is the rotation working end to end: the instant cycle 1 stamped onto
`current_api_snapshot` is exactly the instant cycle 2 reports as `previous`.

**The interval agrees with the timestamps it sits beside.** `53.187 − 51.731 =
1.456 s`, which `Δt` renders to one decimal as `1.5s`. That subtraction is
doable by eye from the printed pair, which is the point of dropping the six
nanosecond digits rather than the whole fractional part.

### Findings for the next bead

**F1 — the fixtures' `taken_at` values never reach the line, so the printed
interval is meaningless as elapsed time.** The bracketed pair is 1.43 s apart
(the gap between the two `run_governor_cycle` calls), not the 5 h the fixture
pair models. This is not a bug in the formatter: `run_governor_cycle` stamps
`taken_at: now` from `Utc::now()` (`src/governor.rs:4328`, `now` bound at
`:4237`) and `UsageData` carries no snapshot time for it to use instead, so the
fixture's own `taken_at` is discarded on the way in. The consequence for any
future test is that **the timestamps in this line cannot be asserted against a
fixture** — only against each other and against the cycle-start lines, as done
above. If a test ever needs a realistic elapsed interval in the rendered line,
that requires a production change (threading a poll timestamp through
`UsageData`), which is out of scope for this bead and should be its own decision.

**F2 — nanosecond precision makes the timestamp pair hard to read and hides the
one number a reader wants.** `to_rfc3339()` emits 9 fractional digits, so the
bracket alone is 76 characters of a ~200-character line, and the interval
(1.43 s here) has to be computed mentally from two 30-character strings.
Candidate fix for a follow-up: render seconds precision
(`to_rfc3339_opts(SecondsFormat::Secs, true)`) and/or append the elapsed
duration, e.g. `[… → …, Δt=1.4s]`. Worth noting this is exactly the interval a
reader needs to judge whether `+5.70%` is alarming — a delta without its
duration is not a rate.

**F3 — the surrounding lines are not reproducible between runs; the two delta
lines are.** An earlier run of the same command in the same working tree
produced a different cycle 1 tail — `5h: 77.5% remaining, resets in 4.0h —
exhausts in 15.2h at 3 workers`, `7d: … CUTOFF_RISK`, `→ safe_worker_count: 0
workers from binding window weekly_scoped` — where the run transcribed above
prints `exhausts in infh` and `insufficient burn rate data`. The state file is a
fresh `TempDir` each run, so this variance comes from host state the cycle reads
outside it (the collector pass over `~/.claude` — note `collector pass: 1 lines`
in cycle 1 above versus `0 lines` in cycle 2 — and the fleet-aggregate SQLite
read). The `window deltas:` and `no previous snapshot` lines were identical
across both runs apart from the wall-clock timestamps, which is the property this
verification depends on. Anything asserting on *other* lines of this capture
would be flaky.

**F4 — no mismatch found in the numbers themselves.** Every delta, every
percentage pair, and both timestamps check out as shown above. The formatting
observations F1–F3 are the only defects, and none of them is a wrong value.

### Test status

`~/.cargo/bin/cargo test`: exit 0 — 740 lib tests plus every integration binary,
0 failed. The `dump_cycle` addition is print-only and changes no assertion;
`two_cycles_emit_the_delta_log_lines` still passes. No production code changed by
this bead.

---

## bf-3r0is — Resolving the formatting findings

Closing bead for **bf-1fwf5**. Each of bf-4o9bk's four findings is settled
below: two needed no code (one was an explicit "nothing wrong"), one is fixed in
`src/governor.rs`, one is out of scope and now has a bead.

**Nothing here changes a number.** F4 stands: every delta, percentage pair and
timestamp bf-4o9bk checked was already correct, and this bead re-checked them
against the post-fix capture above. The changes are to how the timestamps and
the interval are *rendered*.

### Disposition

| Finding | Verdict | Where it went |
| --- | --- | --- |
| **F1** — fixture `taken_at` never reaches the line, so the interval is wall-clock, not modelled time | Real, out of scope | Follow-up bead **bf-1v9x8** |
| **F2** — nanosecond timestamps are unreadable and the interval must be computed mentally | Real, fixed here | `src/governor.rs` — `format_snapshot_instant`, `format_elapsed` |
| **F3** — surrounding lines vary between runs; the two delta lines do not | Not a defect in these lines | No action; reconfirmed, see below |
| **F4** — no mismatch in any number | Not a defect | No action |

No finding was left unaddressed, and none of the five specific problems the bead
listed (wrong sign, missing window label, missing timestamp, misleading
precision, truncated line) turned out to be present beyond the misleading
precision of F2 — signs, labels and timestamps were all correct, and no line was
truncated.

### F2 — the fix

Two private helpers in `src/governor.rs`, used by both formatters:

- **`format_snapshot_instant`** renders an instant with
  `to_rfc3339_opts(SecondsFormat::Millis, true)`. Milliseconds rather than
  seconds because the two timestamps and the new `Δt` have to stay checkable
  against each other by hand — with seconds precision the capture above would
  read `…51Z → …53Z, Δt=1.5s`, and a reader would be right to distrust it. The
  bracketed pair drops from 76 characters to 60, and `Z` replaces `+00:00`.
- **`format_elapsed`** renders the interval, coarsening units as it grows:
  `1.4s`, `5m0s`, `5h30m`. A delta without its duration is not a rate, and this
  is the number an operator needs to judge whether `+5.70%` is alarming.
  Negative intervals (backwards clock, snapshots rotated out of order) print
  signed rather than clamped to `0.0s`, so the anomaly stays visible.

Before and after, on the same two snapshots:

```
window deltas: … 7ds=+1.60% (38.70%→40.30%) [2026-08-06T06:45:40.917196817+00:00 → 2026-08-06T06:45:42.350087396+00:00]
window deltas: … 7ds=+1.60% (38.70%→40.30%) [2026-08-06T06:50:51.731Z → 2026-08-06T06:50:53.187Z, Δt=1.5s]
```

`format_no_previous_snapshot` gets the same instant rendering and deliberately
**no** `Δt` — there is no previous instant to measure from, and a `Δt=0.0s`
there would be the same fabrication as the `0.00%` deltas that function already
refuses to print.

### Tests locking the format in

In `src/governor.rs`, module `governor::window_delta_tests`:

| Test | Locks |
| --- | --- |
| `test_format_window_deltas_positive` (updated) | full line, `Δt=5m0s` over a 5-minute gap |
| `test_format_window_deltas_negative` (updated) | full line, `Δt=5h30m` over a 5.5-hour gap |
| `test_format_window_deltas_subsecond_interval_is_readable` (new) | the exact capture above, byte for byte, plus the absence of the nanosecond digits |
| `test_format_window_deltas_negative_interval_keeps_its_sign` (new) | `Δt=-5.0s` for a backwards clock |
| `test_format_elapsed_units_and_boundaries` (new) | unit selection, sign, and that rounding never spills a field past its range — `59.950s` renders `1m0s`, never `60.0s`; `3599.6s` renders `1h0m`, never `60m0s` |
| `test_format_no_previous_snapshot_line` (new) | full no-baseline line; that function had assertions only in its doctest before |

Both doctests were updated to match. `tests/delta_logging_runtime_test.rs`
needed no change — it asserts on the `window deltas:` / `no previous snapshot`
substrings, which the fix does not touch.

### F3 — reconfirmed, still not actionable

The run that produced the refreshed excerpts is a third data point for F3: its
cycle 1 reports `collector pass: 18 lines, 1 instances, $0.4362 total` and
`CUTOFF_RISK` on two windows, against `1 lines, 0 instances, $0.0000` and no
risk flags in bf-4o9bk's run and a different tail again in the run before that.
The two delta lines were identical across all three apart from the wall clock.
That is the property the verification rests on, so no fix is warranted — the
finding is a caveat for anyone tempted to assert on the *other* lines of this
capture, and no test does.

### F1 — why it became a bead rather than a fix

`format_window_deltas` takes `prev_at` / `curr_at` as arguments; it renders
faithfully whatever the cycle hands it. The problem is upstream: `UsageData`
(`src/poller.rs:236-260`) has no field for when the reading was taken, so
`run_governor_cycle` stamps `taken_at: Utc::now()` (`src/governor.rs:4328`) and
the fixture's own `taken_at` is discarded on the way in. Fixing that means
adding a field to the poller's data model and deciding what a poll timestamp
means for a cached or stale response — a production change well outside delta
*logging*. Filed as **bf-1v9x8** (P3), with the acceptance criterion that a
two-cycle test with readings hours apart must render the fixture interval rather
than the wall-clock gap between cycles.

Worth being precise about the blast radius, since F1 now also applies to the
`Δt` this bead added: in the live daemon the poll happens at the top of the
cycle, so the rendered interval is the true one plus the cycle's own runtime —
close enough to read as a rate. It is only badly wrong where the poll and the
cycle are separated, as in the fixture-driven harness (1.5s rendered for a pair
that models 5h).

### Test status

`~/.cargo/bin/cargo test`: exit 0 — 744 lib tests (740 plus the 4 added here),
every integration binary, and the doctests, 0 failed.
