//! Fixture-based null-tolerance tests for the two deserialization surfaces the
//! governor loads every cycle (CLAUDE.md §4).
//!
//! The inline null tests in `src/poller.rs` and `src/state.rs` exercise the
//! fix with JSON strings written next to the assertions. These tests pin the
//! same behaviour against **committed fixture files** — the full on-disk shape
//! each surface actually reads — so a schema change that only shows up on a
//! realistic document (new sibling field, changed nesting, renamed key) fails
//! here even if every hand-written fragment still parses.
//!
//! - `fixtures/usage/usage_response_null_window.json` — a `/api/oauth/usage`
//!   response whose `weekly_scoped` window is `null`, which the API
//!   legitimately returns when an account has no separate scoped limit. The
//!   poll must parse it (poller.rs `UsageResponse` windows are `Option`) and
//!   the null window must deserialize as `None` — the non-binding reading —
//!   rather than failing the poll and starving the governor of capacity data.
//! - `fixtures/state/governor_state_null_fields.json` — a `governor-state.json`
//!   in the exact shape that caused claudego-55841cf6: every
//!   `f64::INFINITY` field (`predicted_exhaustion_hours`, `margin_hrs`,
//!   `exh_hrs_p25/p50/p75`, `risk_score`, `hard_limit_margin_hrs`) serialized
//!   as `null` because JSON has no infinity literal. The load must succeed
//!   with `null → INFINITY` round-tripped (state.rs
//!   `deserialize_f64_null_as_infinity`), and the learned calibration riding
//!   alongside those fields — burn-rate EMAs, per-model samples, scored
//!   predictions — must come through intact instead of being discarded to a
//!   fresh-start every cycle.
//!
//! Both fixtures are embedded with `include_str!` (bytes fixed at compile
//! time), so the tests never touch the live state file and never depend on a
//! runtime path resolving.

use claude_governor::poller::UsageResponse;
use claude_governor::state::GovernorState;

const USAGE_NULL_WINDOW_JSON: &str =
    include_str!("fixtures/usage/usage_response_null_window.json");
const GOVERNOR_STATE_NULL_FIELDS_JSON: &str =
    include_str!("fixtures/state/governor_state_null_fields.json");

/// Guard that the fixture still exercises the case it exists for: a window
/// the API returns as null. If the fixture is edited to drop the null, these
/// tests must fail loudly rather than pass vacuously.
#[test]
fn fixtures_still_contain_the_null_cases_they_pin() {
    assert!(
        USAGE_NULL_WINDOW_JSON.contains(r#""weekly_scoped": null"#),
        "usage fixture must carry a null weekly_scoped window"
    );
    for field in [
        "predicted_exhaustion_hours",
        "margin_hrs",
        "risk_score",
        "hard_limit_margin_hrs",
    ] {
        assert!(
            GOVERNOR_STATE_NULL_FIELDS_JSON.contains(&format!(r#""{field}": null"#)),
            "state fixture must carry a null {field}"
        );
    }
}

/// The committed usage response with a null `weekly_scoped` window parses,
/// the null window lands as `None` (the non-binding reading), and the
/// sibling windows plus the generic `limits[]` array all come through.
#[test]
fn usage_response_fixture_with_null_window_parses() {
    let resp: UsageResponse =
        serde_json::from_str(USAGE_NULL_WINDOW_JSON).expect("null window must not fail the parse");

    // The null window is None, not an error and not a default-constructed
    // window that could be mistaken for a real reading.
    assert!(resp.weekly_scoped.is_none());

    // Sibling windows present in the same response parse with their values.
    let seven_day = resp.seven_day.as_ref().expect("seven_day present");
    assert_eq!(seven_day.utilization, 42.0);
    assert_eq!(seven_day.resets_at, "2026-09-26T03:00:00Z");
    let five_hour = resp.five_hour.as_ref().expect("five_hour present");
    assert_eq!(five_hour.utilization, 10.5);

    // The generic limits[] array parses alongside the legacy windows,
    // including entries with a null scope and the scoped model metadata.
    let limits = resp.limits.expect("limits array present");
    assert_eq!(limits.len(), 3);
    assert_eq!(limits[0].kind.as_deref(), Some("session"));
    assert!(limits[0].scope.is_none(), "null scope tolerates as None");
    assert_eq!(limits[2].kind.as_deref(), Some("weekly_scoped"));
    let model = limits[2]
        .scope
        .as_ref()
        .and_then(|s| s.model.as_ref())
        .expect("scoped model parsed");
    assert_eq!(model.display_name.as_deref(), Some("Fable"));
}

/// The committed governor-state.json in the claudego-55841cf6 shape — every
/// infinity-valued forecast field serialized as null — loads cleanly, the
/// nulls round-trip to INFINITY, and the learned calibration is retained.
#[test]
fn governor_state_fixture_with_null_inf_fields_loads_with_calibration_intact() {
    let state: GovernorState = serde_json::from_str(GOVERNOR_STATE_NULL_FIELDS_JSON)
        .expect("null state fields must not fail the load");

    // Every null-serialized infinity round-trips to INFINITY on all three
    // windows: exhaustion predictions, margins, cone bounds, risk score, and
    // the hard-platform-limit margin.
    for (name, wf) in [
        ("five_hour", &state.capacity_forecast.five_hour),
        ("seven_day", &state.capacity_forecast.seven_day),
        ("weekly_scoped", &state.capacity_forecast.weekly_scoped),
    ] {
        assert!(
            wf.predicted_exhaustion_hours.is_infinite(),
            "{name}: predicted_exhaustion_hours null must round-trip to INFINITY"
        );
        assert!(
            wf.margin_hrs.is_infinite(),
            "{name}: margin_hrs null must round-trip to INFINITY"
        );
        assert!(wf.exh_hrs_p25.is_infinite());
        assert!(wf.exh_hrs_p50.is_infinite());
        assert!(wf.exh_hrs_p75.is_infinite());
        assert!(
            wf.risk_score.is_infinite(),
            "{name}: risk_score null must round-trip to INFINITY"
        );
        assert!(
            wf.hard_limit_margin_hrs.is_infinite(),
            "{name}: hard_limit_margin_hrs null must round-trip to INFINITY"
        );
    }

    // cone_ratio is only INFINITY where the fixture carries null; where it
    // carries a measured ratio the number survives verbatim.
    assert_eq!(state.capacity_forecast.five_hour.cone_ratio, 1.35);
    assert!(state.capacity_forecast.seven_day.cone_ratio.is_infinite());
    assert_eq!(state.capacity_forecast.weekly_scoped.cone_ratio, 1.1);

    // The non-null parts of the same document are sane: binding decision,
    // safe worker counts, worker pool state, and the usage reading.
    assert_eq!(state.capacity_forecast.binding_window, "weekly_scoped");
    assert_eq!(
        state.capacity_forecast.weekly_scoped.safe_worker_count,
        Some(2)
    );
    assert_eq!(state.capacity_forecast.five_hour.safe_worker_count, Some(4));
    assert_eq!(state.usage.weekly_scoped_pct, 79.0);
    assert_eq!(state.usage.weekly_scoped_model.as_deref(), Some("Fable"));
    assert_eq!(state.workers["claude-anthropic-sonnet"].current, 3);

    // The point of the fix: the learned calibration riding in the same file
    // is not discarded to a fresh start — burn-rate history, EMAs, and the
    // scored-prediction state all come through the load.
    let sonnet = &state.burn_rate.by_model["claude-sonnet-4-20250514"];
    assert_eq!(sonnet.pct_per_worker_per_hour, 1.35);
    assert_eq!(sonnet.samples, 214);
    let fable = &state.burn_rate.by_model["claude-fable-5"];
    assert_eq!(fable.pct_per_worker_per_hour, 3.8);
    assert_eq!(fable.samples, 36);
    assert_eq!(state.burn_rate.fleet_pct_ema_samples, 118);
    assert!(!state.burn_rate.fleet_pct_hr_ema.five_hour.is_infinite());
    assert!(!state.burn_rate.fleet_pct_hr_ema.five_hour.is_nan());
    assert_eq!(state.burn_rate.fleet_pct_hr_ema.seven_day, 1.84);
    assert_eq!(state.burn_rate.usd_per_pct_ema_weekly_scoped, 0.62);
    assert_eq!(state.burn_rate.calibration.predictions_scored, 96);
    assert_eq!(state.burn_rate.calibration.median_error_7ds, -3.2);
    let snapshot = state
        .burn_rate
        .prev_usage_snapshot
        .expect("previous API snapshot retained");
    assert_eq!(snapshot.weekly_scoped_pct, 78.5);
}
