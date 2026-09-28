//! Bounded retention for the token-history JSONL and its SQLite mirror.
//!
//! The working JSONL is the source for the current retention window. Older
//! records are kept as monthly gzip archives beside it; the SQLite file is a
//! derived cache and is pruned to the same window. Rotation and collection
//! append share an advisory lock so a rename can never race an append.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The measured maximum reader lookback is below two days (see the bead
/// evidence and the capped queries in `burn_rate.rs`/`db.rs`), so the locked
/// policy selects this 90-day minimum.
pub const RETENTION_DAYS: i64 = 90;

struct ArchiveWriter {
    temp_path: PathBuf,
    encoder: GzEncoder<File>,
}

/// A process-level advisory lock shared by JSONL append and rotation.
pub struct HistoryLock {
    file: File,
}

impl HistoryLock {
    /// Acquire the exclusive lock associated with `jsonl_path`.
    pub fn acquire(jsonl_path: &Path) -> io::Result<Self> {
        let lock_path = jsonl_path.with_extension("jsonl.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the descriptor belongs to this live File and remains
            // open until Drop releases the lock.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(Self { file })
    }
}

impl Drop for HistoryLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // There is no useful recovery action available from Drop. The
            // descriptor close also releases the lock if this fails.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// Summary of one rotation pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RotationReport {
    pub archived_records: usize,
    pub pruned_instances: usize,
    pub pruned_fleets: usize,
    pub pruned_windows: usize,
}

/// Rotate records older than the retention window and prune the mirror.
///
/// This entry point always checks the files, which makes it useful to callers
/// and tests that need deterministic rotation. The collector uses
/// [`maybe_rotate`] to avoid rescanning a large history on every pass.
pub fn rotate_history(
    jsonl_path: &Path,
    db_path: &Path,
    now: DateTime<Utc>,
) -> Result<RotationReport> {
    let _lock = HistoryLock::acquire(jsonl_path)
        .with_context(|| format!("lock token history {}", jsonl_path.display()))?;
    rotate_locked(jsonl_path, db_path, now)
}

/// Rotate at most once per UTC day. A failed rotation does not advance the
/// marker, so the next collection pass retries it.
pub fn maybe_rotate(jsonl_path: &Path, db_path: &Path) {
    let now = Utc::now();
    let result = (|| -> Result<()> {
        let _lock = HistoryLock::acquire(jsonl_path)
            .with_context(|| format!("lock token history {}", jsonl_path.display()))?;
        let marker_path = marker_path(jsonl_path);
        let today = now.date_naive().to_string();
        if fs::read_to_string(&marker_path)
            .map(|contents| contents.trim() == today)
            .unwrap_or(false)
        {
            return Ok(());
        }

        let report = rotate_locked(jsonl_path, db_path, now)?;
        write_marker(&marker_path, &today)?;
        log::info!(
            "[retention] token history rotation: archived {}, pruned i/f/w = {}/{}/{}",
            report.archived_records,
            report.pruned_instances,
            report.pruned_fleets,
            report.pruned_windows
        );
        Ok(())
    })();

    if let Err(error) = result {
        // Retention must not take the collector down. The JSONL append and
        // SQLite insert can continue, while the next pass retries rotation.
        log::warn!("[retention] rotation skipped: {error:#}");
    }
}

fn rotate_locked(jsonl_path: &Path, db_path: &Path, now: DateTime<Utc>) -> Result<RotationReport> {
    let cutoff = now - Duration::days(RETENTION_DAYS);
    let mut report = RotationReport::default();

    if jsonl_path.exists() {
        report.archived_records = rewrite_jsonl(jsonl_path, cutoff)?;
    }

    if db_path.exists() {
        let conn = crate::db::open_db(db_path)
            .with_context(|| format!("open token history database {}", db_path.display()))?;
        crate::db::create_schema(&conn)?;
        let (i, f, w) = crate::db::prune_before(&conn, cutoff)?;
        report.pruned_instances = i;
        report.pruned_fleets = f;
        report.pruned_windows = w;
        crate::db::reclaim_space(&conn)?;
    }

    Ok(report)
}

fn rewrite_jsonl(jsonl_path: &Path, cutoff: DateTime<Utc>) -> Result<usize> {
    let parent = jsonl_path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = unique_temp_path(parent, "token-history-working");
    let input = File::open(jsonl_path)
        .with_context(|| format!("open token history {}", jsonl_path.display()))?;
    let mut reader = BufReader::new(input);
    let temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .with_context(|| format!("create temporary token history {}", temp_path.display()))?;
    let mut working = BufWriter::new(temp_file);
    let mut archives: BTreeMap<String, ArchiveWriter> = BTreeMap::new();
    let mut line = Vec::new();
    let mut archived_records = 0usize;

    let scan_result = (|| -> Result<()> {
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }

            let archive_month = record_timestamp(&line)
                .filter(|timestamp| *timestamp < cutoff)
                .map(|timestamp| format!("{:04}-{:02}", timestamp.year(), timestamp.month()));

            if let Some(month) = archive_month {
                let archive = get_archive_writer(&mut archives, parent, jsonl_path, &month)?;
                archive.encoder.write_all(&line)?;
                archived_records += 1;
            } else {
                working.write_all(&line)?;
            }
        }
        Ok(())
    })();

    if let Err(error) = scan_result {
        let _ = fs::remove_file(&temp_path);
        for archive in archives.values() {
            let _ = fs::remove_file(&archive.temp_path);
        }
        return Err(error);
    }

    working.flush()?;
    let working_file = working
        .into_inner()
        .map_err(|error| anyhow::anyhow!("flush temporary token history: {}", error))?;
    working_file.sync_all()?;

    if archived_records == 0 {
        drop(archives);
        fs::remove_file(&temp_path)?;
        return Ok(0);
    }

    for (month, archive) in archives {
        let file = archive
            .encoder
            .finish()
            .with_context(|| format!("finish token history archive {month}"))?;
        file.sync_all()?;
        let final_path = archive_path(jsonl_path, &month);
        fs::rename(&archive.temp_path, &final_path)
            .with_context(|| format!("install token history archive {}", final_path.display()))?;
    }

    if let Err(error) = fs::rename(&temp_path, jsonl_path) {
        let _ = fs::remove_file(&temp_path);
        // The archive files are durable, but removing them here would risk
        // destroying a pre-existing archive. A retry can only duplicate lines
        // after a process crash, not during the normal concurrent path; the
        // lock protects the acceptance-critical append/rotation race.
        log::error!(
            "[retention] installed archives but could not replace {}: {}",
            jsonl_path.display(),
            error
        );
        return Err(error.into());
    }

    Ok(archived_records)
}

fn get_archive_writer<'a>(
    archives: &'a mut BTreeMap<String, ArchiveWriter>,
    parent: &Path,
    jsonl_path: &Path,
    month: &str,
) -> Result<&'a mut ArchiveWriter> {
    if !archives.contains_key(month) {
        let temp_path = unique_temp_path(parent, &format!("token-history-{month}"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| format!("create temporary archive {}", temp_path.display()))?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        let existing = archive_path(jsonl_path, month);
        if existing.exists() {
            let existing_file = File::open(&existing)
                .with_context(|| format!("open existing archive {}", existing.display()))?;
            let mut decoder = GzDecoder::new(existing_file);
            let mut bytes = Vec::new();
            decoder
                .read_to_end(&mut bytes)
                .with_context(|| format!("read existing archive {}", existing.display()))?;
            if !bytes.is_empty() {
                encoder.write_all(&bytes)?;
                if !bytes.ends_with(b"\n") {
                    encoder.write_all(b"\n")?;
                }
            }
        }
        archives.insert(month.to_owned(), ArchiveWriter { temp_path, encoder });
    }
    Ok(archives
        .get_mut(month)
        .expect("archive writer was inserted"))
}

fn record_timestamp(line: &[u8]) -> Option<DateTime<Utc>> {
    let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    let record = serde_json::from_slice::<Value>(trimmed).ok()?;
    let timestamp = record.get("ts")?.as_str()?;
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn archive_path(jsonl_path: &Path, month: &str) -> PathBuf {
    let parent = jsonl_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("token-history");
    parent.join(format!("{stem}-{month}.jsonl.gz"))
}

fn marker_path(jsonl_path: &Path) -> PathBuf {
    let parent = jsonl_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("token-history");
    parent.join(format!("{stem}.retention"))
}

fn unique_temp_path(parent: &Path, prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    parent.join(format!(".{prefix}.{}.{}.tmp", std::process::id(), nanos))
}

fn write_marker(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = unique_temp_path(parent, "token-history-marker");
    fs::write(&temp_path, format!("{contents}\n"))?;
    fs::rename(temp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::append_jsonl;
    use rusqlite::params;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn record(ts: DateTime<Utc>, index: usize) -> Value {
        serde_json::json!({
            "r": "i", "ts": ts.to_rfc3339(), "t0": ts.to_rfc3339(),
            "t1": ts.to_rfc3339(), "sess": format!("session-{index}"),
            "sid": format!("session-{index}"), "model": "model",
            "pk": 0, "hr_et": 0, "dow": 0, "input-n": 1,
            "total-usd": 1.0,
        })
    }

    fn jsonl_count(path: &Path) -> usize {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    }

    fn archive_count(path: &Path) -> usize {
        let file = File::open(path).unwrap();
        let mut decoder = GzDecoder::new(file);
        let mut content = String::new();
        decoder.read_to_string(&mut content).unwrap();
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    }

    #[test]
    fn rotates_300_days_preserves_records_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let jsonl = temp.path().join("token-history.jsonl");
        let db = temp.path().join("token-history.db");
        let now = Utc::now();
        let records: Vec<_> = (0..300)
            .map(|index| record(now - Duration::days(index as i64), index))
            .collect();
        let body = records
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        fs::write(&jsonl, body).unwrap();
        crate::db::rebuild_from_jsonl(&jsonl, &db).unwrap();

        let first = rotate_history(&jsonl, &db, now).unwrap();
        assert_eq!(first.archived_records, 209);
        assert_eq!(jsonl_count(&jsonl), 91);

        let archives: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                (path.extension().and_then(|ext| ext.to_str()) == Some("gz")).then_some(path)
            })
            .collect();
        assert!(
            !archives.is_empty(),
            "rotation must create monthly archives"
        );
        let archived_count: usize = archives.iter().map(|path| archive_count(path)).sum();
        assert_eq!(jsonl_count(&jsonl) + archived_count, records.len());

        let conn = crate::db::open_db(&db).unwrap();
        let db_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM i", [], |row| row.get(0))
            .unwrap();
        assert_eq!(db_count, 91);
        let before_jsonl = fs::read(&jsonl).unwrap();
        let before_archives: Vec<_> = archives
            .iter()
            .map(|path| (path.clone(), fs::read(path).unwrap()))
            .collect();

        let second = rotate_history(&jsonl, &db, now).unwrap();
        assert_eq!(second.archived_records, 0);
        assert_eq!(fs::read(&jsonl).unwrap(), before_jsonl);
        for (path, bytes) in before_archives {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn concurrent_append_and_rotation_keep_complete_jsonl_lines() {
        let temp = tempfile::tempdir().unwrap();
        let jsonl = temp.path().join("token-history.jsonl");
        let db = temp.path().join("token-history.db");
        let now = Utc::now();
        append_jsonl(&jsonl, &[record(now - Duration::days(120), 0)]).unwrap();

        let start = Arc::new(Barrier::new(2));
        let append_path = jsonl.clone();
        let append_start = Arc::clone(&start);
        let appender = thread::spawn(move || {
            append_start.wait();
            for index in 1..=50 {
                append_jsonl(&append_path, &[record(Utc::now(), index)]).unwrap();
            }
        });
        start.wait();
        rotate_history(&jsonl, &db, now).unwrap();
        appender.join().unwrap();

        let content = fs::read_to_string(&jsonl).unwrap();
        assert_eq!(content.lines().count(), 50);
        for line in content.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
        let archive = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("gz"))
            .unwrap();
        assert_eq!(archive_count(&archive), 1);
    }

    #[test]
    fn marker_limits_collection_rotation_to_one_pass_per_day() {
        let temp = tempfile::tempdir().unwrap();
        let jsonl = temp.path().join("token-history.jsonl");
        let db = temp.path().join("token-history.db");
        let now = Utc::now();
        append_jsonl(&jsonl, &[record(now - Duration::days(100), 0)]).unwrap();
        maybe_rotate(&jsonl, &db);
        let marker = marker_path(&jsonl);
        let first = fs::read(&marker).unwrap();
        maybe_rotate(&jsonl, &db);
        assert_eq!(fs::read(&marker).unwrap(), first);
    }

    #[test]
    fn db_prune_matches_all_record_tables() {
        let temp = tempfile::tempdir().unwrap();
        let jsonl = temp.path().join("token-history.jsonl");
        let db = temp.path().join("token-history.db");
        let now = Utc::now();
        let old = record(now - Duration::days(100), 0);
        let fresh = record(now - Duration::days(1), 1);
        fs::write(
            &jsonl,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&old).unwrap(),
                serde_json::to_string(&fresh).unwrap()
            ),
        )
        .unwrap();
        let conn = crate::db::open_db(&db).unwrap();
        crate::db::create_schema(&conn).unwrap();
        crate::db::insert_record(&conn, &old).unwrap();
        crate::db::insert_record(&conn, &fresh).unwrap();
        crate::db::insert_record(
            &conn,
            &serde_json::json!({
                "r": "f", "ts": old["ts"], "t0": old["ts"], "t1": old["ts"]
            }),
        )
        .unwrap();
        crate::db::insert_record(
            &conn,
            &serde_json::json!({
                "r": "w", "ts": old["ts"], "win": "five_hour", "reset": old["ts"]
            }),
        )
        .unwrap();
        drop(conn);
        rotate_history(&jsonl, &db, now).unwrap();
        let conn = crate::db::open_db(&db).unwrap();
        for table in ["i", "f", "w"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), params![], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, if table == "i" { 1 } else { 0 });
        }
    }
}
