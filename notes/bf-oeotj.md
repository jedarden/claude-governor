# bf-oeotj — Generalize the third window from `seven_day_sonnet` to dynamic `weekly_scoped`

Umbrella (parent) bead for the weekly_scoped generalization. The third usage window
is no longer the compile-time literal `"seven_day_sonnet"`; it is the generic
`"weekly_scoped"` slot whose identity is whatever model `poller::scoped_weekly()`
resolves this period (display name carried as metadata only). All work landed in the
five child beads below; this parent produced **no source changes** — it is the
verification + close-out step.

## Approach chosen: (a) — generic slot, model name as metadata

Decision recorded in the bead (prefer the smaller option unless per-model
calibration materially matters). Implemented and confirmed in code:

- `burn_rate.rs` — `const WINDOWS: &[&str] = &["five_hour", "seven_day", "weekly_scoped"];`
- `log_capacity_forecast(.., weekly_scoped_model: Option<&str>)` logs the third
  window under the resolved model's display label (e.g. "Fable"), with the doc
  comment: *"Metadata only — the binding key is unchanged."*
- `state.rs` — calibration persisted under fixed `weekly_scoped` fields
  (`usd_per_pct_ema_weekly_scoped`, etc.), **not** a per-model map. Approach (b)
  was correctly rejected: there is no evidence per-model burn characteristics
  differ enough to warrant keying state by model identity.

Null semantics: when `scoped_weekly()` returns `None`, `poller::window_or_default`
maps the window to (0% util, 168h reset) — immediately non-binding, never a
perpetual "insufficient data" hold.

## Child beads (all closed, on `origin/main`)

| Child | Commit | What |
|-------|--------|------|
| bf-2j6u6 | `8c63b95` | Parse generic `limits[]` array from the usage API (poller.rs) |
| bf-4hw10 | `0e8f05e` | Plumb resolved `weekly_scoped` model name through state |
| bf-355vx | `261c05d` | Surface resolved model name in logs/display |
| bf-4vjmw | `0970a1d` | Add binding + null non-binding acceptance tests |
| bf-1mw4x | `51b2fa0` | Finalize: run full suite, confirm green, push |

## Independent re-verification (this session)

Re-ran against `origin/main` (`51b2fa0`) rather than trusting bf-1mw4x's report.

### Parent acceptance criteria — both green
- `burn_rate::tests::weekly_scoped_becomes_binding_when_most_constrained` ✓
  Parameterized over `claude-sonnet-4` **and** `claude-fable-5`. Fixture has
  weekly_scoped at **79%** util, ~2h to reset. Asserts
  `binding_window == "weekly_scoped"`, `weekly_scoped.binding == true`, and
  `safe_worker_count == Some(1)` (no insufficient-data hold). The model-scoped
  window is *selected* as the real constraint — not hardcoded to Sonnet.
- `burn_rate::tests::absent_weekly_scoped_is_immediately_non_binding` ✓
  weekly_scoped absent (`None` burn) → 0% util / 168h reset → `binding == false`,
  never selected, ranks below the real constraint; the binding window that *is*
  selected carries a concrete `safe_worker_count`. No perpetual hold.

### Full suite — 697 passed, 0 failed
`cargo test --lib --tests` + `cargo test --doc`:

| Binary | Passed | Failed | Ignored |
|--------|--------|--------|---------|
| lib unit (burn_rate, poller, state, …) | 638 | 0 | 0 |
| other | 10 | 0 | 0 |
| fixtures | 12 | 0 | 0 |
| governor_cycle_behavior | 15 | 0 | 0 |
| governor_cycle_snapshot | 9 | 0 | 0 |
| safe_mode (integration) | 5 | 0 | 0 |
| doctests | 8 | 0 | 2 (pre-existing `#[ignore]`/`no_run` in state.rs) |
| **total** | **697** | **0** | **2** |

### No stale strings
- `grep -rn "seven_day_sonnet" src/ tests/` → **0 matches**
- `grep -rn "7d-sonnet" src/ tests/` → only **anti-assertions** and **comments**
  (`assert!(!output.contains("7d-sonnet"))`, "never the stale label"). No code
  *emits* the stale label.

Remaining `seven_day_sonnet` hits repo-wide are documentation only
(`docs/plan/plan.md`, `docs/research/usage-tracking.md`, `README.md`,
`config/governor.yaml` comment) — descriptive of the historical API field, not
load-bearing code.

## Git reconciliation
At start, local `main` (`7607f9e`) and `origin/main` (`51b2fa0`) were both titled
`docs(bf-1mw4x)` but had different SHAs — the parallel-dispatch duplicate-commit
artifact (identical trees `315df5d`, identical note file). Resolved with
`git reset --hard origin/main` — zero content loss. This note is committed on top.

## Outcome
All five children landed; umbrella verified green. Parent **bf-oeotj** unblocked
and ready to close.
