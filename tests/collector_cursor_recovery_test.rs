//! End-to-end cursor-corruption recovery (claudego-dddaf7fb).
//!
//! CLAUDE.md's "Known warts" entry recorded: "Token collector cursor file can
//! corrupt (`collector pass failed: Failed to load cursors`)". A failed pass
//! collects nothing, which silently stalls the adaptive p75-EMA burn-rate
//! learning the README lists as a key feature.
//!
//! These tests drive REAL collection passes ([`run_collection_pass_at`]) over
//! seeded session transcripts in a temp home, corrupt the cursor file between
//! passes, and assert the next pass:
//!
//! - completes instead of aborting with "Failed to load cursors";
//! - quarantines the corrupt file under a `.corrupt-<timestamp>` backup;
//! - rebuilds every cursor that survived the corruption, so those files resume
//!   from their last good offset with no re-count at all — and only files
//!   whose cursor was lost re-read from byte 0 (the documented one-time
//!   re-count: a re-count, never data loss);
//! - logs a `cursor recovery` event;
//! - leaves a valid cursor file behind, so the pass after that resumes
//!   incrementally again.

use claude_governor::collector::{run_collection_pass_at, CollectionPaths, CursorStore};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};
use tempfile::TempDir;

// --- log capture: the recovery event is the observable "recovery logged" ---

static RECOVERY_LOGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

struct RecoveryLogger;

impl log::Log for RecoveryLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        RECOVERY_LOGS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push(format!("{}", record.args()));
    }
    fn flush(&self) {}
}

fn init_recovery_logger() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = RECOVERY_LOGS.set(Mutex::new(Vec::new()));
        let _ = log::set_logger(&RecoveryLogger);
        log::set_max_level(log::LevelFilter::Warn);
    });
}

fn logs_containing(pattern: &str) -> Vec<String> {
    RECOVERY_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .iter()
        .filter(|msg| msg.contains(pattern))
        .cloned()
        .collect()
}

// --- session + history helpers ---

/// One assistant-usage JSONL line as Claude Code writes it. Distinct token
/// counts per line let the assertions attribute exactly which lines a pass
/// consumed.
fn usage_line(input_tokens: u64, output_tokens: u64) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":[],"model":"claude-sonnet-4-5","usage":{{"input_tokens":{input_tokens},"output_tokens":{output_tokens},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}},"entrypoint":"cli"}}"#
    )
}

fn seed_session(paths: &CollectionPaths, name: &str, lines: &[String]) -> PathBuf {
    let path = paths.session_base.join("proj").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn append_session(path: &Path, lines: &[String]) {
    let mut f = OpenOptions::new().append(true).open(path).unwrap();
    f.write_all((lines.join("\n") + "\n").as_bytes()).unwrap();
}

/// All instance rows from the history JSONL, in file (append) order. For one
/// session the rows appear in pass order, one per pass that consumed any of
/// its lines.
fn instance_rows(paths: &CollectionPaths, session_stem: &str) -> Vec<Value> {
    let content = fs::read_to_string(&paths.history_path).unwrap();
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("r").and_then(|r| r.as_str()) == Some("i"))
        .filter(|v| v.get("sess").and_then(|s| s.as_str()) == Some(session_stem))
        .collect()
}

fn input_n(row: &Value) -> u64 {
    row.get("input-n").and_then(|v| v.as_u64()).unwrap()
}

fn backup_files(home: &TempDir) -> Vec<PathBuf> {
    let state_dir = home.path().join(".needle").join("state");
    fs::read_dir(&state_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().contains("corrupt-"))
                .unwrap_or(false)
        })
        .collect()
}

// --- the end-to-end scenarios ---

#[test]
fn truncated_cursor_recovers_surviving_offsets_and_resumes_without_recount() {
    init_recovery_logger();
    let home = TempDir::new().unwrap();
    let paths = CollectionPaths::under(home.path());

    let sess_a = seed_session(
        &paths,
        "sess-a.jsonl",
        &[usage_line(100, 10), usage_line(200, 20)],
    );
    let sess_b = seed_session(
        &paths,
        "sess-b.jsonl",
        &[usage_line(1000, 100), usage_line(2000, 200)],
    );

    // Pass 1: first sight of both files — one instance row each, cursors at EOF.
    let pass1 = run_collection_pass_at(&paths).expect("pass 1 should complete");
    assert_eq!(pass1.instance_records, 2);
    assert_eq!(pass1.fleet_records, 1);

    let store = CursorStore::load(&paths.cursor_path).unwrap();
    let off_a = store.get_offset(&sess_a);
    let off_b = store.get_offset(&sess_b);
    assert_eq!(off_a, fs::metadata(&sess_a).unwrap().len());
    assert_eq!(off_b, fs::metadata(&sess_b).unwrap().len());

    // Corrupt the cursor file the way a crash mid-write does: the object is
    // cut short. Entry A survives intact; entry B is cut right after its
    // colon, so nothing of its offset is recoverable.
    let corrupt = format!(
        "{{\n  \"cursors\": {{\n    \"{}\": {},\n    \"{}\":",
        sess_a.display(),
        off_a,
        sess_b.display()
    );
    fs::write(&paths.cursor_path, &corrupt).unwrap();

    // Both sessions keep working after the corruption.
    append_session(&sess_a, &[usage_line(400, 40)]);
    append_session(&sess_b, &[usage_line(4000, 400)]);

    // The headline assertion: the pass after the corruption COMPLETES. The
    // pre-hardening behaviour was `collector pass failed: Failed to load
    // cursors` — nothing collected until someone repaired the file by hand.
    let pass2 = run_collection_pass_at(&paths)
        .expect("a corrupt cursor file must never fail the collection pass");
    assert_eq!(pass2.instance_records, 2);

    // A's cursor survived the truncation: it resumes from its last good
    // offset and counts ONLY the newly appended line — no re-count.
    let rows_a = instance_rows(&paths, "sess-a");
    assert_eq!(rows_a.len(), 2, "one row per pass for sess-a");
    assert_eq!(input_n(&rows_a[1]), 400, "sess-a must not be re-counted");

    // B's cursor was lost: it re-read from byte 0 once — the documented
    // one-time re-count (all of B's lines, old and new, in one row).
    let rows_b = instance_rows(&paths, "sess-b");
    assert_eq!(rows_b.len(), 2, "one row per pass for sess-b");
    assert_eq!(
        input_n(&rows_b[1]),
        1000 + 2000 + 4000,
        "sess-b re-counts its whole file exactly once after losing its cursor"
    );

    // The corrupt file was quarantined with the exact bytes preserved.
    let backups = backup_files(&home);
    assert_eq!(backups.len(), 1, "exactly one .corrupt-<ts> backup");
    assert_eq!(fs::read_to_string(&backups[0]).unwrap(), corrupt);

    // The recovery was logged as an event, naming the rebuilt store.
    let events = logs_containing("cursor recovery");
    assert!(!events.is_empty(), "a cursor recovery event must be logged");
    assert!(events[0].contains("cursor recovery"));

    // The pass left a valid cursor file behind (the pass's own save), with
    // both files at their new EOFs.
    let after = CursorStore::load(&paths.cursor_path).unwrap();
    assert_eq!(
        after.get_offset(&sess_a),
        fs::metadata(&sess_a).unwrap().len()
    );
    assert_eq!(
        after.get_offset(&sess_b),
        fs::metadata(&sess_b).unwrap().len()
    );

    // The pass after recovery is incremental again for BOTH files.
    append_session(&sess_a, &[usage_line(40, 4)]);
    append_session(&sess_b, &[usage_line(400, 40)]);
    let pass3 = run_collection_pass_at(&paths).expect("pass 3 should complete");
    assert_eq!(pass3.instance_records, 2);
    let rows_a = instance_rows(&paths, "sess-a");
    let rows_b = instance_rows(&paths, "sess-b");
    assert_eq!(rows_a.len(), 3);
    assert_eq!(rows_b.len(), 3);
    assert_eq!(input_n(&rows_a[2]), 40, "sess-a back to incremental reads");
    assert_eq!(input_n(&rows_b[2]), 400, "sess-b back to incremental reads");
}

#[test]
fn garbage_cursor_fails_over_to_full_reread_then_resumes_incrementally() {
    init_recovery_logger();
    let home = TempDir::new().unwrap();
    let paths = CollectionPaths::under(home.path());

    let sess = seed_session(
        &paths,
        "sess-only.jsonl",
        &[usage_line(100, 10), usage_line(200, 20)],
    );

    let pass1 = run_collection_pass_at(&paths).expect("pass 1 should complete");
    assert_eq!(pass1.instance_records, 1);

    // Corrupt beyond salvage: no recoverable entry at all.
    let garbage = "<<<clobbered by an interrupted write>>>";
    fs::write(&paths.cursor_path, garbage).unwrap();

    append_session(&sess, &[usage_line(400, 40)]);

    // The pass completes; with nothing salvageable every file re-reads once.
    let pass2 = run_collection_pass_at(&paths)
        .expect("an unsalvageable cursor file must still not fail the pass");
    assert_eq!(pass2.instance_records, 1);

    let rows = instance_rows(&paths, "sess-only");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        input_n(&rows[1]),
        100 + 200 + 400,
        "full re-read: the one-time re-count of every line in the file"
    );

    // Quarantined, logged, and the store was rebuilt on disk so the NEXT pass
    // does not repeat the re-read even if this one had aborted early.
    assert_eq!(backup_files(&home).len(), 1);
    assert!(!logs_containing("cursor recovery").is_empty());
    let after = CursorStore::load(&paths.cursor_path).unwrap();
    assert_eq!(after.get_offset(&sess), fs::metadata(&sess).unwrap().len());

    // The following pass resumes incrementally — the re-count happened once.
    append_session(&sess, &[usage_line(800, 80)]);
    let pass3 = run_collection_pass_at(&paths).expect("pass 3 should complete");
    assert_eq!(pass3.instance_records, 1);
    let rows = instance_rows(&paths, "sess-only");
    assert_eq!(rows.len(), 3);
    assert_eq!(input_n(&rows[2]), 800, "incremental resume after recovery");
}
