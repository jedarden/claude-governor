//! Worker attribution — resolve live Claude Code sessions to NEEDLE workers.
//!
//! The collector writes one instance record per Claude Code session per pass,
//! but `sess` carries the CC session UUID (not the worker name its doc comment
//! used to claim), so nothing could say whose work produced the tokens. This
//! module maps a session UUID to the needle worker currently running it:
//!
//! 1. A needle-dispatched `claude --print` process holds a
//!    `/tmp/claude-<uid>/<munged-cwd>/<session-uuid>/tasks` fd open for the
//!    life of the session. Reading `/proc/*/fd/*` therefore reveals which
//!    session UUIDs are live and which pid owns each one.
//! 2. Walking that pid's ancestry upward reaches the `needle ... run` worker
//!    process that dispatched it. Interactive (operator) sessions never have
//!    a worker in their ancestry, so they never resolve.
//! 3. The worker's session name — the string space the agent `session_pattern`
//!    globs in governor.yaml match — is `needle-{agent name}-{worker id}`
//!    (needle `src/cli/mod.rs`, dots sanitized to underscores). It is taken
//!    from the worker's heartbeat file (`qualified_id` is `{agent}-{worker_id}`)
//!    or, failing that, from the worker process cmdline (`--agent`,
//!    `--identifier`).
//!
//! A session whose process has already exited resolves to `None` and is
//! written unattributed. The scan never guesses from cwd or timing, so an
//! operator session in a repo that also hosts a live worker can never be
//! misattributed; the cost is the tail usage of a session that ends between
//! passes, which stays null instead of carrying a possibly wrong name.

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Default procfs root, overridable so tests can feed a synthetic process tree.
pub const DEFAULT_PROC_ROOT: &str = "/proc";

/// Maximum ancestry hops before giving up. A claude session is dispatched
/// through at most a couple of intermediate shells; 32 is far past any real
/// chain and bounds the walk if a (corrupt) procfs ever showed a pid cycle.
const MAX_ANCESTRY_HOPS: usize = 32;

/// Live-session → worker index for one collection pass.
///
/// Built once per pass and queried per accumulated session; an absent entry
/// means "not attributable right now" (operator session, or the worker's
/// claude process has already exited).
#[derive(Debug, Clone, Default)]
pub struct WorkerAttribution {
    /// CC session UUID → needle worker session name
    sessions: HashMap<String, String>,
}

/// The subset of a needle heartbeat file attribution needs.
///
/// Deliberately not cgov's `worker::Heartbeat`: that struct models the fields
/// worker counting reads and is strict about `session`/`timestamp`; attribution
/// only wants `pid` + `qualified_id` and must tolerate any other shape.
#[derive(Debug, Deserialize)]
struct HeartbeatDoc {
    /// Worker process pid (the `needle run` process itself)
    #[serde(default)]
    pid: Option<i64>,
    /// `{agent name}-{worker id}` — needle `src/cli/mod.rs`
    #[serde(default)]
    qualified_id: Option<String>,
}

impl WorkerAttribution {
    /// Scan the host's live process tree and needle heartbeat registry.
    pub fn scan() -> Self {
        Self::scan_at(Path::new(DEFAULT_PROC_ROOT), &default_heartbeat_dir())
    }

    /// [`scan`] against explicit procfs and heartbeat roots.
    ///
    /// Every read failure (a process exiting mid-scan, an unreadable fd dir,
    /// a missing heartbeat dir) is skipped silently — attribution is
    /// best-effort by design and must never fail a collection pass.
    pub fn scan_at(proc_root: &Path, heartbeat_dir: &Path) -> Self {
        let workers = discover_workers(proc_root, heartbeat_dir);
        let mut sessions = HashMap::new();

        for (pid, session_uuid) in tasks_fd_holders(proc_root) {
            if sessions.contains_key(&session_uuid) {
                continue;
            }
            if let Some(name) = ancestor_worker(proc_root, pid, &workers) {
                sessions.insert(session_uuid, name);
            }
        }

        Self { sessions }
    }

    /// Resolve a CC session UUID (the transcript file stem) to the needle
    /// worker session name running it, or `None` when not attributable.
    pub fn resolve(&self, session_uuid: &str) -> Option<&str> {
        self.sessions.get(session_uuid).map(String::as_str)
    }
}

/// Default needle heartbeat directory (`heartbeat_dir` in governor.yaml).
///
/// Public so [`crate::collector::CollectionPaths::default`] names the same
/// directory the scan reads; production state lives under the real home.
pub fn default_heartbeat_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".needle")
        .join("state")
        .join("heartbeats")
}

/// The worker session name needle assigns: `needle-{agent}-{worker_id}`, with
/// dots sanitized to underscores (`sanitize_session_name` in needle's cli).
///
/// `qualified_id` is needle's `{agent name}-{worker id}`, so prefixing it with
/// `needle-` reproduces the session name exactly — the same string space the
/// agent `session_pattern` globs in governor.yaml are written against
/// (e.g. `needle-claude-print-cgov-sonnet-0` matches `needle-claude-print-cgov-sonnet-*`).
fn worker_session_name(qualified_id: &str) -> String {
    format!("needle-{}", qualified_id).replace('.', "_")
}

/// Parse a `needle ... run ...` argv into `(agent name, identifier)`.
///
/// A process qualifies when its binary basename starts with `needle` and its
/// args contain the `run` subcommand. Both `--flag value` and `--flag=value`
/// forms are accepted; either field may be absent (identifier is NATO-named
/// then, and only the heartbeat can name the worker).
fn parse_needle_run_cmdline(argv: &[String]) -> Option<(Option<String>, Option<String>)> {
    let bin = Path::new(argv.first()?).file_name()?.to_str()?;
    if !bin.starts_with("needle") || !argv.iter().any(|a| a == "run") {
        return None;
    }

    let flag_value = |flag: &str| -> Option<String> {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .cloned()
            .or_else(|| {
                argv.iter()
                    .find_map(|a| a.strip_prefix(&format!("{}=", flag)).map(String::from))
            })
    };

    Some((flag_value("--agent"), flag_value("--identifier")))
}

/// Extract the CC session UUID from a `/tmp/claude-<uid>/<projdir>/<uuid>/...`
/// fd target, or `None` when the path is not a claude session tasks dir.
fn tasks_session_uuid(target: &Path) -> Option<String> {
    let parts: Vec<String> = target
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    let tmp = parts.iter().position(|p| p == "tmp")?;
    let rest = parts.get(tmp + 1..)?;
    let claude_dir = rest.first()?;
    if !claude_dir.starts_with("claude-") || claude_dir.len() <= "claude-".len() {
        return None;
    }

    let session_uuid = rest.get(2)?;
    if !looks_like_session_uuid(session_uuid) {
        return None;
    }

    Some(session_uuid.clone())
}

/// A CC session UUID: 36 chars, dashes at 8/13/18/23, hex elsewhere.
fn looks_like_session_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if *b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Live pids holding a claude session tasks fd: (pid, session UUID) pairs.
fn tasks_fd_holders(proc_root: &Path) -> Vec<(i64, String)> {
    let mut holders = Vec::new();

    for pid in numeric_entries(proc_root) {
        let fd_dir = proc_root.join(pid.to_string()).join("fd");
        let entries = match fs::read_dir(&fd_dir) {
            Ok(e) => e,
            Err(_) => continue, // process exited mid-scan, or not ours
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let target = match fs::read_link(entry.path()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if let Some(uuid) = tasks_session_uuid(&target) {
                holders.push((pid, uuid));
            }
        }
    }

    holders
}

/// Map worker pid → session name for every live `needle run` process.
///
/// The heartbeat registry supplies the authoritative name via `qualified_id`,
/// but only for pids whose cmdline still verifies as a needle run process —
/// pids are recycled, and a stale heartbeat pointing at a reused pid must not
/// adopt an unrelated process's descendants. A live worker whose heartbeat has
/// not landed yet is still named, from its own `--agent`/`--identifier`.
fn discover_workers(proc_root: &Path, heartbeat_dir: &Path) -> HashMap<i64, String> {
    // First pass: which pids are live needle run processes, and what do their
    // cmdlines say?
    let mut verified: HashMap<i64, (Option<String>, Option<String>)> = HashMap::new();
    for pid in numeric_entries(proc_root) {
        let argv = match read_cmdline(proc_root, pid) {
            Some(a) => a,
            None => continue,
        };
        if let Some(parts) = parse_needle_run_cmdline(&argv) {
            verified.insert(pid, parts);
        }
    }

    let mut workers: HashMap<i64, String> = HashMap::new();

    // Heartbeats name the worker even when no --identifier was given (NATO
    // names). Only trusted for pids verified above.
    if let Ok(entries) = fs::read_dir(heartbeat_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map(|ext| ext != "json").unwrap_or(true) {
                continue;
            }
            let doc: HeartbeatDoc =
                match serde_json::from_str(&fs::read_to_string(&path).unwrap_or_default()) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
            if let (Some(pid), Some(qualified_id)) = (doc.pid, doc.qualified_id) {
                if verified.contains_key(&pid) {
                    workers.insert(pid, worker_session_name(&qualified_id));
                }
            }
        }
    }

    // Cmdline fallback for verified workers the heartbeat registry missed.
    for (pid, (agent, identifier)) in verified {
        if workers.contains_key(&pid) {
            continue;
        }
        if let (Some(agent), Some(identifier)) = (agent, identifier) {
            workers.insert(
                pid,
                worker_session_name(&format!("{}-{}", agent, identifier)),
            );
        }
    }

    workers
}

/// Nearest ancestor (or self) of `pid` that is a known worker, by session name.
fn ancestor_worker(proc_root: &Path, pid: i64, workers: &HashMap<i64, String>) -> Option<String> {
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_HOPS {
        let parent = parent_pid(proc_root, current)?;
        if let Some(name) = workers.get(&parent) {
            return Some(name.clone());
        }
        if parent <= 1 {
            return None;
        }
        current = parent;
    }
    None
}

/// Parent pid from `/proc/<pid>/stat` (field 4, after the parenthesised comm).
fn parent_pid(proc_root: &Path, pid: i64) -> Option<i64> {
    let stat = fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let after_comm = stat.rfind(')')? + 1;
    // Fields after comm: state (3), ppid (4) — split starts at field 3.
    stat[after_comm..].split_whitespace().nth(1)?.parse().ok()
}

/// Numeric subdirectory names of `dir`, parsed as pids.
fn numeric_entries(dir: &Path) -> Vec<i64> {
    let mut pids = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if let Ok(pid) = entry.file_name().to_string_lossy().parse::<i64>() {
                pids.push(pid);
            }
        }
    }
    pids
}

/// NUL-separated `/proc/<pid>/cmdline` as argv, or `None` if unreadable.
fn read_cmdline(proc_root: &Path, pid: i64) -> Option<Vec<String>> {
    let bytes = fs::read(proc_root.join(pid.to_string()).join("cmdline")).ok()?;
    let argv: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    if argv.is_empty() {
        None // kernel threads and zombies have no cmdline
    } else {
        Some(argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    const SESSION_UUID: &str = "c9594b9f-f9f3-4c1b-8f02-2f290a3db022";
    const PROJECT_DIR: &str = "-home-coding-claude-governor";

    /// Write a synthetic /proc-style process: cmdline, stat (with ppid), and
    /// optionally one fd symlink.
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
        // stat: pid (comm) S ppid ...
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

    #[test]
    fn session_name_sanitizes_dots_like_needle() {
        // Needle's sanitize_session_name (src/cli/mod.rs) replaces ONLY dots —
        // dashes are tmux-legal and survive. Verified against needle source.
        assert_eq!(
            worker_session_name("claude-code-glm-5.3-flash-glm-cgraph"),
            "needle-claude-code-glm-5_3-flash-glm-cgraph"
        );
        assert_eq!(
            worker_session_name("claude-print-cgov-sonnet-0"),
            "needle-claude-print-cgov-sonnet-0"
        );
    }

    #[test]
    fn cmdline_parse_accepts_both_flag_forms() {
        let space_form: Vec<String> = [
            "/home/coding/.needle/bin/needle-stable",
            "run",
            "--resume",
            "--identifier",
            "glm-cgraph",
            "--count",
            "1",
            "--workspace",
            "/home/coding/claude-governor",
            "--agent",
            "claude-code-glm-5.3-flash",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (agent, identifier) = parse_needle_run_cmdline(&space_form).unwrap();
        assert_eq!(agent.as_deref(), Some("claude-code-glm-5.3-flash"));
        assert_eq!(identifier.as_deref(), Some("glm-cgraph"));

        let eq_form: Vec<String> = ["needle", "run", "--agent=claude-print", "--identifier=abc"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (agent, identifier) = parse_needle_run_cmdline(&eq_form).unwrap();
        assert_eq!(agent.as_deref(), Some("claude-print"));
        assert_eq!(identifier.as_deref(), Some("abc"));

        // Not a worker invocation
        assert!(parse_needle_run_cmdline(&["needle".to_string(), "status".to_string()]).is_none());
        assert!(parse_needle_run_cmdline(&["bash".to_string(), "run".to_string()]).is_none());
        assert!(parse_needle_run_cmdline(&[]).is_none());
    }

    #[test]
    fn tasks_uuid_extraction() {
        let tasks = format!(
            "/tmp/claude-1000/{}/1e40ad47-6155-4c14-855a-c32f721d7cce/tasks",
            PROJECT_DIR
        );
        assert_eq!(
            tasks_session_uuid(Path::new(&tasks)).as_deref(),
            Some("1e40ad47-6155-4c14-855a-c32f721d7cce")
        );

        // Not a claude tasks dir
        assert!(tasks_session_uuid(Path::new("/tmp/needle/prompt-x.md")).is_none());
        assert!(tasks_session_uuid(Path::new("/tmp/claude-1000/someproj/short/tasks")).is_none());
    }

    #[test]
    fn uuid_shape_check() {
        assert!(looks_like_session_uuid(SESSION_UUID));
        assert!(!looks_like_session_uuid("short"));
        assert!(!looks_like_session_uuid(
            "c9594b9f_f9f3_4c1b_8f02_2f290a3db022"
        ));
        assert!(!looks_like_session_uuid(
            "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz"
        ));
    }

    #[test]
    fn dispatched_session_resolves_via_heartbeat_name() {
        let tmp = TempDir::new().unwrap();
        let proc_root = tmp.path().join("proc");
        let hb_dir = tmp.path().join("heartbeats");
        fs::create_dir_all(&hb_dir).unwrap();

        // needle worker 100 -> bash 101 -> claude 102 holding the tasks fd
        write_process(
            &proc_root,
            100,
            1,
            &[
                "needle-stable",
                "run",
                "--resume",
                "--identifier",
                "cgov-sonnet-0",
            ],
            None,
        );
        write_process(
            &proc_root,
            101,
            100,
            &["bash", "-c", "cd /x && claude"],
            None,
        );
        write_process(
            &proc_root,
            102,
            101,
            &["claude", "--print", "--output-format", "stream-json"],
            Some(&format!(
                "/tmp/claude-1000/{}/{}",
                PROJECT_DIR, SESSION_UUID
            )),
        );

        // Heartbeat names worker 100 by qualified_id
        fs::write(
            hb_dir.join("claude-print-cgov-sonnet-0.json"),
            r#"{"worker_id":"cgov-sonnet-0","qualified_id":"claude-print-cgov-sonnet-0","pid":100,"session":"cgov-sonnet-0"}"#,
        )
        .unwrap();

        let attr = WorkerAttribution::scan_at(&proc_root, &hb_dir);
        assert_eq!(
            attr.resolve(SESSION_UUID),
            Some("needle-claude-print-cgov-sonnet-0")
        );
    }

    #[test]
    fn operator_session_is_never_attributed() {
        let tmp = TempDir::new().unwrap();
        let proc_root = tmp.path().join("proc");
        let hb_dir = tmp.path().join("heartbeats");

        // A live worker exists, but the claude session hangs off a plain shell
        // ancestry (sshd -> bash -> claude), not the worker.
        write_process(
            &proc_root,
            100,
            1,
            &[
                "needle-stable",
                "run",
                "--identifier",
                "glm-cgraph",
                "--agent",
                "a-b",
            ],
            None,
        );
        write_process(&proc_root, 200, 1, &["sshd"], None);
        write_process(&proc_root, 201, 200, &["bash"], None);
        write_process(
            &proc_root,
            202,
            201,
            &["claude", "--dangerously-skip-permissions"],
            Some(&format!(
                "/tmp/claude-1000/{}/{}",
                PROJECT_DIR, SESSION_UUID
            )),
        );

        let attr = WorkerAttribution::scan_at(&proc_root, &hb_dir);
        assert_eq!(attr.resolve(SESSION_UUID), None);
    }

    #[test]
    fn recycled_heartbeat_pid_is_not_trusted() {
        let tmp = TempDir::new().unwrap();
        let proc_root = tmp.path().join("proc");
        let hb_dir = tmp.path().join("heartbeats");
        fs::create_dir_all(&hb_dir).unwrap();

        // Heartbeat claims pid 300 is a worker, but pid 300 is now a shell.
        // The claude session under it must stay unattributed.
        write_process(&proc_root, 300, 1, &["bash"], None);
        write_process(
            &proc_root,
            301,
            300,
            &["claude", "--print"],
            Some(&format!(
                "/tmp/claude-1000/{}/{}",
                PROJECT_DIR, SESSION_UUID
            )),
        );
        fs::write(
            hb_dir.join("stale.json"),
            r#"{"worker_id":"ghost","qualified_id":"agent-ghost","pid":300}"#,
        )
        .unwrap();

        let attr = WorkerAttribution::scan_at(&proc_root, &hb_dir);
        assert_eq!(attr.resolve(SESSION_UUID), None);
    }

    #[test]
    fn cmdline_fallback_names_worker_without_heartbeat() {
        let tmp = TempDir::new().unwrap();
        let proc_root = tmp.path().join("proc");
        let hb_dir = tmp.path().join("heartbeats");

        write_process(
            &proc_root,
            100,
            1,
            &[
                "needle",
                "run",
                "--agent=claude-code-glm-5.3-flash",
                "--identifier=glm-cgraph",
            ],
            None,
        );
        write_process(
            &proc_root,
            102,
            100,
            &["claude", "--print"],
            Some(&format!(
                "/tmp/claude-1000/{}/{}",
                PROJECT_DIR, SESSION_UUID
            )),
        );

        let attr = WorkerAttribution::scan_at(&proc_root, &hb_dir);
        assert_eq!(
            attr.resolve(SESSION_UUID),
            Some("needle-claude-code-glm-5_3-flash-glm-cgraph")
        );
    }

    #[test]
    fn missing_proc_root_yields_empty_index() {
        let tmp = TempDir::new().unwrap();
        let attr = WorkerAttribution::scan_at(&tmp.path().join("no-proc"), &tmp.path().join("hb"));
        assert_eq!(attr.resolve(SESSION_UUID), None);
    }
}
