//! Authoritative verification of the installed claude-print NEEDLE adapters.
//!
//! `needle test-agent` is not sufficient proof an adapter works: it resolves
//! `agent_cli` through PATH, while dispatch runs the template's
//! `invoke_template`. An adapter naming a missing binary still reports READY
//! while every dispatch dies at exit 127 (NEEDLE bead `needle-adef2ccd`), and
//! two further historically severe failure classes live in the template's own
//! `unset` list (claude-governor CLAUDE.md adapter rules 3 and 5):
//!
//! - **inherited IDE env** (`CLAUDECODE`, `VSCODE_*`): the child attaches to
//!   the VS Code extension host over loopback instead of ever reaching the
//!   API, and blocks there until `timeout_secs` (46% of dispatches launched
//!   from an interactive shell)
//! - **inherited API-routing env** (`ANTHROPIC_BASE_URL`/`AUTH_TOKEN`/
//!   `MODEL`, `CLAUDE_CODE_SUBAGENT_MODEL`): the session hangs at init until
//!   the watchdog kills it, or answers while billing a proxy pool instead of
//!   the subscription
//!
//! A third class is invisible to a live probe: a template that drops the
//! prompt redirection, `--pretrust-cwd`, `--output-format stream-json` or
//! `--no-inherit-hooks` still answers a trivial prompt with plain output and
//! exit 0 — it only fails in production dispatch, where the output transform
//! needs stream-json, a never-trusted workspace needs the pretrust flag, the
//! strand must not inherit the invoking shell's hooks, and the prompt must
//! actually reach claude-print. The per-adapter `timeout_secs` backstop
//! belongs to the same class: NEEDLE's stuck-detection is blind during a
//! run, so `timeout_secs` is the only thing that kills a hung strand.
//!
//! This module mechanises the checks CLAUDE.md previously prescribed by hand:
//! static checks that each installed template still unsets both variable
//! sets, still carries every required invoke flag, and still pins
//! `timeout_secs` to its documented per-adapter value — plus a live run of
//! the template's `invoke_template` verbatim with a trivial prompt,
//! requiring exit 0. Shared by `cgov doctor` (check `claude_print_adapters`)
//! and `deploy/install-claude-print-adapters.sh`, which mirrors the same
//! constants in bash.
//!
//! The bash mirror is enforced, not trusted: `cargo test` parses of the
//! installer's arrays fail on divergence from the constants below
//! (`installer_bash_variable_lists_match_the_rust_constants`,
//! `installer_bash_flag_and_timeout_pins_match_the_rust_constants`), and the
//! installer cross-checks these same constants at run time. A constant added
//! to one copy without the other therefore fails loudly in both places,
//! instead of silently shrinking the check to exactly the variables that
//! caused the incidents. New variables, flags and timeout pins go into both
//! lists together.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Adapter rule 3: inherited IDE env makes the child attach to the editor's
/// extension host over loopback instead of reaching the API.
pub const IDE_ENV_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_SSE_PORT",
    "VSCODE_IPC_HOOK_CLI",
    "VSCODE_GIT_IPC_HANDLE",
    "VSCODE_GIT_ASKPASS_NODE",
    "VSCODE_GIT_ASKPASS_MAIN",
    "VSCODE_GIT_ASKPASS_EXTRA_ARGS",
    "VSCODE_INJECTION",
    "VSCODE_NONCE",
    "VSCODE_PID",
    "VSCODE_CWD",
];

/// Adapter rule 5: inherited API-routing env sends the session to a proxy
/// pool (or nowhere) instead of the subscription endpoint.
pub const API_ROUTING_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];

/// The substrings every `invoke_template` must carry — the parts of the
/// dispatch contract CLAUDE.md §2 learned the hard way, and that a trivial
/// live probe cannot see (a plain-text answer exits 0 with none of them):
///
/// - `< {prompt_file}` — rule 1. Without the redirection claude-print
///   launches with no prompt, produces nothing and exits instantly, while
///   NEEDLE's re-dispatch loop churns real beads into stuck `in_progress`.
/// - `--pretrust-cwd` — rule 4. Without it, a claude that has never trusted
///   the workspace renders its trust dialog with the *refusing* option
///   highlighted; claude-print confirms whatever is highlighted and the
///   strand dies as a misleading `internal_error` before doing any work.
/// - `--output-format stream-json` — `needle-transform-claude` parses this
///   format and nothing else.
/// - `--no-inherit-hooks` — isolation: the dispatched strand must not run
///   the invoking shell's hooks (claude-print still installs its own Stop
///   hook).
///
/// Matched as substrings on purpose: the template is a one-line shell
/// command, and a rewrite that no longer contains the exact flag text is
/// precisely the drift this gate exists to stop.
pub const REQUIRED_INVOKE_FLAGS: &[&str] = &[
    "< {prompt_file}",
    "--pretrust-cwd",
    "--output-format stream-json",
    "--no-inherit-hooks",
];

/// The exact `timeout_secs` each adapter must pin, by adapter name — the
/// tight backstops CLAUDE.md §2 records (opus 1200s, fable 600s). Equality,
/// not a ceiling: a silently raised value is a worker wedged that much
/// longer on a hung strand, and a silently lowered one starts killing
/// legitimate strands, so changing a pin must be a deliberate act — edit
/// both copies (this constant and `ADAPTER_TIMEOUT_PINS` in
/// `deploy/install-claude-print-adapters.sh`); the sync gates fail until
/// they agree. An adapter whose name has no pin here fails the check loudly
/// rather than inheriting one.
pub const ADAPTER_TIMEOUT_PINS: &[(&str, u64)] = &[
    ("claude-print-fable", 600),
    ("claude-print-opus", 1200),
];

/// The canonical adapter templates this build ships, embedded at compile
/// time as `(filename, bytes)` pairs from `deploy/needle-adapters/`.
///
/// `cgov doctor`'s parity check compares the adapters installed on whatever
/// host it runs on against these bytes, so drift on a second host — a
/// hand-edited template under `~/.config/needle/adapters/`, or an install
/// older than the repo's latest rule change — fails doctor there instead of
/// surfacing as a dispatch failure (see
/// `docs/notes/second-host-provisioning.md`). Cargo tracks include_str!
/// files as rebuild inputs, so a template edit rebuilds every binary that
/// carries it and the comparison is always against the exact surface this
/// binary was built from — the same embedded-not-read rule the drift gates
/// below follow for the same shared-close-gate-cache reason.
pub const CANONICAL_ADAPTERS: &[(&str, &str)] = &[
    (
        "claude-print-fable.yaml",
        include_str!("../deploy/needle-adapters/claude-print-fable.yaml"),
    ),
    (
        "claude-print-opus.yaml",
        include_str!("../deploy/needle-adapters/claude-print-opus.yaml"),
    ),
];

/// Upper bound on the live probe. A trivial prompt normally finishes in well
/// under a minute; the cap only bites on a hung dispatch, which is exactly
/// the failure this probe exists to surface.
pub const LIVE_PROBE_TIMEOUT_SECS: u64 = 240;

/// The trivial prompt the live probe sends. One model turn, no tools.
pub const PROBE_PROMPT: &str = "Adapter verification probe from cgov: reply with the single word OK and nothing else. Do not use any tools.";

/// Where the probe prompt and session live. A stable directory (not a fresh
/// temp dir per run) so `--pretrust-cwd` writes its trust entry once.
pub fn probe_workspace() -> Result<PathBuf, String> {
    let dir = dirs::cache_dir()
        .ok_or_else(|| "cannot determine the cache directory".to_string())?
        .join("claude-print-adapter-verify");
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    Ok(dir)
}

/// The live NEEDLE adapters directory dispatch actually reads.
pub fn default_adapters_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("needle")
        .join("adapters")
}

/// The adapter fields the verification needs. Unknown YAML keys are ignored on
/// purpose: the adapters carry dispatch settings (provider, cost, …) that are
/// NEEDLE's business, not ours.
#[derive(Debug, Clone, Deserialize)]
pub struct AdapterYaml {
    pub name: String,
    #[serde(default)]
    pub invoke_template: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct InstalledAdapter {
    pub path: PathBuf,
    pub adapter: AdapterYaml,
}

/// Load every `claude-print-*.yaml` adapter from `dir`, sorted by filename.
///
/// `Err` when the directory is missing/unreadable or a template does not
/// parse — a half-written adapter must fail the check loudly rather than be
/// skipped, because dispatch would read that same broken file.
pub fn load_installed_adapters(dir: &Path) -> Result<Vec<InstalledAdapter>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return Err(format!("cannot read {}: {}", dir.display(), e)),
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("claude-print-") && n.ends_with(".yaml"))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();

    let mut adapters = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let adapter: AdapterYaml = serde_yaml::from_str(&raw)
            .map_err(|e| format!("cannot parse {}: {}", path.display(), e))?;
        if adapter.invoke_template.trim().is_empty() {
            return Err(format!("{} has no invoke_template", path.display()));
        }
        adapters.push(InstalledAdapter { path, adapter });
    }
    Ok(adapters)
}

// ---------------------------------------------------------------------------
// Host parity — installed surface vs the canonical deployment this build ships
// ---------------------------------------------------------------------------

/// One drift finding between a host's installed claude-print surface and the
/// canonical deployment the running cgov build embeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParityDrift {
    /// A canonical adapter has no installed counterpart on this host.
    MissingAdapter {
        filename: String,
    },
    /// The installed adapter's bytes differ from the canonical template — a
    /// hand-edit under the live adapters directory, or an install older or
    /// newer than this cgov build.
    DivergedAdapter {
        filename: String,
        installed_bytes: usize,
        canonical_bytes: usize,
    },
    /// An absolute claude-print path the canonical templates call is absent
    /// or not executable on this host — every dispatch through it dies at
    /// exit 127 while `needle test-agent` still reports READY.
    BinaryPathMissing {
        path: String,
    },
}

impl std::fmt::Display for ParityDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParityDrift::MissingAdapter { filename } => {
                write!(f, "{filename} is not installed")
            }
            ParityDrift::DivergedAdapter {
                filename,
                installed_bytes,
                canonical_bytes,
            } => write!(
                f,
                "{filename} differs from the canonical template \
                 ({installed_bytes} bytes installed vs {canonical_bytes} canonical)"
            ),
            ParityDrift::BinaryPathMissing { path } => write!(
                f,
                "{path} is missing or not executable — dispatch through it dies at exit 127"
            ),
        }
    }
}

/// The absolute claude-print paths one template calls: whitespace-delimited
/// tokens starting with `/` and ending in `/claude-print`, the installer's
/// own `grep -ho '/[^ ]*/claude-print'` rule. The templates call the binary
/// by absolute path on purpose (NEEDLE's dispatch shell PATH is not the
/// interactive shell's), and the installer is what establishes that path —
/// normally a symlink to the real binary — so a host where the path is gone
/// is a host where dispatch dies at exit 127. The bare `claude-print` tokens
/// (`agent_cli`, `version_command`) do not start with `/` and never match.
fn absolute_binary_paths_in(template: &str) -> Vec<String> {
    template
        .split_whitespace()
        .filter(|t| t.starts_with('/') && t.ends_with("/claude-print"))
        .map(str::to_string)
        .collect()
}

/// The absolute claude-print paths the canonical templates call, deduplicated
/// in first-seen order.
pub fn canonical_binary_paths() -> Vec<String> {
    let mut paths = Vec::new();
    for (_, template) in CANONICAL_ADAPTERS {
        for path in absolute_binary_paths_in(template) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// Is `path` an existing, executable file?
fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
    }
}

/// Compare a host's installed claude-print surface against the canonical
/// deployment the running build embeds: every canonical adapter must be
/// installed byte-identical, and every absolute binary path the canonical
/// templates call must exist and be executable here.
///
/// `Err` only when a file that should be comparable cannot be read at all
/// (e.g. permissions) — the caller reports that separately from drift. A
/// missing directory or a missing adapter is drift, not an error: an empty
/// directory comes back as every canonical adapter missing.
pub fn host_parity_drift(
    dir: &Path,
    binary_paths: &[String],
) -> Result<Vec<ParityDrift>, String> {
    let mut drift = Vec::new();

    for (filename, canonical) in CANONICAL_ADAPTERS {
        let installed_path = dir.join(filename);
        match fs::read(&installed_path) {
            Ok(installed) => {
                if installed != canonical.as_bytes() {
                    drift.push(ParityDrift::DivergedAdapter {
                        filename: (*filename).to_string(),
                        installed_bytes: installed.len(),
                        canonical_bytes: canonical.len(),
                    });
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                drift.push(ParityDrift::MissingAdapter {
                    filename: (*filename).to_string(),
                });
            }
            Err(e) => {
                return Err(format!("cannot read {}: {}", installed_path.display(), e));
            }
        }
    }

    for path in binary_paths {
        if !is_executable_file(Path::new(path)) {
            drift.push(ParityDrift::BinaryPathMissing { path: path.clone() });
        }
    }

    Ok(drift)
}

/// Which documented failure class a required variable belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvClass {
    /// Adapter rule 3.
    Ide,
    /// Adapter rule 5.
    ApiRouting,
}

impl std::fmt::Display for EnvClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvClass::Ide => write!(f, "inherited IDE env"),
            EnvClass::ApiRouting => write!(f, "inherited API-routing env"),
        }
    }
}

/// The variables an `unset` segment of the template actually scrubs.
///
/// The templates are flat one-line shell (`cd {workspace} && unset V1 V2 … &&
/// /path/to/claude-print …`), so splitting on `&&` and reading the segments
/// that *begin* with `unset` is exact for the documented shape. A variable
/// only mentioned elsewhere (e.g. in a flag value) does not count.
pub fn scrubbed_vars(invoke_template: &str) -> BTreeSet<String> {
    invoke_template
        .split("&&")
        .filter_map(|segment| {
            let mut tokens = segment.split_whitespace();
            let first = tokens
                .next()?
                .trim_start_matches('"')
                .trim_start_matches('\'');
            if first == "unset" {
                Some(tokens)
            } else {
                None
            }
        })
        .flat_map(|tokens| {
            tokens.map(|t| {
                t.trim_matches('"')
                    .trim_matches('\'')
                    .trim_matches(',')
                    .to_string()
            })
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Required rule-3/rule-5 variables the template does NOT unset, with the
/// failure class each one belongs to. Empty means the static scrub check
/// passes.
pub fn missing_scrub_vars(invoke_template: &str) -> Vec<(EnvClass, &'static str)> {
    let scrubbed = scrubbed_vars(invoke_template);
    IDE_ENV_VARS
        .iter()
        .map(|v| (EnvClass::Ide, *v))
        .chain(
            API_ROUTING_ENV_VARS
                .iter()
                .map(|v| (EnvClass::ApiRouting, *v)),
        )
        .filter(|(_, var)| !scrubbed.contains(*var))
        .collect()
}

/// Required invoke flags the template does NOT carry. Substring match — see
/// [`REQUIRED_INVOKE_FLAGS`] for why that is exact here rather than loose.
pub fn missing_invoke_flags(invoke_template: &str) -> Vec<&'static str> {
    REQUIRED_INVOKE_FLAGS
        .iter()
        .copied()
        .filter(|flag| !invoke_template.contains(flag))
        .collect()
}

/// Why the adapter's `timeout_secs` violates its pin, if it does. `None`
/// means the value matches [`ADAPTER_TIMEOUT_PINS`] exactly.
pub fn timeout_violation(adapter_name: &str, timeout_secs: Option<u64>) -> Option<String> {
    let pinned = ADAPTER_TIMEOUT_PINS
        .iter()
        .find(|(name, _)| *name == adapter_name)
        .map(|(_, secs)| *secs);
    let Some(pinned) = pinned else {
        return Some(format!(
            "no timeout_secs pin for this adapter name — add it to ADAPTER_TIMEOUT_PINS in \
             src/adapter_verify.rs and deploy/install-claude-print-adapters.sh (both copies)"
        ));
    };
    match timeout_secs {
        None => Some(format!(
            "omits timeout_secs — NEEDLE's stuck-detection is blind during a run, so \
             timeout_secs is the only killer of a hung strand; the pin is {pinned}s"
        )),
        Some(actual) if actual != pinned => Some(format!(
            "timeout_secs is {actual}s, pinned at {pinned}s — edit both copies to change a pin"
        )),
        Some(_) => None,
    }
}

/// One bash array's elements out of an installer-source text, in declaration
/// order.
///
/// The installer declares the rule-3/rule-5 lists as flat arrays of bare
/// env-var names (`RULE3_IDE_VARS=(CLAUDECODE …)`) and the invoke/timeout
/// pins as quoted or `name=value` elements
/// (`REQUIRED_INVOKE_FLAGS=("< {prompt_file}" …)`). The parse anchors on a
/// declaration line — the name at the start of a line (leading whitespace
/// allowed), immediately followed by `=(` — so a comment that merely mentions
/// the array cannot satisfy it, and reads to the first `)`, which is exact
/// for these arrays across wrapped lines. Quoting is stripped but not
/// interpreted: an element that stops being a bare identifier or a plain
/// quoted string is precisely the drift the sync checks exist to flag.
#[cfg(test)]
fn bash_array_vars(installer_src: &str, array_name: &str) -> Result<Vec<String>, String> {
    let marker = format!("{}=(", array_name);
    let mut search_from = 0usize;
    let decl_start = loop {
        let rel = installer_src[search_from..]
            .find(&marker)
            .ok_or_else(|| format!("no {}=( declaration found", array_name))?;
        let abs = search_from + rel;
        let line_start = installer_src[..abs].rfind('\n').map_or(0, |i| i + 1);
        if installer_src[line_start..abs].trim().is_empty() {
            break abs;
        }
        search_from = abs + marker.len();
    };

    let body = &installer_src[decl_start + marker.len()..];
    let close = body
        .find(')')
        .ok_or_else(|| format!("{} is never closed", array_name))?;
    let vars: Vec<String> = bash_array_words(&body[..close]);
    if vars.is_empty() {
        return Err(format!("{} declares no variables", array_name));
    }
    Ok(vars)
}

/// Whitespace-split of a bash array body that keeps a double- or
/// single-quoted run whole and strips the quotes — needed because the flag
/// list carries elements with spaces (`"< {prompt_file}"`). Bare elements
/// parse through the same path, so the variable arrays behave exactly as
/// before.
#[cfg(test)]
fn bash_array_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words.into_iter().filter(|w| !w.is_empty()).collect()
}

/// Values planted in the probe child's environment for every rule-3/rule-5
/// variable.
///
/// The probe deliberately simulates the worst-case parent shell (an
/// interactive Claude Code session, a proxy-routed worker): if the template
/// scrubs properly the values never reach claude-print and the call goes to
/// the real subscription endpoint; if a regression drops an `unset`, the
/// poison makes the failure deterministic (closed port, invalid model/auth)
/// instead of a silent pass on a clean parent env. The values are dummies, so
/// a leak of the probe env itself is harmless.
pub fn poisoned_env() -> Vec<(&'static str, &'static str)> {
    fn poison_value_for(var: &str) -> &'static str {
        match var {
            "CLAUDECODE" => "1",
            "CLAUDE_CODE_SSE_PORT" => "0",
            // Closed port on loopback: instant connection refusal if it leaks.
            "ANTHROPIC_BASE_URL" => "http://127.0.0.1:9",
            v if v.starts_with("ANTHROPIC_") => "cgov-adapter-probe-poison",
            _ => "/nonexistent-cgov-adapter-probe",
        }
    }

    IDE_ENV_VARS
        .iter()
        .chain(API_ROUTING_ENV_VARS.iter())
        .map(|var| (*var, poison_value_for(var)))
        .collect()
}

/// Render the template the way dispatch would, for a probe run.
///
/// Rejects any remaining `{word}` placeholder: dispatch substitutes values we
/// do not invent here, and running a half-rendered command line would test
/// the template's error handling rather than the template.
pub fn render_invoke_template(
    template: &str,
    workspace: &Path,
    model: &str,
    prompt_file: &Path,
) -> Result<String, String> {
    let rendered = template
        .replace("{workspace}", &workspace.to_string_lossy())
        .replace("{model}", model)
        .replace("{prompt_file}", &prompt_file.to_string_lossy());

    if let Some(placeholder) = find_unrendered_placeholder(&rendered) {
        return Err(format!(
            "unsupported placeholder {{{}}} in invoke_template — this check only substitutes \
             {{workspace}}, {{model}} and {{prompt_file}}",
            placeholder
        ));
    }
    Ok(rendered)
}

/// Find a remaining `{word}` placeholder, ignoring shell syntax that merely
/// looks bracey (`${VAR}`, `{1..3}`).
fn find_unrendered_placeholder(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && (i == 0 || bytes[i - 1] != b'$') {
            // `${VAR}` is shell variable syntax, not a dispatch placeholder —
            // the `$` sits before the brace, so the brace's contents alone
            // look exactly like `{word}`.
            if let Some(rel_end) = s[i + 1..].find('}') {
                let inner = &s[i + 1..i + 1 + rel_end];
                let looks_like_placeholder = !inner.is_empty()
                    && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if looks_like_placeholder {
                    return Some(inner.to_string());
                }
                i += rel_end + 2;
                continue;
            }
        }
        i += 1;
    }
    None
}

/// Outcome of one probe command run.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub timed_out: bool,
    /// `None` when killed by a signal (or by us on timeout).
    pub exit_code: Option<i32>,
    pub stdout_bytes: usize,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub duration: Duration,
}

impl RunOutcome {
    /// Success is the bar CLAUDE.md sets: exit 0 with output. The
    /// silent-empty-output case exits 0 too (that is how the exit-127
    /// incident presented), so an empty stdout must not pass.
    pub fn is_success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0) && self.stdout_bytes > 0
    }

    pub fn failure_reason(&self) -> String {
        if self.timed_out {
            return format!(
                "timed out after {}s — the hung-dispatch signature of the IDE/API-routing env failures",
                self.duration.as_secs()
            );
        }
        match self.exit_code {
            Some(0) => {
                "exit 0 but no output — the silent-empty-output signature of a broken dispatch"
                    .to_string()
            }
            Some(code) => format!("exit code {}", code),
            None => "terminated by signal".to_string(),
        }
    }

    pub fn into_result(self) -> Result<Duration, String> {
        if self.is_success() {
            return Ok(self.duration);
        }
        Err(format!(
            "{} | stderr: {}",
            self.failure_reason(),
            one_line_tail(&self.stderr_tail, 300)
        ))
    }
}

/// Last `max` characters of `s`, flattened to one line (doctor rows and
/// installer output are line-based).
fn one_line_tail(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut tail: String = flat.chars().rev().take(max).collect();
    tail = tail.chars().rev().collect();
    tail
}

/// Run a rendered invoke_template under `bash -c` with the poisoned
/// rule-3/rule-5 environment, enforcing `timeout` as a hard backstop the way
/// NEEDLE's `timeout_secs` does.
fn run_probe(rendered: &str, timeout: Duration) -> Result<RunOutcome, String> {
    let mut command = Command::new("bash");
    command.arg("-c").arg(rendered);
    for (var, value) in poisoned_env() {
        command.env(var, value);
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot spawn bash for invoke_template: {}", e))?;

    // Drain both pipes on threads: a probe that out-fills its pipe must not
    // deadlock against a parent that is still polling try_wait.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_handle = thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });
    let stderr_handle = thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("cannot poll the invoke_template process: {}", e)),
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    Ok(RunOutcome {
        timed_out,
        exit_code: status.and_then(|s| s.code()),
        stdout_bytes: stdout.len(),
        stdout_tail: one_line_tail(&stdout, 400),
        stderr_tail: one_line_tail(&stderr, 400),
        duration: start.elapsed(),
    })
}

/// Run the adapter's `invoke_template` verbatim with a trivial prompt and
/// require exit 0 (plus output). `workspace` doubles as the `{workspace}`
/// substitution and the home of the probe prompt file.
pub fn live_probe_in(adapter: &InstalledAdapter, workspace: &Path) -> Result<Duration, String> {
    let prompt_file = workspace.join("probe-prompt.txt");
    fs::write(&prompt_file, PROBE_PROMPT)
        .map_err(|e| format!("cannot write {}: {}", prompt_file.display(), e))?;

    let model = adapter.adapter.model.as_deref().unwrap_or("claude");
    let rendered = render_invoke_template(
        &adapter.adapter.invoke_template,
        workspace,
        model,
        &prompt_file,
    )?;

    let timeout = Duration::from_secs(
        adapter
            .adapter
            .timeout_secs
            .unwrap_or(LIVE_PROBE_TIMEOUT_SECS)
            .clamp(30, LIVE_PROBE_TIMEOUT_SECS),
    );
    run_probe(&rendered, timeout)?.into_result()
}

/// [`live_probe_in`] against the standard probe workspace.
pub fn live_probe(adapter: &InstalledAdapter) -> Result<Duration, String> {
    live_probe_in(adapter, &probe_workspace()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    /// The committed artifacts these drift gates police, embedded at compile
    /// time so the test binary is self-contained. NEEDLE's close gate re-runs
    /// tests through the shared /build/target-workers dir, where a binary
    /// built in one `git archive` extraction is reused in another (archive
    /// mtimes carry the commit time, so every extraction of one commit looks
    /// fresh to cargo); a runtime read at `env!("CARGO_MANIFEST_DIR")` then
    /// points at a long-deleted checkout and both gates fail with ENOENT —
    /// seen 2026-09-18: `cargo test` through the shared cache failed 2/909
    /// with "cannot read /data/build/scratch/cg-b385e643-dod-jLa6zv/..." from
    /// a commit whose in-tree run was green. Same defect class the stale-hold
    /// suite fixed for the controller script (tests/glm_quota_controller_
    /// stale_hold.rs). Cargo tracks include_str! files as rebuild inputs, so
    /// an installer or adapter edit still triggers a fresh build.
    const INSTALLER_SH: &str = include_str!("../deploy/install-claude-print-adapters.sh");
    const ADAPTER_OPUS_YAML: &str =
        include_str!("../deploy/needle-adapters/claude-print-opus.yaml");
    const ADAPTER_FABLE_YAML: &str =
        include_str!("../deploy/needle-adapters/claude-print-fable.yaml");

    fn installer_source() -> String {
        INSTALLER_SH.to_string()
    }

    /// Space-joined rule-3 + rule-5 sets, derived from the constants rather
    /// than retyped — a hand-copied list inside this very test module is
    /// exactly the drift this module polices elsewhere.
    fn all_required_vars() -> String {
        IDE_ENV_VARS
            .iter()
            .chain(API_ROUTING_ENV_VARS.iter())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The committed adapters, materialized under their committed names (see
    /// INSTALLER_SH for why they are embedded rather than read from the
    /// checkout) and loaded through the production loader. Shared by the
    /// three committed-template gates below.
    fn committed_adapters() -> Vec<InstalledAdapter> {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("claude-print-opus.yaml"), ADAPTER_OPUS_YAML)
            .expect("write opus adapter");
        std::fs::write(
            tmp.path().join("claude-print-fable.yaml"),
            ADAPTER_FABLE_YAML,
        )
        .expect("write fable adapter");
        load_installed_adapters(tmp.path()).expect("repo adapters must load")
    }

    #[test]
    fn committed_adapter_templates_scrub_both_variable_sets() {
        let adapters = committed_adapters();

        assert!(
            adapters.len() >= 2,
            "expected the opus and fable adapters, found {}",
            adapters.len()
        );
        for adapter in &adapters {
            let missing = missing_scrub_vars(&adapter.adapter.invoke_template);
            assert!(
                missing.is_empty(),
                "{} stops unsetting {:?}",
                adapter.adapter.name,
                missing.iter().map(|(_, v)| *v).collect::<Vec<_>>()
            );
        }
    }

    /// The dispatch contract a trivial live probe cannot see (see the
    /// REQUIRED_INVOKE_FLAGS doc): a hand-edited template that drops one of
    /// these still answers the probe with plain output and exit 0, and fails
    /// only in production dispatch.
    #[test]
    fn committed_adapter_templates_carry_required_invoke_flags() {
        for adapter in committed_adapters() {
            let missing = missing_invoke_flags(&adapter.adapter.invoke_template);
            assert!(
                missing.is_empty(),
                "{} drops required invoke flags {:?} — dispatch needs them even though \
                 the live probe cannot miss them",
                adapter.adapter.name,
                missing
            );
        }
    }

    /// timeout_secs is the only killer of a hung strand (NEEDLE's
    /// stuck-detection is blind during a run), so each adapter's value is
    /// pinned exactly, and an unpinned adapter name must fail rather than
    /// inherit a ceiling.
    #[test]
    fn committed_adapter_templates_pin_timeout_secs() {
        let adapters = committed_adapters();
        for adapter in &adapters {
            if let Some(violation) =
                timeout_violation(&adapter.adapter.name, adapter.adapter.timeout_secs)
            {
                panic!("{}: {}", adapter.adapter.name, violation);
            }
        }
        // Every committed adapter is covered by a pin — no name slipped in
        // without one.
        assert_eq!(
            adapters.len(),
            ADAPTER_TIMEOUT_PINS.len(),
            "an adapter was added without a timeout_secs pin, or a pin lost its adapter"
        );
    }

    // -- host parity (CANONICAL_ADAPTERS / host_parity_drift) -----------------

    /// The embedded canonical adapters are the committed template bytes under
    /// their installed filenames — the parity check compares live installs
    /// against exactly what the repo ships.
    #[test]
    fn canonical_adapters_carry_the_committed_template_bytes() {
        let canonical: std::collections::BTreeMap<&str, &str> =
            CANONICAL_ADAPTERS.iter().copied().collect();
        assert_eq!(
            canonical.get("claude-print-opus.yaml").copied(),
            Some(ADAPTER_OPUS_YAML)
        );
        assert_eq!(
            canonical.get("claude-print-fable.yaml").copied(),
            Some(ADAPTER_FABLE_YAML)
        );
    }

    /// The embedded bytes must survive the production loader and pass every
    /// static contract gate — a canonical template that cannot be loaded or
    /// is itself non-compliant would make parity impossible (every host would
    /// either match a broken canonical or drift from a good install).
    #[test]
    fn canonical_adapters_load_and_pass_the_static_contract() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        for (filename, bytes) in CANONICAL_ADAPTERS {
            std::fs::write(tmp.path().join(filename), bytes).expect("write canonical adapter");
        }
        let adapters = load_installed_adapters(tmp.path()).expect("canonical adapters must load");
        assert_eq!(adapters.len(), CANONICAL_ADAPTERS.len());
        for adapter in &adapters {
            assert!(
                missing_scrub_vars(&adapter.adapter.invoke_template).is_empty(),
                "{} stops unsetting required vars",
                adapter.adapter.name
            );
            assert!(
                missing_invoke_flags(&adapter.adapter.invoke_template).is_empty(),
                "{} drops required invoke flags",
                adapter.adapter.name
            );
            assert_eq!(
                timeout_violation(&adapter.adapter.name, adapter.adapter.timeout_secs),
                None,
                "{} violates its timeout pin",
                adapter.adapter.name
            );
        }
    }

    #[test]
    fn canonical_binary_paths_extracts_only_absolute_dispatch_paths() {
        // Both committed templates call the same absolute path; the bare
        // `claude-print` tokens (agent_cli, version_command) never match.
        assert_eq!(
            canonical_binary_paths(),
            vec!["/home/coding/.local/bin/claude-print".to_string()]
        );
        // A path in the middle of the template counts; a relative one and a
        // path not ending in /claude-print do not.
        let found = absolute_binary_paths_in(
            "x /opt/cp/claude-print --flag /usr/bin/other claude-print /rel/claude-print/x",
        );
        assert_eq!(found, vec!["/opt/cp/claude-print".to_string()]);
    }

    fn install_canonical(dir: &Path) {
        for (filename, bytes) in CANONICAL_ADAPTERS {
            std::fs::write(dir.join(filename), bytes).expect("write adapter");
        }
    }

    #[test]
    fn parity_passes_a_byte_identical_host_with_the_binary_path_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("adapters");
        std::fs::create_dir(&dir).unwrap();
        install_canonical(&dir);

        let bin = tmp.path().join("claude-print");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        make_executable(&bin);

        let drift = host_parity_drift(&dir, &[bin.display().to_string()])
            .expect("readable install must not err");
        assert!(
            drift.is_empty(),
            "byte-identical host must carry no drift: {drift:?}"
        );
    }

    #[test]
    fn parity_reports_a_diverged_installed_template_with_both_byte_counts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("adapters");
        std::fs::create_dir(&dir).unwrap();
        install_canonical(&dir);
        // The hand-edit shape: someone touched the live template on this host.
        let (filename, canonical) = CANONICAL_ADAPTERS[1];
        std::fs::write(dir.join(filename), format!("{canonical}\n# local edit\n")).unwrap();

        let drift = host_parity_drift(&dir, &[]).expect("readable install must not err");
        assert_eq!(
            drift,
            vec![ParityDrift::DivergedAdapter {
                filename: filename.to_string(),
                installed_bytes: canonical.len() + "\n# local edit\n".len(),
                canonical_bytes: canonical.len(),
            }]
        );
        let rendered = drift[0].to_string();
        assert!(rendered.contains("differs from the canonical template"), "{rendered}");
        assert!(rendered.contains(filename), "{rendered}");
    }

    #[test]
    fn parity_reports_a_missing_canonical_adapter_and_a_missing_dir_as_drift() {
        // An adapters dir without one of the canonical pair (an install older
        // than the repo, or a hand deletion) is drift, not an error.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("adapters");
        std::fs::create_dir(&dir).unwrap();
        let (only, bytes) = CANONICAL_ADAPTERS[0];
        std::fs::write(dir.join(only), bytes).unwrap();

        let drift = host_parity_drift(&dir, &[]).expect("readable install must not err");
        assert_eq!(
            drift,
            vec![ParityDrift::MissingAdapter {
                filename: "claude-print-opus.yaml".to_string(),
            }]
        );

        // A host that was never provisioned has no adapters dir at all: still
        // drift (every canonical adapter missing), not an error — the caller
        // distinguishes not-provisioned from drifted by inventorying first.
        let absent = tmp.path().join("no-such-dir");
        let drift = host_parity_drift(&absent, &[]).expect("missing dir is drift, not an error");
        assert_eq!(drift.len(), CANONICAL_ADAPTERS.len());
        assert!(drift
            .iter()
            .all(|d| matches!(d, ParityDrift::MissingAdapter { .. })));
    }

    #[test]
    fn parity_fails_the_missing_dispatch_binary_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("adapters");
        std::fs::create_dir(&dir).unwrap();
        install_canonical(&dir);

        let drift = host_parity_drift(&dir, &["/nonexistent-cgov-parity/claude-print".to_string()])
            .expect("readable install must not err");
        assert_eq!(
            drift,
            vec![ParityDrift::BinaryPathMissing {
                path: "/nonexistent-cgov-parity/claude-print".to_string(),
            }]
        );
        assert!(
            drift[0].to_string().contains("exit 127"),
            "the finding must name the dispatch failure it causes: {}",
            drift[0]
        );
    }

    #[test]
    fn parity_errs_when_an_installed_file_cannot_be_read_at_all() {
        let tmp = tempfile::TempDir::new().unwrap();
        // A regular file where the adapters directory should be: every
        // canonical path is unreadable (ENOTDIR) — that is an error to
        // report, not drift.
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, "file").unwrap();

        let err = host_parity_drift(&blocker, &[]).expect_err("unreadable install must err");
        assert!(err.contains("cannot read"), "err: {err}");
    }

    #[test]
    fn missing_scrub_detects_a_dropped_unset_segment() {
        // The rule-5 regression from the record: a template rebuilt without
        // the API-routing unsets at all.
        let template = format!(
            "cd {{workspace}} && unset {} && /usr/bin/claude-print < {{prompt_file}}",
            "CLAUDECODE VSCODE_PID VSCODE_CWD"
        );
        let missing = missing_scrub_vars(&template);
        let missing_names: Vec<_> = missing.iter().map(|(_, v)| *v).collect();
        assert!(missing_names.contains(&"ANTHROPIC_BASE_URL"));
        assert!(missing_names.contains(&"CLAUDE_CODE_SUBAGENT_MODEL"));
        // The three the template still unsets are satisfied…
        assert!(!missing_names.contains(&"CLAUDECODE"));
        assert!(!missing_names.contains(&"VSCODE_PID"));
        assert!(!missing_names.contains(&"VSCODE_CWD"));
        // …but a partial IDE unset list leaves the rest of rule 3 missing too.
        assert!(missing.contains(&(EnvClass::Ide, "VSCODE_NONCE")));
        assert!(missing.contains(&(EnvClass::ApiRouting, "ANTHROPIC_BASE_URL")));
    }

    #[test]
    fn missing_scrub_reports_each_failure_class() {
        let missing = missing_scrub_vars("cd {workspace} && /usr/bin/claude-print < {prompt_file}");
        assert_eq!(
            missing.len(),
            IDE_ENV_VARS.len() + API_ROUTING_ENV_VARS.len()
        );
        assert!(missing.contains(&(EnvClass::Ide, "CLAUDECODE")));
        assert!(missing.contains(&(EnvClass::ApiRouting, "ANTHROPIC_BASE_URL")));
    }

    #[test]
    fn missing_invoke_flags_detects_a_dropped_flag_and_names_it() {
        // The recorded rule-4 shape: a template that lost --pretrust-cwd but
        // kept everything else.
        let template = "cd {workspace} && /usr/bin/claude-print --output-format stream-json \
                        --no-inherit-hooks < {prompt_file}";
        let missing = missing_invoke_flags(template);
        assert_eq!(missing, vec!["--pretrust-cwd"]);

        // A bare template drops everything, each named exactly as it must
        // appear in the template.
        let all = missing_invoke_flags("cd {workspace} && /usr/bin/claude-print");
        assert_eq!(all.len(), REQUIRED_INVOKE_FLAGS.len());
        assert!(all.contains(&"< {prompt_file}"));
    }

    #[test]
    fn timeout_violation_flags_absent_unpinned_and_drifted_values() {
        // A raised pin silently lengthens how long a hung strand wedges a
        // worker; a lowered one starts killing legitimate strands. Both are
        // drift — the pin is exact, not a ceiling.
        let raised = timeout_violation("claude-print-opus", Some(9600)).expect("raised pin drifts");
        assert!(raised.contains("9600s"), "msg: {}", raised);
        assert!(raised.contains("pinned at 1200s"), "msg: {}", raised);

        let lowered =
            timeout_violation("claude-print-fable", Some(60)).expect("lowered pin drifts");
        assert!(lowered.contains("pinned at 600s"), "msg: {}", lowered);

        let absent = timeout_violation("claude-print-opus", None).expect("absent timeout drifts");
        assert!(absent.contains("omits timeout_secs"), "msg: {}", absent);

        let unpinned =
            timeout_violation("claude-print-sonnet", Some(1200)).expect("unpinned name drifts");
        assert!(
            unpinned.contains("no timeout_secs pin"),
            "msg: {}",
            unpinned
        );
        assert!(
            unpinned.contains("both copies"),
            "the remedy must name the mirror: {}",
            unpinned
        );

        assert_eq!(timeout_violation("claude-print-opus", Some(1200)), None);
        assert_eq!(timeout_violation("claude-print-fable", Some(600)), None);
    }

    #[test]
    fn scrubbed_vars_only_counts_segments_starting_with_unset() {
        let scrubbed = scrubbed_vars(
            "cd {workspace} && unset CLAUDECODE VSCODE_PID && echo \"unset NOT_A_SCRUB\" \
             && unset 'ANTHROPIC_BASE_URL' && run",
        );
        assert!(scrubbed.contains("CLAUDECODE"));
        assert!(scrubbed.contains("VSCODE_PID"));
        assert!(scrubbed.contains("ANTHROPIC_BASE_URL"), "quoted var counts");
        assert!(
            !scrubbed.contains("NOT_A_SCRUB"),
            "echoed text is not an unset"
        );
        assert!(!scrubbed.contains("run"));
    }

    #[test]
    fn render_substitutes_every_dispatch_placeholder() {
        let rendered = render_invoke_template(
            "cd {workspace} && /bin/claude-print --model {model} < {prompt_file}",
            Path::new("/tmp/ws"),
            "opus",
            Path::new("/tmp/ws/p.txt"),
        )
        .expect("all placeholders known");
        assert_eq!(
            rendered,
            "cd /tmp/ws && /bin/claude-print --model opus < /tmp/ws/p.txt"
        );
    }

    #[test]
    fn render_rejects_unknown_placeholders_but_keeps_shell_braces() {
        let err = render_invoke_template(
            "cd {workspace} && /bin/tool --session {session} < {prompt_file}",
            Path::new("/tmp/ws"),
            "opus",
            Path::new("/tmp/ws/p.txt"),
        )
        .expect_err("unknown placeholder must refuse to run");
        assert!(err.contains("{session}"), "err: {}", err);

        // Shell syntax that merely looks bracey stays legal.
        let rendered = render_invoke_template(
            "cd {workspace} && echo ${HOME} && echo {1..3}",
            Path::new("/tmp/ws"),
            "opus",
            Path::new("/tmp/ws/p.txt"),
        )
        .expect("bash braces are not placeholders");
        assert!(rendered.contains("${HOME}"));
    }

    #[test]
    fn probe_poison_reaches_a_child_that_does_not_scrub() {
        let outcome = run_probe(
            "echo \"base=$ANTHROPIC_BASE_URL code=$CLAUDECODE\"",
            Duration::from_secs(30),
        )
        .expect("probe spawns");
        assert!(outcome.stdout_tail.contains("http://127.0.0.1:9"));
        // The CLAUDECODE poison is the literal "1".
        assert!(outcome.stdout_tail.contains("code=1"));
    }

    #[test]
    fn probe_success_requires_exit_zero_and_output() {
        let ok = run_probe("echo ok", Duration::from_secs(30)).expect("probe spawns");
        assert!(ok.is_success());

        let silent = run_probe("true", Duration::from_secs(30)).expect("probe spawns");
        assert!(!silent.is_success());
        assert!(silent.failure_reason().contains("no output"));

        let failed =
            run_probe("echo boom >&2; exit 3", Duration::from_secs(30)).expect("probe spawns");
        assert_eq!(failed.exit_code, Some(3));
        assert!(failed.failure_reason().contains("3"));
    }

    #[test]
    fn probe_enforces_its_timeout_backstop() {
        let slow = run_probe("sleep 30", Duration::from_secs(1)).expect("probe spawns");
        assert!(slow.timed_out);
        assert!(!slow.is_success());
        assert!(slow.failure_reason().contains("timed out"));
    }

    #[test]
    fn live_probe_passes_a_stub_that_scrubs_and_answers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let stub = tmp.path().join("stub-answer.sh");
        // The stub inherits the poisoned env unless the template scrubs it —
        // assert the scrub actually happened by recording what it saw.
        let seen = tmp.path().join("seen-env");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nenv | grep -c '^ANTHROPIC_BASE_URL=' > {}\necho probe-ok\n",
                seen.display()
            ),
        )
        .unwrap();
        make_executable(&stub);

        let yaml = format!(
            "name: stub-probe\ninvoke_template: \"cd {{workspace}} && unset {} && {} < {{prompt_file}}\"\nmodel: opus\ntimeout_secs: 60\n",
            all_required_vars(),
            stub.display()
        );
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("claude-print-stub.yaml"), &yaml).unwrap();
        let adapters = load_installed_adapters(dir.path()).unwrap();
        assert_eq!(adapters.len(), 1);
        assert!(missing_scrub_vars(&adapters[0].adapter.invoke_template).is_empty());

        let duration = live_probe_in(&adapters[0], tmp.path()).expect("stub probe passes");
        assert!(duration.as_secs() < 60);

        let seen = std::fs::read_to_string(&seen).unwrap().trim().to_string();
        assert_eq!(
            seen, "0",
            "poisoned ANTHROPIC_BASE_URL leaked through the unset"
        );
    }

    #[test]
    fn live_probe_fails_a_stub_that_exits_nonzero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let stub = tmp.path().join("stub-fail.sh");
        std::fs::write(&stub, "#!/bin/sh\necho proxy refused >&2\nexit 124\n").unwrap();
        make_executable(&stub);

        let yaml = format!(
            "name: stub-fail\ninvoke_template: \"cd {{workspace}} && unset {} && {} < {{prompt_file}}\"\ntimeout_secs: 60\n",
            all_required_vars(),
            stub.display()
        );
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("claude-print-stub.yaml"), &yaml).unwrap();
        let adapters = load_installed_adapters(dir.path()).unwrap();

        let err = live_probe_in(&adapters[0], tmp.path()).expect_err("nonzero exit must fail");
        assert!(err.contains("124"), "err: {}", err);
        assert!(err.contains("proxy refused"), "err: {}", err);
    }

    #[test]
    fn load_reports_a_broken_template_instead_of_skipping_it() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("claude-print-broken.yaml"),
            "name: broken\ninvoke_template: \"\"\n",
        )
        .unwrap();

        let err = load_installed_adapters(dir.path()).expect_err("empty template must fail loudly");
        assert!(err.contains("invoke_template"), "err: {}", err);
    }

    #[test]
    fn bash_array_parses_declarations_not_comments() {
        // A comment naming the array must not satisfy the parse; the
        // declaration line must, including across wrapped lines.
        let src = "# RULE3_IDE_VARS=(NOT A REAL DECL)\nRULE3_IDE_VARS=(A B\n    C)\n";
        assert_eq!(
            bash_array_vars(src, "RULE3_IDE_VARS").expect("parses"),
            vec!["A".to_string(), "B".to_string(), "C".to_string()]
        );
        // Indented declarations count; mid-line mentions do not.
        let indented = "  RULE5_API_VARS=(X Y)\n";
        assert_eq!(
            bash_array_vars(indented, "RULE5_API_VARS").expect("parses"),
            vec!["X".to_string(), "Y".to_string()]
        );
        let mid_line = "echo the RULE5_API_VARS=(is not a decl here)\n";
        assert!(bash_array_vars(mid_line, "RULE5_API_VARS").is_err());
        assert!(bash_array_vars("nothing here", "RULE5_API_VARS")
            .err()
            .unwrap()
            .contains("no RULE5_API_VARS=("));
    }

    /// The installer's bash mirror of the rule-3/rule-5 constants must stay
    /// set-identical. Without this, a variable added to one copy silently
    /// shrank the check in exactly the places that verify the other — the
    /// installer's scrub check and `cgov doctor`'s would disagree about what
    /// "scrubbed" means, and neither would fail. The installer cross-checks
    /// these same constants at run time; this test catches the drift for
    /// every `cargo test` run, before anyone installs anything.
    #[test]
    fn installer_bash_variable_lists_match_the_rust_constants() {
        let installer = installer_source();
        for (rust_vars, bash_vars, rust_name, bash_name) in [
            (
                IDE_ENV_VARS.to_vec(),
                bash_array_vars(&installer, "RULE3_IDE_VARS")
                    .expect("RULE3_IDE_VARS must be declared in the installer"),
                "IDE_ENV_VARS",
                "RULE3_IDE_VARS",
            ),
            (
                API_ROUTING_ENV_VARS.to_vec(),
                bash_array_vars(&installer, "RULE5_API_VARS")
                    .expect("RULE5_API_VARS must be declared in the installer"),
                "API_ROUTING_ENV_VARS",
                "RULE5_API_VARS",
            ),
        ] {
            let rust_set: BTreeSet<_> = rust_vars.into_iter().collect();
            let bash_set: BTreeSet<_> = bash_vars.iter().map(String::as_str).collect();
            let missing: Vec<_> = rust_set.difference(&bash_set).collect();
            let extra: Vec<_> = bash_set.difference(&rust_set).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{} in deploy/install-claude-print-adapters.sh diverges from {} in \
                 src/adapter_verify.rs — the bash list is missing {:?} and carries \
                 unlisted {:?}. Update both copies together; the installer's own \
                 sync check fails the install on the same drift.",
                bash_name,
                rust_name,
                missing,
                extra
            );
        }
    }

    /// The installer's bash mirrors of [`REQUIRED_INVOKE_FLAGS`] and
    /// [`ADAPTER_TIMEOUT_PINS`] must stay identical to the constants — the
    /// same two-gate rule as the variable lists above. Without it, a flag or
    /// pin added to one copy would leave the installer's static template
    /// check covering a different invoke contract than `cgov doctor`'s, and
    /// neither would fail. `check_flag_and_timeout_sync` in the installer
    /// cross-checks these same constants at run time.
    #[test]
    fn installer_bash_flag_and_timeout_pins_match_the_rust_constants() {
        let installer = installer_source();

        let bash_flag_vec =
            bash_array_vars(&installer, "REQUIRED_INVOKE_FLAGS")
                .expect("REQUIRED_INVOKE_FLAGS must be declared in the installer");
        let bash_flags: BTreeSet<&str> = bash_flag_vec.iter().map(String::as_str).collect();
        let rust_flags: BTreeSet<&str> = REQUIRED_INVOKE_FLAGS.iter().copied().collect();
        assert_eq!(
            bash_flags, rust_flags,
            "REQUIRED_INVOKE_FLAGS in deploy/install-claude-print-adapters.sh diverges \
             from src/adapter_verify.rs (bash-only: {:?}, rust-only: {:?}) — update \
             both copies together; the installer's own sync check fails the install \
             on the same drift.",
            bash_flags
                .difference(&rust_flags)
                .collect::<Vec<_>>(),
            rust_flags
                .difference(&bash_flags)
                .collect::<Vec<_>>(),
        );

        let mut bash_pins: Vec<(String, u64)> =
            bash_array_vars(&installer, "ADAPTER_TIMEOUT_PINS")
                .expect("ADAPTER_TIMEOUT_PINS must be declared in the installer")
                .into_iter()
                .map(|element| match element.split_once('=') {
                    Some((name, secs)) => secs.parse::<u64>().map(|secs| (name.to_string(), secs)).unwrap_or_else(
                        |_| {
                            panic!(
                                "ADAPTER_TIMEOUT_PINS element {element:?} does not end in \
                                 =<seconds>"
                            )
                        },
                    ),
                    None => panic!("ADAPTER_TIMEOUT_PINS element {element:?} is not name=value"),
                })
                .collect();
        bash_pins.sort();
        let mut rust_pins: Vec<(String, u64)> = ADAPTER_TIMEOUT_PINS
            .iter()
            .map(|(name, secs)| ((*name).to_string(), *secs))
            .collect();
        rust_pins.sort();
        assert_eq!(
            bash_pins, rust_pins,
            "ADAPTER_TIMEOUT_PINS in deploy/install-claude-print-adapters.sh diverges \
             from src/adapter_verify.rs (bash: {:?}, rust: {:?}) — update both copies \
             together; the installer's own sync check fails the install on the same \
             drift.",
            bash_pins, rust_pins,
        );
    }
}
