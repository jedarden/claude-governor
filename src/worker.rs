//! Worker Management - scaling operations for Claude Code agents
//!
//! This module handles:
//! - `scale_up(n)`: launch new agent workers via shell command
//! - `scale_down_graceful(n)`: find idle workers, send SIGINT via tmux, kill after timeout
//! - `count_workers()`: verify worker count via heartbeat files + tmux sessions

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
    let check_interval = Duration::from_secs(2);
    let mut elapsed = Duration::ZERO;
    let timeout = Duration::from_secs(config.graceful_timeout_secs);

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
fn find_workers_to_stop(n: usize, config: &WorkerConfig) -> Vec<String> {
    let heartbeats = read_heartbeats(&config.heartbeat_dir, &config.session_prefix);

    // Sort workers: idle first, then by heartbeat age (oldest first)
    let mut workers: Vec<_> = heartbeats.into_iter().collect();
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
fn read_heartbeats(dir: &Path, session_prefix: &str) -> HashMap<String, Heartbeat> {
    let mut heartbeats = HashMap::new();

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
                Ok(hb) => {
                    if hb.session.starts_with(session_prefix) {
                        heartbeats.insert(hb.session.clone(), hb);
                    }
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

        // Create heartbeat files whose sessions match the prefix "test-worker"
        fs::write(
            config.heartbeat_dir.join("test-worker-1.json"),
            r#"{"session":"test-worker-1","timestamp":"2026-03-20T10:00:00Z","is_idle":true,"current_task":null,"model":"sonnet"}"#,
        ).unwrap();
        fs::write(
            config.heartbeat_dir.join("test-worker-2.json"),
            r#"{"session":"test-worker-2","timestamp":"2026-03-20T10:00:00Z","is_idle":false,"current_task":"task-123","model":"sonnet"}"#,
        ).unwrap();
        // Non-JSON file should be ignored
        fs::write(config.heartbeat_dir.join("readme.txt"), "hello").unwrap();
        // Heartbeat from a different project (different prefix) should be excluded
        fs::write(
            config.heartbeat_dir.join("other-project-1.json"),
            r#"{"session":"other-project-1","timestamp":"2026-03-20T10:00:00Z","is_idle":true,"current_task":null,"model":"sonnet"}"#,
        ).unwrap();

        let count = count_heartbeat_files(&config.heartbeat_dir, &config.session_prefix);
        assert_eq!(count, 2);
    }

    #[test]
    fn read_heartbeats_parses_files() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        fs::write(
            config.heartbeat_dir.join("test-worker-1.json"),
            r#"{"session":"test-worker-1","timestamp":"2026-03-20T10:00:00Z","is_idle":true,"current_task":null,"model":"sonnet"}"#,
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

        // Create busy worker (prefixed)
        fs::write(
            config.heartbeat_dir.join("test-worker-busy.json"),
            r#"{"session":"test-worker-busy","timestamp":"2026-03-20T10:00:00Z","is_idle":false,"current_task":"task-1","model":"sonnet"}"#,
        ).unwrap();

        // Create idle worker (prefixed)
        fs::write(
            config.heartbeat_dir.join("test-worker-idle.json"),
            r#"{"session":"test-worker-idle","timestamp":"2026-03-20T10:00:00Z","is_idle":true,"current_task":null,"model":"sonnet"}"#,
        ).unwrap();

        let to_stop = find_workers_to_stop(1, &config);

        // Should prefer idle worker
        assert_eq!(to_stop, vec!["test-worker-idle"]);
    }

    #[test]
    fn find_workers_to_stop_limited_by_n() {
        let temp = TempDir::new().unwrap();
        let config = test_config(&temp);

        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        for i in 0..5 {
            fs::write(
                config.heartbeat_dir.join(format!("test-worker-{}.json", i)),
                format!(
                    r#"{{"session":"test-worker-{}","timestamp":"2026-03-20T10:00:00Z","is_idle":true,"current_task":null,"model":"sonnet"}}"#,
                    i
                ),
            ).unwrap();
        }

        let to_stop = find_workers_to_stop(2, &config);

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
    fn scale_down_graceful_dry_run() {
        let temp = TempDir::new().unwrap();
        let mut config = test_config(&temp);
        config.heartbeat_dir = temp.path().join("heartbeats");
        fs::create_dir_all(&config.heartbeat_dir).unwrap();

        // Create a heartbeat file
        fs::write(
            config.heartbeat_dir.join("test-worker-1.json"),
            r#"{"session":"test-worker-1","timestamp":"2026-03-20T10:00:00Z","is_idle":true,"current_task":null,"model":"sonnet"}"#,
        ).unwrap();

        let result = scale_down_graceful(1, &config, true);

        assert_eq!(result.targeted, 1);
        assert_eq!(result.signaled, 1);
        assert_eq!(result.graceful, 1);
        assert_eq!(result.force_killed, 0);
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
}
