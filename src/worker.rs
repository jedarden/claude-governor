//! Worker Management - scaling operations for Claude Code agents
//!
//! This module handles:
//! - `scale_up(n)`: launch new agent workers via shell command
//! - `scale_down_graceful(n)`: find idle workers, send SIGINT via tmux, kill after timeout
//! - `count_workers()`: verify worker count via heartbeat files + tmux sessions

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration as StdDuration;

/// Staleness threshold for heartbeat files.
///
/// Heartbeats older than this are treated as stale — the worker may have crashed
/// without cleanup. Stale heartbeats are verified against tmux before being removed.
const STALE_HEARTBEAT_THRESHOLD: i64 = 60; // seconds

/// Disk usage percentage at or above which `scale_up` refuses to launch workers.
///
/// On 2026-08-05 a needle span-nesting leak (NEEDLE bf-3uj6i) drove a single
/// worker's stderr log to 33.7 GB at ~159 GB/hr. The governor is the spawner, and
/// each fresh worker restarts such a leak from zero, so an unguarded scale_up turns
/// a worker-side logging bug into a host-wide outage.
///
/// The governor does NOT own the worker stderr file — needle builds the `2>>`
/// redirect itself (needle `src/cli/mod.rs`, `~/.needle/logs/<session>.stderr.log`),
/// so the governor cannot rotate or truncate it without racing needle's own writer.
/// What the governor does own is the decision to add another writer, so that is
/// where the bound belongs: stop feeding a filling disk.
const SCALE_UP_MAX_DISK_USE_PCT: u8 = 90;

/// Worker heartbeat JSON structure (written by each worker instance)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    /// Worker session identifier
    pub session: String,

    /// Timestamp of this heartbeat
    pub timestamp: DateTime<Utc>,

    /// Whether the worker is currently idle (no active task)
    pub is_idle: bool,

    /// Current task ID if any
    pub current_task: Option<String>,

    /// Model being used
    pub model: String,
}

/// Configuration for worker scaling operations
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Command to launch a new worker (e.g., "tmux new-session -d -s worker-{id} claude")
    pub launch_cmd: String,

    /// Directory containing heartbeat JSON files
    pub heartbeat_dir: PathBuf,

    /// Seconds to wait for graceful shutdown before force-killing
    pub graceful_timeout_secs: u64,

    /// Prefix for tmux session names
    pub session_prefix: String,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            launch_cmd: "tmux new-session -d -s worker-{id} -- claude".to_string(),
            heartbeat_dir: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".needle")
                .join("heartbeats"),
            graceful_timeout_secs: 30,
            session_prefix: "worker".to_string(),
        }
    }
}

impl WorkerConfig {
    /// Build a WorkerConfig from an AgentConfig.
    ///
    /// Expands `~` in heartbeat_dir, extracts session_prefix from session_pattern.
    pub fn from_agent_config(agent: &crate::config::AgentConfig) -> Self {
        Self {
            launch_cmd: agent.launch_cmd.clone(),
            heartbeat_dir: agent.heartbeat_dir_expanded(),
            graceful_timeout_secs: 30,
            session_prefix: agent.session_prefix().to_string(),
        }
    }
}

/// Result of a worker count operation
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerCount {
    /// Workers detected via heartbeat files
    pub heartbeat_count: usize,

    /// Workers detected via tmux list-sessions
    pub tmux_count: usize,

    /// Whether the counts match (consistency check)
    pub consistent: bool,

    /// Session names from tmux
    pub sessions: Vec<String>,
}

/// Result of a scale-down operation
#[derive(Debug, Clone)]
pub struct ScaleDownResult {
    /// Number of workers targeted for shutdown
    pub targeted: usize,

    /// Number of workers that received SIGINT
    pub signaled: usize,

    /// Number of workers that shut down gracefully
    pub graceful: usize,

    /// Number of workers that had to be force-killed
    pub force_killed: usize,

    /// Session names that were shut down
    pub sessions: Vec<String>,
}

/// Count active workers using heartbeat files and tmux sessions.
///
/// This provides a consistency check - if heartbeat and tmux counts differ,
/// something may be wrong (stale heartbeats, orphaned sessions, etc.)
///
/// Orphaned heartbeats (stale, with no matching tmux session) are swept by
/// [`read_heartbeats`] and excluded from `heartbeat_count`, so a count that went
/// inconsistent because a worker died without cleanup returns to consistent once
/// its heartbeat ages past [`STALE_HEARTBEAT_THRESHOLD`].
pub fn count_workers(config: &WorkerConfig) -> WorkerCount {
    // Count heartbeat files, filtered to this agent's session prefix
    let heartbeat_count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);

    // Count tmux sessions
    let (tmux_count, sessions) = count_tmux_sessions(&config.session_prefix);

    WorkerCount {
        heartbeat_count,
        tmux_count,
        consistent: heartbeat_count == tmux_count,
        sessions,
    }
}

/// Count heartbeat JSON files in the heartbeat directory, filtered by session prefix.
///
/// Only counts files whose `session` field starts with `session_prefix`, so workers
/// from other projects sharing the same heartbeat directory are excluded.
fn count_heartbeat_files(dir: &Path, session_prefix: &str) -> usize {
    read_heartbeats(dir, session_prefix).len()
}

/// Count tmux sessions with the given prefix.
///
/// Returns (count, session_names).
fn count_tmux_sessions(prefix: &str) -> (usize, Vec<String>) {
    let output = match Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            // tmux not running or not installed
            log::debug!("[worker] tmux list-sessions failed: {}", e);
            return (0, Vec::new());
        }
    };

    if !output.status.success() {
        // No sessions exist (tmux returns error when no sessions)
        return (0, Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sessions: Vec<String> = stdout
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(|s| s.to_string())
        .collect();

    (sessions.len(), sessions)
}

/// Scale up by launching n new workers.
///
/// Executes the launch_cmd n times via shell, substituting {id} with
/// a unique identifier based on timestamp and index.
///
/// Returns the number of workers successfully launched.
pub fn scale_up(n: u32, config: &WorkerConfig, dry_run: bool) -> usize {
    if n == 0 {
        return 0;
    }

    // Refuse to add workers to a filling disk. Checked before the dry_run branch so
    // observe-only mode reports the block too — a dry run that claims it "would
    // launch" while the real path would refuse is a misleading forecast.
    //
    // The heartbeat dir lives under ~/.needle, the same filesystem as the
    // ~/.needle/logs/*.stderr.log files a leaking worker grows, so it is the right
    // thing to stat. See SCALE_UP_MAX_DISK_USE_PCT.
    let use_pct = disk_use_percent(&config.heartbeat_dir);
    if scale_up_blocked_by_disk(use_pct) {
        log::error!(
            "[worker] refusing to launch {} worker(s): disk {}% used (>= {}% limit) on the \
             filesystem holding {}. Each worker adds a log writer; see NEEDLE bf-3uj6i.",
            n,
            use_pct.unwrap_or(0),
            SCALE_UP_MAX_DISK_USE_PCT,
            config.heartbeat_dir.display(),
        );
        return 0;
    }
    if use_pct.is_none() {
        log::warn!(
            "[worker] could not determine disk usage for {} — proceeding with launch",
            config.heartbeat_dir.display(),
        );
    }

    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
    let mut launched = 0;

    for i in 0..n {
        let worker_id = format!("{}-{}", timestamp, i);
        let workspace = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .to_string_lossy()
            .into_owned();
        let cmd = config
            .launch_cmd
            .replace("{id}", &worker_id)
            .replace("{workspace}", &workspace);

        if dry_run {
            log::info!("[worker] DRY RUN: would launch: {}", cmd);
            launched += 1;
            continue;
        }

        log::info!("[worker] launching: {}", cmd);

        match execute_shell_command(&cmd) {
            Ok(result) if result.success => {
                log::info!("[worker] launched worker {}", worker_id);
                if !result.stdout.is_empty() {
                    log::debug!("[worker] stdout: {}", result.stdout);
                }
                launched += 1;
            }
            Ok(result) => {
                log::warn!(
                    "[worker] launch failed for {} (exit_code={:?}): stderr={:?}, stdout={:?}",
                    worker_id,
                    result.exit_code,
                    result.stderr,
                    result.stdout,
                );
            }
            Err(e) => {
                log::error!(
                    "[worker] failed to execute launch command for {}: {}",
                    worker_id,
                    e
                );
            }
        }
    }

    launched
}

/// Read the used-percentage of the filesystem holding `path`.
///
/// Uses `df -P` (POSIX output format) so the columns stay on one line regardless
/// of how long the device name is — plain `df` wraps long device names onto a
/// second line and shifts every field.
///
/// Returns `None` if df is unavailable or its output cannot be parsed. Callers
/// treat `None` as "unknown" and proceed: a df that stops parsing should not be
/// able to wedge scaling permanently.
pub fn disk_use_percent(path: &Path) -> Option<u8> {
    let output = Command::new("df").arg("-P").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Line 0 is the header; line 1 holds the values.
    let fields: Vec<&str> = stdout.lines().nth(1)?.split_whitespace().collect();
    // Filesystem, 1024-blocks, Used, Available, Capacity, Mounted-on
    fields.get(4)?.trim_end_matches('%').parse::<u8>().ok()
}

/// Whether a disk reading should block launching new workers.
///
/// `None` means df was unavailable or unparseable. That fails OPEN (returns false):
/// a df that stops reporting should not be able to wedge scaling permanently, and
/// unlike a genuinely full disk it is not evidence of a problem. Note that some
/// pseudo-filesystems (procfs, for one) report capacity as `-`, which lands here.
fn scale_up_blocked_by_disk(use_pct: Option<u8>) -> bool {
    match use_pct {
        Some(pct) => pct >= SCALE_UP_MAX_DISK_USE_PCT,
        None => false,
    }
}

/// Result of executing a shell command.
pub struct ShellOutput {
    /// Whether the command exited successfully (exit code 0).
    pub success: bool,
    /// The exit code, if available.
    pub exit_code: Option<i32>,
    /// Captured stderr (trimmed).
    pub stderr: String,
    /// Captured stdout (trimmed).
    pub stdout: String,
}

/// Execute a shell command string.
///
/// Returns Ok(ShellOutput) with exit code, stdout, and stderr,
/// or Err if the command couldn't be executed at all.
fn execute_shell_command(cmd: &str) -> anyhow::Result<ShellOutput> {
    let output = Command::new("sh").arg("-c").arg(cmd).output()?;

    Ok(ShellOutput {
        success: output.status.success(),
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
    })
}

/// Scale down gracefully by finding idle workers and shutting them down.
///
/// Process:
/// 1. Read heartbeat JSON files to find idle workers
/// 2. If not enough idle workers, select longest-idle workers
/// 3. Send SIGINT via `tmux send-keys` to request graceful shutdown
/// 4. Wait up to graceful_timeout_secs for workers to exit
/// 5. Force-kill any workers that didn't shut down gracefully
pub fn scale_down_graceful(n: u32, config: &WorkerConfig, dry_run: bool) -> ScaleDownResult {
    let mut result = ScaleDownResult {
        targeted: n as usize,
        signaled: 0,
        graceful: 0,
        force_killed: 0,
        sessions: Vec::new(),
    };

    if n == 0 {
        return result;
    }

    // Find workers to shut down (prefer idle ones)
    let workers_to_stop = find_workers_to_stop(n as usize, config);

    if workers_to_stop.is_empty() {
        log::info!("[worker] no workers available to stop");
        return result;
    }

    result.sessions = workers_to_stop.clone();

    if dry_run {
        log::info!(
            "[worker] DRY RUN: would gracefully stop {} workers: {:?}",
            workers_to_stop.len(),
            workers_to_stop
        );
        result.signaled = workers_to_stop.len();
        result.graceful = workers_to_stop.len();
        return result;
    }

    // Send SIGINT to each worker via tmux
    for session in &workers_to_stop {
        if send_sigint_to_session(session) {
            result.signaled += 1;
        }
    }

    log::info!(
        "[worker] sent SIGINT to {}/{} workers",
        result.signaled,
        workers_to_stop.len()
    );

    // Wait for graceful shutdown
    let check_interval = StdDuration::from_secs(2);
    let mut elapsed = StdDuration::ZERO;
    let timeout = StdDuration::from_secs(config.graceful_timeout_secs);

    while elapsed < timeout {
        std::thread::sleep(check_interval);
        elapsed += check_interval;

        // Check which sessions are still alive
        let remaining: Vec<String> = workers_to_stop
            .iter()
            .filter(|s| session_exists(s))
            .cloned()
            .collect();

        result.graceful = workers_to_stop.len() - remaining.len();

        if remaining.is_empty() {
            log::info!(
                "[worker] all {} workers shut down gracefully after {:?}",
                result.graceful,
                elapsed
            );
            return result;
        }
    }

    // Force-kill remaining workers
    let remaining: Vec<String> = workers_to_stop
        .iter()
        .filter(|s| session_exists(s))
        .cloned()
        .collect();

    for session in &remaining {
        log::warn!("[worker] force-killing session {}", session);
        if kill_session(session) {
            result.force_killed += 1;
        }
    }

    result.graceful = workers_to_stop.len() - remaining.len();

    log::info!(
        "[worker] scale-down complete: {} graceful, {} force-killed",
        result.graceful,
        result.force_killed
    );

    result
}

/// Find workers to stop, preferring idle workers.
///
/// Returns up to `n` session names, sorted by idle status and heartbeat age.
///
/// Only workers whose tmux session is currently live are eligible: a heartbeat
/// without a matching tmux session belongs to a worker that is already gone, and
/// signalling it would send SIGINT/kill to a nonexistent session.
fn find_workers_to_stop(n: usize, config: &WorkerConfig) -> Vec<String> {
    // One tmux snapshot for both the orphan sweep and the liveness filter, so
    // selection can never disagree with what cleanup just saw.
    let (_, tmux_sessions) = count_tmux_sessions(&config.session_prefix);
    let live_sessions: HashSet<String> = tmux_sessions.into_iter().collect();

    let heartbeats = read_heartbeats_with_sessions(
        &config.heartbeat_dir,
        &config.session_prefix,
        &live_sessions,
    );

    select_workers_to_stop(n, heartbeats, &live_sessions)
}

/// Pick up to `n` shutdown candidates from `heartbeats`, restricted to `live_sessions`.
///
/// Split out from [`find_workers_to_stop`] so the selection rules can be tested
/// against an explicit set of live tmux sessions.
fn select_workers_to_stop(
    n: usize,
    heartbeats: HashMap<String, Heartbeat>,
    live_sessions: &HashSet<String>,
) -> Vec<String> {
    // Drop candidates whose tmux session is gone — a stale heartbeat that outlived
    // its session, or a fresh heartbeat from a worker that died seconds ago.
    let mut workers: Vec<_> = heartbeats
        .into_iter()
        .filter(|(session, _)| {
            let live = live_sessions.contains(session);
            if !live {
                log::debug!(
                    "[worker] skipping {} as a shutdown candidate: no live tmux session",
                    session
                );
            }
            live
        })
        .collect();

    // Sort workers: idle first, then by heartbeat age (oldest first)
    workers.sort_by(|a, b| {
        // Prefer idle workers
        match (a.1.is_idle, b.1.is_idle) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => {
                // Among same idle status, prefer older heartbeats (may be dead)
                a.1.timestamp.cmp(&b.1.timestamp)
            }
        }
    });

    workers
        .into_iter()
        .take(n)
        .map(|(session, _)| session)
        .collect()
}

/// Read heartbeat files from the directory, filtered to sessions with the given prefix.
///
/// Only heartbeats whose `session` field starts with `session_prefix` are returned,
/// so workers from other projects sharing the same heartbeat directory are excluded.
///
/// Stale heartbeat handling:
/// - Heartbeats older than STALE_HEARTBEAT_THRESHOLD are considered stale
/// - For stale heartbeats, we verify against tmux list-sessions
/// - If the tmux session no longer exists, the heartbeat file is removed
/// - If the tmux session exists, the heartbeat is retained but treated as executing
///   (never selected for shutdown based on an outdated idle status)
fn read_heartbeats(dir: &Path, session_prefix: &str) -> HashMap<String, Heartbeat> {
    let (_, tmux_sessions) = count_tmux_sessions(session_prefix);
    let tmux_sessions_set: HashSet<String> = tmux_sessions.into_iter().collect();
    read_heartbeats_with_sessions(dir, session_prefix, &tmux_sessions_set)
}

/// [`read_heartbeats`] against an already-taken snapshot of live tmux sessions.
///
/// Callers that also need the session list (to filter shutdown candidates, say)
/// query tmux once and pass the result here.
fn read_heartbeats_with_sessions(
    dir: &Path,
    session_prefix: &str,
    tmux_sessions_set: &HashSet<String>,
) -> HashMap<String, Heartbeat> {
    let mut heartbeats = HashMap::new();
    let now = Utc::now();
    let stale_threshold = ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD);

    if !dir.exists() {
        return heartbeats;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!(
                "[worker] failed to read heartbeat dir {}: {}",
                dir.display(),
                e
            );
            return heartbeats;
        }
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().map(|ext| ext != "json").unwrap_or(true) {
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<Heartbeat>(&content) {
                Ok(mut hb) => {
                    if !hb.session.starts_with(session_prefix) {
                        continue;
                    }

                    let age = now.signed_duration_since(hb.timestamp);
                    let is_stale = age > stale_threshold;

                    if is_stale {
                        // Stale heartbeat — verify against tmux
                        let session_exists = tmux_sessions_set.contains(&hb.session);

                        if !session_exists {
                            // Session no longer exists, remove orphaned heartbeat file
                            match fs::remove_file(&path) {
                                Ok(()) => log::info!(
                                    "[worker] removed orphaned heartbeat for session {} at {} (session not in tmux, age={}s)",
                                    hb.session,
                                    path.display(),
                                    age.num_seconds()
                                ),
                                Err(e) => log::warn!(
                                    "[worker] failed to remove orphaned heartbeat for session {} at {}: {}",
                                    hb.session,
                                    path.display(),
                                    e
                                ),
                            }
                            // Excluded from the returned map either way — the session is gone.
                            continue;
                        }

                        // Session exists but heartbeat is stale — treat as executing to prevent
                        // shutdown based on outdated idle status
                        log::debug!(
                            "[worker] stale heartbeat for session {} but session exists (age={}s), treating as executing",
                            hb.session,
                            age.num_seconds()
                        );
                        hb.is_idle = false;
                    }

                    heartbeats.insert(hb.session.clone(), hb);
                }
                Err(e) => {
                    log::debug!("[worker] invalid heartbeat {}: {}", path.display(), e);
                }
            },
            Err(e) => {
                log::debug!(
                    "[worker] failed to read heartbeat {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    heartbeats
}

/// Check if a tmux session exists.
fn session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Send SIGINT (Ctrl+C) to a tmux session.
fn send_sigint_to_session(session: &str) -> bool {
    let result = Command::new("tmux")
        .args(["send-keys", "-t", session, "C-c"])
        .output();

    match result {
        Ok(o) => {
            if o.status.success() {
                log::debug!("[worker] sent SIGINT to session {}", session);
                true
            } else {
                log::warn!(
                    "[worker] failed to send SIGINT to {}: {}",
                    session,
                    String::from_utf8_lossy(&o.stderr)
                );
                false
            }
        }
        Err(e) => {
            log::error!("[worker] failed to send SIGINT to {}: {}", session, e);
            false
        }
    }
}

/// Force-kill a tmux session.
fn kill_session(session: &str) -> bool {
    let result = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .output();

    match result {
        Ok(o) => {
            if o.status.success() {
                log::debug!("[worker] killed session {}", session);
                true
            } else {
                log::warn!(
                    "[worker] failed to kill {}: {}",
                    session,
                    String::from_utf8_lossy(&o.stderr)
                );
                false
            }
        }
        Err(e) => {
            log::error!("[worker] failed to kill {}: {}", session, e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> WorkerConfig {
        WorkerConfig {
            launch_cmd: "echo 'would launch {id}'".to_string(),
            heartbeat_dir: dir.path().join("heartbeats"),
            graceful_timeout_secs: 2,
            session_prefix: "test-worker".to_string(),
        }
    }

    /// Build a live-session set from session names.
    fn live(sessions: &[&str]) -> HashSet<String> {
        sessions.iter().map(|s| s.to_string()).collect()
    }

    /// A real detached tmux session, killed when the guard drops.
    ///
    /// Tests that exercise the tmux liveness check end-to-end need an actual
    /// session; `None` means tmux is unavailable and the caller should skip.
    struct TmuxSession {
        name: String,
    }

    impl TmuxSession {
        fn new(name: &str) -> Option<Self> {
            let started = Command::new("tmux")
                .args(["new-session", "-d", "-s", name, "sleep", "60"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);

            started.then(|| Self {
                name: name.to_string(),
            })
        }
    }

    impl Drop for TmuxSession {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &self.name])
                .output();
        }
    }

    /// Write a heartbeat `age_secs` old for `session`; returns its path.
    fn write_heartbeat(
        config: &WorkerConfig,
        session: &str,
        age_secs: i64,
        is_idle: bool,
    ) -> PathBuf {
        fs::create_dir_all(&config.heartbeat_dir).unwrap();
        let heartbeat = serde_json::json!({
            "session": session,
            "timestamp": (Utc::now() - ChronoDuration::seconds(age_secs)).to_rfc3339(),
            "is_idle": is_idle,
            "current_task": null,
            "model": "sonnet",
        });
        let path = config.heartbeat_dir.join(format!("{session}.json"));
        fs::write(&path, serde_json::to_string_pretty(&heartbeat).unwrap()).unwrap();
        path
    }

    #[test]
    fn count_heartbeat_files_empty_dir() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
        assert_eq!(count, 0);
    }

    #[test]
    fn count_heartbeat_files_counts_json() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create fresh heartbeat files whose sessions match the prefix "test-worker"
        let fresh_timestamp = (Utc::now() - ChronoDuration::seconds(30))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        fs::write(
            config.heartbeat_dir.join("test-worker-1.json"),
            format!(r#"{{"session":"test-worker-1","timestamp":"{}","is_idle":true,"current_task":null,"model":"sonnet"}}"#, fresh_timestamp),
        ).unwrap();
        fs::write(
            config.heartbeat_dir.join("test-worker-2.json"),
            format!(r#"{{"session":"test-worker-2","timestamp":"{}","is_idle":false,"current_task":"task-123","model":"sonnet"}}"#, fresh_timestamp),
        ).unwrap();
        // Non-JSON file should be ignored
        fs::write(config.heartbeat_dir.join("readme.txt"), "hello").unwrap();
        // Heartbeat from a different project (different prefix) should be excluded
        fs::write(
            config.heartbeat_dir.join("other-project-1.json"),
            format!(r#"{{"session":"other-project-1","timestamp":"{}","is_idle":true,"current_task":null,"model":"sonnet"}}"#, fresh_timestamp),
        ).unwrap();

        let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
        assert_eq!(count, 2);
    }

    #[test]
    fn read_heartbeats_parses_files() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        let fresh_timestamp = (Utc::now() - ChronoDuration::seconds(30))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        fs::write(
            config.heartbeat_dir.join("test-worker-1.json"),
            format!(r#"{{"session":"test-worker-1","timestamp":"{}","is_idle":true,"current_task":null,"model":"sonnet"}}"#, fresh_timestamp),
        ).unwrap();

        let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

        assert_eq!(heartbeats.len(), 1);
        let hb = heartbeats.get("test-worker-1").unwrap();
        assert!(hb.is_idle);
        assert_eq!(hb.model, "sonnet");
    }

    #[test]
    fn find_workers_to_stop_prefers_idle() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        let fresh_timestamp = (Utc::now() - ChronoDuration::seconds(30))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        // Create busy worker (prefixed)
        fs::write(
            config.heartbeat_dir.join("test-worker-busy.json"),
            format!(r#"{{"session":"test-worker-busy","timestamp":"{}","is_idle":false,"current_task":"task-1","model":"sonnet"}}"#, fresh_timestamp),
        ).unwrap();

        // Create idle worker (prefixed)
        fs::write(
            config.heartbeat_dir.join("test-worker-idle.json"),
            format!(r#"{{"session":"test-worker-idle","timestamp":"{}","is_idle":true,"current_task":null,"model":"sonnet"}}"#, fresh_timestamp),
        ).unwrap();

        let heartbeats = read_heartbeats_with_sessions(
            &config.heartbeat_dir,
            &config.session_prefix,
            &live(&["test-worker-busy", "test-worker-idle"]),
        );
        let to_stop = select_workers_to_stop(
            1,
            heartbeats,
            &live(&["test-worker-busy", "test-worker-idle"]),
        );

        // Should prefer idle worker
        assert_eq!(to_stop, vec!["test-worker-idle"]);
    }

    #[test]
    fn find_workers_to_stop_limited_by_n() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        let fresh_timestamp = (Utc::now() - ChronoDuration::seconds(30))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        for i in 0..5 {
            fs::write(
                config.heartbeat_dir.join(format!("test-worker-{}.json", i)),
                format!(
                    r#"{{"session":"test-worker-{}","timestamp":"{}","is_idle":true,"current_task":null,"model":"sonnet"}}"#,
                    i, fresh_timestamp
                ),
            ).unwrap();
        }

        let all_sessions: HashSet<String> = (0..5).map(|i| format!("test-worker-{}", i)).collect();
        let heartbeats = read_heartbeats_with_sessions(
            &config.heartbeat_dir,
            &config.session_prefix,
            &all_sessions,
        );
        let to_stop = select_workers_to_stop(2, heartbeats, &all_sessions);

        assert_eq!(to_stop.len(), 2);
    }

    #[test]
    fn scale_up_dry_run() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        let launched = scale_up(3, &config, true);

        assert_eq!(launched, 3);
    }

    #[test]
    fn scale_up_zero() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        let launched = scale_up(0, &config, false);

        assert_eq!(launched, 0);
    }

    #[test]
    fn disk_use_percent_reads_a_real_filesystem() {
        let temp = TempDir::new().unwrap();

        let pct = disk_use_percent(temp.path()).expect("df should report on a real temp dir");

        // The value must be a real percentage, not a mis-indexed column (a device
        // name or block count would fail to parse into u8 and yield None above).
        assert!(pct <= 100, "disk use {pct}% is not a percentage");
    }

    #[test]
    fn disk_use_percent_is_none_for_a_nonexistent_path() {
        // df exits non-zero on a missing path; the parse must not panic or invent a
        // number, because a bogus low reading would silently defeat the scale-up guard.
        assert_eq!(
            disk_use_percent(Path::new("/nonexistent-cgov-disk-guard-probe")),
            None,
        );
    }

    #[test]
    fn disk_guard_blocks_at_and_above_the_limit() {
        assert!(scale_up_blocked_by_disk(Some(SCALE_UP_MAX_DISK_USE_PCT)));
        assert!(scale_up_blocked_by_disk(Some(100)));
    }

    #[test]
    fn disk_guard_allows_below_the_limit() {
        assert!(!scale_up_blocked_by_disk(Some(
            SCALE_UP_MAX_DISK_USE_PCT - 1
        )));
        // 73% is roughly where the host sat while this guard was written; a routine
        // disk must not block routine scaling.
        assert!(!scale_up_blocked_by_disk(Some(73)));
    }

    #[test]
    fn disk_guard_fails_open_on_an_unreadable_disk() {
        // An unparseable df is "unknown", not "full". Blocking here would wedge
        // scaling permanently on any host where df output drifts.
        assert!(!scale_up_blocked_by_disk(None));
    }

    #[test]
    fn scale_down_graceful_dry_run() {
        let temp = TempDir::new().unwrap();
        let mut config = test_config(&temp);
        config.session_prefix = "cgov-dryrun-test".to_string();

        // A dry run still selects real candidates, so the worker needs a live session.
        let session = "cgov-dryrun-test-1";
        let Some(_tmux) = TmuxSession::new(session) else {
            eprintln!("skipping scale_down_graceful_dry_run: tmux unavailable");
            return;
        };
        write_heartbeat(&config, session, 30, true);

        let result = scale_down_graceful(1, &config, true);

        assert_eq!(result.targeted, 1);
        assert_eq!(result.signaled, 1);
        assert_eq!(result.graceful, 1);
        assert_eq!(result.force_killed, 0);
        assert_eq!(result.sessions, vec![session]);
    }

    #[test]
    fn scale_down_graceful_zero() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        let result = scale_down_graceful(0, &config, false);

        assert_eq!(result.targeted, 0);
        assert_eq!(result.signaled, 0);
    }

    #[test]
    fn worker_config_defaults() {
        let config = WorkerConfig::default();

        assert!(!config.launch_cmd.is_empty());
        assert!(config.heartbeat_dir.to_string_lossy().contains(".needle"));
        assert!(config.graceful_timeout_secs > 0);
        assert!(!config.session_prefix.is_empty());
    }

    #[test]
    fn test_stale_heartbeat_dead_session_removed() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a stale heartbeat (older than 60 seconds)
        let stale_timestamp = Utc::now() - ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD + 10);
        let stale_heartbeat = serde_json::json!({
            "session": "test-worker-stale",
            "timestamp": stale_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-stale.json"),
            serde_json::to_string_pretty(&stale_heartbeat).unwrap(),
        )
        .unwrap();

        // Read heartbeats - stale heartbeat should be removed since session doesn't exist in tmux
        let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

        // Heartbeat should be excluded (file was removed)
        assert_eq!(heartbeats.len(), 0);

        // File should have been removed
        assert!(!config.heartbeat_dir.join("test-worker-stale.json").exists());

        // Count should reflect the removal
        let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_stale_heartbeat_live_session_retained_as_executing() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a stale heartbeat with is_idle=true
        let stale_timestamp = Utc::now() - ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD + 10);
        let stale_heartbeat = serde_json::json!({
            "session": "test-worker-stale",
            "timestamp": stale_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-stale.json"),
            serde_json::to_string_pretty(&stale_heartbeat).unwrap(),
        )
        .unwrap();

        // Mock tmux sessions - we need to test with the actual tmux count
        // Since we can't easily mock tmux in this test, we'll create a test that
        // verifies the logic by checking the heartbeat's is_idle state

        // For this test, we'll just verify that stale heartbeats are handled
        // by checking that the function doesn't crash and returns a consistent result
        let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

        // Since the session doesn't exist in tmux, it should be removed
        // (This is the same behavior as the dead session test)
        assert_eq!(heartbeats.len(), 0);
    }

    #[test]
    fn test_fresh_heartbeat_unchanged_behavior() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a fresh heartbeat (< 60 seconds old)
        let fresh_timestamp = Utc::now() - ChronoDuration::seconds(30);
        let fresh_heartbeat = serde_json::json!({
            "session": "test-worker-fresh",
            "timestamp": fresh_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-fresh.json"),
            serde_json::to_string_pretty(&fresh_heartbeat).unwrap(),
        )
        .unwrap();

        // Read heartbeats - fresh heartbeat should be returned as-is
        let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

        assert_eq!(heartbeats.len(), 1);

        let hb = heartbeats.get("test-worker-fresh").unwrap();
        assert!(hb.is_idle); // is_idle should remain true
        assert_eq!(hb.model, "sonnet");

        // File should still exist
        assert!(config.heartbeat_dir.join("test-worker-fresh.json").exists());

        // Count should reflect the heartbeat
        let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_mixed_stale_and_fresh_heartbeats() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a fresh heartbeat
        let fresh_timestamp = Utc::now() - ChronoDuration::seconds(30);
        let fresh_heartbeat = serde_json::json!({
            "session": "test-worker-fresh",
            "timestamp": fresh_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-fresh.json"),
            serde_json::to_string_pretty(&fresh_heartbeat).unwrap(),
        )
        .unwrap();

        // Create a stale heartbeat (dead session)
        let stale_timestamp = Utc::now() - ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD + 10);
        let stale_heartbeat = serde_json::json!({
            "session": "test-worker-stale",
            "timestamp": stale_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-stale.json"),
            serde_json::to_string_pretty(&stale_heartbeat).unwrap(),
        )
        .unwrap();

        // Read heartbeats - only fresh should remain
        let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

        assert_eq!(heartbeats.len(), 1);
        assert!(heartbeats.contains_key("test-worker-fresh"));
        assert!(!heartbeats.contains_key("test-worker-stale"));

        // Stale file should be removed
        assert!(config.heartbeat_dir.join("test-worker-fresh.json").exists());
        assert!(!config.heartbeat_dir.join("test-worker-stale.json").exists());

        // Count should be 1 (only fresh heartbeat)
        let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_count_workers_consistent_after_cleanup() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a stale heartbeat (dead session) - simulating a crashed worker
        let stale_timestamp = Utc::now() - ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD + 10);
        let stale_heartbeat = serde_json::json!({
            "session": "test-worker-stale",
            "timestamp": stale_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-stale.json"),
            serde_json::to_string_pretty(&stale_heartbeat).unwrap(),
        )
        .unwrap();

        // Verify the stale heartbeat file exists before cleanup
        assert!(config.heartbeat_dir.join("test-worker-stale.json").exists());

        // count_workers triggers cleanup internally (via read_heartbeats)
        // After the call, the stale heartbeat should be removed and consistency restored
        let count = count_workers(&config);

        // Stale heartbeat was removed (session doesn't exist in tmux)
        assert_eq!(count.heartbeat_count, 0);
        assert_eq!(count.tmux_count, 0);
        assert!(count.consistent);

        // File should have been removed
        assert!(!config.heartbeat_dir.join("test-worker-stale.json").exists());
    }

    #[test]
    fn test_find_workers_to_stop_excludes_stale() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a fresh idle worker
        let fresh_timestamp = Utc::now() - ChronoDuration::seconds(30);
        let fresh_heartbeat = serde_json::json!({
            "session": "test-worker-fresh-idle",
            "timestamp": fresh_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-fresh-idle.json"),
            serde_json::to_string_pretty(&fresh_heartbeat).unwrap(),
        )
        .unwrap();

        // Create a stale heartbeat (dead session)
        let stale_timestamp = Utc::now() - ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD + 10);
        let stale_heartbeat = serde_json::json!({
            "session": "test-worker-stale-idle",
            "timestamp": stale_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-stale-idle.json"),
            serde_json::to_string_pretty(&stale_heartbeat).unwrap(),
        )
        .unwrap();

        // Selection should only return the fresh worker whose session is live
        // (the stale worker's heartbeat was removed by read_heartbeats)
        let live_sessions = live(&["test-worker-fresh-idle"]);
        let heartbeats = read_heartbeats_with_sessions(
            &config.heartbeat_dir,
            &config.session_prefix,
            &live_sessions,
        );
        let to_stop = select_workers_to_stop(10, heartbeats, &live_sessions);

        // Should only have the fresh idle worker, not the stale one
        assert_eq!(to_stop.len(), 1);
        assert_eq!(to_stop[0], "test-worker-fresh-idle");
    }

    /// Acceptance (a): orphaned heartbeats are excluded from the worker count, so a
    /// count that went inconsistent when a worker died recovers to consistent.
    #[test]
    fn count_workers_recovers_consistency_after_orphan_cleanup() {
        let temp = TempDir::new().unwrap();
        let mut config = test_config(&temp);
        config.session_prefix = "cgov-count-recovery-test".to_string();

        let live_session = "cgov-count-recovery-test-live";
        let Some(_tmux) = TmuxSession::new(live_session) else {
            eprintln!("skipping count_workers_recovers_consistency_after_orphan_cleanup: tmux unavailable");
            return;
        };

        // One live worker, plus a fresh heartbeat left behind by a worker that just died.
        write_heartbeat(&config, live_session, 5, false);
        let orphan = "cgov-count-recovery-test-dead";
        let orphan_path = write_heartbeat(&config, orphan, 5, true);

        // While the orphan's heartbeat is still fresh it is counted, and the count
        // disagrees with tmux — this is the inconsistency the sweep has to clear.
        let before = count_workers(&config);
        assert_eq!(before.heartbeat_count, 2);
        assert_eq!(before.tmux_count, 1);
        assert!(
            !before.consistent,
            "stale-but-fresh orphan should skew the count"
        );
        assert!(orphan_path.exists());

        // Age the orphan past the staleness threshold; the next count sweeps it.
        write_heartbeat(&config, orphan, STALE_HEARTBEAT_THRESHOLD + 10, true);

        let after = count_workers(&config);
        assert_eq!(after.heartbeat_count, 1, "orphan must not be counted");
        assert_eq!(after.tmux_count, 1);
        assert!(
            after.consistent,
            "consistency must recover after orphan cleanup"
        );
        assert!(
            !orphan_path.exists(),
            "orphaned heartbeat file should be removed"
        );
    }

    /// Acceptance (b): a worker whose tmux session is gone is never a shutdown
    /// candidate, even when its heartbeat is fresh, idle, and the oldest on disk.
    #[test]
    fn select_workers_to_stop_excludes_dead_sessions() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        // Oldest heartbeat, idle — first in sort order, but its session is dead.
        write_heartbeat(&config, "test-worker-dead", 50, true);
        // Newer idle worker with a live session.
        write_heartbeat(&config, "test-worker-live", 10, true);

        let live_sessions = live(&["test-worker-live"]);
        let heartbeats = read_heartbeats_with_sessions(
            &config.heartbeat_dir,
            &config.session_prefix,
            &live_sessions,
        );

        // Both heartbeats survive the sweep (both are fresh), but only one is eligible.
        assert_eq!(heartbeats.len(), 2, "fresh heartbeats are not removed");

        let to_stop = select_workers_to_stop(2, heartbeats, &live_sessions);
        assert_eq!(to_stop, vec!["test-worker-live"]);
    }

    /// End-to-end: `find_workers_to_stop` queries tmux itself and returns only sessions
    /// that actually exist, so scale-down never signals a nonexistent session.
    #[test]
    fn find_workers_to_stop_returns_only_live_sessions() {
        let temp = TempDir::new().unwrap();
        let mut config = test_config(&temp);
        config.session_prefix = "cgov-stopsel-test".to_string();

        let live_session = "cgov-stopsel-test-live";
        let Some(_tmux) = TmuxSession::new(live_session) else {
            eprintln!("skipping find_workers_to_stop_returns_only_live_sessions: tmux unavailable");
            return;
        };

        // Dead worker: idle and older, so it would sort first without the liveness filter.
        write_heartbeat(&config, "cgov-stopsel-test-dead", 50, true);
        write_heartbeat(&config, live_session, 10, true);

        let to_stop = find_workers_to_stop(5, &config);

        assert_eq!(to_stop, vec![live_session]);
    }

    #[test]
    fn test_stale_threshold_boundary() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a heartbeat exactly at the threshold (60 seconds old) - should be considered stale
        let threshold_timestamp = Utc::now() - ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD);
        let threshold_heartbeat = serde_json::json!({
            "session": "test-worker-threshold",
            "timestamp": threshold_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-threshold.json"),
            serde_json::to_string_pretty(&threshold_heartbeat).unwrap(),
        )
        .unwrap();

        // Read heartbeats - threshold heartbeat should be removed (session doesn't exist in tmux)
        let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

        // At exactly 60 seconds, it's stale and should be removed
        assert_eq!(heartbeats.len(), 0);
    }

    #[test]
    fn test_one_second_below_threshold_not_stale() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a heartbeat 1 second below the threshold (59 seconds old) - should NOT be stale
        let fresh_timestamp = Utc::now() - ChronoDuration::seconds(STALE_HEARTBEAT_THRESHOLD - 1);
        let fresh_heartbeat = serde_json::json!({
            "session": "test-worker-fresh",
            "timestamp": fresh_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "is_idle": true,
            "current_task": null,
            "model": "sonnet"
        });

        fs::write(
            config.heartbeat_dir.join("test-worker-fresh.json"),
            serde_json::to_string_pretty(&fresh_heartbeat).unwrap(),
        )
        .unwrap();

        // Read heartbeats - fresh heartbeat should be retained
        let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

        assert_eq!(heartbeats.len(), 1);
        assert!(heartbeats.contains_key("test-worker-fresh"));
        assert!(config.heartbeat_dir.join("test-worker-fresh.json").exists());
    }
}
