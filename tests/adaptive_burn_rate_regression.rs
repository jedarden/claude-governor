//! End-to-end regression pins for the adaptive burn-rate estimator
//! (claudego-8c1f2121).
//!
//! `docs/plan/plan.md` ("Per-Window EMA and Capacity Forecast") documents
//! learning per-worker consumption as a p75 EMA with alpha = 0.2, plus a
//! conservative baseline fallback while the estimator is still cold. These
//! tests drive the public `estimate_burn_rates` entry point over synthetic
//! collector intervals and pin each documented behaviour end to end:
//!
//! - **cold-start baseline fallback** — `cold_start_window_seeds_baseline_rate_and_widens_the_cone`
//! - **convergence to observed per-worker consumption** — `ema_converges_to_observed_per_worker_consumption`
//! - **p75 EMA calculation / outlier handling** — `p75_nearest_rank_ignores_an_outlier_that_drags_the_mean`
//! - **sample aging** — `ema_ages_old_samples_geometrically`
//!
//! The unit-level pins for the p75 nearest-rank statistic, the EMA recurrence
//! itself, the `< MIN_SAMPLES_FOR_EMA` baseline fallback and the staleness
//! tiers live in `src/burn_rate.rs`'s test module; this file pins the
//! behaviours that only exist once the whole pipeline runs.

use std::collections::HashMap;

use claude_governor::burn_rate::{
    estimate_burn_rates, BaselineBurnRates, InstanceRecord, ModelWindowEma, WindowUtilization,
};
use claude_governor::state::EstimateQuality;

const MODEL: &str = "claude-sonnet-4-20250514";

/// The agent `session_pattern` globs the records below simulate matching
/// (fleet attribution, claudego-892a82b1): every record built by
/// [`five_hour_only_record`] carries a needle worker name these globs match,
/// so the burn is classified as this pool's own.
fn fleet_patterns() -> Vec<String> {
    vec!["needle-claude-*".to_string()]
}

/// One collector interval in which the session burned `five_hour_delta`
/// percentage points of the 5h window over one hour. The other windows carry
/// no annotated delta (`None`), so they produce no per-instance rate — the
/// same shape as a session that only touched the 5h window.
fn five_hour_only_record(session: &str, five_hour_delta: f64) -> InstanceRecord {
    InstanceRecord {
        session: session.to_string(),
        worker: Some(format!("needle-claude-{session}")),
        model: MODEL.to_string(),
        total_usd: 1.0,
        total_tokens: 100_000,
        windows: vec![
            WindowUtilization::from_pct_delta("five_hour", Some(five_hour_delta), 41.0, 40.0),
            WindowUtilization::from_pct_delta("seven_day", None, 55.0, 55.0),
            WindowUtilization::from_pct_delta("weekly_scoped", None, 0.0, 0.0),
        ],
    }
}

/// Utilization / hours-remaining snapshots in which the 5h window is live at
/// 41%, the 7d window exists at 55% but has no burn history, and the
/// model-scoped weekly window is absent (the API's 0% sentinel).
fn snapshots() -> (HashMap<String, f64>, HashMap<String, f64>) {
    let mut utilization = HashMap::new();
    utilization.insert("five_hour".to_string(), 41.0);
    utilization.insert("seven_day".to_string(), 55.0);
    utilization.insert("weekly_scoped".to_string(), 0.0);

    let mut hrs_left = HashMap::new();
    hrs_left.insert("five_hour".to_string(), 3.0);
    hrs_left.insert("seven_day".to_string(), 100.0);
    hrs_left.insert("weekly_scoped".to_string(), 100.0);

    (utilization, hrs_left)
}

#[test]
fn cold_start_window_seeds_baseline_rate_and_widens_the_cone() {
    let baseline = BaselineBurnRates {
        pct_per_worker_per_hour: 4.0,
        dollars_per_worker_per_hour: 10.0,
    };
    let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();
    let (utilization, hrs_left) = snapshots();

    // Two workers, and the only measured burn is on the 5h window: the 7d
    // window exists (55% utilization) but has never seen a sample.
    let records = vec![five_hour_only_record("w1", 3.0)];
    let (estimate, forecast) = estimate_burn_rates(
        &records,
        &fleet_patterns(),
        1.0,
        2,
        2,
        &mut ema_state,
        &baseline,
        &utilization,
        90.0,
        &hrs_left,
    );

    assert!(estimate.had_valid_data);

    // The measured window stays measured: 3.0%/hr total from the single
    // record, calibrated, and a single sample means no spread (cone ratio 1).
    let five = &forecast.five_hour;
    assert!((five.fleet_pct_per_hour - 3.0).abs() < 1e-9);
    assert!(matches!(five.estimate_quality, EstimateQuality::Calibrated));
    assert!((five.cone_ratio - 1.0).abs() < 1e-9);

    // The cold 7d window must NOT read as an infinite-headroom 0%/hr: it is
    // seeded at the configured baseline across the fleet (4.0 * 2 workers).
    let seven = &forecast.seven_day;
    assert!(
        (seven.fleet_pct_per_hour - 8.0).abs() < 1e-9,
        "cold window must be seeded at baseline * workers, got {}",
        seven.fleet_pct_per_hour
    );
    assert!(matches!(seven.estimate_quality, EstimateQuality::ColdStart));

    // The seeding widens the cone by using the full fleet rate as the spread:
    // rate_fast/rate_slow = (1 + 0.675) / (1 - 0.675) = 1.675 / 0.325.
    assert!(
        (seven.cone_ratio - 1.675 / 0.325).abs() < 1e-9,
        "cold window must carry the widened uncertainty cone, got {}",
        seven.cone_ratio
    );
    // ... and despite the seeded rate the pessimistic p75 path still
    // authorises workers from the baseline rather than reporting None.
    assert!(seven.safe_worker_count.is_some());

    // An ABSENT window (the 0% sentinel) is deliberately not seeded: 0%/hr is
    // the correct reading for a window the API is not reporting this period.
    let weekly = &forecast.weekly_scoped;
    assert!((weekly.fleet_pct_per_hour - 0.0).abs() < 1e-9);
    assert!(matches!(weekly.estimate_quality, EstimateQuality::ColdStart));
}

#[test]
fn ema_converges_to_observed_per_worker_consumption() {
    let baseline = BaselineBurnRates {
        pct_per_worker_per_hour: 2.0,
        dollars_per_worker_per_hour: 6.0,
    };
    let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();
    let (utilization, hrs_left) = snapshots();

    // Run one estimation cycle per collector interval. Two workers each burn
    // `session_delta`/1h; the EMA is fed each session's rate DIVIDED by the
    // worker count, so steady 0.5%/hr sessions on 2 workers must converge on
    // 0.25%/worker/hr — not on the fleet total.
    let run_cycle = |session_delta: f64, ema_state: &mut HashMap<(String, String), ModelWindowEma>| {
        let records = vec![
            five_hour_only_record("w1", session_delta),
            five_hour_only_record("w2", session_delta),
        ];
        estimate_burn_rates(
            &records,
            &fleet_patterns(),
            1.0,
            2,
            2,
            ema_state,
            &baseline,
            &utilization,
            90.0,
            &hrs_left,
        )
    };

    let key = (MODEL.to_string(), "five_hour".to_string());

    // First interval at 2.0%/hr per session (1.0%/worker/hr): the cold EMA
    // initializes directly, and the second same-valued sample keeps it there.
    let (estimate, forecast) = run_cycle(2.0, &mut ema_state);
    let ema = estimate.ema_state.get(&key).unwrap();
    assert_eq!(ema.samples, 2);
    assert!((ema.ema_pct - 1.0).abs() < 1e-9);
    assert!(
        (forecast.five_hour.fleet_pct_per_hour - 2.0).abs() < 1e-9,
        "fleet stats must track the observed per-session rate"
    );

    // Consumption settles at 0.5%/hr per session (0.25%/worker/hr). With
    // alpha = 0.2 the EMA walks 0.25 + 0.75 * 0.8^k toward it, where k counts
    // SAMPLES: both records land in the same (model, window) key, so each
    // settling cycle applies the update twice and the per-cycle end values
    // are the k = 2, 4, 6, 8 points of that trajectory.
    let expected = [0.73, 0.5572, 0.446608, 0.37582912];
    for (k, want) in expected.iter().enumerate() {
        let (estimate, _) = run_cycle(0.5, &mut ema_state);
        let ema = estimate.ema_state.get(&key).unwrap();
        assert!(
            (ema.ema_pct - want).abs() < 1e-9,
            "per-worker EMA after settling cycle {}: got {}, want {}",
            k + 1,
            ema.ema_pct,
            want
        );
    }

    // ... and 26 samples later (0.75 * 0.8^30 < 0.001) it has converged on
    // the observed per-worker consumption, with the fleet total still
    // reported separately.
    for _ in 0..26 {
        let _ = run_cycle(0.5, &mut ema_state);
    }
    let (estimate, forecast) = run_cycle(0.5, &mut ema_state);
    let ema = estimate.ema_state.get(&key).unwrap();
    assert_eq!(ema.samples, 64);
    assert!(
        (ema.ema_pct - 0.25).abs() < 0.01,
        "EMA must converge to the observed 0.25%/worker/hr, got {}",
        ema.ema_pct
    );
    assert!(
        (forecast.five_hour.fleet_pct_per_hour - 0.5).abs() < 1e-9,
        "per-session fleet stats stay at the observed 0.5%/hr"
    );
    assert!(matches!(
        forecast.five_hour.estimate_quality,
        EstimateQuality::Calibrated
    ));
}

#[test]
fn p75_nearest_rank_ignores_an_outlier_that_drags_the_mean() {
    let baseline = BaselineBurnRates {
        pct_per_worker_per_hour: 2.0,
        dollars_per_worker_per_hour: 6.0,
    };
    let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();
    let (utilization, hrs_left) = snapshots();

    // Three steady sessions at 2.0%/hr and one outlier 10x faster. The plan
    // pins the sizing statistic to the nearest-rank p75 precisely because the
    // mean "underestimates risk when variance is high" — one runaway session
    // must not drag the p75 the way it drags the mean.
    //
    // Note on the plan's guard list: the "> 3σ discard" is documented there
    // but not implemented in the shipped estimator — every sample reaches the
    // EMA. The shipped outlier handling is the p75 statistic itself, and that
    // is what this test pins.
    let records = vec![
        five_hour_only_record("w1", 2.0),
        five_hour_only_record("w2", 2.0),
        five_hour_only_record("w3", 2.0),
        five_hour_only_record("outlier", 20.0),
    ];
    let (estimate, forecast) = estimate_burn_rates(
        &records,
        &fleet_patterns(),
        1.0,
        2,
        2,
        &mut ema_state,
        &baseline,
        &utilization,
        90.0,
        &hrs_left,
    );

    let stats = estimate.fleet_stats.get("five_hour").unwrap();

    // Nearest-rank p75 of [2.0, 2.0, 2.0, 20.0]: ceil(4 * 0.75) = 3rd of the
    // sorted values = 2.0 — the outlier sits above the rank and is ignored.
    assert!(
        (stats.p75_pct_hr - 2.0).abs() < 1e-9,
        "p75 must stay at the steady sessions' rate, got {}",
        stats.p75_pct_hr
    );
    // The mean, by contrast, is dragged from 2.0 to 6.5 by the same sample —
    // exactly the failure mode the p75 choice exists to prevent.
    assert!(
        (stats.mean_pct_hr - 6.5).abs() < 1e-9,
        "mean must show the drag the p75 avoids, got {}",
        stats.mean_pct_hr
    );
    assert!(stats.p75_pct_hr < stats.mean_pct_hr);

    // The heterogeneity is not hidden: it surfaces as spread (a cone wider
    // than 1) rather than as an inflated central estimate.
    assert!(forecast.five_hour.cone_ratio > 1.0);

    // All four samples were kept — the shipped estimator does not discard
    // outliers from the EMA (the documented 3σ guard is unimplemented), so
    // the EMA state records every sample and its tail does carry the outlier.
    let key = (MODEL.to_string(), "five_hour".to_string());
    let ema = estimate.ema_state.get(&key).unwrap();
    assert_eq!(ema.samples, 4);
    // Per-worker samples in record order [1, 1, 1, 10]: the last update puts
    // the EMA at 0.2 * 10 + 0.8 * 1 = 2.8.
    assert!((ema.ema_pct - 2.8).abs() < 1e-9);

    assert!(matches!(
        forecast.five_hour.estimate_quality,
        EstimateQuality::Calibrated
    ));
}

#[test]
fn ema_ages_old_samples_geometrically() {
    let baseline = BaselineBurnRates {
        pct_per_worker_per_hour: 2.0,
        dollars_per_worker_per_hour: 6.0,
    };
    let mut ema_state: HashMap<(String, String), ModelWindowEma> = HashMap::new();
    let (utilization, hrs_left) = snapshots();

    // One worker, one interval per cycle. Calibrate at 4.0%/worker/hr, then
    // observe a sustained 1.0%/worker/hr: with alpha = 0.2 each new sample
    // retires 20% of the remainder, so the old level decays geometrically —
    // e_k = 1.0 + 3.0 * 0.8^(k-1) for the k-th sample. That decay IS sample
    // aging in this estimator: no sample is ever deleted, its weight just
    // shrinks by 0.8 per newer sample.
    let run_cycle = |session_delta: f64, ema_state: &mut HashMap<(String, String), ModelWindowEma>| {
        let records = vec![five_hour_only_record("w1", session_delta)];
        estimate_burn_rates(
            &records,
            &fleet_patterns(),
            1.0,
            1,
            1,
            ema_state,
            &baseline,
            &utilization,
            90.0,
            &hrs_left,
        )
    };

    let key = (MODEL.to_string(), "five_hour".to_string());

    let (estimate, _) = run_cycle(4.0, &mut ema_state);
    let ema = estimate.ema_state.get(&key).unwrap();
    assert_eq!(ema.samples, 1);
    assert!((ema.ema_pct - 4.0).abs() < 1e-9);

    // Expected values: 1.0 + 3.0 * 0.8^(k-1) for k = 2, 3, 4.
    let expected = [3.4, 2.92, 2.536];
    for (k, want) in expected.iter().enumerate() {
        let (estimate, _) = run_cycle(1.0, &mut ema_state);
        let ema = estimate.ema_state.get(&key).unwrap();
        assert_eq!(ema.samples, k as u32 + 2);
        assert!(
            (ema.ema_pct - want).abs() < 1e-9,
            "aged EMA at sample {}: got {}, want {}",
            k + 2,
            ema.ema_pct,
            want
        );
    }

    // The aging rate is exactly (1 - alpha) per sample: between consecutive
    // post-target samples the above-target residue shrinks by 0.8
    // (2.536 - 1 = 1.536 → 1.536 * 0.8 = 1.2288 above the 1.0 target).
    let (estimate, _) = run_cycle(1.0, &mut ema_state);
    let ema = estimate.ema_state.get(&key).unwrap();
    assert!((ema.ema_pct - 2.2288).abs() < 1e-9);
}
