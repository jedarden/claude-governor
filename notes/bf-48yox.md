# bf-48yox — Final sweep for stray delta logic + `cargo check`

**Date:** 2026-08-05
**Scope:** read-only verification, no code changes
**Files under review:** `src/governor.rs` (12560 lines), plus every other `.rs` in the repo
**Tree state:** `src/` clean at verification time (`git status` showed only
`.beads/issues.jsonl` modified and an untracked note), so all line numbers below are current.
**Parent umbrella:** bf-3t7xa "Verify delta computation location"

## Method

Grepped the whole repo for the four markers named in the bead —
`WindowPctSnapshot`, `calculate_window_pct_delta`, and
`p5h_delta` / `p7d_delta` / `p7ds_delta`:

```
grep -rn -E 'calculate_window_pct_delta|WindowPctSnapshot|p5h_delta|p7d_delta|p7ds_delta' \
     --include='*.rs' src/ tests/
```

Each hit was then classified production vs. test by computing the top-level
`#[cfg(test)] mod …` line ranges per file and testing membership, rather than eyeballing.

**346 occurrences total → 50 production, 296 test** (271 inside `#[cfg(test)]` modules in
`src/`, 25 in `tests/`). Test-module ranges used for `src/governor.rs`:
`governor_state_tests` 814–954, `window_delta_tests` 1287–3451, `tests` 6642–9259,
`mock_poller_tests` 9500–11425, `annotation_guard_tests` 11427–12263,
`is_structurally_inactive_tests` 12265–12560.

## 1. Production sweep — `src/governor.rs` (41 occurrences, 8 sites)

| # | Lines | Enclosing fn | What it does | Account |
| --- | --- | --- | --- | --- |
| A | 281–291 | `check_window_reset` (doc + signature) | takes two `&db::WindowPctSnapshot` to detect a window reset | **justified** — type use only; no delta arithmetic, no state write |
| B | 1172–1189 | `calculate_window_pct_delta` (rustdoc + definition) | the `current − previous` primitive itself | **justified** — the definition |
| C | 1195–1255 | `window_deltas_from_snapshots` | **AUTHORITATIVE BLOCK.** `match (previous, current)`; arm `(Some(prev), Some(curr))` @ 1233 builds `prev_pct` 1234–1238 and `curr_pct` 1239–1243, calls `calculate_window_pct_delta` @ 1244, returns `(Some(delta_5h), Some(delta_7d), Some(delta_7ds))` @ 1245; `_ => (None, None, None)` @ 1253 | **authoritative** |
| D | 4174–4206 | `run_governor_cycle` | calls C @ 4174–4177; `match` @ 4179–4199 is **logging only** (every arm body is `log::info!`/`log::debug!`); unconditional `state.p5h_delta / p7d_delta / p7ds_delta = …` @ 4204–4206 | **accounted** — the consumer of C; unconditional assignment is what makes a stale `Some(..)` unable to survive a baseline-less cycle |
| E | 4499–4510 | `run_governor_cycle` | EMA / burn-rate path: builds `old_pct`/`new_pct`, calls `calculate_window_pct_delta` @ 4510 under `if let Some(snap) = old_snapshot` @ 4493 **plus** `!state.usage.stale` and `60s ≤ elapsed ≤ 1800s` | **justified** — different consumer (burn-rate forecasting); **writes none of the three state fields**. Single-`Some` because the "current" side reads plain `f64` off `state.usage`, so a Some/Some pattern is not expressible |
| F | 4644–4649 | `run_governor_cycle` | builds two `db::WindowPctSnapshot`s to hand to `db::annotate_window_pct_deltas` | **justified** — DB record annotation; no `calculate_window_pct_delta` call, no state write |
| G | 6018–6047 | `run_observe_cycle_internal` | **inline second copy** of C: `if let (Some(prev), Some(curr))` @ 6015–6017, `prev_pct` 6018–6022, `curr_pct` 6023–6027, call @ 6028–6029, `Some(..)` writes @ 6037–6039, `else` @ 6040 clearing all three to `None` @ 6045–6047 | **reported — see §3.** Correctly guarded and behaviorally identical to C, but a duplicate |
| H | 6235–6246 | `run_observe_cycle_internal` | mirror of E on the observe path | **justified** — same reasoning as E; no state write |

## 2. Production sweep — all other files (9 occurrences)

| Site | What it is | Account |
| --- | --- | --- |
| `src/db.rs:690` | `pub struct WindowPctSnapshot` | the type definition |
| `src/db.rs:715–716` | `annotate_window_pct_deltas(old_pct, new_pct, …)` params | DB annotation consumer; no delta arithmetic on the three state fields |
| `src/state.rs:829, 833, 837` | `pub p5h_delta / p7d_delta / p7ds_delta: Option<f64>` field declarations | the fields themselves |
| `src/state.rs:878–880` | `impl Default for GovernorState` → all three `None` | correct pre-first-poll state; not a delta computation |

Everything else that matched — `src/alerts.rs:1066–1068`, `src/burn_rate.rs:3929/4013/4018`,
`src/capacity_summary.rs:285–287`, `src/narrator.rs:611–613`,
`src/snapshot_fixtures.rs:751–1001` (doc comments on tests), `src/state.rs:1390–1392`,
`src/status_display.rs:729–731`, `src/db.rs` ×8, and all 25 hits in
`tests/governor_cycle_snapshot_test.rs` — sits inside a `#[cfg(test)] mod`. None is
production.

**No file outside `src/governor.rs` computes a window delta or writes the three state
fields.**

## 3. Stray delta logic outside the if-let pattern: NONE

Answering the parent's criterion literally: **no production code computes a window pct
delta or writes `p5h_delta`/`p7d_delta`/`p7ds_delta` outside a Some/Some guard.**

- The only two production writers of the three state fields are **D (4204–4206)** and
  **G (6037–6039 / 6045–6047)**. D's values come from C's Some/Some arm; G's are produced
  inside its own `if let (Some, Some)` with an `else` that clears all three. Both paths
  cover the no-baseline case explicitly, so neither can leak a stale `Some(..)`.
- The three sites that touch `WindowPctSnapshot` **without** a Some/Some guard — E
  (4499–4510), F (4644–4649), H (6235–6246) — are burn-rate/EMA and DB-annotation
  consumers. None calls into the delta state fields, and E/H are guarded *more* strictly
  than C (staleness + elapsed-window bounds on top of the `Some`).

**One carry-forward, not a defect (re-confirming bf-4xxa6 and bf-4928z):** site G is a
second copy of the decision rather than a call to `window_deltas_from_snapshots`. Both
copies agree today; the risk is drift, since a fix to the helper would silently not reach
`run_observe_cycle_internal`. Collapsing 6015–6052 onto the helper would reduce it to the
same three unconditional assignments used at 4204–4206. Out of scope for this bead — no
code was changed.

A third copy of the same shape lives at **2407–2409** in `#[cfg(test)] mod
window_delta_tests`, under a comment saying it "simulates the delta computation that
happens in run_governor_cycle". It is test-local and not production, but it will drift too.

## 4. `cargo check`

### Gotcha found while running it

`cargo` on `PATH` resolves to `/home/coding/.local/bin/cargo`, a bash wrapper that offloads
builds through `systemd-run … 2>/dev/null` (line 9). **It discards cargo's entire stderr**,
which is where `Checking …`, all warnings, all errors, and `Finished` go. Result: every
`cargo check` invocation through the wrapper returns exit 0 with **zero bytes of output on
both streams**, whether or not the crate compiles. Do not trust a silent success from
`cargo check` in this repo — invoke `~/.cargo/bin/cargo` directly. The target directory is
also `/home/coding/target`, not `./target`.

### Verbatim result

Run with `src/governor.rs` touched first, so this is a genuine re-check of the file under
review rather than a cache hit:

```
$ touch src/governor.rs
$ ~/.cargo/bin/cargo check --all-targets
    Checking claude-governor v0.1.1 (/home/coding/claude-governor)
warning: unused import: `std::collections::HashMap`
  --> src/alerts.rs:12:5
   |
12 | use std::collections::HashMap;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^
...
warning: `claude-governor` (lib) generated 15 warnings (run `cargo fix --lib -p claude-governor` to apply 10 suggestions)
warning: `claude-governor` (lib test) generated 18 warnings (11 duplicates) (run `cargo fix --lib -p claude-governor --tests` to apply 7 suggestions)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.66s
$ echo $?
0
```

- **Exit status: 0.**
- **Errors: 0** (`grep -c '^error' → 0`).
- Warnings: 40 lines, all pre-existing lint noise — unused imports
  (`alerts.rs:12`, `capacity_summary.rs:24`, `narrator.rs:14`, `burn_rate.rs:3716/3828/3903`),
  unused variables (`governor.rs:5904`, `5905`, `6216`, `7600`, `7637`, `7883`, `7888`),
  `unnecessary parentheses` (`governor.rs:6314/6320/6326`), dead code
  (`burn_rate.rs:1077 effective_burn_rate`, `governor.rs:128 is_structurally_inactive`),
  and similar in `tests/`. **None touches `WindowPctSnapshot`,
  `calculate_window_pct_delta`, or the three delta fields.**

A full `--all-targets` run including the integration tests in `tests/` also exits 0 with
the same warning set plus `safe_mode_stdout_notification_test`,
`weekly_scoped_model_rotation_test`, and `first_startup_cold_start_test` warnings.

## Verdict on the parent bead (bf-3t7xa) acceptance criteria

**PASS, with one correction to the parent's own line citation.** All window-pct delta
computation in production is inside a Some/Some guard, and there is no stray delta logic
outside the if-let pattern: the governor polling path's decision was extracted into
`window_deltas_from_snapshots` (`src/governor.rs:1228–1255`), whose
`(Some(prev), Some(curr))` arm builds both snapshots, calls `calculate_window_pct_delta`,
and returns `Some`s — with `_ => (None, None, None)` for the no-baseline case — and
`run_governor_cycle` assigns that result to the three state fields unconditionally at
4204–4206; the observe path keeps its own equivalent `if let (Some, Some)` / `else` at
6015–6052. The three remaining production `WindowPctSnapshot` sites (4499–4510, 4644–4649,
6235–6246) belong to the burn-rate/EMA and DB-annotation consumers, write none of the three
state fields, and are guarded at least as strictly. The correction: the parent's cited
range `governor.rs:2585–2609` is now **test code** inside `#[cfg(test)] mod
window_delta_tests` — the citation was pinned to a grep pattern the refactor moved, so it
should be updated to 1228–1255 + 4174–4206 (governor path) and 6015–6052 (observe path).
The crate compiles: `cargo check --all-targets` exits 0 with 0 errors. The only open item
is the non-blocking duplication of the decision between C and G, already recorded as a
carry-forward by bf-4xxa6 and bf-4928z and left unchanged here.
