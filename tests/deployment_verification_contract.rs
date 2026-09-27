//! Contract pin for `docs/notes/deployment-verification.md`, the designated
//! deployment-verification procedure (claudego-b1f7b8ea).
//!
//! docs/notes/bf-48qtz.md designates the runbook as "the reproducible,
//! non-rotting verification procedure" to run whenever deployed governor
//! health must be checked. But unlike `scripts/verify-pluck-config.sh` —
//! whose `--self-test` mode pins its own check inventory and which
//! tests/pluck_config_selftest_test.rs drills with mutated copies — nothing
//! pinned the runbook itself, so the rot the designation warns about was one
//! un-flagged rename away: a `cgov` invocation edited past the CLI it
//! invokes, a systemd unit renamed, a doctor check id rechristened — and the
//! runbook keeps calling itself reproducible while its steps silently stop
//! working.
//!
//! This file gives the runbook the same self-test treatment, in five gates:
//!
//! 1. *every command parses* — each `cgov …` invocation the runbook carries
//!    (fenced blocks and inline code) is extracted from its own text and
//!    parsed against the real binary: `<args…> --help` exits 0 only when the
//!    subcommand and every flag still exist, so a command that rots fails
//!    here without this file being told;
//! 2. *the documented inventory survives* — the invocations the procedure
//!    relies on must still be findable in the text, so a step cannot be
//!    silently dropped;
//! 3. *the topology is real* — every systemd unit the runbook names must be
//!    a unit the repo actually ships (compiled in, so a renamed template
//!    fails at compile time) or one of doctor's legacy monolith names, every
//!    shipped unit must still be named, and each shipped unit's ExecStart
//!    must parse against the binary;
//! 4. *the criteria tables name real checks* — every check id in the
//!    pass/fail tables must exist in a live `cgov doctor --json` inventory,
//!    and every documented id must still be covered by the tables;
//! 5. *the designation and remediation resolve* — bf-48qtz.md must still
//!    designate this file, and the `claude_print_adapters` remediation must
//!    still point at the shipped installer.
//!
//! Mutation drills at the bottom prove each gate fires on mutated copies of
//! the runbook, in the shape of tests/pluck_config_selftest_test.rs.
//!
//! Runtime isolation follows tests/retired_component_surface_sweep.rs: every
//! child `cgov` runs with `HOME` and the XDG dirs pointed into a fresh
//! `TempDir`, so nothing in the operator's real config is read or written.
//! Doctor runs with `--skip-live` (no subscription dispatch); its exit code
//! is irrelevant — a bare sandbox fails unrelated checks — only the parsed
//! JSON check inventory is read. All runbook/doc/unit bytes are compiled in
//! (`include_str!`), because close-gate binaries are reused from a shared
//! cache across extractions where a runtime read can point at a deleted
//! checkout.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;
use tempfile::TempDir;

/// The designated runbook, embedded at compile time.
const RUNBOOK: &str = include_str!("../docs/notes/deployment-verification.md");

/// The doc that designates the runbook as authoritative. Its continued
/// existence and designation are part of the contract.
const DESIGNATOR: &str = include_str!("../docs/notes/bf-48qtz.md");

/// The remediation path the criteria tables send operators to.
const INSTALLER_SH: &str = include_str!("../deploy/install-claude-print-adapters.sh");

/// The systemd unit templates the repo actually ships. Compiled in, so a
/// renamed or removed template fails before the test even runs.
const SHIPPED_UNIT_TEMPLATES: [&str; 3] = [
    include_str!("../config/claude-governor-observe.service"),
    include_str!("../config/claude-governor-act.service"),
    include_str!("../config/claude-token-collector.service"),
];

const SHIPPED_UNIT_NAMES: [&str; 3] = [
    "claude-governor-observe.service",
    "claude-governor-act.service",
    "claude-token-collector.service",
];

/// doctor's legacy monolith units (`MONOLITH_SERVICES` in src/doctor.rs). A
/// live monolith running the enforcing daemon satisfies both the observe and
/// act checks, so the runbook's topology table may name these; any other
/// unit name it uses must be a shipped unit.
const LEGACY_UNIT_NAMES: [&str; 2] = ["claude-governor.service", "cgov.service"];

/// The invocations the procedure relies on, as extracted token lists (the
/// leading `cgov` is implied). Each must (a) parse against the real binary
/// and (b) still be findable in the runbook's own text.
const DOCUMENTED_INVOCATIONS: [&[&str]; 10] = [
    &["doctor"],
    &["doctor", "--json"],
    &["status"],
    &["status", "--summary"],
    &["workers"],
    &["config"],
    &["start", "observe"],
    &["start", "act"],
    &["start", "collector"],
    &["enable"],
];

/// The doctor mode the step-2 prose documents rather than invokes:
/// `--skip-live` never appears as a contiguous `cgov …` invocation in the
/// text, but it is part of the procedure's contract — pinned as a phrase in
/// the step-2 region and parsed against the binary directly.
const DOCTOR_FLAGS_IN_PROSE: [&[&str]; 1] = [&["doctor", "--skip-live"]];

/// The check ids the criteria tables are documented to cover, in table order
/// (hard failures, then expected warnings; deduplicated). Each must still be
/// named in the tables, and each must exist in the live doctor inventory.
const DOCUMENTED_CHECK_IDS: [&str; 16] = [
    "config_parseable",
    "sqlite_integrity",
    "jsonl_db_sync",
    "state_freshness",
    "observe_running",
    "collector_running",
    "api_reachability",
    "pricing_coverage",
    "disk_space",
    "claude_print_adapters",
    "prediction_accuracy",
    "burn_rate_samples",
    "oauth_token",
    "act_running",
    "alert_fp_telemetry",
    "log_file",
];

// -- extraction ----------------------------------------------------------------

/// Code segments of the runbook: fenced-block lines (with `\` continuations
/// joined) and inline backtick spans, in order. Every `cgov` invocation the
/// runbook carries lives in one of these.
fn code_segments(doc: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut in_fence = false;
    let mut pending: Option<String> = None;
    for line in doc.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            // Shell line continuation: join so a wrapped `cgov …` command is
            // one segment.
            let piece = match pending.take() {
                Some(mut prev) => {
                    prev.push(' ');
                    prev.push_str(trimmed);
                    prev
                }
                None => trimmed.to_string(),
            };
            if piece.ends_with('\\') {
                pending = Some(piece[..piece.len() - 1].to_string());
            } else if !piece.is_empty() {
                segments.push(piece);
            }
        } else {
            // Inline spans: odd indices between backticks.
            for (idx, span) in line.split('`').enumerate() {
                if idx % 2 == 1 {
                    let span = span.trim();
                    if !span.is_empty() {
                        segments.push(span.to_string());
                    }
                }
            }
        }
    }
    segments
}

/// The `cgov …` invocation a segment carries, if any: the tokens after the
/// `cgov` word, cut at the first token carrying shell syntax that ends or
/// redirects the command, so `cgov doctor --json >/dev/null; d=$?` yields
/// `["doctor", "--json"]`.
fn segment_invocation(segment: &str) -> Option<Vec<String>> {
    let mut words = segment.split_whitespace().peekable();
    loop {
        match words.peek() {
            Some(w) if *w == "cgov" => break,
            Some(_) => {
                words.next();
            }
            None => return None,
        }
    }
    words.next(); // consume `cgov`
    let mut args = Vec::new();
    for word in words {
        if word
            .chars()
            .any(|c| matches!(c, ';' | '|' | '>' | '<' | '&' | '$' | '(' | '#'))
        {
            break;
        }
        args.push(word.to_string());
    }
    if args.is_empty() {
        None // a bare `cgov` is not an invocation this pin can check
    } else {
        Some(args)
    }
}

/// Every distinct `cgov …` invocation in the runbook text.
fn extracted_invocations(doc: &str) -> Vec<Vec<String>> {
    let mut seen: Vec<Vec<String>> = Vec::new();
    for segment in code_segments(doc) {
        if let Some(inv) = segment_invocation(&segment) {
            if !seen.contains(&inv) {
                seen.push(inv);
            }
        }
    }
    seen
}

/// Every distinct `*.service` token in the runbook text — the units its
/// step-1 checks and topology table name.
fn extracted_unit_names(doc: &str) -> Vec<String> {
    let mut units = Vec::new();
    for segment in code_segments(doc) {
        for token in segment.split_whitespace() {
            let token = token.trim_matches(|c| matches!(c, '`' | ',' | ')' | '('));
            if token.ends_with(".service") && !units.iter().any(|u| u == token) {
                units.push(token.to_string());
            }
        }
    }
    units
}

/// The text between two headings (first hit each). The runbook's section
/// skeleton is itself part of the pin, so a missing heading panics rather
/// than silently auditing an empty region.
fn section<'a>(doc: &'a str, start: &str, end: &str) -> &'a str {
    let from = doc
        .find(start)
        .unwrap_or_else(|| panic!("runbook lost its `{start}` section"));
    let rest = &doc[from..];
    let to = rest.find(end).unwrap_or(rest.len());
    &rest[..to]
}

/// Inline code spans in `text` that are snake_case identifiers — the shape
/// of a doctor check id. Applied only to the criteria region, where a bare
/// snake_case span is a check id by construction.
fn snake_case_spans(text: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for (idx, span) in text.split('`').enumerate() {
        if idx % 2 != 1 {
            continue;
        }
        let span = span.trim();
        let snake = !span.is_empty()
            && span.starts_with(|c: char| c.is_ascii_lowercase())
            && span.contains('_')
            && span
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if snake && !ids.iter().any(|i| i == span) {
            ids.push(span.to_string());
        }
    }
    ids
}

// -- the live CLI surface --------------------------------------------------------

/// Child-process environment isolation: `HOME` and the XDG dirs pointed into
/// a throwaway sandbox (tests/retired_component_surface_sweep.rs recipe), so
/// a child `cgov` can never read or write the operator's real config.
fn sandboxed<'a>(command: &'a mut Command, sandbox: &Path) -> &'a mut Command {
    command
        .env("HOME", sandbox)
        .env("XDG_CONFIG_HOME", sandbox.join(".config"))
        .env("XDG_DATA_HOME", sandbox.join(".local/share"))
        .env("XDG_STATE_HOME", sandbox.join(".local/state"))
        .env("XDG_CACHE_HOME", sandbox.join(".cache"))
}

/// Run `<bin> <args…> --help` in the sandbox. Exit 0 means clap accepted the
/// subcommand and every flag — i.e. the invocation still parses against the
/// current CLI surface. (`Cli::parse()` is main's first statement, so `--help`
/// short-circuits before anything touches the filesystem.)
fn parses_against_cli(bin: &Path, sandbox: &Path, args: &[String]) -> Result<(), String> {
    let invocation = format!("cgov {}", args.join(" "));
    let output = sandboxed(
        Command::new(bin).args(args).arg("--help"),
        sandbox,
    )
    .output()
    .unwrap_or_else(|e| panic!("spawn `{invocation} --help`: {e}"));
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "runbook invocation `{invocation}` no longer parses against the cgov CLI \
             (with --help it exited {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The live doctor check inventory: every id a `cgov doctor --json` report
/// carries in a throwaway sandbox. Fetched once per process — the report's
/// exit code is irrelevant (a bare sandbox fails unrelated checks); only the
/// parsed ids are read.
fn live_check_ids(bin: &Path) -> &'static [String] {
    static IDS: OnceLock<Vec<String>> = OnceLock::new();
    IDS.get_or_init(|| {
        let sandbox = TempDir::new().expect("doctor sandbox tmpdir");
        let output = sandboxed(
            Command::new(bin).args(["doctor", "--skip-live", "--json"]),
            sandbox.path(),
        )
        .output()
        .expect("spawn `cgov doctor --skip-live --json`");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "`cgov doctor --skip-live --json` did not emit a JSON report ({e}); \
                 stdout:\n{}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        report["checks"]
            .as_array()
            .unwrap_or_else(|| panic!("doctor report carries no checks array"))
            .iter()
            .map(|check| {
                check["check"]
                    .as_str()
                    .expect("each doctor check carries an id")
                    .to_string()
            })
            .collect()
    })
    .as_slice()
}

// -- the gates --------------------------------------------------------------------

/// Run every gate over `doc` against the live binary. Returns one message
/// per failure; empty means the runbook is fully in contract.
fn audit_runbook(bin: &Path, doc: &str) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    let sandbox = TempDir::new().expect("cli sandbox tmpdir");

    // Gate 1 — every invocation extracted from the runbook parses.
    let invocations = extracted_invocations(doc);
    if invocations.is_empty() {
        failures.push(
            "no `cgov …` invocation could be extracted from the runbook — either \
             the extractor rotted or the procedure lost its commands"
                .to_string(),
        );
    }
    for invocation in &invocations {
        if let Err(msg) = parses_against_cli(bin, sandbox.path(), invocation) {
            failures.push(msg);
        }
    }

    // Gate 2 — the documented inventory is still findable in the text.
    for expected in DOCUMENTED_INVOCATIONS {
        let args: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        if !invocations.contains(&args) {
            failures.push(format!(
                "documented invocation `cgov {}` is no longer findable in the \
                 runbook — the procedure dropped a step the pin requires",
                expected.join(" ")
            ));
        }
    }
    let step_two = section(doc, "## Step 2", "## Step 3");
    for flags in DOCTOR_FLAGS_IN_PROSE {
        let args: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
        let flag = flags.last().expect("non-empty flag entry");
        if !step_two.contains(flag) {
            failures.push(format!(
                "the step-2 region no longer documents `{flag}` — the doctor \
                 contract lost a documented mode"
            ));
        }
        if let Err(msg) = parses_against_cli(bin, sandbox.path(), &args) {
            failures.push(msg);
        }
    }

    // Gate 3 — the topology is real: unit names vs shipped templates, and
    // each shipped unit's ExecStart parses against the binary.
    let named_units = extracted_unit_names(doc);
    for unit in &named_units {
        let known = SHIPPED_UNIT_NAMES.contains(&unit.as_str())
            || LEGACY_UNIT_NAMES.contains(&unit.as_str());
        if !known {
            failures.push(format!(
                "runbook names systemd unit `{unit}`, which is neither a unit the \
                 repo ships (config/*.service) nor one of doctor's legacy monolith \
                 names — the topology rotted"
            ));
        }
    }
    for unit in SHIPPED_UNIT_NAMES {
        if !named_units.iter().any(|u| u == unit) {
            failures.push(format!(
                "shipped unit `{unit}` is no longer named in the runbook — the \
                 topology pin lost a unit"
            ));
        }
    }
    for (template, name) in SHIPPED_UNIT_TEMPLATES.iter().zip(SHIPPED_UNIT_NAMES) {
        assert!(
            template.contains("[Unit]"),
            "{name} is no longer a systemd unit template"
        );
        let exec_start = template
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .unwrap_or_else(|| panic!("{name} carries no ExecStart"));
        let mut words = exec_start.split_whitespace();
        let binary = words
            .next()
            .unwrap_or_else(|| panic!("{name} ExecStart is empty"));
        assert!(
            binary.ends_with("cgov"),
            "{name} ExecStart no longer runs cgov: {binary}"
        );
        let args: Vec<String> = words.map(|w| w.to_string()).collect();
        if let Err(msg) = parses_against_cli(bin, sandbox.path(), &args) {
            failures.push(msg);
        }
    }

    // Gate 4 — the criteria tables name real checks, and every documented id
    // is still covered. Scope: the two criteria tables and their prose.
    let criteria = section(doc, "### Hard failures", "### Cascade signature");
    let named_ids = snake_case_spans(criteria);
    let live = live_check_ids(bin);
    for id in &named_ids {
        if !live.iter().any(|l| l == id) {
            failures.push(format!(
                "criteria tables name check id `{id}`, which a live `cgov doctor \
                 --json` report does not carry — the tables rotted"
            ));
        }
    }
    for id in DOCUMENTED_CHECK_IDS {
        if !named_ids.iter().any(|n| n == id) {
            failures.push(format!(
                "documented check id `{id}` is no longer named in the criteria \
                 tables — the runbook's pass/fail contract lost a check"
            ));
        }
    }

    // Gate 5 — the designation and the remediation path resolve.
    if !doc.contains("](bf-48qtz.md)") {
        failures.push(
            "the runbook no longer links its designator (bf-48qtz.md) — the claim \
             that this is *the* procedure lost its source"
                .to_string(),
        );
    }
    if !DESIGNATOR.contains("deployment-verification.md") {
        failures.push(
            "docs/notes/bf-48qtz.md no longer designates deployment-verification.md \
             as the verification procedure"
                .to_string(),
        );
    }
    if !doc.contains("deploy/install-claude-print-adapters.sh") {
        failures.push(
            "the runbook no longer names deploy/install-claude-print-adapters.sh — \
             the claude_print_adapters remediation rotted"
                .to_string(),
        );
    }
    assert!(
        !INSTALLER_SH.is_empty(),
        "the claude-print adapter installer went missing"
    );

    failures
}

// -- the shipped runbook is in contract --------------------------------------------

#[test]
fn the_shipped_runbook_passes_every_gate() {
    let failures = audit_runbook(Path::new(env!("CARGO_BIN_EXE_cgov")), RUNBOOK);
    assert!(
        failures.is_empty(),
        "the deployment-verification runbook drifted from the live surface:\n  - {}",
        failures.join("\n  - ")
    );
}

// -- mutation drills: every silent way the runbook can rot ---------------------------

/// Replace `from` with `to`, asserting the anchor occurs — a mutation against
/// a missing anchor would audit the unmutated runbook and prove nothing.
fn replace_anchor(doc: &str, from: &str, to: &str, label: &str) -> String {
    assert!(doc.contains(from), "{label} anchor missing from the runbook");
    doc.replace(from, to)
}

fn audit_failures(doc: &str) -> Vec<String> {
    audit_runbook(Path::new(env!("CARGO_BIN_EXE_cgov")), doc)
}

/// A documented step collapses to a shorter still-valid invocation: the
/// command still parses, so only the inventory gate may catch the loss.
#[test]
fn a_dropped_documented_invocation_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "cgov status --summary",
        "cgov status",
        "status --summary",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("`cgov status --summary` is no longer findable")),
        "the inventory gate must name the dropped step:\n{}",
        failures.join("\n")
    );
}

/// A flag the runbook invokes no longer exists on the CLI: the parse gate
/// must fail and name the rotted flag.
#[test]
fn a_rotted_flag_in_the_runbook_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "cgov doctor --json",
        "cgov doctor --jsonn",
        "doctor --json",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("no longer parses") && f.contains("--jsonn")),
        "the parse gate must name the rotted flag:\n{}",
        failures.join("\n")
    );
}

/// A brand-new step that was never real: appended as a fenced block so the
/// documented inventory still passes — only the parse gate fires.
#[test]
fn an_unknown_subcommand_in_the_runbook_is_detected() {
    let mutated = format!("{RUNBOOK}\n```bash\ncgov vaporize --summary\n```\n");
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("no longer parses") && f.contains("vaporize")),
        "the parse gate must name the invented subcommand:\n{}",
        failures.join("\n")
    );
}

/// A step-2 documented mode disappears from the prose: the doctor contract
/// loses a documented flag and the phrase gate must say which.
#[test]
fn a_dropped_skip_live_documentation_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "`--skip-live` omits",
        "`--no-probe` omits",
        "skip-live phrase",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("no longer documents `--skip-live`")),
        "the phrase gate must name the dropped mode:\n{}",
        failures.join("\n")
    );
}

/// A unit name drifts in the runbook: the phantom unit must be flagged, and
/// the shipped unit it displaced must be reported as lost.
#[test]
fn a_unit_rename_in_the_runbook_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "claude-governor-observe.service",
        "claude-governor-observer.service",
        "observe unit",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("`claude-governor-observer.service`")),
        "the topology gate must name the phantom unit:\n{}",
        failures.join("\n")
    );
    assert!(
        failures
            .iter()
            .any(|f| f.contains("`claude-governor-observe.service` is no longer named")),
        "the topology gate must report the shipped unit as lost:\n{}",
        failures.join("\n")
    );
}

/// A criteria-table check id is renamed to something the live doctor report
/// does not carry: the live-inventory gate must name the phantom id.
#[test]
fn a_check_id_renamed_in_the_tables_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "| `state_freshness` | state file ≥600s old",
        "| `state_freshnes` | state file ≥600s old",
        "hard-failure state_freshness row",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("`state_freshnes`") && f.contains("does not carry")),
        "the live-inventory gate must name the phantom check id:\n{}",
        failures.join("\n")
    );
}

/// A check id disappears from both criteria tables: the coverage gate must
/// report the documented id as lost.
#[test]
fn a_dropped_check_id_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "| `oauth_token` | credential file unreadable/unparseable | re-auth; collector/daemon will recover. Expiring-soon is only a WARN (auto-refresh) |",
        "",
        "hard-failure oauth_token row",
    );
    let mutated = replace_anchor(
        &mutated,
        "| `oauth_token` | expiring soon | Auto-refreshes; only a *failing* refresh is a problem |",
        "",
        "warnings oauth_token row",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("`oauth_token` is no longer named in the criteria tables")),
        "the coverage gate must report the documented id as lost:\n{}",
        failures.join("\n")
    );
}

/// The runbook stops linking its designator: the claim that this is *the*
/// procedure loses its source and the designation gate must fire.
#[test]
fn a_dropped_designation_link_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "[bf-48qtz.md](bf-48qtz.md)",
        "an archived note",
        "designation link",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("no longer links its designator")),
        "the designation gate must fire:\n{}",
        failures.join("\n")
    );
}

/// The claude_print_adapters remediation stops naming the shipped installer:
/// the remediation gate must fire.
#[test]
fn a_dropped_remediation_path_is_detected() {
    let mutated = replace_anchor(
        RUNBOOK,
        "deploy/install-claude-print-adapters.sh",
        "the adapter installer",
        "installer path",
    );
    let failures = audit_failures(&mutated);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("no longer names deploy/install-claude-print-adapters.sh")),
        "the remediation gate must fire:\n{}",
        failures.join("\n")
    );
}
