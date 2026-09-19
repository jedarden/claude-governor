//! Read-only verified-closure yield reader over the NEEDLE attempt ledger
//! (claudego-bba5584b).
//!
//! Since 2026-09-12 NEEDLE appends one `attempt.resolved` JSON line per
//! dispatch to `~/.needle/logs/*.jsonl` carrying the adapter, the outcome and
//! the estimated cost (NEEDLE plan revision 34, section 4.9; ADR-029/030).
//! That makes "verified closures per dollar" measurable per adapter — the
//! governor currently scales on quota-window exhaustion and burn rate alone
//! and cannot see that two adapters can burn the same dollars for very
//! different verified output.
//!
//! This module reads that ledger and aggregates it per adapter. It is
//! deliberately **read-only**: it opens log files for reading and never
//! creates, truncates, renames or writes anything (enforced by a
//! filesystem-snapshot test in `tests/ledger_yield.rs`).
//!
//! Rows are excluded when any of these hold:
//!
//! - fixture rows: `worker_id` (or `data.worker`) ends with `-test-worker`,
//!   or the row's workspace is `.` — test invocations must never be counted
//!   as fleet spend (ADR-030, "consumers skip fixture rows");
//! - `data.costed` is explicitly `false` — an attempt whose adapter reported
//!   no usage must not be charged to a cost-per-closure figure. The flag is
//!   absent on ledger rows written before NEEDLE N-T47 ships, and absence
//!   means costed;
//! - `data.outcome` is `decomposed` — a split that produced child beads is a
//!   resolution class, not a success, and earns no verified credit
//!   (ADR-030 decision 1);
//! - the row is partial: missing/unparseable timestamp (it cannot be placed
//!   in the window), missing adapter or missing outcome — ignored, never a
//!   read error;
//! - the row's timestamp is before the rolling window start.
//!
//! `estimated_cost_usd` is optional: rows without it still count as attempts
//! (three adapters report no usage today), they just contribute no cost. Cost
//! per verified closure is therefore a floor, exactly as in plan section 4.9.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Default rolling window: three days of attempts. Long enough to average
/// out a single bad bead, short enough to track an adapter change within a
/// shift. Override with `CGOV_LEDGER_WINDOW_HOURS`.
pub const DEFAULT_WINDOW_HOURS: u64 = 72;

/// Environment override for the ledger directory.
pub const ENV_LOGS_DIR: &str = "CGOV_LEDGER_LOGS_DIR";

/// Environment override for the rolling window in hours.
pub const ENV_WINDOW_HOURS: &str = "CGOV_LEDGER_WINDOW_HOURS";

/// Files whose mtime is older than the window start are skipped without being
/// read — rows are appended as they resolve, so nothing in such a file can
/// carry an in-window timestamp. The grace margin only absorbs clock skew
/// between the writing worker and this reader.
const MTIME_PRUNE_GRACE: Duration = Duration::hours(1);

/// Where and over how long to read the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerYieldSettings {
    /// Directory holding the `*.jsonl` ledger files (`~/.needle/logs`).
    pub logs_dir: PathBuf,
    /// Rolling window length in hours.
    pub window_hours: u64,
}

impl LedgerYieldSettings {
    /// Defaults from the environment, falling back to the live NEEDLE log
    /// directory and [`DEFAULT_WINDOW_HOURS`].
    pub fn from_env() -> Self {
        let logs_dir = std::env::var(ENV_LOGS_DIR)
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_logs_dir());
        let window_hours = window_hours_from_env();
        Self {
            logs_dir,
            window_hours,
        }
    }
}

/// The live NEEDLE ledger directory.
pub fn default_logs_dir() -> PathBuf {
    std::env::var(ENV_LOGS_DIR)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".needle")
                .join("logs")
        })
}

/// Rolling window length from the environment, or [`DEFAULT_WINDOW_HOURS`].
/// An unparseable or zero value falls back to the default rather than
/// widening the window to everything.
pub fn window_hours_from_env() -> u64 {
    std::env::var(ENV_WINDOW_HOURS)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&h| h > 0)
        .unwrap_or(DEFAULT_WINDOW_HOURS)
}

/// Verified-closure economics for one adapter (and, once, for the fleet).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AdapterYield {
    pub adapter: String,
    /// In-window, non-excluded `attempt.resolved` rows.
    pub attempts: u64,
    /// Attempts resolving `verified_success`.
    pub verified: u64,
    /// `verified / attempts`, `None` when the adapter has no attempts.
    pub verified_yield: Option<f64>,
    /// Sum of `estimated_cost_usd` over attempts. A floor: rows without the
    /// field (and costed=false rows, excluded outright) contribute nothing.
    pub cost_usd: f64,
    /// `cost_usd / verified`, `None` when nothing verified — never infinity.
    pub cost_per_verified_usd: Option<f64>,
}

/// Per-adapter verified-closure economics over one rolling window.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LedgerYieldReport {
    pub window_hours: u64,
    pub window_start: DateTime<Utc>,
    pub computed_at: DateTime<Utc>,
    /// Fleet totals across all adapters (same exclusions).
    pub attempts: u64,
    pub verified: u64,
    pub verified_yield: Option<f64>,
    pub cost_usd: f64,
    pub cost_per_verified_usd: Option<f64>,
    /// Rows seen inside the window that were excluded or partial (fixture,
    /// decomposed, costed=false, missing adapter/outcome/timestamp, unparseable
    /// line). Rolloff of pre-window rows is not counted — those files are not
    /// read at all.
    pub rows_ignored: u64,
    /// By adapter, ordered by name for stable output.
    pub by_adapter: BTreeMap<String, AdapterYield>,
}

impl LedgerYieldReport {
    fn empty(now: DateTime<Utc>, window_hours: u64) -> Self {
        Self {
            window_hours,
            window_start: now - Duration::hours(window_hours as i64),
            computed_at: now,
            attempts: 0,
            verified: 0,
            verified_yield: None,
            cost_usd: 0.0,
            cost_per_verified_usd: None,
            rows_ignored: 0,
            by_adapter: BTreeMap::new(),
        }
    }
}

/// One `attempt.resolved` ledger line, tolerating every field being absent.
/// Anything required by the aggregation that is missing here marks the row
/// partial, and partial rows are ignored rather than read errors.
#[derive(Debug, Deserialize)]
struct RawAttempt {
    timestamp: Option<String>,
    event_type: Option<String>,
    worker_id: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    data: Option<RawAttemptData>,
}

#[derive(Debug, Deserialize)]
struct RawAttemptData {
    #[serde(default)]
    adapter: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
    /// Explicitly false = the adapter reported no usage for this attempt.
    #[serde(default)]
    costed: Option<bool>,
    #[serde(default, rename = "estimated_cost_usd")]
    estimated_cost_usd: Option<f64>,
    #[serde(default)]
    worker: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
}

/// The marker every `attempt.resolved` line carries. Scanning for this byte
/// substring first keeps serde parsing off the ~99% of ledger lines that are
/// other event types.
const EVENT_TAG: &str = "\"attempt.resolved\"";

/// Read the ledger under `logs_dir` and aggregate per-adapter verified-closure
/// economics over the `window_hours` ending at `now`.
///
/// Read-only: opens files for reading, never writes. A missing directory
/// yields an empty report (the fleet simply has no ledger yet); other I/O
/// errors propagate. An unreadable individual file is skipped after counting
/// nothing — a wedged log must not take status down with it.
pub fn read_ledger_yield(
    logs_dir: &Path,
    now: DateTime<Utc>,
    window_hours: u64,
) -> io::Result<LedgerYieldReport> {
    let window_start = now - Duration::hours(window_hours as i64);
    let mut report = LedgerYieldReport::empty(now, window_hours);

    let entries = match fs::read_dir(logs_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e),
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        // Files untouched since before the window cannot contain an in-window
        // row (rows are appended with their resolution timestamp), so skip
        // them without reading.
        let prune_before = window_start - MTIME_PRUNE_GRACE;
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|mtime| mtime >= prune_before.into())
            .unwrap_or(true); // if mtime is unknowable, read the file anyway
        if fresh {
            paths.push(path);
        }
    }
    paths.sort();

    for path in paths {
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            // A file that vanished mid-scan or is unreadable must not take
            // the whole report down.
            Err(_) => continue,
        };
        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if !line.contains(EVENT_TAG) {
                continue;
            }
            fold_line(&line, window_start, &mut report);
        }
    }

    finalize(&mut report);
    Ok(report)
}

/// Classify one candidate line and fold it into the report.
fn fold_line(line: &str, window_start: DateTime<Utc>, report: &mut LedgerYieldReport) {
    let ignored = &mut report.rows_ignored;

    let row: RawAttempt = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(_) => {
            *ignored += 1;
            return;
        }
    };
    if row.event_type.as_deref() != Some("attempt.resolved") {
        // A line merely mentioning the tag (e.g. a summary event listing
        // counts) is not itself an attempt.
        return;
    }

    // Out-of-window or unplaceable rows are ignored, not errors.
    let ts = row
        .timestamp
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    if !ts.is_some_and(|ts| ts >= window_start) {
        *ignored += 1;
        return;
    }

    let data = row.data.as_ref();
    let worker_id = row
        .worker_id
        .as_deref()
        .or(data.and_then(|d| d.worker.as_deref()));
    if worker_id.is_some_and(|w| w.ends_with("-test-worker")) {
        // Fixture rows from test invocations (ADR-030: consumers skip them).
        *ignored += 1;
        return;
    }

    let workspace = row
        .workspace
        .as_deref()
        .or(data.and_then(|d| d.workspace.as_deref()));
    if workspace == Some(".") {
        // Relative-path workspace rows are also fixture invocations.
        *ignored += 1;
        return;
    }

    let outcome = data.and_then(|d| d.outcome.as_deref());
    if outcome == Some("decomposed") {
        // ADR-030 decision 1: a decomposition earns no verified credit and is
        // not counted as an attempt here either.
        *ignored += 1;
        return;
    }

    if data.and_then(|d| d.costed) == Some(false) {
        // Adapter reported no usage; charging it to cost-per-closure would be
        // fabrication (ADR-030 decision 2).
        *ignored += 1;
        return;
    }

    let adapter = match data.and_then(|d| d.adapter.as_deref()) {
        Some(a) if !a.is_empty() => a.to_string(),
        // Partial row: no adapter to attribute the attempt to.
        _ => {
            *ignored += 1;
            return;
        }
    };
    if outcome.is_none() {
        // Partial row: an attempt that never resolved to a class.
        *ignored += 1;
        return;
    }

    let cost = data
        .and_then(|d| d.estimated_cost_usd)
        .filter(|c| c.is_finite())
        .unwrap_or(0.0);
    let verified = outcome == Some("verified_success");

    report.attempts += 1;
    if verified {
        report.verified += 1;
    }
    report.cost_usd += cost;

    let entry = report
        .by_adapter
        .entry(adapter.clone())
        .or_insert_with(|| AdapterYield {
            adapter: adapter.clone(),
            attempts: 0,
            verified: 0,
            verified_yield: None,
            cost_usd: 0.0,
            cost_per_verified_usd: None,
        });
    entry.attempts += 1;
    if verified {
        entry.verified += 1;
    }
    entry.cost_usd += cost;
}

/// Derive the ratio fields once counting is done.
fn finalize(report: &mut LedgerYieldReport) {
    for entry in report.by_adapter.values_mut() {
        entry.verified_yield = ratio(entry.verified, entry.attempts);
        entry.cost_per_verified_usd = ratio_f64(entry.cost_usd, entry.verified);
    }
    report.verified_yield = ratio(report.verified, report.attempts);
    report.cost_per_verified_usd = ratio_f64(report.cost_usd, report.verified);
}

fn ratio(num: u64, den: u64) -> Option<f64> {
    if den == 0 {
        None
    } else {
        Some(num as f64 / den as f64)
    }
}

fn ratio_f64(num: f64, den: u64) -> Option<f64> {
    if den == 0 {
        None
    } else {
        Some(num / den as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt_row(
        ts: DateTime<Utc>,
        worker: &str,
        workspace: &str,
        adapter: &str,
        outcome: &str,
    ) -> String {
        format!(
            r#"{{"timestamp":"{}","event_type":"attempt.resolved","worker_id":"{}","session_id":"deadbeef","bead_id":"x-1","workspace":"{}","data":{{"adapter":"{}","outcome":"{}","estimated_cost_usd":1.5,"exit_code":0}}}}"#,
            ts.to_rfc3339(),
            worker,
            workspace,
            adapter,
            outcome
        )
    }

    #[test]
    fn fold_counts_verified_and_cost_per_adapter() {
        let now = Utc::now();
        let window_start = now - Duration::hours(2);
        let ts = now - Duration::hours(1);
        let mut report = LedgerYieldReport::empty(now, 72);

        for (adapter, outcome) in [
            ("adapter-a", "verified_success"),
            ("adapter-a", "work_failure"),
            ("adapter-a", "verified_success"),
            ("adapter-b", "indeterminate"),
        ] {
            fold_line(
                &attempt_row(ts, "worker-x", "/repo", adapter, outcome),
                window_start,
                &mut report,
            );
        }

        finalize(&mut report);
        let a = &report.by_adapter["adapter-a"];
        assert_eq!(a.attempts, 3);
        assert_eq!(a.verified, 2);
        assert_eq!(a.verified_yield, Some(2.0 / 3.0));
        assert_eq!(a.cost_usd, 4.5);
        assert_eq!(a.cost_per_verified_usd, Some(2.25));

        let b = &report.by_adapter["adapter-b"];
        assert_eq!(b.attempts, 1);
        assert_eq!(b.verified, 0);
        assert_eq!(b.verified_yield, Some(0.0));
        assert_eq!(b.cost_per_verified_usd, None);

        assert_eq!(report.attempts, 4);
        assert_eq!(report.verified, 2);
        assert_eq!(report.rows_ignored, 0);
    }

    #[test]
    fn ratio_never_divides_by_zero() {
        assert_eq!(ratio(1, 0), None);
        assert_eq!(ratio_f64(1.0, 0), None);
    }
}
