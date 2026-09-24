# Off-Peak Promotion Windows — Source, Configuration, and Semantics

How cgov models a limited-time off-peak usage promotion (e.g. the March 2026
2x promotion, background in
[`docs/research/off-hours-promotion.md`](../research/off-hours-promotion.md)):
where the multiplier is configured, how timezones and boundaries are handled,
and where exactly it enters burn-rate and remaining-capacity forecasting.
Written alongside the regression suite
`tests/offpeak_promotion_window_forecasting.rs` (claudego-4ec0225e).

## 1. Source of truth

| What | Where |
|---|---|
| Configuration file | `config/promotions.json` (repo root; currently `[]` — no active promotion) |
| Loader | `schedule::load_promotions` (`src/schedule.rs`), path from `default_promotions_path()` (`src/main.rs`) |
| Loaded by | `cgov daemon`, `cgov _observe`, `cgov _act` (once at startup, promotions for the whole process lifetime) and `cgov simulate` |
| Semantics | `Promotion` struct + `get_multiplier_at` / `effective_hours_remaining_from` (`src/schedule.rs`) |

The file holds a JSON **array** of promotion objects. A missing file, invalid
JSON, or an unreadable file is not an error: the loader logs and returns an
empty list, which means **1x everywhere** — the governor runs the same code
path with and without a promotion. Editing the file requires a daemon restart;
promotions are not re-read mid-flight.

## 2. Configuration schema

```json
[
  {
    "name": "March 2026 2x off-peak",
    "start_date": "2026-03-15",
    "end_date": "2026-03-25",
    "peak_start_hour_et": 8,
    "peak_end_hour_et": 14,
    "offpeak_multiplier": 2.0,
    "applies_to": ["weekly_scoped"]
  }
]
```

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | — | Human-readable label; surfaces in state/logs |
| `start_date` | `YYYY-MM-DD` | — | First **Eastern** calendar day the promotion is active (inclusive) |
| `end_date` | `YYYY-MM-DD` | — | First **Eastern** calendar day it is no longer active (exclusive) |
| `peak_start_hour_et` | hour | `8` | Start of the weekday peak block |
| `peak_end_hour_et` | hour | `14` | End of the weekday peak block |
| `offpeak_multiplier` | float | — | Multiplier applied to off-peak hours for listed windows (2.0 = 2x) |
| `applies_to` | string[] | — | Which subscription windows get the boost: `five_hour`, `seven_day`, `weekly_scoped` |

`applies_to` is the per-subscription-window gate. A promotion listing only
`weekly_scoped` leaves `five_hour` and `seven_day` at exactly 1.0 / raw
wall-clock hours even in the middle of the promotion's off-peak — this is the
behaviour the `multiplier_applies_to_filtering` unit test and the integration
suite pin.

## 3. Timezone handling

- Peak/off-peak classification and promotion-date activation are computed in
  **America/New_York** via `chrono-tz` (`to_eastern`), so DST transitions are
  handled by the tz database — March 2026 is EDT (UTC-4) throughout the
  example window. Internally everything is `DateTime<Utc>`; the conversion
  happens only where the wall-clock bands are defined.
- The promotion's date bounds are **Eastern calendar dates**: `is_promo_active_at`
  converts the instant to ET *first*, then compares the ET date against
  `[start_date, end_date)`. Consequences worth internalising (both pinned by
  regression tests):
  - `2026-03-15T01:00Z` is March 15 on the UTC calendar but still 21:00
    March 14 in New York → promotion **not** active.
  - `2026-03-25T02:00Z` is March 25 on the UTC calendar but still 22:00
    March 24 in New York → promotion **still** active; the last Eastern
    evening keeps its boost.
- An unparseable `start_date`/`end_date` makes that promotion permanently
  inactive (logged warning), not an error.

## 4. Boundary handling

All boundaries are **half-open** — the start is in, the end is out:

| Boundary | Rule | Consequence |
|---|---|---|
| Peak hours | `[peak_start, peak_end)` weekdays | 08:00 ET is peak, 14:00 ET is off-peak |
| Weekend | Saturday/Sunday ET | Always off-peak, all day, regardless of peak hours |
| Promotion dates | `[start_date, end_date)` in ET | The first day is active; `end_date` itself is not |
| Capacity walk | 1-minute steps over `[now, reset_time)` | `reset <= now` yields 0.0; partial minutes are pro-rated |

## 5. Where the multiplier enters the forecast

Two independent paths, gated differently (observe cycle, `src/governor.rs`):

1. **Remaining capacity** — `effective_hours_remaining_from(now, reset_time,
   promotions, window)` walks the span at 1-minute resolution, multiplying
   each minute by `get_multiplier_at` (2x only for windows listed in
   `applies_to`, only off-peak, only in the date range). Lands in
   `state.schedule.effective_hours_remaining_{five_hour,seven_day,weekly_scoped}`.
   This path uses the **declared** config directly — it is *not* gated on
   empirical validation.
2. **Burn-rate side** — `validate_promotion_from_db` compares the observed
   off-peak/peak tokens-per-percent ratio from the collector mirror against
   the declared multiplier; `effective_multiplier` then picks:
   - within ±10% of declared → the **declared** multiplier;
   - observed ratio > 2.5 (anomaly) → the observed ratio;
   - anything else (including "no data" / < 10 samples per side) →
     conservative **1.0**.

   The result lands per-window in `state.schedule.promo_multiplier_*` (1.0
   during peak or for unlisted windows regardless of validation) and in the
   `state.burn_rate.promotion_*` / `offpeak_ratio_*` fields.

The asymmetry is deliberate and pinned by
`unvalidated_promotion_keeps_burn_multiplier_at_1x_but_capacity_still_boosts`:
capacity forecasting trusts the configured multiplier, burn attribution
demands evidence. The validation constants live in `src/burn_rate.rs`
(`MIN_VALIDATION_SAMPLES`, `VALIDATION_TOLERANCE`, `PROMO_NOT_APPLYING_THRESHOLD`,
`ANOMALY_THRESHOLD`).

## 6. Regression coverage

| Surface | Location |
|---|---|
| Multiplier, effective hours, transitions (units) | `#[cfg(test)] mod tests` in `src/schedule.rs` |
| Full observe-cycle window/time matrix + ET date bounds (integration) | `tests/offpeak_promotion_window_forecasting.rs` |
| Empirical validation live path (annotation → mirror → validator) | `tests/promotion_validation_live_path_test.rs` |
