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
