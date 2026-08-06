# bf-3t7xa — Final verdict: is delta computation confined to the Some-Some block?

**Bead:** bf-3btuv (final child of bf-3t7xa)
**Date:** 2026-08-05
**Scope:** compile check + written verdict. No code changes.

## 1. Compile check

Run with the real binary, because the `cargo` wrapper on `PATH` discards stderr and
exits 0 with no output even when compilation fails:

```
$ ~/.cargo/bin/cargo check 2>&1; echo "EXIT_STATUS: $?"
```

**Exit status: 0.** 15 warnings, 0 errors. Full output verbatim:

```
    Checking claude-governor v0.1.1 (/home/coding/claude-governor)
warning: unused import: `std::collections::HashMap`
  --> src/alerts.rs:12:5
   |
12 | use std::collections::HashMap;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `std::collections::HashMap`
  --> src/capacity_summary.rs:24:5
   |
24 | use std::collections::HashMap;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^

warning: unnecessary parentheses around method argument
    --> src/governor.rs:6314:9
     |
6314 |         (state.usage.five_hour_resets_at.parse::<DateTime<Utc>>()
     |         ^
6315 |             .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
6316 |             .unwrap_or(0.0)),
     |                            ^
     |
     = note: `#[warn(unused_parens)]` (part of `#[warn(unused)]`) on by default
help: remove these parentheses
     |
6314 ~         state.usage.five_hour_resets_at.parse::<DateTime<Utc>>()
6315 |             .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
6316 ~             .unwrap_or(0.0),
     |

warning: unnecessary parentheses around method argument
    --> src/governor.rs:6320:9
     |
6320 |         (state.usage.seven_day_resets_at.parse::<DateTime<Utc>>()
     |         ^
6321 |             .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
6322 |             .unwrap_or(0.0)),
     |                            ^
     |
help: remove these parentheses
     |
6320 ~         state.usage.seven_day_resets_at.parse::<DateTime<Utc>>()
6321 |             .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
6322 ~             .unwrap_or(0.0),
     |

warning: unnecessary parentheses around method argument
    --> src/governor.rs:6326:9
     |
6326 |         (state.usage.sonnet_resets_at.parse::<DateTime<Utc>>()
     |         ^
6327 |             .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
6328 |             .unwrap_or(0.0)),
     |                            ^
     |
help: remove these parentheses
     |
6326 ~         state.usage.sonnet_resets_at.parse::<DateTime<Utc>>()
6327 |             .map(|rt| (rt - now).num_seconds().max(0) as f64 / 3600.0)
6328 ~             .unwrap_or(0.0),
     |

warning: unused import: `std::collections::HashMap`
  --> src/narrator.rs:14:5
   |
14 | use std::collections::HashMap;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^

warning: unused doc comment
   --> src/poller.rs:293:17
    |
293 |                 /// **Model-agnostic weekly_scoped pct source: reads from limits[].percent**
    |                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
294 |                 utilization: limit.percent.unwrap_or(0.0),
    |                 ----------------------------------------- rustdoc does not generate documentation for expression fields
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: variable does not need to be mutable
    --> src/governor.rs:5147:13
     |
5147 |         let mut forecast = forecast;
     |             ----^^^^^^^^
     |             |
     |             help: remove this `mut`
     |
     = note: `#[warn(unused_mut)]` (part of `#[warn(unused)]`) on by default

warning: unused variable: `composite_risk_config`
    --> src/governor.rs:5904:5
     |
5904 |     composite_risk_config: &CompositeRiskConfig,
     |     ^^^^^^^^^^^^^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_composite_risk_config`
     |
     = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default

warning: unused variable: `cone_scaling_config`
    --> src/governor.rs:5905:5
     |
5905 |     cone_scaling_config: &ConeScalingConfig,
     |     ^^^^^^^^^^^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_cone_scaling_config`

warning: variable `total_tmux_count` is assigned to, but never used
    --> src/governor.rs:6198:9
     |
6198 |     let mut total_tmux_count = 0usize;
     |         ^^^^^^^^^^^^^^^^^^^^
     |
     = note: consider using `_total_tmux_count` instead

warning: unused variable: `target_ceiling`
    --> src/governor.rs:6216:9
     |
6216 |     let target_ceiling = pricing_config.daemon.target_ceiling;
     |         ^^^^^^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_target_ceiling`

warning: value assigned to `total_tmux_count` is never read
    --> src/governor.rs:6201:9
     |
6201 |         total_tmux_count += wc_count.tmux_count;
     |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
     |
     = help: maybe it is overwritten before being read?
     = note: `#[warn(unused_assignments)]` (part of `#[warn(unused)]`) on by default

warning: function `effective_burn_rate` is never used
    --> src/burn_rate.rs:1077:4
     |
1077 | fn effective_burn_rate(ema: &ModelWindowEma, baseline: &BaselineBurnRates) -> (f64, f64) {
     |    ^^^^^^^^^^^^^^^^^^^
     |
     = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: function `is_structurally_inactive` is never used
   --> src/governor.rs:128:4
    |
128 | fn is_structurally_inactive(window: &UsageWindow, state: &state::GovernorState) -> bool {
    |    ^^^^^^^^^^^^^^^^^^^^^^^^

warning: `claude-governor` (lib) generated 15 warnings (run `cargo fix --lib -p claude-governor` to apply 10 suggestions)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.69s
EXIT_STATUS: 0
```

None of the 15 warnings touches the delta code. The three `governor.rs` warnings nearest
the delta block (`5904`, `5905`, `6198`/`6201`, `6216`) are unused parameters and an
unused tmux counter in `run_observe_cycle_internal` and its neighbours, unrelated to
`p5h_delta` / `p7d_delta` / `p7ds_delta`.

## 2. Verdict

**Neither a clean yes nor a clean no.** The parent bead asks whether delta computation is
confined to a single Some-Some block. In production there are **two** sites, and they
answer the question differently:

**Site 1 — `window_deltas_from_snapshots`, `src/governor.rs:1228-1255`. Clean.**
The whole computation is inside the `(Some(prev), Some(curr))` match arm at `1233-1246`:
`prev_pct` is built at `1234-1238`, `curr_pct` at `1239-1243`, `calculate_window_pct_delta`
is called at `1244`, and the arm returns `(Some, Some, Some)` at `1245`. Every other input
shape falls to `_ => (None, None, None)` at `1253`. Nothing leaks outside the match. This
is the site the parent's four checks describe, and it passes all four.

The one wrinkle: its caller `run_governor_cycle` assigns the three state fields
**unconditionally** at `4204-4206`, outside any Some-Some block — the `match` above it at
`4179-4199` exists only to pick a log line and assigns nothing. That is not stray logic.
The Some-Some decision has simply been pushed down one level into the helper, whose `_` arm
guarantees the fields are `Some` only when both snapshots exist, so no stale `Some(..)` can
survive a cycle. The in-code comment at `4201-4203` states exactly this intent. Read
strictly, `4204-4206` is an intentional exception to "all delta logic inside the if-let";
read by behaviour, the invariant holds.

**Site 2 — `run_observe_cycle_internal`, `src/governor.rs:6014-6052`. A second inline copy.**
This function never calls the helper. It re-implements it: `if let (Some(prev), Some(curr))`
at `6015-6017`, `prev_pct` at `6018-6022`, `curr_pct` at `6023-6027`,
`calculate_window_pct_delta` at `6028-6029`, and the three `Some(..)` assignments at
`6037-6039`, with a matching `else` at `6040-6052` that resets all three to `None` at
`6045-6047`. Structurally this *does* satisfy the parent's checks — everything is inside
the if-let or its else — but it satisfies them in a second place, by hand-maintained
duplication rather than by shared code.

Both are live production paths: `run_governor_cycle` is called by the daemon loop at
`6583` and `6616`; `run_observe_cycle_internal` is reached via `run_observe` at `5837`,
dispatched from `src/main.rs:1203` → `1655`, and its result is persisted at `6528-6530`.

So the honest answer is: **delta computation is confined to a Some-Some block at each of
the two production sites, but it is not confined to *one* site.** The duplication is
behaviourally equivalent today (verified field-by-field in bf-1uqqx: identical
`WindowPctSnapshot` construction, identical field pairing, identical `Some` wrapping,
identical `(None, None, None)` fallback, identical preconditions), differing only in log
detail — `run_governor_cycle` logs prev/curr percentages alongside the deltas at
`4188-4192`, the observe copy logs deltas only at `6031-6034`. The risk is not present-day
correctness but drift: the inline copy has **zero** test coverage (no test in `tests/`
calls `run_observe` or `run_observe_cycle_internal`), while ~12 unit tests and 7
integration assertions cover the helper. Every test that "proves the first-poll contract"
proves it for the helper only. The `Mirrors run_governor_cycle` comment at `6044` is the
sole thing holding the invariant, and it is already partly stale — it claims to mirror a
function that has since moved to the helper and to unconditional assignment.

Recommendation carried forward from bf-1uqqx: delegate `6014-6052` to
`window_deltas_from_snapshots`. That is a separate bead; nothing was changed here.

## 3. The parent bead's line numbers are stale

**`governor.rs:2585-2609` — cited in the bf-3t7xa description — is wrong and must not be
repeated.** That range is not production code and contains no delta computation. It sits
inside `fn test_consecutive_snapshots_governor_cycle` (starts `2249`), inside
`#[cfg(test)] mod window_delta_tests` (`1287-3451`). Lines `2585-2590` are an `assert!` on
`aggregate_deltas.weekly_scoped`; `2592-2611` serialize `GovernorState` and check that the
three delta field names survive round-tripping. Verified against the working tree at
commit `bd1ead8`.

Corrected anchors — use these instead:

| What | Correct location | Notes |
|---|---|---|
| Canonical Some-Some delta computation | `src/governor.rs:1228-1255` | `window_deltas_from_snapshots`; arm at `1233-1246`, fallback `_` at `1253` |
| `prev_pct` construction (canonical) | `src/governor.rs:1234-1238` | inside the Some-Some arm |
| `curr_pct` construction (canonical) | `src/governor.rs:1239-1243` | inside the Some-Some arm |
| `calculate_window_pct_delta` call (canonical) | `src/governor.rs:1244` | inside the Some-Some arm |
| Helper definition | `src/governor.rs:1181` | `pub fn calculate_window_pct_delta` |
| Helper invocation from `run_governor_cycle` | `src/governor.rs:4174-4177` | fn declared at `4055` |
| State delta assignment (`run_governor_cycle`) | `src/governor.rs:4204-4206` | **unconditional, outside the match** — intentional exception |
| Logging-only match | `src/governor.rs:4179-4199` | assigns nothing |
| Second inline copy — if-let | `src/governor.rs:6015-6039` | `run_observe_cycle_internal`, declared at `5898` |
| Second inline copy — else / None reset | `src/governor.rs:6040-6052` | assignments at `6045-6047` |
| State delta assignment (`run_observe_cycle_internal`) | `src/governor.rs:6037-6039` | inside Some-Some ✓ |
| Field declarations | `src/state.rs:829, 833, 837` | `Option<f64>` |
| `Default` initialisers | `src/state.rs:878-880` | all `None` |
| ~~`src/governor.rs:2585-2609`~~ | **stale — test assertions only** | `test_consecutive_snapshots_governor_cycle`, `window_delta_tests` |

## Acceptance criteria

- [x] `cargo check` exit status and output reported verbatim from `~/.cargo/bin/cargo` — exit 0, 15 warnings, 0 errors
- [x] Verdict paragraph covering both production sites — §2, nuanced rather than a forced yes/no
- [x] Explicit statement that the parent bead line numbers were stale, with the correct ones — §3
- [x] Findings written to `notes/bf-3t7xa-verdict.md`

## Related

- `notes/bf-56fov.md` — full enumeration of every production delta assignment
- `notes/bf-1uqqx.md` — equivalence audit of the duplicate computation
- `notes/bf-1z5eg.md`, `notes/bf-40fjd.md`, `notes/bf-1row2-some-some-containment.md`
