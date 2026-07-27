# bf-1mw4x — Finalize & verify the weekly_scoped generalization

Final child (4 of 4) of parent **bf-oeotj** (generalize the third window from
hardcoded `seven_day_sonnet` to the dynamic `weekly_scoped` model). This child is
the land/finalize step: run the full suite, reconcile fixtures if needed, confirm
no stale strings remain, and ensure the chain is committed and pushed.

The code work itself landed in children 1–3 (already on `origin/main`):

| Child | Commit | What |
|-------|--------|------|
| bf-2j6u6 | `8c63b95` | Parse generic `limits[]` array from the usage API (poller.rs) |
| bf-4hw10 | `0e8f05e` | Plumb resolved `weekly_scoped` model name through state |
| bf-355vx | `261c05d` | Surface resolved model name in logs/display |
| bf-4vjmw | `0970a1d` | Add binding + null non-binding acceptance tests |

This child produced **no source changes** — it is verification only.

## Verification results

### 1. Full test suite — 100% green
`cargo test --lib --tests` + `cargo test --doc`:

| Binary | Passed | Failed | Ignored |
|--------|--------|--------|---------|
| lib unit (incl. burn_rate, poller, state) | 638 | 0 | 0 |
| fixtures | 12 | 0 | 0 |
| governor_cycle_behavior | 15 | 0 | 0 |
| governor_cycle_snapshot | 9 | 0 | 0 |
| safe_mode (integration) | 5 | 0 | 0 |
| (other) | 10 | 0 | 0 |
| doctests | 8 | 0 | 2 (pre-existing intentional `ignore`/`compile`-only in state.rs) |

Total **689 passed, 0 failed, 0 filtered**. The 2 ignored doctests are pre-existing
`#[ignore]`/`no_run` items in `state.rs`, unrelated to this work.

### 2. Parent's two core acceptance criteria — verified green (run explicitly)
- `burn_rate::tests::weekly_scoped_becomes_binding_when_most_constrained` ✓
  (parameterized over Sonnet + Fable — the model-scoped weekly window at 79% with a
  near reset wins on risk_score and is *selected* as binding, not hardcoded to Sonnet)
- `burn_rate::tests::absent_weekly_scoped_is_immediately_non_binding` ✓
  (weekly_scoped absent → 0% util / far reset → immediately non-binding, never held
  pending as "insufficient data")

### 3. No stale display/fixture strings
- `grep -rn "seven_day_sonnet" src/ tests/` → **0 matches**
- `grep -rn "7d-sonnet" src/ tests/` → only **anti-assertions** and **comments**
  (`assert!(!output.contains("7d-sonnet"))`, "never the stale `7d-sonnet`/`sonnet`
  label"). No code *emits* the stale label. This is the intended clean state.

### 4. Snapshot/JSON fixtures — no reconciliation needed
- `src/snapshot_fixtures.rs` and `tests/fixtures.rs` are **Rust struct builders**
  already generalized to `weekly_scoped` (`weekly_scoped_pct`, `weekly_scoped:
  WindowForecast`, `binding_window: "weekly_scoped"`).
- There are **no serialized golden/JSON snapshot files** in the repo (no insta,
  no `UPDATE_SNAPSHOTS`, no `expected_*.json`).
- The snapshot tests are compute-and-assert, and all pass → the fixtures already
  match actual output. Nothing to regenerate; nothing hand-edited.

### 5. Local/origin divergence — reconciled
Local `main` had `528e7e8` while `origin/main` had `0970a1d` — both the bf-4vjmw
child with **identical trees** (`git rev-parse <c>^{tree}` matched: `21029de…`), a
duplicate-commit artifact of parallel dispatch. Resolved by `git reset --hard
origin/main` (no content loss — trees were identical). The full chain
(`8c63b95 → 0e8f05e → 261c05d → 0970a1d`) is present on `origin/main`.

## Outcome
All four children are committed and pushed on `origin/main`. Parent **bf-oeotj**
is unblocked and ready to close.
