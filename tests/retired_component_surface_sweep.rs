//! End-to-end retired-component surface sweep (claudego-8e03c5b4).
//!
//! The standalone polish queue, its timer and seeder, and the subscription
//! generator pool were retired on 2026-09-16 (see CLAUDE.md, "Retired
//! 2026-09-16"). The prohibition is enforced mechanically in three places —
//! `RETIRED_REFERENCE_MARKERS` rejection at config load, doctor's
//! `retired_component_refs` / `retired_component_units` checks, and init's
//! retired-unit sweep — but until now nothing proved the *surfaces* clean end
//! to end. Unit tests pin individual templates and check functions; this file
//! runs the real binary and asserts that no path an operator can walk offers
//! to set up, install, or recreate a retired component:
//!
//! 1. **Fresh install.** `cgov init` under a temp `XDG_CONFIG_HOME` writes
//!    config with zero retired references, prints none, and creates no
//!    systemd unit directory; `cgov doctor` on that fresh install passes
//!    both retired-component checks and its entire JSON report contains
//!    zero retired markers.
//! 2. **Failure directions.** When doctor *does* flag a retired pool (a
//!    poisoned config) or a retired unit still installed (planted files),
//!    the remediation text must demand removal and bar recreation — never
//!    offer install/re-enable steps.
//! 3. **File sweep.** Every template init embeds (`config/`) and every
//!    script under `deploy/` is scanned via `include_str!` for the same
//!    markers — the manual `grep` sweep from
//!    `docs/notes/retired-component-surface-sweep.md`, executable by the
//!    test suite.
//!
//! Everything the sweep needs is compiled in (`include_str!`), and the
//! doctor/init runs only exercise the temp sandbox — so the file passes from
//! a shared-cache test binary in a clean extraction, where the source tree
//! the binary was built from no longer exists.
//!
//! Isolation: every child `cgov` runs with `HOME`, `XDG_CONFIG_HOME`,
//! `XDG_DATA_HOME`, `XDG_STATE_HOME`, and `XDG_CACHE_HOME` pointed into a
//! fresh `TempDir` (same recipe as `scale_safe_mode_stdout_test.rs`), so
//! nothing in the developer's real config is read or written. Init runs with
//! `--no-systemd` so it never invokes `systemctl` against the *live* user
//! session — the unit-install branch is content-pinned by
//! `test_init_embedded_templates_have_no_retired_references` in `main.rs`
//! instead. Doctor runs with `--skip-live` (no real subscription dispatch);
//! it exits non-zero when unrelated checks fail on a bare sandbox (no OAuth
//! credentials, no state file), so these tests parse the JSON report rather
//! than requiring exit 0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

use claude_governor::config::RETIRED_REFERENCE_MARKERS;

/// The retired seeder unit (claude-polish-seeder), as pinned by
/// `RETIRED_POLISH_UNITS` in `doctor.rs`. Planted in the sandbox's systemd
/// user dir to walk doctor's `retired_component_units` failure direction.
const RETIRED_UNIT_SERVICE: &str = "claude-polish-seeder.service";

/// The seeder's timer half — the dangerous one on a stale install, since the
/// timer keeps launching the seeder on schedule with nothing maintaining the
/// queue it fed.
const RETIRED_UNIT_TIMER: &str = "claude-polish-seeder.timer";

/// A minimal config that references a retired pool the way a pre-retirement
/// install would: an `agents:` entry named after the retired pool, with the
/// fields that echo it (`find_retired_references` scans exactly the
/// top-level keys, the `agents:` names, and these three fields).
const POISONED_CONFIG: &str = r#"
pricing:
  models: {}
agents:
  polish-opus:
    launch_cmd: "needle run --agent polish-opus"
    session_pattern: "polish-opus-*"
    heartbeat_dir: "/tmp/polish-heartbeats"
"#;

/// Every file init embeds and every script shipped under `deploy/`. This list
/// is the sweep inventory: when a template or deploy script is added, add it
/// here so it joins the sweep. Files are compiled in via `include_str!` so
/// the sweep reads the committed bytes rather than a source tree that may be
/// gone by the time a cached test binary runs. The retired unit names need no
/// separate entries — each contains the `polish` marker.
const SWEPT_FILES: &[(&str, &str)] = &[
    ("config/governor.yaml", include_str!("../config/governor.yaml")),
    (
        "config/claude-governor-observe.service",
        include_str!("../config/claude-governor-observe.service"),
    ),
    (
        "config/claude-governor-act.service",
        include_str!("../config/claude-governor-act.service"),
    ),
    (
        "config/claude-token-collector.service",
        include_str!("../config/claude-token-collector.service"),
    ),
    ("config/promotions.json", include_str!("../config/promotions.json")),
    (
        "deploy/claude-governor-observe.service",
        include_str!("../deploy/claude-governor-observe.service"),
    ),
    (
        "deploy/install-claude-print-adapters.sh",
        include_str!("../deploy/install-claude-print-adapters.sh"),
    ),
    (
        "deploy/needle-adapters/claude-print-opus.yaml",
        include_str!("../deploy/needle-adapters/claude-print-opus.yaml"),
    ),
    (
        "deploy/needle-adapters/claude-print-fable.yaml",
        include_str!("../deploy/needle-adapters/claude-print-fable.yaml"),
    ),
];

/// A hermetic install sandbox: a temp dir that every child `cgov` sees as
/// `$HOME` and as every XDG directory's parent.
struct Sandbox(TempDir);

impl Sandbox {
    fn new() -> Self {
        Sandbox(TempDir::new().expect("failed to create temp dir"))
    }

    fn config_home(&self) -> PathBuf {
        self.0.path().join("config")
    }

    /// Path `cgov` resolves as the governor config in this sandbox.
    fn governor_yaml(&self) -> PathBuf {
        self.config_home().join("claude-governor").join("governor.yaml")
    }

    /// Run the real `cgov` binary against this sandbox.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cgov"))
            .args(args)
            .env("HOME", self.0.path())
            .env("XDG_CONFIG_HOME", self.config_home())
            .env("XDG_DATA_HOME", self.0.path().join("data"))
            .env("XDG_STATE_HOME", self.0.path().join("state"))
            .env("XDG_CACHE_HOME", self.0.path().join("cache"))
            .output()
            .expect("failed to run cgov binary")
    }

    /// Run `cgov init` the way the sweep always does: hermetic, and never
    /// touching the live systemd session.
    fn init(&self) -> Output {
        self.run(&["init", "--no-systemd"])
    }

    /// Run `cgov doctor` and deserialize its JSON report. Doctor exits 1 on
    /// any failed check, which on a bare sandbox is expected (no OAuth
    /// credentials, no state file) — the report on stdout is the product.
    fn doctor_report(&self) -> Value {
        let output = self.run(&["doctor", "--json", "--skip-live"]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "doctor did not print a valid JSON report ({e})\nstdout:\n{stdout}\nstderr:\n{stderr}"
            )
        })
    }
}

/// Markers from the retired-component vocabulary found in `text`
/// (case-insensitive).
fn marker_hits(text: &str) -> Vec<&'static str> {
    let lowered = text.to_lowercase();
    RETIRED_REFERENCE_MARKERS
        .iter()
        .copied()
        .filter(|marker| lowered.contains(marker))
        .collect()
}

/// Every file under `root`, recursively. Missing directories yield nothing —
/// the sweep walks whatever the init run actually created.
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// Verbs that would make a sentence an install/recreate offer. Word-boundary
/// matched so `disable` does not trip `enable` (see `find_verb`).
const SUGGESTION_VERBS: &[&str] = &[
    "install",
    "reinstall",
    "recreate",
    "re-enable",
    "reenable",
    "enable",
    "set up",
    "setup",
    "restore",
    "create",
];

/// Negations that put a suggestion verb in a prohibition instead of an offer
/// ("do not recreate", "must not be re-enabled", ...).
const NEGATORS: &[&str] = &[
    "do not",
    "does not",
    "don't",
    "never",
    "cannot",
    "can not",
    "must not",
    "no longer",
    "refuses to",
];

/// Find `verb` in a lowercased sentence at a word boundary: the characters
/// immediately before and after must not be ASCII alphabetic. Without this,
/// the `enable` in `systemctl --user disable` (and the `install` inside
/// `reinstall`) would match and every disable instruction would read as an
/// install offer.
fn find_verb(sentence_lower: &str, verb: &str) -> Option<usize> {
    let bytes = sentence_lower.as_bytes();
    let mut start = 0;
    while let Some(rel) = sentence_lower[start..].find(verb) {
        let pos = start + rel;
        let end = pos + verb.len();
        let before_ok = pos == 0 || !bytes[pos - 1].is_ascii_alphabetic();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphabetic();
        if before_ok && after_ok {
            return Some(pos);
        }
        start = pos + 1;
    }
    None
}

/// Sentences in a doctor remediation string that name a retired component
/// AND carry an install/recreate verb not governed by a negator — i.e. the
/// ways doctor would be offering to bring a retired component back. A
/// remediation with no offenses may still mention retired components (its
/// job is to name the violation); it just may not suggest restoring one.
fn suggestion_offenses(remediation: &str) -> Vec<String> {
    let mut offenses = Vec::new();
    for sentence in remediation.split(|c| c == '.' || c == ';' || c == '\n') {
        let lowered = sentence.to_lowercase();
        if !RETIRED_REFERENCE_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker))
        {
            continue;
        }
        for verb in SUGGESTION_VERBS {
            if let Some(pos) = find_verb(&lowered, verb) {
                let governed = NEGATORS.iter().any(|negator| {
                    lowered
                        .find(negator)
                        .map(|np| np + negator.len() <= pos)
                        .unwrap_or(false)
                });
                if !governed {
                    offenses.push(format!(
                        "remediation sentence {sentence:?} suggests {verb:?} for a retired component"
                    ));
                }
            }
        }
    }
    offenses
}

/// Look up one check from a doctor JSON report, failing with the full report
/// if the check is missing (a check disappearing from the run is itself a
/// regression this sweep should catch).
fn check<'r>(report: &'r Value, name: &str) -> &'r Value {
    report["checks"]
        .as_array()
        .expect("doctor report has a checks array")
        .iter()
        .find(|c| c["check"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("doctor report is missing the {name} check:\n{report}"))
}

/// The positive direction: a fresh install is entirely free of retired
/// components. Init's stdout names no retired marker, every file it writes
/// scans clean, and no systemd unit directory is created at all — the
/// operator is never offered a retired unit to install.
#[test]
fn fresh_init_writes_no_retired_component_setup() {
    let sandbox = Sandbox::new();

    let output = sandbox.init();
    assert!(
        output.status.success(),
        "cgov init --no-systemd must succeed, got {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        marker_hits(&stdout),
        Vec::<&'static str>::new(),
        "init stdout must not name a retired component:\n{stdout}"
    );

    let written = files_under(sandbox.0.path());
    assert!(
        !written.is_empty(),
        "init should have written something under the sandbox"
    );
    for file in &written {
        let contents = std::fs::read_to_string(file).unwrap_or_else(|e| {
            panic!("init wrote {} but it cannot be read as text: {e}", file.display())
        });
        assert_eq!(
            marker_hits(&contents),
            Vec::<&'static str>::new(),
            "init-written file {} references a component retired 2026-09-16",
            file.display()
        );
    }

    assert!(
        !sandbox.config_home().join("systemd").exists(),
        "init --no-systemd must not create a systemd unit directory"
    );
}

/// The core acceptance direction: doctor run against a fresh init reports
/// both retired-component checks as passing, carries no remediation for
/// them, and its entire report — every message and every remediation —
/// contains zero retired markers. Doctor output that starts suggesting
/// retired-component setup on a clean install is exactly the regression
/// this sweep exists to catch.
#[test]
fn fresh_doctor_passes_retired_checks_with_zero_markers() {
    let sandbox = Sandbox::new();
    let init_output = sandbox.init();
    assert!(
        init_output.status.success(),
        "the doctor run needs a completed init behind it"
    );

    let report = sandbox.doctor_report();

    // The whole report — messages and remediations alike — is marker-free.
    assert_eq!(
        marker_hits(&report.to_string()),
        Vec::<&'static str>::new(),
        "doctor output on a fresh install must not name a retired component:\n{report}"
    );

    let refs = check(&report, "retired_component_refs");
    assert_eq!(
        refs["status"].as_str(),
        Some("pass"),
        "a fresh init config must pass retired_component_refs:\n{refs}"
    );
    assert!(
        refs["remediation"].is_null(),
        "a passing check must carry no remediation:\n{refs}"
    );

    let units = check(&report, "retired_component_units");
    assert_eq!(
        units["status"].as_str(),
        Some("pass"),
        "a sandbox with no systemd dir must pass retired_component_units:\n{units}"
    );
    assert!(
        units["remediation"].is_null(),
        "a passing check must carry no remediation:\n{units}"
    );

    // The fresh config is not just marker-free, it loads: init must not
    // emit a config the daemon itself refuses.
    assert_eq!(
        check(&report, "config_parseable")["status"].as_str(),
        Some("pass"),
        "the config init writes must parse and validate"
    );

    // Belt and braces: no remediation anywhere in the report offers to
    // install or recreate anything retired. Vacuous while the report is
    // marker-free, but it keeps this test honest if a future check grows a
    // remediation that quotes an operator's stale config back at them.
    for c in report["checks"].as_array().unwrap() {
        if let Some(remediation) = c["remediation"].as_str() {
            assert!(
                suggestion_offenses(remediation).is_empty(),
                "check {} offers to install or recreate a retired component: {}",
                c["check"],
                remediation
            );
        }
    }
}

/// The first failure direction: doctor run against a config that names a
/// retired pool. The check must fail, and the remediation must demand
/// removal and bar recreation — never offer an install or re-enable path.
#[test]
fn doctor_on_retired_pool_config_demands_removal_never_reinstall() {
    let sandbox = Sandbox::new();
    assert!(sandbox.init().status.success(), "init must succeed first");

    std::fs::write(sandbox.governor_yaml(), POISONED_CONFIG).expect("write poisoned config");

    let report = sandbox.doctor_report();

    // The daemon-start path refuses the config too: the doctor check that
    // loads the config through `GovernorConfig::load_from_path` fails on it.
    assert_eq!(
        check(&report, "config_parseable")["status"].as_str(),
        Some("fail"),
        "a config naming a retired pool must be rejected at load:\n{report}"
    );

    let refs = check(&report, "retired_component_refs");
    assert_eq!(
        refs["status"].as_str(),
        Some("fail"),
        "the retired pool must be flagged:\n{report}"
    );

    let remediation = refs["remediation"]
        .as_str()
        .expect("a failed check must carry remediation");
    let lowered = remediation.to_lowercase();
    assert!(
        lowered.contains("remove"),
        "the remediation must demand removal: {remediation}"
    );
    assert!(
        lowered.contains("do not recreate"),
        "the remediation must bar recreation: {remediation}"
    );
    assert_eq!(
        suggestion_offenses(remediation),
        Vec::<String>::new(),
        "the refs remediation must not offer to install or recreate: {remediation}"
    );
}

/// The second failure direction: retired units physically planted in the
/// systemd user dir, as an install from before the retirement would leave
/// them. Doctor must demand removal (with the do-not-recreate bar), never
/// offer to re-enable or reinstall them.
#[test]
fn doctor_on_planted_retired_units_demands_removal_never_reinstall() {
    let sandbox = Sandbox::new();
    assert!(sandbox.init().status.success(), "init must succeed first");

    let unit_dir = sandbox.config_home().join("systemd").join("user");
    std::fs::create_dir_all(&unit_dir).expect("create systemd user dir");
    for (unit, body) in [
        (RETIRED_UNIT_SERVICE, "[Service]\nExecStart=polish-seeder\n"),
        (RETIRED_UNIT_TIMER, "[Timer]\nOnCalendar=hourly\n"),
    ] {
        std::fs::write(unit_dir.join(unit), body)
            .unwrap_or_else(|e| panic!("plant {unit}: {e}"));
    }

    let report = sandbox.doctor_report();

    let units = check(&report, "retired_component_units");
    assert_eq!(
        units["status"].as_str(),
        Some("fail"),
        "the planted retired units must be flagged:\n{report}"
    );

    let message = units["message"].as_str().expect("failure message");
    for unit in [RETIRED_UNIT_SERVICE, RETIRED_UNIT_TIMER] {
        assert!(
            message.contains(unit),
            "the failure must name {unit}: {message}"
        );
    }

    let remediation = units["remediation"]
        .as_str()
        .expect("a failed check must carry remediation");
    let lowered = remediation.to_lowercase();
    assert!(
        lowered.contains("remove"),
        "the remediation must demand removal: {remediation}"
    );
    assert!(
        lowered.contains("do not recreate"),
        "the remediation must bar recreation: {remediation}"
    );
    assert_eq!(
        suggestion_offenses(remediation),
        Vec::<String>::new(),
        "the units remediation must not offer to install or re-enable: {remediation}"
    );
}

/// The file half of the sweep — the manual grep over the embedded templates
/// and deploy scripts, executed against bytes compiled into this test. Every
/// file init embeds and everything under deploy/ must be entirely free of
/// the retired-component vocabulary.
#[test]
fn embedded_templates_and_deploy_scripts_are_marker_free() {
    for (name, contents) in SWEPT_FILES {
        assert_eq!(
            marker_hits(contents),
            Vec::<&'static str>::new(),
            "{name} references a component retired 2026-09-16"
        );
    }
}

/// Pin the suggestion detector's own semantics, so a future edit to the verb
/// or negator lists cannot silently turn it into something that passes
/// everything (or flags everything). The live remediation texts are exercised
/// against the real binary by the two failure-direction tests above; these
/// cases cover the shapes those runs cannot reach.
#[test]
fn suggestion_detector_flags_offers_and_spares_prohibitions() {
    // Plain offers to bring a retired component back must be flagged.
    assert!(
        !suggestion_offenses("Run cgov init to reinstall claude-polish-seeder.service").is_empty(),
        "an affirmative reinstall suggestion must be flagged"
    );
    assert!(
        !suggestion_offenses("Enable the generator-pool timer").is_empty(),
        "an affirmative enable suggestion must be flagged"
    );

    // Prohibitions and history must not be flagged.
    assert!(
        suggestion_offenses("Do not recreate the polish queue").is_empty(),
        "a negated recreate must not be flagged"
    );
    assert!(
        suggestion_offenses("Do not re-enable the polish units").is_empty(),
        "a negated re-enable must not be flagged"
    );
    assert!(
        suggestion_offenses("The polish loop retired 2026-09-16; NEEDLE strands replaced it")
            .is_empty(),
        "history naming a retired component carries no suggestion"
    );

    // Word boundaries: `disable` contains the letters of `enable` but is the
    // opposite instruction — the detector must not flag a disable step just
    // because the sentence also names a retired component.
    assert!(
        suggestion_offenses("Disable the polish timer with systemctl --user disable").is_empty(),
        "disable must not trip the enable verb"
    );
}
