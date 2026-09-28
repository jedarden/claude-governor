//! Fixture-matrix tests for the `/api/oauth/usage` wire contract
//! (`docs/research/usage-tracking.md` §2) — the clauses the mockito suite in
//! `usage_polling_contract.rs` does not exercise, pinned here against
//! **committed fixture files** in the `null_tolerance_fixture_test.rs`
//! convention (`include_str!`, bytes fixed at compile time, guard tests so
//! editing away the pinned case fails loudly instead of passing vacuously).
//!
//! - `fixtures/usage/usage_response_documented_shape.json` — the §2 response
//!   structure verbatim: the known windows, the unknown-to-cgov sibling
//!   fields (`seven_day_sonnet`, `extra_usage`, …), and the generic
//!   `limits[]` array in one realistic body. A schema change that only shows
//!   up on the full document fails here even though every hand-written
//!   fragment in the mockito suite still parses.
//! - `fixtures/usage/usage_response_inactive_and_unknown_limits.json` — the
//!   two wire clauses the suite left unpinned: `is_active: false` on a
//!   top-level window and on a `limits[]` entry (the documented
//!   only-`false`-means-structurally-inactive semantics — the governor's
//!   `is_structurally_inactive` unit tests construct these values directly,
//!   but no wire fixture ever carried them through `UsageResponse`), and an
//!   unknown `kind` value, which the §2 contract rules promise is tolerated
//!   without disturbing `weekly_scoped` resolution.
//! - `fixtures/usage/usage_response_non_utc_reset_offsets.json` — the same
//!   reset instant expressed at `+05:30` and `Z`; `hours_remaining` must be
//!   computed from the normalized UTC instant, so the two forms agree.
//!   Every existing test used only `+00:00`/`Z` shapes.
//!
//! Response-condition classes (claudego-6d535c0a) — the §10 fixture corpus is
//! the committed home for the four wire-condition classes the usage API
//! produces. successful → `usage_response_documented_shape.json`; expired →
//! `usage_response_non_utc_reset_offsets.json` (its fixed instants are months
//! in the past, so the parse must yield long-negative hours, not an error);
//! incomplete and invalid are pinned by the fixtures below:
//!
//! - `fixtures/usage/usage_response_incomplete.json` — a plan-shaped body in
//!   which every scoped/op window is `null` and the generic `limits[]` array
//!   is absent outright: the named windows parse through, the absent ones
//!   default, and the whole poll still succeeds.
//! - `fixtures/usage/usage_response_invalid_wrong_typed_utilization.json` and
//!   `fixtures/usage/usage_response_invalid_missing_required_field.json` —
//!   the two invalid-body clauses: `utilization` present but wrong-typed, and
//!   a present window with `utilization` absent outright. Both fail the
//!   `UsageResponse` parse; `fetch_usage` wraps the same serde error as
//!   `PollerError::ParseError` (that end-to-end hop is the mockito suite's,
//!   tests/usage_polling_contract.rs).

use chrono::{DateTime, Utc};

use claude_governor::poller::{UsageData, UsageResponse};

const DOCUMENTED_SHAPE_JSON: &str =
    include_str!("fixtures/usage/usage_response_documented_shape.json");
const INACTIVE_AND_UNKNOWN_JSON: &str =
    include_str!("fixtures/usage/usage_response_inactive_and_unknown_limits.json");
const NON_UTC_OFFSETS_JSON: &str =
    include_str!("fixtures/usage/usage_response_non_utc_reset_offsets.json");
const INCOMPLETE_JSON: &str = include_str!("fixtures/usage/usage_response_incomplete.json");
const INVALID_WRONG_TYPED_JSON: &str =
    include_str!("fixtures/usage/usage_response_invalid_wrong_typed_utilization.json");
const INVALID_MISSING_FIELD_JSON: &str =
    include_str!("fixtures/usage/usage_response_invalid_missing_required_field.json");

/// Guard that each fixture still carries the case it exists for. If a fixture
/// is edited to drop its pinned clause, these must fail loudly rather than
/// let the parse tests pass vacuously.
#[test]
fn fixtures_still_contain_the_clauses_they_pin() {
    // Documented shape: the unknown-to-cgov sibling fields and the generic
    // limits[] array must both be present for the body to stay realistic.
    assert!(
        DOCUMENTED_SHAPE_JSON.contains(r#""seven_day_sonnet""#),
        "documented-shape fixture must carry the unknown-to-cgov seven_day_sonnet field"
    );
    assert!(
        DOCUMENTED_SHAPE_JSON.contains(r#""extra_usage""#),
        "documented-shape fixture must carry the extra_usage field"
    );
    assert!(
        DOCUMENTED_SHAPE_JSON.contains(r#""kind": "weekly_scoped""#),
        "documented-shape fixture must carry the limits[] weekly_scoped entry"
    );

    assert!(
        INACTIVE_AND_UNKNOWN_JSON.contains(r#""is_active": false"#),
        "inactive/unknown fixture must carry an is_active: false clause"
    );
    assert!(
        INACTIVE_AND_UNKNOWN_JSON.contains(r#""kind": "weekly_capacity_v3""#),
        "inactive/unknown fixture must carry an unknown limits[] kind"
    );

    // Both strings denote 2026-03-18T13:59:59.918852 UTC — the premise the
    // offset-normalization test asserts on.
    assert!(
        NON_UTC_OFFSETS_JSON.contains("+05:30") && NON_UTC_OFFSETS_JSON.contains("13:59:59.918852Z"),
        "offset fixture must carry the +05:30 and Z forms of the same instant"
    );
}

/// The §2 documented body — known windows, unknown-to-cgov siblings, and the
/// generic `limits[]` array in one realistic response — parses with the full
/// field mapping intact.
#[test]
fn documented_shape_fixture_parses_with_full_field_mapping() {
    let resp: UsageResponse =
        serde_json::from_str(DOCUMENTED_SHAPE_JSON).expect("documented shape must parse");

    // The known legacy windows, verbatim.
    let five_hour = resp.five_hour.as_ref().expect("five_hour present");
    assert_eq!(five_hour.utilization, 14.0);
    assert_eq!(five_hour.resets_at, "2026-03-18T13:59:59.918852+00:00");
    let seven_day = resp.seven_day.as_ref().expect("seven_day present");
    assert_eq!(seven_day.utilization, 82.0);
    assert_eq!(seven_day.resets_at, "2026-03-20T03:00:00.918880+00:00");

    // The unknown-to-cgov sibling windows (seven_day_sonnet, extra_usage, the
    // null seven_day_* variants) are additive tolerance, not parse failures:
    // serde ignores what the struct does not name. Their presence in this
    // realistic body is what makes that promise bite — a renamed or re-nested
    // documented field fails here before it ships.
    // (No assertion needed beyond the parse succeeding.)

    // The generic limits[] array with its per-entry metadata.
    let limits = resp.limits.expect("limits array present");
    assert_eq!(limits.len(), 2);
    let session = &limits[0];
    assert_eq!(session.kind.as_deref(), Some("session"));
    assert_eq!(session.group.as_deref(), Some("default"));
    assert_eq!(session.severity.as_deref(), Some("low"));
    assert_eq!(session.percent, Some(14.0));
    assert_eq!(session.is_active, Some(true));
    let scoped = &limits[1];
    assert_eq!(scoped.kind.as_deref(), Some("weekly_scoped"));
    let model = scoped
        .scope
        .as_ref()
        .and_then(|s| s.model.as_ref())
        .expect("scoped model parsed");
    assert_eq!(model.id.as_deref(), Some("claude-fable-5"));
    assert_eq!(model.display_name.as_deref(), Some("Fable"));
}

/// `is_active: false` at both levels (top-level window and `limits[]` entry)
/// and an unknown `kind` value ride through the parse: utilization still
/// flows, the flag round-trips for the governor's structural-inactivity
/// predicate, the unknown entry lands in `limits` unconsumed, and the
/// authoritative `weekly_scoped` entry still resolves despite its odd
/// siblings.
#[test]
fn inactive_window_and_unknown_kind_fixture_round_trips() {
    let resp: UsageResponse = serde_json::from_str(INACTIVE_AND_UNKNOWN_JSON)
        .expect("inactive flags and unknown kind must not fail the parse");

    // Window level: only `false` is meaningful — it parses through verbatim
    // (Some(false) on five_hour, Some(true) on seven_day) while utilization
    // and resets_at flow untouched, so a structurally-inactive window is
    // never mistaken for a parse failure or a data-absent one.
    let five_hour = resp.five_hour.as_ref().expect("five_hour present");
    assert_eq!(five_hour.is_active, Some(false));
    assert_eq!(five_hour.utilization, 22.5);
    assert_eq!(five_hour.resets_at, "2026-03-18T13:59:59Z");
    let seven_day = resp.seven_day.as_ref().expect("seven_day present");
    assert_eq!(seven_day.is_active, Some(true));

    // Entry level: the explicit false round-trips, and the entry carrying no
    // is_active key at all lands as None — the absent-means-active reading
    // the §2 table documents.
    let limits = resp.limits.expect("limits array present");
    assert_eq!(limits.len(), 3);
    assert_eq!(limits[0].is_active, Some(false));
    assert_eq!(limits[0].percent, Some(22.0));

    // The unknown kind is tolerated: parsed into `limits` with its fields
    // intact, simply not consumed by any lookup.
    assert_eq!(limits[1].kind.as_deref(), Some("weekly_capacity_v3"));
    assert_eq!(limits[1].percent, Some(55.0));
    assert_eq!(limits[1].is_active, None, "absent is_active parses as None");

    // weekly_scoped resolution is undisturbed by the odd siblings.
    let data = UsageData {
        weekly_scoped_utilization: 0.0,
        weekly_scoped_resets_at: String::new(),
        weekly_scoped_hours_remaining: 0.0,
        weekly_scoped_model: None,
        seven_day_utilization: 0.0,
        seven_day_resets_at: String::new(),
        seven_day_hours_remaining: 0.0,
        five_hour_utilization: 0.0,
        five_hour_resets_at: String::new(),
        five_hour_hours_remaining: 0.0,
        limits: limits.clone(),
        timestamp: Utc::now(),
        stale: false,
    };
    let (model, window) = data
        .scoped_weekly()
        .expect("weekly_scoped must resolve past an unknown kind");
    assert_eq!(model, "Fable");
    assert_eq!(window.utilization, 79.0);
    assert_eq!(window.resets_at, "2026-03-20T03:59:59Z");
}

/// The same reset instant expressed at `+05:30` and `Z` must yield identical
/// `hours_remaining`: the poller parses `resets_at` to a UTC instant before
/// any time math, so the wire offset is presentation, not semantics.
#[test]
fn non_utc_reset_offsets_normalize_to_identical_hours_remaining() {
    let resp: UsageResponse =
        serde_json::from_str(NON_UTC_OFFSETS_JSON).expect("offset forms must parse");

    let five_hour = resp.five_hour.as_ref().expect("five_hour present");
    let seven_day = resp.seven_day.as_ref().expect("seven_day present");

    // Premise: the two fixture strings denote the same instant.
    let offset_form: DateTime<Utc> = chrono::DateTime::parse_from_rfc3339(&five_hour.resets_at)
        .expect("+05:30 form parses")
        .with_timezone(&Utc);
    let z_form: DateTime<Utc> = chrono::DateTime::parse_from_rfc3339(&seven_day.resets_at)
        .expect("Z form parses")
        .with_timezone(&Utc);
    assert_eq!(offset_form, z_form, "fixture premise: same instant");

    // The wire string is preserved verbatim for consumers…
    assert_eq!(five_hour.resets_at, "2026-03-18T19:29:59.918852+05:30");
    assert_eq!(seven_day.resets_at, "2026-03-18T13:59:59.918852Z");

    // …while the derived hours agree: both computed against the same
    // normalized instant. The two calls sit microseconds apart on the clock,
    // hence the tolerance. The instant is fixed in the past (2026-03-18), so
    // both readings are negative and only grow more so — the comparison
    // cannot flake with the passage of time.
    let offset_hours = five_hour.hours_remaining().expect("offset form computes");
    let z_hours = seven_day.hours_remaining().expect("Z form computes");
    assert!(
        (offset_hours - z_hours).abs() < 1e-6,
        "same instant in different offsets must yield identical hours: {offset_hours} vs {z_hours}"
    );
    assert!(
        offset_hours < -4000.0,
        "the fixed past instant must read as long-expired, got {offset_hours}"
    );
}

/// Guard that the incomplete/invalid fixtures still carry the clauses they
/// exist for — same convention as [`fixtures_still_contain_the_clauses_they_pin`].
#[test]
fn incomplete_and_invalid_fixtures_still_contain_the_clauses_they_pin() {
    // Incomplete: scoped/op windows null, the generic limits[] array absent
    // outright (not merely empty — the array key itself must be missing).
    assert!(
        INCOMPLETE_JSON.contains(r#""weekly_scoped": null"#),
        "incomplete fixture must carry a null weekly_scoped window"
    );
    assert!(
        INCOMPLETE_JSON.contains(r#""seven_day_opus": null"#),
        "incomplete fixture must carry a null seven_day_opus window"
    );
    assert!(
        !INCOMPLETE_JSON.contains("\"limits\""),
        "incomplete fixture must omit the limits[] key outright"
    );

    // Invalid #1: utilization present but wrong-typed.
    assert!(
        INVALID_WRONG_TYPED_JSON.contains(r#""utilization": "high""#),
        "wrong-typed fixture must carry a string utilization"
    );

    // Invalid #2: a present window whose utilization key is absent outright.
    assert!(
        INVALID_MISSING_FIELD_JSON.contains("\"resets_at\""),
        "missing-field fixture must keep resets_at so it isolates the one absent field"
    );
    assert!(
        !INVALID_MISSING_FIELD_JSON.contains("utilization"),
        "missing-field fixture must omit utilization outright — that absence is the pinned clause"
    );
}

/// An incomplete response — named windows present, every scoped/op window
/// `null`, and the generic `limits[]` array absent outright — parses, and the
/// parse shows precisely which data arrived: the named windows verbatim,
/// everything else absent (poll() turns an absent `limits` into an empty set
/// and a null window into the non-binding defaults).
#[test]
fn incomplete_fixture_parses_named_windows_and_defaults_the_rest() {
    let resp: UsageResponse =
        serde_json::from_str(INCOMPLETE_JSON).expect("an incomplete response must still parse");

    // The named windows flow through verbatim…
    let five_hour = resp.five_hour.as_ref().expect("five_hour present");
    assert_eq!(five_hour.utilization, 61.5);
    assert_eq!(five_hour.resets_at, "2026-03-18T13:59:59.918852+00:00");
    let seven_day = resp.seven_day.as_ref().expect("seven_day present");
    assert_eq!(seven_day.utilization, 12.0);

    // …while every scoped/op window is null → None, not a parse failure.
    assert!(resp.weekly_scoped.is_none());
    assert!(resp.limits.is_none(), "absent limits[] parses as None");

    // With no limits[] entry anywhere, no model-scoped weekly cap can resolve:
    // the reading is usable but weekly_scoped stays non-binding.
    let data = UsageData {
        weekly_scoped_utilization: 0.0,
        weekly_scoped_resets_at: String::new(),
        weekly_scoped_hours_remaining: 0.0,
        weekly_scoped_model: None,
        seven_day_utilization: seven_day.utilization,
        seven_day_resets_at: seven_day.resets_at.clone(),
        seven_day_hours_remaining: 0.0,
        five_hour_utilization: five_hour.utilization,
        five_hour_resets_at: five_hour.resets_at.clone(),
        five_hour_hours_remaining: 0.0,
        limits: Vec::new(), // poll(): usage.limits.unwrap_or_default()
        timestamp: Utc::now(),
        stale: false,
    };
    assert!(data.scoped_weekly().is_none());
    assert!(data.weekly_scoped_model.is_none());
}

/// The invalid-body fixtures are the committed tripwires for the parse-failure
/// boundary. Both must FAIL the `UsageResponse` parse, and the missing-field
/// error must name the absent field (the diagnosability the poller's
/// `ParseError` log relies on). The missing-`utilization` case is the
/// dangerous direction: a future `#[serde(default)]` on `UsageWindow::
/// utilization` would silently read absence as 0% — manufactured headroom the
/// scaling decision could act on — and this fixture fails that build.
#[test]
fn invalid_fixtures_fail_the_parse_missing_field_error_names_the_field() {
    // Wrong-typed utilization: a string where the float belongs. (serde's
    // type-mismatch message does not carry the field name, so only the
    // failure itself is asserted — the variant wrapping is pinned
    // end-to-end by wrong_typed_window_field_fails_the_poll in the mockito
    // suite.)
    let wrong_typed = serde_json::from_str::<UsageResponse>(INVALID_WRONG_TYPED_JSON);
    let err = wrong_typed.expect_err("a string utilization must fail the parse");
    assert!(
        err.to_string().contains("invalid type"),
        "expected a type-mismatch error, got: {err}"
    );

    // Missing utilization on a present window: serde defaults apply only to
    // absent optional keys, and `utilization` has no default — absence is a
    // hard failure whose message names the field.
    let missing = serde_json::from_str::<UsageResponse>(INVALID_MISSING_FIELD_JSON);
    let err = missing.expect_err("a present window without utilization must fail the parse");
    assert!(
        err.to_string().contains("utilization"),
        "the error must name the missing field so the log is diagnosable: {err}"
    );
}
