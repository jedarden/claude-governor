//! Acceptance tests for the read-only NEEDLE attempt-ledger reader
//! (claudego-bba5584b).
//!
//! Coverage, per the bead's acceptance criteria:
//! 1. a fixture JSONL yields the expected per-adapter numbers;
//! 2. missing or partial rows are ignored (never read errors);
//! 3. the reader performs no write (the ledger directory is byte-identical,
//!    same sizes and mtimes, before and after).

use chrono::{DateTime, Duration, Utc};
use claude_governor::ledger_yield::read_ledger_yield;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

/// A `attempt.resolved` row with the full field set the live ledger carries.
fn resolved_row(
    ts: DateTime<Utc>,
    worker_id: &str,
    workspace: &str,
    adapter: &str,
    outcome: &str,
    cost_usd: f64,
) -> serde_json::Value {
    json!({
        "timestamp": ts.to_rfc3339(),
        "event_type": "attempt.resolved",
        "worker_id": worker_id,
        "session_id": "1f1e4c96",
        "sequence": 78,
        "bead_id": "armor-8317f313",
        "workspace": workspace,
        "data": {
            "adapter": adapter,
            "attempt_id": "01a09c44-9d49-7ff2-b2bc-dfbf0f638eb3",
            "bead_id": "armor-8317f313",
            "duration_ms": 1886683,
            "estimated_cost_usd": cost_usd,
            "exit_code": 0,
            "model": "glm-5.3",
            "outcome": outcome,
            "provisional": true,
            "schema_version": 1,
            "tokens_in": 79889,
            "tokens_out": 26570,
            "worker": worker_id,
            "workspace": workspace,
        }
    })
}

fn write_ledger(dir: &Path, name: &str, rows: &[serde_json::Value]) {
    let mut f = fs::File::create(dir.join(name)).expect("create fixture file");
    for row in rows {
        f.write_all(row.to_string().as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
    }
}

/// Build the shared fixture: two real adapters with known numbers plus every
/// row shape that must be excluded or ignored.
///
/// Real rows (adapter "flash", then "glm") are chosen so every aggregate is
/// distinct and therefore actually asserted:
///   flash: verified $2.00, work_failure $1.00, verified $0.50
///   glm:   indeterminate (no cost field at all), verified $4.00
fn fixture_rows(now: DateTime<Utc>) -> Vec<serde_json::Value> {
    let in_window = now - Duration::hours(1);
    let before_window = now - Duration::hours(96);
    vec![
        // The counted attempts.
        resolved_row(
            in_window,
            "worker-a",
            "/home/coding/ARMOR",
            "flash",
            "verified_success",
            2.0,
        ),
        resolved_row(
            in_window,
            "worker-a",
            "/home/coding/ARMOR",
            "flash",
            "work_failure",
            1.0,
        ),
        resolved_row(
            in_window,
            "worker-b",
            "/home/coding/pdftract",
            "flash",
            "verified_success",
            0.5,
        ),
        {
            // No estimated_cost_usd at all — three real adapters report none.
            let mut r = resolved_row(
                in_window,
                "worker-a",
                "/home/coding/ARMOR",
                "glm",
                "indeterminate",
                0.0,
            );
            r["data"]
                .as_object_mut()
                .unwrap()
                .remove("estimated_cost_usd");
            r
        },
        resolved_row(
            in_window,
            "worker-b",
            "/home/coding/pdftract",
            "glm",
            "verified_success",
            4.0,
        ),
        // Excluded: fixture worker (the -test-worker suffix).
        resolved_row(
            in_window,
            "echo-test-test-worker",
            "/home/coding/ARMOR",
            "flash",
            "verified_success",
            9.0,
        ),
        // Excluded: relative-path workspace fixture row.
        resolved_row(in_window, "worker-a", ".", "flash", "verified_success", 9.0),
        {
            // Excluded: adapter reported no usage (ADR-030 decision 2).
            let mut r = resolved_row(
                in_window,
                "worker-a",
                "/home/coding/ARMOR",
                "flash",
                "verified_success",
                9.0,
            );
            r["data"]["costed"] = json!(false);
            r
        },
        // Excluded: decomposition is a resolution class, not a success
        // (ADR-030 decision 1).
        resolved_row(
            in_window,
            "worker-a",
            "/home/coding/ARMOR",
            "glm",
            "decomposed",
            9.0,
        ),
        // Ignored: resolved before the window opened.
        resolved_row(
            before_window,
            "worker-a",
            "/home/coding/ARMOR",
            "flash",
            "verified_success",
            9.0,
        ),
        // Ignored: not parseable JSON at all.
        json!({"nonsense": true}),
        {
            // Ignored: partial — no adapter to attribute the attempt to.
            let mut r = resolved_row(
                in_window,
                "worker-a",
                "/home/coding/ARMOR",
                "flash",
                "work_failure",
                1.0,
            );
            r["data"].as_object_mut().unwrap().remove("adapter");
            r
        },
        {
            // Ignored: partial — an attempt with no resolution class.
            let mut r = resolved_row(
                in_window,
                "worker-a",
                "/home/coding/ARMOR",
                "flash",
                "work_failure",
                1.0,
            );
            r["data"].as_object_mut().unwrap().remove("outcome");
            r
        },
        {
            // Ignored: partial — no timestamp, so it cannot be placed in a
            // window.
            let mut r = resolved_row(
                in_window,
                "worker-a",
                "/home/coding/ARMOR",
                "flash",
                "work_failure",
                1.0,
            );
            r.as_object_mut().unwrap().remove("timestamp");
            r
        },
        // A non-attempt event that merely mentions the tag is neither counted
        // nor ignored — it is not an attempt row.
        json!({
            "timestamp": in_window.to_rfc3339(),
            "event_type": "attempt.resolved.count",
            "data": {"note": "attempt.resolved appears below"},
        }),
    ]
}

#[test]
fn fixture_yields_expected_per_adapter_numbers() {
    let now = Utc::now();
    let dir = tempfile::tempdir().expect("tempdir");
    write_ledger(dir.path(), "workers-2026-09-14.jsonl", &fixture_rows(now));
    // A second file in the same directory must also be folded in.
    write_ledger(
        dir.path(),
        "workers-2026-09-13.jsonl",
        &[resolved_row(
            now - Duration::hours(2),
            "worker-a",
            "/home/coding/ARMOR",
            "flash",
            "verified_success",
            1.0,
        )],
    );
    // Unrelated files are not read.
    fs::write(dir.path().join("workers-2026-09-14.jsonl.bak"), "garbage").unwrap();
    fs::write(dir.path().join("README.md"), "not a ledger").unwrap();

    let report = read_ledger_yield(dir.path(), now, 72).expect("read fixture ledger");

    // Per adapter: attempts, verified, yield, cost, cost per verified.
    let flash = &report.by_adapter["flash"];
    assert_eq!(flash.adapter, "flash");
    assert_eq!(flash.attempts, 4);
    assert_eq!(flash.verified, 3);
    assert_eq!(flash.verified_yield, Some(0.75));
    assert_eq!(flash.cost_usd, 4.5);
    assert_eq!(flash.cost_per_verified_usd, Some(1.5)); // 4.5 / 3 verified

    let glm = &report.by_adapter["glm"];
    assert_eq!(glm.attempts, 2);
    assert_eq!(glm.verified, 1);
    assert_eq!(glm.verified_yield, Some(0.5));
    // The cost-less indeterminate attempt still counts as an attempt.
    assert_eq!(glm.cost_usd, 4.0);
    assert_eq!(glm.cost_per_verified_usd, Some(4.0));

    // Fleet totals, and the exclusion/ignore bookkeeping:
    // 5 excluded rows (test-worker, dot workspace, costed=false, decomposed,
    // pre-window) + 3 ignored partials (no adapter, no outcome, no
    // timestamp). The tag-less garbage line and the attempt.resolved.count
    // summary event are not attempt rows at all, so they land in neither
    // bucket.
    assert_eq!(report.attempts, 6);
    assert_eq!(report.verified, 4);
    assert_eq!(report.verified_yield, Some(4.0 / 6.0));
    // $4.50 flash + $4.00 glm.
    assert_eq!(report.cost_usd, 8.5);
    assert_eq!(report.cost_per_verified_usd, Some(8.5 / 4.0));
    assert_eq!(report.rows_ignored, 8);

    // Window bookkeeping is reported so a reader can tell which span a
    // figure describes.
    assert_eq!(report.window_hours, 72);
    assert_eq!(report.window_start, now - Duration::hours(72));
    assert_eq!(report.computed_at, now);
}

#[test]
fn empty_and_missing_ledgers_yield_empty_reports() {
    let now = Utc::now();

    let empty = tempfile::tempdir().unwrap();
    let report = read_ledger_yield(empty.path(), now, 72).unwrap();
    assert_eq!(report.attempts, 0);
    assert_eq!(report.verified, 0);
    assert_eq!(report.verified_yield, None);
    assert_eq!(report.cost_usd, 0.0);
    assert_eq!(report.cost_per_verified_usd, None);
    assert!(report.by_adapter.is_empty());

    // A fleet with no ledger yet (fresh host) reads as empty, not as an
    // error — status must still render.
    let report = read_ledger_yield(Path::new("/nonexistent/needle/logs"), now, 72).unwrap();
    assert_eq!(report.attempts, 0);
    assert!(report.by_adapter.is_empty());
}

#[test]
fn reader_performs_no_write() {
    let now = Utc::now();
    let dir = tempfile::tempdir().unwrap();
    write_ledger(dir.path(), "workers-2026-09-14.jsonl", &fixture_rows(now));

    let snapshot = |dir: &Path| -> Vec<(String, u64, SystemTime)> {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let meta = e.metadata().unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    meta.len(),
                    meta.modified().unwrap(),
                )
            })
            .collect();
        entries.sort();
        entries
    };

    let before = snapshot(dir.path());
    let report_a = read_ledger_yield(dir.path(), now, 72).unwrap();
    let report_b = read_ledger_yield(dir.path(), now, 72).unwrap();
    let after = snapshot(dir.path());

    // Same files, same sizes, same mtimes: nothing was written, truncated,
    // renamed or created by reading.
    assert_eq!(before, after);
    // And reading is deterministic.
    assert_eq!(report_a, report_b);
}

#[test]
fn window_boundary_and_cost_field_edges() {
    let now = Utc::now();
    let dir = tempfile::tempdir().unwrap();

    // Exactly at the window start counts (the window is inclusive); a
    // nanosecond before does not.
    let window_start = now - Duration::hours(72);
    write_ledger(
        dir.path(),
        "workers.jsonl",
        &[
            resolved_row(window_start, "w", "/r", "flash", "verified_success", 1.0),
            resolved_row(
                window_start - Duration::nanoseconds(1),
                "w",
                "/r",
                "flash",
                "verified_success",
                1.0,
            ),
            {
                // A non-finite cost never poisons the totals.
                let mut r = resolved_row(window_start, "w", "/r", "glm", "work_failure", 0.0);
                r["data"]["estimated_cost_usd"] = json!(f64::NAN);
                r
            },
        ],
    );

    let report = read_ledger_yield(dir.path(), now, 72).unwrap();
    let flash = &report.by_adapter["flash"];
    assert_eq!(flash.attempts, 1);
    assert_eq!(flash.cost_usd, 1.0);
    let glm = &report.by_adapter["glm"];
    assert_eq!(glm.attempts, 1);
    assert_eq!(glm.cost_usd, 0.0);
    assert_eq!(report.rows_ignored, 1);
}
