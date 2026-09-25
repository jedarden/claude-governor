//! Hermetic verification of `scripts/verify-pluck-config.sh`
//! (claudego-d1a2e865).
//!
//! The script is this repository's authoritative live-state check — the one
//! command that answers "which config path / default workspace / backend /
//! store layout / CLI contract is current?" (docs/bead-visibility-troubleshooting.md).
//! Its verdicts steer documentation reconciliation, so a silent regression in
//! the script would misdirect every future drift investigation. These tests
//! pin its detection behavior by running it, unchanged, against isolated
//! sandboxes: a throwaway `HOME` for the config paths, a workspace with a
//! constructed bead-rs store layout, and `needle`/`bead` test doubles on
//! PATH whose output each scenario controls. One scenario at a time breaks
//! exactly one input, and the test asserts the script notices — the
//! detection surface the task names: incorrect backend bindings, config
//! paths, workspace resolution, bead-rs layouts, CLI output changes, and the
//! exact exclusion-label semantics (`[]` means "built-in default set", never
//! "exclude nothing").
//!
//! Everything is asserted against the script's real PASS/FAIL/note lines and
//! its `== N passed, M failed ==` summary, so both the failure attribution
//! and the failure *counting* are pinned. The host's real `bead` and `needle`
//! are never invoked: the sandbox PATH puts the stub bin first, and the one
//! scenario that asserts their absence strips every PATH entry carrying
//! either binary. The script needs the host toolchain (grep, sed, jq), so
//! the remaining PATH entries are inherited from the test process — the same
//! entries `cargo test` itself needs. `jq` must resolve or every CLI-shape
//! check degrades; that prerequisite is guarded loudly in the sandbox
//! constructor rather than left to fail as a confusing cascade.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// The shipped validator, embedded at compile time — the tested text cannot
/// drift from the shipped one, and nothing is read from CARGO_MANIFEST_DIR at
/// run time (close-gate test binaries are reused from a shared cache where
/// the extraction may already be deleted).
const VERIFY_SH: &str = include_str!("../scripts/verify-pluck-config.sh");

// -- fixtures ---------------------------------------------------------------

/// The workspace's entire `.needle.yaml`: the backend binding the script's
/// check 1 demands.
const VALID_BACKEND_YAML: &str = "bead_cli:\n  backend: bead-rs\n";

/// The superseded binding docs/bf-2bxsv.md used to describe — the script must
/// call it out, not accept it.
const FORGE_BACKEND_YAML: &str = "bead_cli:\n  backend: bf\n";

/// The active config's `strands.pluck` section exactly as deployed
/// (docs/plan/pluck-configuration.md): an explicitly empty exclude_labels
/// list, which PluckStrand replaces with its built-in default set.
const VALID_CONFIG_YAML: &str = "strands:\n  pluck:\n    exclude_labels: []\n    split_after_failures: 3\n    persistent_starvation_records: true\n";

/// A non-empty exclude_labels list: the exact-label semantics under test —
/// these three labels, verbatim, not the built-in default set.
const EXPLICIT_LABELS_CONFIG_YAML: &str = "strands:\n  pluck:\n    exclude_labels:\n      - deferred\n      - human\n      - blocked\n    split_after_failures: 3\n";

/// Config without a strands.pluck section: PluckConfig defaults must apply,
/// and the script must say so in a note rather than failing.
const NO_PLUCK_CONFIG_YAML: &str = "daemon:\n  interval_secs: 60\n";

/// Legacy config carrying the marker — the exact text the script greps for —
/// and so tolerated, with a note.
const LEGACY_CONFIG_WITH_MARKER: &str = "# LEGACY v1 layout — the v2 loader does NOT read it; kept so stale tooling fails loudly.\nworkspace:\n  default: /home/coding/claude-governor\n";

/// The same file minus the marker — indistinguishable from a live v1 config
/// that the loader might misread, which is exactly what check 2 refuses.
const LEGACY_CONFIG_WITHOUT_MARKER: &str =
    "# kept from the v1 layout\nworkspace:\n  default: /home/coding/claude-governor\n";

/// The ready-frontier JSONL line the `bead` double emits: one JSON object per
/// line — the bead-rs CLI contract the script's check 5 pins.
const BEAD_ID: &str = "cgtest-d1a2e865";
const READY_JSONL_LINE: &str =
    "{\"id\":\"cgtest-d1a2e865\",\"status\":\"open\",\"priority\":2,\"title\":\"fixture\"}";

/// `bead show ID --json` must emit a JSON *array* (pipelined through
/// `jq '.[0]'` by consumers) — this is the correct shape.
const SHOW_JSON_ARRAY: &str = "[{\"id\":\"cgtest-d1a2e865\",\"status\":\"open\"}]";

/// The CLI-output drift scenarios: a release that starts emitting a top-level
/// array from `list` or a bare object from `show` breaks every consumer that
/// trusts the pinned contract; the script must fail loudly on both.
const LIST_JSON_ARRAY: &str = "[{\"id\":\"cgtest-d1a2e865\",\"status\":\"open\"}]";
const SHOW_JSON_OBJECT: &str = "{\"id\":\"cgtest-d1a2e865\",\"status\":\"open\"}";

/// Test double for the `needle` CLI. Only `config --get workspace.default`
/// is exercised; the answer and the exit status are driven by FAKE_NEEDLE_*
/// variables, and every invocation is logged so tests can assert on the
/// script's actual CLI usage. Builtins and printf only — it runs under the
/// script's environment with an unmodified shell.
const NEEDLE_STUB: &str = r#"#!/usr/bin/env sh
# Test double for `needle` (see the Rust test file for the contract).
printf '%s\n' "needle $*" >> "$FAKE_CLI_LOG"
if [ "${FAKE_NEEDLE_FAIL:-0}" = "1" ]; then
    echo "fake-needle: simulated failure" >&2
    exit 1
fi
if [ "$1" = "config" ] && [ "$2" = "--get" ] && [ "$3" = "workspace.default" ]; then
    printf '%s\n' "${FAKE_NEEDLE_WORKSPACE_DEFAULT:-}"
    exit 0
fi
echo "fake-needle: unexpected invocation: $*" >&2
exit 64
"#;

/// Test double for the `bead` CLI (bead-rs store). `list --ready` /
/// `list --status open` / `show ID --json` each emit canned output from
/// FAKE_BEAD_* variables; every invocation is logged. Anything else is an
/// unexpected contract surface and fails loudly.
const BEAD_STUB: &str = r#"#!/usr/bin/env sh
# Test double for `bead` (see the Rust test file for the contract).
printf '%s\n' "bead $*" >> "$FAKE_CLI_LOG"
if [ "$1" = "show" ]; then
    printf '%s\n' "${FAKE_BEAD_SHOW_JSON:-}"
    exit 0
fi
if [ "$1" = "list" ]; then
    for arg in "$@"; do
        if [ "$arg" = "--ready" ]; then
            printf '%s\n' "${FAKE_BEAD_READY_JSONL:-}"
            exit 0
        fi
        if [ "$arg" = "--status" ]; then
            printf '%s\n' "${FAKE_BEAD_OPEN_JSONL:-}"
            exit 0
        fi
    done
fi
echo "fake-bead: unexpected invocation: $*" >&2
exit 64
"#;

// -- sandbox ----------------------------------------------------------------

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
}

/// The PATH the script's own text processing needs, inherited from the test
/// process. The stub bin is prepended for scenarios where the doubles must
/// win; it is omitted (and host entries carrying `bead`/`needle` stripped)
/// for the not-on-PATH scenario.
fn inherited_path_dirs() -> Vec<PathBuf> {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Host PATH entries removed for the absence scenario: any directory that
/// would resolve the real `bead` or `needle` (~/.cargo/bin, ~/.local/bin on
/// the fleet boxes).
fn inherited_path_without_cli_tools() -> String {
    inherited_path_dirs()
        .into_iter()
        .filter(|d| !d.join("bead").exists() && !d.join("needle").exists())
        .map(|d| d.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

fn path_with_stub_dir(stub_bin: &Path) -> String {
    let mut dirs = vec![stub_bin.to_string_lossy().into_owned()];
    dirs.extend(
        inherited_path_dirs()
            .into_iter()
            .map(|d| d.to_string_lossy().into_owned()),
    );
    dirs.join(":")
}

/// One throwaway world for the script: a `HOME` holding the active and legacy
/// NEEDLE configs, a workspace with a constructed bead-rs store layout, and a
/// stub bin with the `needle`/`bead` doubles. Every default is a fully
/// current configuration — the script must exit 0 on an unmutated sandbox —
/// and each failing test mutates exactly one input.
struct PluckSandbox {
    dir: TempDir,
}

impl PluckSandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("sandbox tmpdir");

        // The script materialized under its own scripts/ path shape, so its
        // identity (and any relative assumptions) match the repo layout.
        fs::create_dir_all(dir.path().join("scripts")).expect("scripts dir");
        write_executable(
            &dir.path().join("scripts/verify-pluck-config.sh"),
            VERIFY_SH.as_bytes(),
        );

        // The doubles. PATH ordering (stub bin first) makes these the
        // `bead`/`needle` the script resolves.
        fs::create_dir(dir.path().join("bin")).expect("stub bin dir");
        write_executable(&dir.path().join("bin/needle"), NEEDLE_STUB.as_bytes());
        write_executable(&dir.path().join("bin/bead"), BEAD_STUB.as_bytes());

        // Active + legacy config under the sandbox HOME.
        fs::create_dir_all(dir.path().join("home/.config/needle")).expect("config dir");
        fs::write(
            dir.path().join("home/.config/needle/config.yaml"),
            VALID_CONFIG_YAML,
        )
        .expect("active config");
        fs::create_dir_all(dir.path().join("home/.needle")).expect("legacy dir");
        fs::write(
            dir.path().join("home/.needle/config.yaml"),
            LEGACY_CONFIG_WITH_MARKER,
        )
        .expect("legacy config");

        // bead-rs store layout in the workspace: SQLite live store, durable
        // checkpoint dir, workspace identity — and no bf-era flat JSONL.
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join(".beads/checkpoint")).expect("checkpoint dir");
        fs::write(ws.join(".needle.yaml"), VALID_BACKEND_YAML).expect("backend binding");
        fs::write(ws.join(".beads/beads.db"), "SQLite format 3 fixture").expect("beads.db");
        fs::write(
            ws.join(".beads/config.json"),
            "{\"workspace\":\"fixture\",\"backend\":\"bead-rs\"}",
        )
        .expect("store config.json");

        // jq backs every CLI-shape check; if the test environment cannot
        // resolve it the scenario is an environment problem and must say so
        // instead of failing as a pile of unrelated shape assertions.
        let probe = Command::new("sh")
            .arg("-c")
            .arg("command -v jq >/dev/null 2>&1")
            .env("PATH", path_with_stub_dir(&dir.path().join("bin")))
            .status()
            .expect("probe PATH for jq");
        assert!(
            probe.success(),
            "jq does not resolve on this host's PATH; the script's CLI-contract \
             checks cannot run here — install jq or fix PATH"
        );

        PluckSandbox { dir }
    }

    fn script(&self) -> PathBuf {
        self.dir.path().join("scripts/verify-pluck-config.sh")
    }

    fn workspace(&self) -> PathBuf {
        self.dir.path().join("ws")
    }

    fn active_config(&self) -> PathBuf {
        self.dir.path().join("home/.config/needle/config.yaml")
    }

    fn legacy_config(&self) -> PathBuf {
        self.dir.path().join("home/.needle/config.yaml")
    }

    fn cli_log(&self) -> PathBuf {
        self.dir.path().join("cli.log")
    }

    /// Run the materialized script against the sandbox workspace with the
    /// sandbox HOME, the doubles on PATH, and the default canned CLI answers.
    /// `tweak` rewrites single environment variables for one scenario.
    fn run_with(&self, tweak: impl FnOnce(&mut Command)) -> Run {
        let ws = self.workspace();
        let mut cmd = Command::new("bash");
        cmd.arg(self.script())
            .arg(&ws)
            .env("HOME", self.dir.path().join("home"))
            .env("PATH", path_with_stub_dir(&self.dir.path().join("bin")))
            .env("FAKE_CLI_LOG", self.cli_log())
            // The double answers with the sandbox workspace itself, so the
            // unmutated sandbox resolves cleanly.
            .env("FAKE_NEEDLE_WORKSPACE_DEFAULT", &ws)
            .env("FAKE_BEAD_READY_JSONL", READY_JSONL_LINE)
            .env("FAKE_BEAD_OPEN_JSONL", READY_JSONL_LINE)
            .env("FAKE_BEAD_SHOW_JSON", SHOW_JSON_ARRAY);
        tweak(&mut cmd);
        let out = cmd.output().expect("spawn the verification script");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Run {
            code: out.status.code(),
            out: text,
        }
    }

    fn run(&self) -> Run {
        self.run_with(|_| {})
    }

    /// The not-on-PATH scenario: no stub bin, and no host PATH entry that
    /// resolves the real `bead` or `needle`.
    fn run_without_cli_tools_on_path(&self) -> Run {
        self.run_with(|cmd| {
            cmd.env("PATH", inherited_path_without_cli_tools());
        })
    }

    fn set_backend_yaml(&self, content: &str) {
        fs::write(self.workspace().join(".needle.yaml"), content).expect("rewrite .needle.yaml");
    }

    fn remove_backend_yaml(&self) {
        fs::remove_file(self.workspace().join(".needle.yaml")).expect("remove .needle.yaml");
    }

    fn remove_active_config(&self) {
        fs::remove_file(self.active_config()).expect("remove active config");
    }

    fn set_active_config(&self, content: &str) {
        fs::write(self.active_config(), content).expect("rewrite active config");
    }

    fn set_legacy_config(&self, content: &str) {
        fs::write(self.legacy_config(), content).expect("rewrite legacy config");
    }

    fn remove_store_piece(&self, rel: &str) {
        let p = self.workspace().join(".beads").join(rel);
        if p.is_dir() {
            fs::remove_dir_all(&p).expect("remove store dir");
        } else {
            fs::remove_file(&p).expect("remove store file");
        }
    }

    fn add_bf_era_flat_store(&self) {
        fs::write(
            self.workspace().join(".beads/issues.jsonl"),
            "{\"id\":\"bf-156nn7\",\"status\":\"open\"}\n",
        )
        .expect("write bf-era flat store");
    }

    fn write_starvation_events(&self) {
        let state = self.dir.path().join("home/.needle/state");
        fs::create_dir_all(&state).expect("state dir");
        fs::write(
            state.join("starvation_events.jsonl"),
            "{\"kind\":\"open_beads\"}\n",
        )
        .expect("starvation events");
    }
}

struct Run {
    code: Option<i32>,
    out: String,
}

impl Run {
    /// The script's own tally line, verbatim — both the failure attribution
    /// and the failure counting are under test.
    fn summary(&self) -> &str {
        self.out
            .lines()
            .find(|l| l.starts_with("== ") && l.contains(" passed, ") && l.ends_with(" =="))
            .unwrap_or("<no summary line found>")
    }

    fn expect_summary(&self, passed: u32, failed: u32) {
        assert_eq!(
            self.summary(),
            format!("== {passed} passed, {failed} failed =="),
            "summary tally mismatch\nfull output:\n{}",
            self.out
        );
    }

    fn expect_exit(&self, code: i32) {
        assert_eq!(
            self.code,
            Some(code),
            "exit code mismatch\nfull output:\n{}",
            self.out
        );
    }
}

// -- check 1: backend binding ----------------------------------------------

#[test]
fn a_fully_current_configuration_passes_every_check() {
    let sb = PluckSandbox::new();
    let run = sb.run();

    run.expect_exit(0);
    run.expect_summary(15, 0);
    assert!(
        run.out.contains("Current-state snapshot verified"),
        "the all-clear verdict line must appear:\n{}",
        run.out
    );
    for pass_line in [
        "PASS: backend:",
        "PASS: config: active config at ",
        "PASS: workspace: workspace.default resolves to ",
        "PASS: store:",
        "PASS: cli:",
        "PASS: pluck: strands.pluck section present",
        "PASS: pluck: split_after_failures = 3",
        "PASS: pluck: persistent_starvation_records = true",
    ] {
        assert!(
            run.out.contains(pass_line),
            "expected `{pass_line}`:\n{}",
            run.out
        );
    }
    // The legacy file is present WITH the marker: tolerated, with the note.
    assert!(
        run.out.contains("exists and carries the legacy-v1 marker"),
        "a marked legacy config must produce the explanatory note:\n{}",
        run.out
    );
    // The `[]` semantics note — the exact exclusion-label contract.
    assert!(
        run.out
            .contains("exclude_labels is [] — PluckStrand substitutes its built-in default set"),
        "an empty configured list must be explained as built-in-default substitution:\n{}",
        run.out
    );
    // No starvation snapshot was written in this sandbox: a note, not a
    // failure (pinned for real in diagnostics_absence_never_fails_the_script).
    assert!(
        run.out.contains("no snapshot written yet"),
        "an absent diagnostics snapshot must be a note:\n{}",
        run.out
    );

    // The script must have exercised the real contract surfaces in order:
    // the workspace query, the ready-frontier JSONL, and the show call with
    // the id it parsed out of the ready line. The open-list fallback exists
    // only for an id-less ready line and must not have run.
    let log = fs::read_to_string(sb.cli_log()).expect("cli log");
    assert!(
        log.contains("needle config --get workspace.default"),
        "the workspace query must go through `needle config --get`:\n{log}"
    );
    assert!(
        log.contains("bead list --ready --json --limit 1"),
        "the ready frontier must be read via `bead list --ready --json`:\n{log}"
    );
    assert!(
        log.contains(&format!("bead show {BEAD_ID} --json")),
        "`bead show --json` must run against the id parsed from the ready line:\n{log}"
    );
    assert!(
        !log.contains("--status open"),
        "the open-list fallback must not run when the ready line carried an id:\n{log}"
    );
}

#[test]
fn a_missing_backend_declaration_is_detected() {
    let sb = PluckSandbox::new();
    sb.remove_backend_yaml();
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("does not declare 'backend: bead-rs'"),
        "a workspace without a backend binding must be named:\n{}",
        run.out
    );
}

#[test]
fn a_non_bead_rs_backend_declaration_is_detected() {
    let sb = PluckSandbox::new();
    sb.set_backend_yaml(FORGE_BACKEND_YAML);
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("does not declare 'backend: bead-rs'"),
        "a bead-forge binding must be rejected, not silently accepted:\n{}",
        run.out
    );
}

// -- check 2: config paths ---------------------------------------------------

#[test]
fn a_missing_active_config_is_detected() {
    let sb = PluckSandbox::new();
    sb.remove_active_config();
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(11, 1);
    assert!(
        run.out.contains("config:"),
        "the failure must be attributed to the config check:\n{}",
        run.out
    );
    assert!(
        run.out.contains("missing"),
        "the missing active config must be named:\n{}",
        run.out
    );
    // The strands.pluck values come from the same file: with it gone they
    // must degrade to notes, never to extra failures.
    assert!(
        run.out.contains("PluckConfig defaults apply"),
        "a missing config must fall back to the defaults note:\n{}",
        run.out
    );
}

#[test]
fn a_legacy_config_without_the_v1_marker_is_detected() {
    let sb = PluckSandbox::new();
    sb.set_legacy_config(LEGACY_CONFIG_WITHOUT_MARKER);
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(15, 1);
    assert!(
        run.out.contains("exists without the legacy-v1 marker"),
        "an unmarked legacy config must be flagged as potentially misreadable:\n{}",
        run.out
    );
}

#[test]
fn a_failing_needle_config_query_is_detected() {
    let sb = PluckSandbox::new();
    let run = sb.run_with(|cmd| {
        cmd.env("FAKE_NEEDLE_FAIL", "1");
    });

    run.expect_exit(1);
    run.expect_summary(13, 2);
    assert!(
        run.out
            .contains("'needle config --get workspace.default' failed"),
        "a v2 loader that cannot answer must fail the config check:\n{}",
        run.out
    );
    // The workspace comparison is fed by the same query: it must fail too,
    // not silently compare empty strings into a pass.
    assert!(
        run.out.contains("workspace: workspace.default is"),
        "the workspace resolution must fail when the query behind it fails:\n{}",
        run.out
    );
}

// -- check 3: workspace resolution ------------------------------------------

#[test]
fn a_workspace_default_mismatch_is_detected() {
    let sb = PluckSandbox::new();
    // The realistic drift: the loader still points at this repository while
    // the script was invoked against some other workspace.
    let run = sb.run_with(|cmd| {
        cmd.env(
            "FAKE_NEEDLE_WORKSPACE_DEFAULT",
            "/home/coding/claude-governor",
        );
    });
    let ws = sb.workspace();

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains(&format!(
            "workspace.default is '/home/coding/claude-governor', expected '{}'",
            ws.display()
        )),
        "the mismatch must name both the resolved and expected paths:\n{}",
        run.out
    );
}

// -- check 4: bead-rs store layout ------------------------------------------

#[test]
fn a_missing_beads_db_is_detected() {
    let sb = PluckSandbox::new();
    sb.remove_store_piece("beads.db");
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("beads.db missing"),
        "the missing SQLite live store must be named:\n{}",
        run.out
    );
}

#[test]
fn a_missing_checkpoint_dir_is_detected() {
    let sb = PluckSandbox::new();
    sb.remove_store_piece("checkpoint");
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("checkpoint/ missing"),
        "the missing durable checkpoint dir must be named:\n{}",
        run.out
    );
}

#[test]
fn a_missing_store_config_json_is_detected() {
    let sb = PluckSandbox::new();
    sb.remove_store_piece("config.json");
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("config.json missing"),
        "the missing workspace identity file must be named:\n{}",
        run.out
    );
}

#[test]
fn a_bf_era_flat_store_is_detected() {
    let sb = PluckSandbox::new();
    sb.add_bf_era_flat_store();
    let run = sb.run();

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("exists — bf-era flat store"),
        "the flat bf-era store must be flagged even alongside a beads.db:\n{}",
        run.out
    );
}

// -- check 5: CLI contract / output changes ---------------------------------

#[test]
fn a_bead_list_that_emits_a_json_array_is_detected() {
    let sb = PluckSandbox::new();
    // A CLI change from JSONL to a top-level array is exactly the drift this
    // check exists for: the first line is then not one object.
    let run = sb.run_with(|cmd| {
        cmd.env("FAKE_BEAD_READY_JSONL", LIST_JSON_ARRAY);
    });

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("first line is not a JSON object"),
        "an array-shaped `bead list --json` must fail the JSONL check:\n{}",
        run.out
    );
}

#[test]
fn a_bead_show_that_emits_a_bare_object_is_detected() {
    let sb = PluckSandbox::new();
    let run = sb.run_with(|cmd| {
        cmd.env("FAKE_BEAD_SHOW_JSON", SHOW_JSON_OBJECT);
    });

    run.expect_exit(1);
    run.expect_summary(14, 1);
    assert!(
        run.out.contains("did not emit a JSON array"),
        "an object-shaped `bead show --json` must fail the array check:\n{}",
        run.out
    );
}

#[test]
fn a_bead_cli_absent_from_path_is_detected() {
    let sb = PluckSandbox::new();
    let run = sb.run_without_cli_tools_on_path();

    run.expect_exit(1);
    // Four checks fail together (the bead lookup, the JSONL probe fed by the
    // missing CLI, and the needle query + workspace comparison fed by the
    // missing needle), and the show check degrades to a note — the exact
    // cascade a missing CLI produces.
    run.expect_summary(10, 4);
    assert!(
        run.out.contains("cli: bead not on PATH"),
        "a missing bead binary must fail the CLI check:\n{}",
        run.out
    );
    assert!(
        run.out.contains("no open/ready bead available"),
        "with no CLI output at all the show check must degrade to a note:\n{}",
        run.out
    );
}

#[test]
fn a_silent_bead_cli_is_detected() {
    let sb = PluckSandbox::new();
    // A CLI that exits 0 with no output (the wrapper-discards-stderr failure
    // shape) must not pass as a healthy empty frontier.
    let run = sb.run_with(|cmd| {
        cmd.env("FAKE_BEAD_READY_JSONL", "");
        cmd.env("FAKE_BEAD_OPEN_JSONL", "");
    });

    run.expect_exit(1);
    run.expect_summary(13, 1);
    assert!(
        run.out.contains("first line is not a JSON object: <empty>"),
        "total CLI silence must be reported as an empty first line:\n{}",
        run.out
    );
}

// -- check 6: exact exclusion-label semantics --------------------------------

#[test]
fn an_explicit_exclude_labels_list_is_reported_verbatim() {
    let sb = PluckSandbox::new();
    sb.set_active_config(EXPLICIT_LABELS_CONFIG_YAML);
    let run = sb.run();

    run.expect_exit(0);
    run.expect_summary(14, 0);
    // The configured labels are the exclusion set — printed as configured,
    // with none of the built-in-default substitution language.
    assert!(
        run.out.contains("- deferred")
            && run.out.contains("- human")
            && run.out.contains("- blocked"),
        "a configured list must be echoed verbatim:\n{}",
        run.out
    );
    assert!(
        !run.out.contains("substitutes its built-in default set"),
        "a non-empty list must never be described as default substitution:\n{}",
        run.out
    );
}

#[test]
fn a_missing_pluck_section_falls_back_to_defaults() {
    let sb = PluckSandbox::new();
    sb.set_active_config(NO_PLUCK_CONFIG_YAML);
    let run = sb.run();

    run.expect_exit(0);
    run.expect_summary(12, 0);
    assert!(
        run.out.contains("no strands.pluck section"),
        "an absent section must be reported:\n{}",
        run.out
    );
    assert!(
        run.out.contains("PluckConfig defaults apply"),
        "the fallback must be named as PluckConfig defaults:\n{}",
        run.out
    );
}

// -- check 7: diagnostics snapshot -------------------------------------------

#[test]
fn a_diagnostics_snapshot_present_is_a_pass() {
    let sb = PluckSandbox::new();
    sb.write_starvation_events();
    let run = sb.run();

    run.expect_exit(0);
    run.expect_summary(16, 0);
    assert!(
        run.out.contains("PASS: diagnostics:")
            && run.out.contains("starvation_events.jsonl present"),
        "a written snapshot must be an explicit pass:\n{}",
        run.out
    );
}
