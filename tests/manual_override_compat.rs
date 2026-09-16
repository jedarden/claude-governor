//! Schema backward/forward-compatibility coverage for the manual-override
//! state (bead claudego-9cfb78d2, child of claudego-dbc81746).
//!
//! `ManualOverride` and the `GovernorState.manual_override` field shipped in
//! 45a7be4 (claudego-fbe8df91). The resolution and lifecycle halves of that
//! contract live in `manual_override_lifecycle.rs`; this file pins the schema
//! half so a pre-override state file keeps loading and a current one keeps
//! round-tripping:
//!
//! - a state file written by the pre-override version — no `manual_override`
//!   key — loads through the real `load_state` path with its legacy values
//!   intact and `manual_override == None`;
//! - a state carrying a `ManualOverride`, with `expires_at` both `Some` and
//!   `None` (the README contract's TTL and hold-until-`--clear` shapes),
//!   round-trips through the real save/load path field-identically;
//! - `#[serde(default)]` holds at the container level on both structs, so any
//!   future field addition keeps old files loadable and unknown keys stay
//!   ignored rather than rejected.
//!
//! The real load path is load-bearing here: `load_state` deliberately degrades
//! a file that fails to parse to a fresh default state instead of erroring, so
//! a naive "it loaded and the field is None" assertion would pass even if the
//! fixture had been rejected as corrupt. Every legacy-load assertion therefore
//! also checks distinctive values that could only have come from the fixture.

use chrono::{Duration, TimeZone, Utc};
use claude_governor::state::{load_state, save_state, GovernorState, ManualOverride, WorkerState};
use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;

fn state_path(dir: &TempDir) -> PathBuf {
    dir.path().join("governor-state.json")
}

/// A fixed instant so round-trip assertions compare exact field values.
fn instant(hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 16, hour, 0, 0).unwrap()
}

/// A hand-written state file in the pre-override schema (45a7be4^, da07cfc).
///
/// It carries the flat fields a pre-override `save_state` wrote with
/// distinctive values; the complex sub-structs (`capacity_forecast`,
/// `burn_rate`, …) are omitted and stand in from defaults, because the
/// guarantee under test is missing-key tolerance, not fixture completeness.
/// There is no `manual_override` key — this file predates claudego-fbe8df91.
fn legacy_state_json() -> Value {
    json!({
        "updated_at": "2026-09-16T11:00:00Z",
        "usage": {
            "sonnet_pct": 0.0,
            "all_models_pct": 41.5,
            "five_hour_pct": 63.25,
            "sonnet_resets_at": "",
            "seven_day_resets_at": "2026-09-19T00:00:00Z",
            "five_hour_resets_at": "2026-09-16T15:00:00Z",
            "stale": false,
            "weekly_scoped_model": null,
            "weekly_scoped_pct": 41.5
        },
        "workers": {
            "needle-sonnet": { "current": 2, "target": 2, "min": 0, "max": 8 }
        },
        "alerts": [],
        "safe_mode": {
            "active": false,
            "entered_at": null,
            "trigger": null,
            "median_error_at_entry": null,
            "predictions_since_entry": 0,
            "scored_at_entry": 0
        },
        "token_refresh_failing": false,
        "p5h_delta": -3.5,
        "p7d_delta": 12.0,
        "p7ds_delta": null
    })
}

#[test]
fn legacy_state_file_without_manual_override_loads_with_none() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    std::fs::write(&path, legacy_state_json().to_string()).unwrap();

    // The raw deserializer must accept the file outright. This sits outside
    // `load_state` on purpose: that path degrades a parse failure to a fresh
    // default instead of erroring, which would launder a real
    // incompatibility into a silently-passing assertion.
    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: GovernorState = serde_json::from_str(&raw).unwrap();
    assert!(
        parsed.manual_override.is_none(),
        "a pre-override file must deserialize with no override"
    );
    assert_eq!(
        parsed.workers["needle-sonnet"].current, 2,
        "legacy values must survive the parse — an empty workers map here \
         means the file failed to deserialize and defaults were substituted"
    );

    // The real load path, the one the daemons and `cgov scale` use.
    let loaded = load_state(&path).unwrap();
    assert!(
        loaded.manual_override.is_none(),
        "loading a pre-override state file must yield manual_override: None"
    );
    assert_eq!(loaded.updated_at, instant(11));
    assert_eq!(loaded.p5h_delta, Some(-3.5));
    assert_eq!(loaded.p7d_delta, Some(12.0));
    assert_eq!(loaded.p7ds_delta, None);
    assert_eq!(loaded.usage.five_hour_pct, 63.25);
    assert_eq!(loaded.usage.all_models_pct, 41.5);
    assert!(!loaded.safe_mode.active);
    assert_eq!(loaded.workers["needle-sonnet"].max, 8);

    // It also saves cleanly — and the save writes the current, complete
    // schema: the key the legacy file lacked is present (null) on disk after
    // this version touches the file.
    save_state(&loaded, &path).unwrap();
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        doc.get("manual_override").is_some(),
        "a save by this version must write the manual_override key so the \
         on-disk schema is self-describing"
    );
    assert_eq!(doc["manual_override"], Value::Null);

    let reloaded = load_state(&path).unwrap();
    assert!(reloaded.manual_override.is_none());
    assert_eq!(
        reloaded.p5h_delta,
        Some(-3.5),
        "legacy values must survive a full load/save/load cycle"
    );
}

#[test]
fn complete_pre_override_state_file_loads_with_none() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);

    // Serialize a complete current-schema state, then strip the one key the
    // pre-override version never wrote. 45a7be4's only schema change was
    // adding that key, so the result is the exact pre-override schema —
    // every field present — even if key order differs, which the loader is
    // indifferent to.
    let mut state = GovernorState::new();
    state.workers.insert(
        "needle-sonnet".to_string(),
        WorkerState {
            current: 3,
            target: 3,
            min: 0,
            max: 8,
        },
    );
    save_state(&state, &path).unwrap();
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        doc.as_object_mut()
            .unwrap()
            .remove("manual_override")
            .is_some(),
        "the current schema must write the key before it can be stripped"
    );
    std::fs::write(&path, doc.to_string()).unwrap();

    let loaded = load_state(&path).unwrap();
    assert!(
        loaded.manual_override.is_none(),
        "the complete pre-override schema must load with no override"
    );
    assert_eq!(
        loaded.workers["needle-sonnet"].current, 3,
        "a fresh default here means the stripped file was rejected as corrupt"
    );
}

#[test]
fn manual_override_round_trips_field_identical_with_and_without_expiry() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let now = instant(12);

    for (label, expires_at) in [
        ("ttl-bound", Some(now + Duration::hours(2))),
        ("hold-until-clear", None),
    ] {
        let mut state = GovernorState::new();
        state.updated_at = now;
        state.manual_override = Some(ManualOverride {
            target: 5,
            set_at: now,
            expires_at,
            source: "cli".to_string(),
        });

        // Raw serde round-trip first: a field dropped or mangled by the
        // serializer itself fails here, loudly, with no fallback in the way.
        let raw = serde_json::to_string(&state).unwrap();
        let raw_back: GovernorState = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            raw_back.manual_override, state.manual_override,
            "{label}: raw serde round-trip must be field-identical"
        );

        // Then the real save/load path — this is the persistence clause of
        // the README contract: a state file written by this version still
        // loads, and the record that comes back is field-identical.
        save_state(&state, &path).unwrap();
        let loaded = load_state(&path).unwrap();
        assert!(
            loaded.manual_override.is_some(),
            "{label}: the override must survive a save/load cycle"
        );
        let ov = loaded.manual_override.as_ref().unwrap();
        assert_eq!(ov.target, 5, "{label}: target must round-trip exactly");
        assert_eq!(ov.set_at, now, "{label}: set_at must round-trip exactly");
        assert_eq!(
            ov.expires_at, expires_at,
            "{label}: expires_at must round-trip exactly — None means hold \
             until an explicit `cgov scale --clear`"
        );
        assert_eq!(ov.source, "cli", "{label}: source must round-trip exactly");
        assert_eq!(
            loaded.manual_override, state.manual_override,
            "{label}: the whole record must come back field-identical"
        );
    }
}

#[test]
fn serde_defaults_keep_old_and_future_files_loadable() {
    // ManualOverride: an entirely empty object builds the default record —
    // the container-level #[serde(default)] that makes every field optional
    // for files written before that field existed.
    let empty: ManualOverride = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.target, 0);
    assert_eq!(empty.expires_at, None);
    assert_eq!(empty.source, "cli");

    // A partial record fills in only what is missing.
    let partial: ManualOverride = serde_json::from_str(r#"{"target": 3}"#).unwrap();
    assert_eq!(partial.target, 3);
    assert_eq!(partial.source, "cli");

    // GovernorState likewise: `{}` — and a file carrying only keys this
    // schema has never heard of — deserializes to the default state with no
    // override. Unknown keys being ignored rather than rejected is also what
    // lets an older binary load a file a newer one wrote.
    let bare: GovernorState = serde_json::from_str("{}").unwrap();
    assert!(bare.manual_override.is_none());

    let futuristic: GovernorState =
        serde_json::from_str(r#"{"some_field_added_later": {"nested": true}}"#).unwrap();
    assert!(
        futuristic.manual_override.is_none(),
        "unknown keys must be ignored, not rejected — the same tolerance old \
         readers rely on for files written by newer versions"
    );
}
