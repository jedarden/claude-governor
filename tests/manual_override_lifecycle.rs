//! Integration tests for the `cgov scale` manual-override lifecycle (the
//! contract documented in README "Manual scale override").
//!
//! These exercise the public state/governor API against real files — set,
//! restart persistence, clamp/un-clamp, expiry, hold-until-clear, brake
//! suspension, explicit clear, and the act-owned and observe-owned saves'
//! interactions with a CLI write that lands mid-cycle — so the documented
//! behavior is guaranteed, not aspirational. The real act-cycle precedence
//! path is covered in `manual_override_act_cycle.rs`.

use chrono::{Duration, Utc};
use claude_governor::governor::{
    aggregate_worker_bounds, resolve_manual_override, ManualOverrideResolution,
    EMERGENCY_BRAKE_THRESHOLD, MANUAL_OVERRIDE_DEFAULT_TTL_HOURS,
};
use claude_governor::state::{
    merge_act_owned, merge_observe_owned, save_state, with_state_lock, GovernorState,
    ManualOverride, SafeModeState, WorkerState,
};
use std::path::PathBuf;
use tempfile::TempDir;

/// What `cgov scale N` stores (mirrors `manual_override_record` in the
/// binary: raw count, source "cli", TTL hours — 0 means hold until --clear).
fn cli_override(count: u32, ttl_hours: u64, now: chrono::DateTime<Utc>) -> ManualOverride {
    ManualOverride {
        target: count,
        set_at: now,
        expires_at: if ttl_hours == 0 {
            None
        } else {
            Some(now + Duration::hours(ttl_hours as i64))
        },
        source: "cli".to_string(),
    }
}

/// Two pools with envelope bounds [0, 4]: sonnet (min 0, max 4) and opus
/// (min 1, max 2) — min-of-mins 0, max-of-maxes 4, the envelope
/// `compute_target_workers` clamps the fleet total against.
fn two_pool_state() -> GovernorState {
    let mut state = GovernorState::new();
    state.workers.insert(
        "sonnet".to_string(),
        WorkerState {
            current: 1,
            target: 1,
            min: 0,
            max: 4,
        },
    );
    state.workers.insert(
        "opus".to_string(),
        WorkerState {
            current: 1,
            target: 1,
            min: 1,
            max: 2,
        },
    );
    state
}

fn state_path(dir: &TempDir) -> PathBuf {
    dir.path().join("governor-state.json")
}

#[test]
fn envelope_bounds_are_min_of_mins_max_of_maxes() {
    let state = two_pool_state();

    assert_eq!(
        aggregate_worker_bounds(&state),
        Some((0, 4)),
        "aggregate bounds must be the same envelope the computed target clamps \
         against — set-time validation and reconcile-time clamping must name \
         and use the identical range"
    );
    assert_eq!(
        aggregate_worker_bounds(&GovernorState::new()),
        None,
        "no configured agents means no bounds to validate against"
    );
}

#[test]
fn override_survives_a_restart_and_binds_until_expiry() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let now = Utc::now();

    // `cgov scale 3` with the default TTL: persist and "restart" by reloading.
    let mut state = two_pool_state();
    state.manual_override = Some(cli_override(3, MANUAL_OVERRIDE_DEFAULT_TTL_HOURS, now));
    save_state(&state, &path).unwrap();

    let reloaded = claude_governor::state::load_state(&path).unwrap();
    assert_eq!(
        reloaded.manual_override.as_ref().map(|ov| ov.target),
        Some(3),
        "the override must survive a daemon restart (persistence clause)"
    );

    // Within the TTL it binds; per the contract the act cycle replaces its
    // computed target with this clamped total.
    let resolution = resolve_manual_override(&mut reloaded.clone(), now);
    assert_eq!(
        resolution,
        ManualOverrideResolution::Applied { applied_target: 3 }
    );

    // Past the TTL it is dropped from state: computed targets resume, and the
    // drop is durable once the caller saves.
    let mut expired = reloaded;
    let resolution = resolve_manual_override(&mut expired, now + Duration::hours(3));
    assert_eq!(resolution, ManualOverrideResolution::ExpiredOrAbsent);
    assert!(
        expired.manual_override.is_none(),
        "expiry must clear the field so the act-owned save persists it"
    );
    save_state(&expired, &path).unwrap();
    let after = claude_governor::state::load_state(&path).unwrap();
    assert!(
        after.manual_override.is_none(),
        "an expired override must not come back with the next restart"
    );
}

#[test]
fn raw_target_is_stored_and_clamped_only_at_application() {
    let mut state = two_pool_state();
    let now = Utc::now();
    state.manual_override = Some(cli_override(9, 1, now));

    // Set-time validation (what `cgov scale` runs) rejects 9 up front —
    // outside the envelope [0, 4] — but a stored raw pin can still exceed a
    // LATER envelope: the raw count is kept so raising an agent's max_workers
    // un-clamps it without a re-run.
    state.workers.get_mut("opus").unwrap().max = 8;

    let resolution = resolve_manual_override(&mut state, now);
    assert_eq!(
        resolution,
        ManualOverrideResolution::Applied { applied_target: 8 },
        "the stored raw 9 binds at the raised envelope max of 8"
    );
    assert_eq!(
        state.manual_override.unwrap().target,
        9,
        "application clamps; only expiry or --clear rewrites the stored pin"
    );
}

#[test]
fn zero_ttl_holds_until_clear_and_clear_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let now = Utc::now();

    // `cgov scale 3 --ttl 0`: no expiry — the clock never ends the pin.
    let mut state = two_pool_state();
    state.manual_override = Some(cli_override(3, 0, now));
    save_state(&state, &path).unwrap();

    let mut reloaded = claude_governor::state::load_state(&path).unwrap();
    let resolution = resolve_manual_override(&mut reloaded, now + Duration::days(30));
    assert_eq!(
        resolution,
        ManualOverrideResolution::Applied { applied_target: 3 },
        "--ttl 0 binds until an explicit --clear, not until the clock runs out"
    );

    // `cgov scale --clear` (the locked load-take-save shape run_scale_command
    // uses) removes it; a second clear is a no-op, not an error.
    with_state_lock(&path, || {
        let mut s = claude_governor::state::load_state(&path).unwrap();
        assert!(s.manual_override.take().is_some(), "first clear removes it");
        save_state(&s, &path)
    })
    .unwrap();
    with_state_lock(&path, || {
        let mut s = claude_governor::state::load_state(&path).unwrap();
        assert!(
            s.manual_override.take().is_none(),
            "second clear is a no-op"
        );
        save_state(&s, &path)
    })
    .unwrap();

    let after = claude_governor::state::load_state(&path).unwrap();
    let resolution = resolve_manual_override(&mut after.clone(), Utc::now());
    assert_eq!(resolution, ManualOverrideResolution::ExpiredOrAbsent);
}

#[test]
fn emergency_brake_suspends_the_pin_without_consuming_it() {
    let mut state = two_pool_state();
    let now = Utc::now();
    state.manual_override = Some(cli_override(3, 1, now));
    // Any window at/above the brake threshold suspends the override.
    state.capacity_forecast.five_hour.current_utilization = EMERGENCY_BRAKE_THRESHOLD;

    let resolution = resolve_manual_override(&mut state, now);
    assert_eq!(
        resolution,
        ManualOverrideResolution::SuspendedByBrake,
        "the fleet brakes to 0 while the override stays stored"
    );
    assert!(
        state.manual_override.is_some(),
        "suspension must not consume the pin: it resumes when the brake clears"
    );

    // Brake clears (observe recalibrated utilization down): the same stored
    // pin binds again without operator action.
    state.capacity_forecast.five_hour.current_utilization = 10.0;
    let resolution = resolve_manual_override(&mut state, now + Duration::minutes(5));
    assert_eq!(
        resolution,
        ManualOverrideResolution::Applied { applied_target: 3 }
    );
}

#[test]
fn cli_scale_write_survives_an_act_save_that_was_in_flight() {
    // The persistence clause's sharpest edge: the act cycle loads state, the
    // CLI writes an override mid-cycle under the lock, and the act cycle then
    // saves its stale load-time snapshot. The save must keep the operator's
    // pin (merge_act_owned copies the field only when the cycle itself
    // changed it — the same three-way rule safe_mode uses).
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let now = Utc::now();

    let mut base = two_pool_state();
    save_state(&base, &path).unwrap();

    // Act loads (no override yet)...
    base = claude_governor::state::load_state(&path).unwrap();
    let manual_override_at_load = base.manual_override.clone();
    assert!(manual_override_at_load.is_none());

    // ...the CLI writes under the lock...
    with_state_lock(&path, || {
        let mut s = claude_governor::state::load_state(&path).unwrap();
        s.manual_override = Some(cli_override(3, MANUAL_OVERRIDE_DEFAULT_TTL_HOURS, now));
        save_state(&s, &path)
    })
    .unwrap();

    // ...and the act cycle saves its stale in-memory snapshot.
    with_state_lock(&path, || {
        let mut disk = claude_governor::state::load_state(&path).unwrap();
        merge_act_owned(
            &mut disk,
            &base,
            &SafeModeState::default(),
            &manual_override_at_load,
        );
        save_state(&disk, &path)
    })
    .unwrap();

    let after = claude_governor::state::load_state(&path).unwrap();
    assert_eq!(
        after.manual_override.as_ref().map(|ov| ov.target),
        Some(3),
        "no loop-side save may revert a cgov scale write — including one that \
         landed while an act cycle was in flight"
    );
}

#[test]
fn observe_save_never_touches_a_stored_pin() {
    // The act-ownership clause's other half: merge_observe_owned never
    // touches manual_override, so a concurrent observe cycle saving its stale
    // load-time snapshot cannot revert a cgov scale write — the mirror image
    // of cli_scale_write_survives_an_act_save_that_was_in_flight.
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let now = Utc::now();

    let base = two_pool_state();
    save_state(&base, &path).unwrap();

    // Observe loads (no override yet)...
    let observed = claude_governor::state::load_state(&path).unwrap();
    assert!(observed.manual_override.is_none());

    // ...the CLI writes under the lock...
    with_state_lock(&path, || {
        let mut s = claude_governor::state::load_state(&path).unwrap();
        s.manual_override = Some(cli_override(3, MANUAL_OVERRIDE_DEFAULT_TTL_HOURS, now));
        save_state(&s, &path)
    })
    .unwrap();

    // ...and the observe cycle saves its stale in-memory snapshot.
    with_state_lock(&path, || {
        let mut disk = claude_governor::state::load_state(&path).unwrap();
        assert!(
            disk.manual_override.is_some(),
            "precondition: the CLI write reached disk"
        );
        merge_observe_owned(&mut disk, &observed, &SafeModeState::default());
        save_state(&disk, &path)
    })
    .unwrap();

    let after = claude_governor::state::load_state(&path).unwrap();
    assert_eq!(
        after.manual_override.as_ref().map(|ov| ov.target),
        Some(3),
        "merge_observe_owned never touches manual_override: no observe-side \
         save may revert a cgov scale write"
    );
}

#[test]
fn act_side_expiry_drop_reaches_disk_over_a_stale_on_disk_copy() {
    // The mirror image: the override expired during the act cycle (the cycle
    // dropped it from its in-memory copy), and the save must persist that
    // drop even though the on-disk copy still carries the override.
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let now = Utc::now();

    let mut base = two_pool_state();
    base.manual_override = Some(cli_override(3, 1, now));
    save_state(&base, &path).unwrap();

    let loaded = claude_governor::state::load_state(&path).unwrap();
    let manual_override_at_load = loaded.manual_override.clone();

    // The cycle resolves past expiry — the drop happens in memory.
    let mut acting = loaded;
    let resolution = resolve_manual_override(&mut acting, now + Duration::hours(2));
    assert_eq!(resolution, ManualOverrideResolution::ExpiredOrAbsent);

    with_state_lock(&path, || {
        let mut disk = claude_governor::state::load_state(&path).unwrap();
        assert!(
            disk.manual_override.is_some(),
            "precondition: the on-disk copy still carries the override"
        );
        merge_act_owned(
            &mut disk,
            &acting,
            &SafeModeState::default(),
            &manual_override_at_load,
        );
        save_state(&disk, &path)
    })
    .unwrap();

    let after = claude_governor::state::load_state(&path).unwrap();
    assert!(
        after.manual_override.is_none(),
        "the cycle's expiry drop must reach disk"
    );
}

#[test]
fn newer_cli_pin_survives_act_expiry_merge() {
    // If an older pin expires while the operator writes a replacement pin,
    // the act save must preserve the newer on-disk value rather than applying
    // its stale expiry-clear over the CLI write.
    let dir = TempDir::new().unwrap();
    let path = state_path(&dir);
    let now = Utc::now();

    let mut base = two_pool_state();
    base.manual_override = Some(cli_override(3, 1, now));
    save_state(&base, &path).unwrap();

    let loaded = claude_governor::state::load_state(&path).unwrap();
    let manual_override_at_load = loaded.manual_override.clone();
    let mut acting = loaded;
    assert_eq!(
        resolve_manual_override(&mut acting, now + Duration::hours(2)),
        ManualOverrideResolution::ExpiredOrAbsent
    );

    // The CLI replaces the expired pin while the act cycle is still in flight.
    with_state_lock(&path, || {
        let mut disk = claude_governor::state::load_state(&path).unwrap();
        disk.manual_override = Some(cli_override(1, 0, now + Duration::minutes(1)));
        save_state(&disk, &path)
    })
    .unwrap();

    with_state_lock(&path, || {
        let mut disk = claude_governor::state::load_state(&path).unwrap();
        merge_act_owned(
            &mut disk,
            &acting,
            &SafeModeState::default(),
            &manual_override_at_load,
        );
        save_state(&disk, &path)
    })
    .unwrap();

    let after = claude_governor::state::load_state(&path).unwrap();
    assert_eq!(
        after.manual_override.as_ref().map(|ov| ov.target),
        Some(1),
        "a newer CLI pin must win over an older act expiry-clear"
    );
    assert_eq!(after.manual_override.unwrap().expires_at, None);
}
