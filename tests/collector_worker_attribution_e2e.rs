//! End-to-end worker-attribution stamp through a real collection pass
//! (claudego-a586d42f).
//!
//! claudego-a542d686 taught the collector to stamp each instance record with
//! the dispatching NEEDLE worker, resolved from the live process tree
//! (`WorkerAttribution`), and `record_is_fleet` classifies records against the
//! agents' `session_pattern` globs. The scan mechanics have unit tests in
//! `src/worker_attribution.rs` and the classification has fixture tests — but
//! nothing pinned the join this split exists to prove: a REAL collection pass
//! over a live-looking tree writing the stamp into the token-history feed
//! (JSONL and the SQLite mirror the governor reads fleet records from).
//!
//! These tests drive [`run_collection_pass_with_engine`] over a seeded
//! transcript plus a synthetic /proc tree and heartbeat registry, and assert:
//!
//! - a worker-dispatched session lands in the feed stamped with the
//!   `needle-{agent}-{worker_id}` session name (named via the heartbeat, as in
//!   production) and classifies FLEET against the repo template's
//!   `session_pattern` glob;
//! - a worker from a foreign pool stamps with its own name yet classifies
//!   EXOGENOUS (the populated-but-foreign case);
//! - an operator session (plain shell ancestry) lands unattributed — the
//!   `worker` key is absent from the record entirely — and classifies
//!   EXOGENOUS;
//! - the SQLite mirror carries the same stamp (the burn model reads fleet
//!   records from the DB, not the JSONL).

use claude_governor::burn_rate::record_is_fleet;
use claude_governor::collector::{run_collection_pass_with_engine, CollectionPaths};
use claude_governor::db;
use claude_governor::pricing::PricingEngine;
use rusqlite::params;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Engine built from the repo's config template — same rationale as
/// `collector_cursor_recovery_test.rs`: collection passes must not depend on
/// the machine's live governor.yaml. The template's fleet pattern is
/// `needle-claude-anthropic-sonnet-cgov-sonnet-*`, and these tests name their
/// fixtures against that string space.
fn template_engine() -> PricingEngine {
    PricingEngine::from_config_path(Path::new("config/governor.yaml"))
        .expect("repo config template must load")
}

/// The repo template's fleet `session_pattern` (config/governor.yaml).
fn template_fleet_patterns() -> Vec<String> {
    vec!["needle-claude-anthropic-sonnet-cgov-sonnet-*".to_string()]
}

/// A valid CC session UUID: 36 chars, dashes at 8/13/18/23.
const SESSION_UUID: &str = "1e40ad47-6155-4c14-855a-c32f721d7cce";

/// One assistant-usage JSONL line as Claude Code writes it. The model must be
/// Anthropic-native (`claude-*`) — non-claude models are deliberately excluded
/// from the feed (they do not consume Anthropic quota).
fn usage_line() -> String {
    format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":[],"model":"claude-sonnet-4-5","usage":{{"input_tokens":1000,"output_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":500}}}},"entrypoint":"cli"}}"#
    )
}

/// Seed a transcript whose stem is the session UUID the /proc tree holds open.
fn seed_session(paths: &CollectionPaths) -> PathBuf {
    let path = paths
        .session_base
        .join("proj")
        .join(format!("{SESSION_UUID}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, usage_line() + "\n").unwrap();
    path
}

/// Write a synthetic /proc-style process: cmdline, stat (with ppid), and
/// optionally one fd symlink. Same shape `src/worker_attribution.rs` tests use.
fn write_process(
    proc_root: &Path,
    pid: i64,
    ppid: i64,
    argv: &[&str],
    fd_target: Option<&str>,
) {
    let dir = proc_root.join(pid.to_string());
    fs::create_dir_all(dir.join("fd")).unwrap();
    fs::write(
        dir.join("cmdline"),
        format!("{}\0", argv.join("\0")).into_bytes(),
    )
    .unwrap();
    fs::write(
        dir.join("stat"),
        format!(
            "{} ({}) S {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0",
            pid, argv[0], ppid
        ),
    )
    .unwrap();
    if let Some(target) = fd_target {
        symlink(target, dir.join("fd").join("19")).unwrap();
    }
}

/// Lay down a worker-dispatched process tree: needle run (pid 100) -> bash
/// (101) -> claude (102) holding the session's tasks fd. The argv carries no
/// `--agent`/`--identifier`, so the worker can only be named through the
/// heartbeat registry — the production naming path.
fn seed_dispatched_tree(paths: &CollectionPaths, qualified_id: &str) {
    let proc_root = &paths.proc_root;
    write_process(
        proc_root,
        100,
        1,
        &["needle-stable", "run", "--resume", "--count", "1"],
        None,
    );
    write_process(proc_root, 101, 100, &["bash", "-c", "claude"], None);
    write_process(
        proc_root,
        102,
        101,
        &["claude", "--print", "--output-format", "stream-json"],
        Some(&format!("/tmp/claude-1000/proj/{SESSION_UUID}")),
    );

    let hb_dir = &paths.heartbeat_dir;
    fs::create_dir_all(hb_dir).unwrap();
    fs::write(
        hb_dir.join(format!("{qualified_id}.json")),
        format!(r#"{{"worker_id":"w","qualified_id":"{qualified_id}","pid":100}}"#),
    )
    .unwrap();
}

/// Lay down an operator tree: sshd (200) -> bash (201) -> claude (202) holding
/// the same session fd, with no needle worker anywhere in the ancestry.
fn seed_operator_tree(paths: &CollectionPaths) {
    let proc_root = &paths.proc_root;
    write_process(proc_root, 200, 1, &["sshd"], None);
    write_process(proc_root, 201, 200, &["bash"], None);
    write_process(
        proc_root,
        202,
        201,
        &["claude", "--dangerously-skip-permissions"],
        Some(&format!("/tmp/claude-1000/proj/{SESSION_UUID}")),
    );
    // No heartbeat directory at all — an empty/unreadable registry must leave
    // the record unattributed, never fail the pass.
}

/// All instance records from the history JSONL, in append order.
fn instance_records(paths: &CollectionPaths) -> Vec<serde_json::Value> {
    let content = fs::read_to_string(&paths.history_path).unwrap();
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("r").and_then(|r| r.as_str()) == Some("i"))
        .collect()
}

#[test]
fn dispatched_session_stamps_worker_into_the_feed_and_classifies_fleet() {
    let tmp = TempDir::new().unwrap();
    let paths = CollectionPaths::under(tmp.path());
    seed_session(&paths);
    seed_dispatched_tree(&paths, "claude-anthropic-sonnet-cgov-sonnet-0");

    let result = run_collection_pass_with_engine(&paths, &template_engine()).unwrap();
    assert_eq!(result.instance_records, 1, "one usage line, one instance record");

    let records = instance_records(&paths);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["sess"], SESSION_UUID);
    // The heartbeat names the worker; the scan resolves the session to it and
    // the feed carries the `needle-{agent}-{worker_id}` session name.
    assert_eq!(
        records[0]["worker"],
        "needle-claude-anthropic-sonnet-cgov-sonnet-0"
    );

    // The stamp is what the fleet/exogenous classification consumes.
    let worker = records[0]["worker"].as_str();
    assert!(record_is_fleet(worker, &template_fleet_patterns()));

    // The SQLite mirror — what the burn model actually reads — carries it too.
    let conn = db::open_db(&paths.db_path).unwrap();
    let db_worker: String = conn
        .query_row(
            "SELECT worker FROM i WHERE sess = ?1",
            params![SESSION_UUID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(db_worker, "needle-claude-anthropic-sonnet-cgov-sonnet-0");
}

#[test]
fn foreign_pool_worker_stamps_but_classifies_exogenous() {
    let tmp = TempDir::new().unwrap();
    let paths = CollectionPaths::under(tmp.path());
    seed_session(&paths);
    // A live needle worker, but from a pool the template's pattern does not
    // cover — another governor instance's worker is real burn that is not
    // ours to scale.
    seed_dispatched_tree(&paths, "claude-anthropic-sonnet-some-other-pool-0");

    run_collection_pass_with_engine(&paths, &template_engine()).unwrap();

    let records = instance_records(&paths);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]["worker"],
        "needle-claude-anthropic-sonnet-some-other-pool-0",
        "populated: the worker name is real, just not ours"
    );
    assert!(!record_is_fleet(
        records[0]["worker"].as_str(),
        &template_fleet_patterns()
    ));
}

#[test]
fn operator_session_lands_unattributed_and_classifies_exogenous() {
    let tmp = TempDir::new().unwrap();
    let paths = CollectionPaths::under(tmp.path());
    seed_session(&paths);
    seed_operator_tree(&paths);

    run_collection_pass_with_engine(&paths, &template_engine()).unwrap();

    let records = instance_records(&paths);
    assert_eq!(records.len(), 1);
    // `worker` serializes with skip_serializing_if — an unattributed record
    // carries no key at all (matching every pre-stamp record in the live feed,
    // which stay exogenous forever and are never backfilled).
    assert!(
        records[0].get("worker").is_none(),
        "operator session must land unattributed, got: {}",
        records[0]["worker"]
    );
    assert!(!record_is_fleet(None, &template_fleet_patterns()));
}
