//! Drift-sensitivity pin for the installer's own bash sync gate,
//! `check_var_list_sync` in `deploy/install-claude-print-adapters.sh`.
//!
//! Two gates keep the installer's bash variable lists (`RULE3_IDE_VARS` /
//! `RULE5_API_VARS`) set-identical to the Rust constants in
//! `src/adapter_verify.rs`:
//!
//! * the cargo-test parse `installer_bash_variable_lists_match_the_rust_constants`
//!   fails when the committed arrays drift, and
//! * `check_var_list_sync` (installer section 4) fails the install on the
//!   same drift at run time.
//!
//! The first gate has unit tests. Nothing pinned the second: if
//! `check_var_list_sync` itself stopped detecting divergence — a broken
//! `diff` invocation, swapped arguments, an early `return 0` — the installer
//! would print ✓ over drifted arrays and no test anywhere would fail. These
//! tests close that gap by extracting the two arrays, the color variables and
//! the three helper functions from the installer into a temp sandbox,
//! sourcing that sandbox under `bash`, and driving `check_var_list_sync`
//! directly:
//!
//! * the committed fragments pass, printing SYNC-OK;
//! * one extra variable in a bash array fails the gate and is named in the
//!   drift report;
//! * one removed variable fails the gate the same way;
//! * the four-way drill runs both real gates against isolated Rust-side and
//!   Bash-side additions/removals and checks their diagnostics.
//!
//! The installer script itself is NEVER executed — it writes
//! `~/.config/needle/adapters` and links binaries, which a test must not do.
//! Extraction is anchor-based, not line-number-based, so edits elsewhere in
//! the script do not break the pin.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const INSTALLER: &str = "deploy/install-claude-print-adapters.sh";

/// The committed installer and the source file its gate cross-checks,
/// embedded at compile time so the test binary is self-contained. NEEDLE's
/// close gate re-runs tests through the shared /build/target-workers dir,
/// where a binary built in one `git archive` extraction is reused in another
/// (archive mtimes carry the commit time, so every extraction of one commit
/// looks fresh to cargo); a runtime read at `env!("CARGO_MANIFEST_DIR")` then
/// points at a long-deleted checkout and the gate fails with ENOENT. Same
/// defect class the stale-hold suite fixed for the controller script, pinned
/// for the lib-side drift gates in src/adapter_verify.rs on 2026-09-18.
/// Cargo tracks include_str! files as rebuild inputs, so an edit to either
/// embedded file still triggers a fresh build.
const INSTALLER_SH: &str = include_str!("../deploy/install-claude-print-adapters.sh");
const ADAPTER_VERIFY_SRC: &str = include_str!("../src/adapter_verify.rs");
const CARGO_TOML: &str = include_str!("../Cargo.toml");
const CARGO_LOCK: &str = include_str!("../Cargo.lock");
const ADAPTER_OPUS_YAML: &str = include_str!("../deploy/needle-adapters/claude-print-opus.yaml");
const ADAPTER_FABLE_YAML: &str = include_str!("../deploy/needle-adapters/claude-print-fable.yaml");
const CONFIG_GOVERNOR_YAML: &str = include_str!("../config/governor.yaml");

/// The library target needs every module declared by `src/lib.rs`. Keeping
/// these files embedded avoids reading a possibly stale checkout when cargo
/// reuses this integration-test binary from its shared target directory.
const LIB_SOURCES: &[(&str, &str)] = &[
    ("src/lib.rs", include_str!("../src/lib.rs")),
    ("src/adapter_verify.rs", ADAPTER_VERIFY_SRC),
    ("src/alerts.rs", include_str!("../src/alerts.rs")),
    ("src/burn_rate.rs", include_str!("../src/burn_rate.rs")),
    ("src/calibrator.rs", include_str!("../src/calibrator.rs")),
    (
        "src/capacity_summary.rs",
        include_str!("../src/capacity_summary.rs"),
    ),
    ("src/collector.rs", include_str!("../src/collector.rs")),
    ("src/config.rs", include_str!("../src/config.rs")),
    ("src/db.rs", include_str!("../src/db.rs")),
    ("src/doctor.rs", include_str!("../src/doctor.rs")),
    ("src/governor.rs", include_str!("../src/governor.rs")),
    (
        "src/ledger_yield.rs",
        include_str!("../src/ledger_yield.rs"),
    ),
    ("src/narrator.rs", include_str!("../src/narrator.rs")),
    ("src/poller.rs", include_str!("../src/poller.rs")),
    ("src/pricing.rs", include_str!("../src/pricing.rs")),
    ("src/schedule.rs", include_str!("../src/schedule.rs")),
    ("src/simulator.rs", include_str!("../src/simulator.rs")),
    (
        "src/snapshot_fixtures.rs",
        include_str!("../src/snapshot_fixtures.rs"),
    ),
    ("src/state.rs", include_str!("../src/state.rs")),
    (
        "src/status_display.rs",
        include_str!("../src/status_display.rs"),
    ),
    ("src/worker.rs", include_str!("../src/worker.rs")),
    (
        "src/worker_attribution.rs",
        include_str!("../src/worker_attribution.rs"),
    ),
];

/// Marker the harness prints only when the sourced gate returned success.
const OK_MARKER: &str = "SYNC-OK";

fn installer_source() -> String {
    INSTALLER_SH.to_string()
}

/// Lines from the first line declaring `name=(` (only whitespace may precede
/// the name, so comments and mid-line mentions like the printf inside
/// `scrub_missing` do not satisfy the anchor) through the first line
/// containing `)` — the installer's array shape, including wrapped bodies.
fn extract_array(src: &str, name: &str) -> String {
    let anchor = format!("{}=(", name);
    let mut lines: Vec<&str> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if !lines.is_empty() {
            lines.push(line);
            if trimmed.contains(')') {
                break;
            }
        } else if trimmed.starts_with(&anchor) {
            lines.push(line);
            if trimmed.contains(')') {
                break;
            }
        }
    }
    assert!(
        !lines.is_empty(),
        "no {}=( declaration found in {}",
        name,
        INSTALLER
    );
    lines.join("\n")
}

/// Lines from the `name() {` definition through the next line that is exactly
/// `}` at column 0 — the shape of every helper function in the installer
/// (bodies indented, closing brace flush left). Call sites (`if !
/// check_var_list_sync; then`) and comment mentions start with other text and
/// cannot satisfy the anchor.
fn extract_function(src: &str, name: &str) -> String {
    let anchor = format!("{}()", name);
    let mut lines: Vec<&str> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if !lines.is_empty() {
            lines.push(line);
            if trimmed == "}" {
                break;
            }
        } else if trimmed.starts_with(&anchor)
            && trimmed[anchor.len()..].trim_start().starts_with('{')
        {
            lines.push(line);
        }
    }
    assert!(
        !lines.is_empty(),
        "no {}() {{ definition found in {}",
        name,
        INSTALLER
    );
    lines.join("\n")
}

/// The color assignments `check_var_list_sync` interpolates into its drift
/// message (`${RED}`, `${NC}`), so the sourced copy prints exactly what the
/// installer would.
fn extract_color_vars(src: &str) -> String {
    src.lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with("RED=") || l.starts_with("NC="))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How the sandbox diverges from the committed installer fragments.
enum Mutation {
    /// Leave the extracted fragments exactly as committed.
    Clean,
    /// Append `var` to one array's elements — one side of a copy-paste edit.
    Extra {
        array: &'static str,
        var: &'static str,
    },
    /// Drop `var` from one array's elements — the other side of the same edit.
    Missing {
        array: &'static str,
        var: &'static str,
    },
}

/// Apply `mutation` to `decl` when it declares `this_array`; otherwise return
/// it untouched.
fn mutate_decl(decl: &str, mutation: &Mutation, this_array: &str) -> String {
    let (target, var, extra) = match mutation {
        Mutation::Clean => return decl.to_string(),
        Mutation::Extra { array, var } => (*array, *var, true),
        Mutation::Missing { array, var } => (*array, *var, false),
    };
    if target != this_array {
        return decl.to_string();
    }
    let base = decl.trim_end();
    assert!(
        base.ends_with(')'),
        "{} declaration lost its closing paren: {:?}",
        this_array,
        decl
    );
    let open = base.find('(').unwrap_or_else(|| {
        panic!(
            "{} declaration has no opening paren: {:?}",
            this_array, decl
        )
    });
    let name = &base[..open];
    let vars: Vec<&str> = base[open + 1..base.len() - 1].split_whitespace().collect();
    if extra {
        format!("{}({} {})", name, vars.join(" "), var)
    } else {
        let kept: Vec<&str> = vars.iter().copied().filter(|v| *v != var).collect();
        assert_eq!(
            kept.len() + 1,
            vars.len(),
            "{} is not an element of {} — pick a variable the array actually declares",
            var,
            this_array
        );
        format!("{}({})", name, kept.join(" "))
    }
}

/// Write the sandbox holding only what `check_var_list_sync` needs: `REPO_DIR`
/// (the real repo, read-only — the gate reads `src/adapter_verify.rs` from
/// it), the color vars, the two arrays, and the three helper functions. All
/// text extracted from the installer; the installer never executes.
fn write_sandbox(dir: &Path, mutation: &Mutation) -> PathBuf {
    let installer = installer_source();
    // `check_var_list_sync` extracts the Rust constants from
    // `${REPO_DIR}/src/adapter_verify.rs`, so the sandbox materializes an
    // embedded copy of that file under its own REPO_DIR rather than pointing
    // at the checkout (see INSTALLER_SH for why the checkout must not be read
    // at test runtime).
    let repo_src = dir.join("src");
    fs::create_dir_all(&repo_src).expect("create sandbox src dir");
    let verify_rs = repo_src.join("adapter_verify.rs");
    fs::write(&verify_rs, ADAPTER_VERIFY_SRC)
        .unwrap_or_else(|e| panic!("cannot write {}: {}", verify_rs.display(), e));
    let repo_dir = dir.to_str().expect("sandbox dir is valid UTF-8");
    assert!(
        !repo_dir.contains('\'') && !repo_dir.contains('\n'),
        "sandbox dir {:?} needs shell-safe quoting",
        repo_dir
    );

    let mut script = String::new();
    writeln!(&mut script, "#!/usr/bin/env bash").unwrap();
    writeln!(
        &mut script,
        "# Sandbox assembled by tests/adapter_var_sync.rs from fragments of"
    )
    .unwrap();
    writeln!(
        &mut script,
        "# {} — extraction only; the installer itself never runs.",
        INSTALLER
    )
    .unwrap();
    writeln!(&mut script, "REPO_DIR='{}'", repo_dir).unwrap();
    writeln!(&mut script, "{}", extract_color_vars(&installer)).unwrap();
    writeln!(
        &mut script,
        "{}",
        mutate_decl(
            &extract_array(&installer, "RULE3_IDE_VARS"),
            mutation,
            "RULE3_IDE_VARS"
        )
    )
    .unwrap();
    writeln!(
        &mut script,
        "{}",
        mutate_decl(
            &extract_array(&installer, "RULE5_API_VARS"),
            mutation,
            "RULE5_API_VARS"
        )
    )
    .unwrap();
    writeln!(
        &mut script,
        "{}",
        extract_function(&installer, "rust_rule_vars")
    )
    .unwrap();
    writeln!(
        &mut script,
        "{}",
        extract_function(&installer, "bash_rule_vars")
    )
    .unwrap();
    writeln!(
        &mut script,
        "{}",
        extract_function(&installer, "check_var_list_sync")
    )
    .unwrap();

    let path = dir.join("installer-fragments.sh");
    fs::write(&path, script).unwrap_or_else(|e| panic!("cannot write {}: {}", path.display(), e));
    path
}

/// Source the sandbox under `bash` and run the extracted gate. On gate success
/// the harness prints `SYNC-OK`; on gate failure the gate's own drift report
/// (which names the offending variable) is already on stdout. `set -u` makes
/// an extraction gap (a variable or function the sandbox failed to provide)
/// fail loudly instead of silently altering the gate's behaviour.
fn run_gate(sandbox: &Path) -> (bool, String) {
    let script = format!(
        "set -u; source '{}' && if check_var_list_sync; then echo {}; else exit 1; fi",
        sandbox.display(),
        OK_MARKER
    );
    let output = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap_or_else(|e| panic!("cannot spawn bash to source {}: {}", sandbox.display(), e));
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

/// One single-sided edit from the manually recorded four-way drill.
#[derive(Clone, Copy)]
enum DrillMutation {
    RustAddition,
    RustRemoval,
    BashAddition,
    BashRemoval,
}

impl DrillMutation {
    fn label(self) -> &'static str {
        match self {
            Self::RustAddition => "Rust-side addition",
            Self::RustRemoval => "Rust-side removal",
            Self::BashAddition => "Bash-side addition",
            Self::BashRemoval => "Bash-side removal",
        }
    }

    fn variable(self) -> &'static str {
        match self {
            Self::RustAddition | Self::BashAddition => "VSCODE_DRILL_PROBE",
            Self::RustRemoval => "VSCODE_CWD",
            Self::BashRemoval => "ANTHROPIC_SMALL_FAST_MODEL",
        }
    }

    fn array(self) -> &'static str {
        match self {
            Self::RustAddition | Self::RustRemoval | Self::BashAddition => "RULE3_IDE_VARS",
            Self::BashRemoval => "RULE5_API_VARS",
        }
    }

    fn rust_name(self) -> &'static str {
        match self {
            Self::RustAddition | Self::RustRemoval | Self::BashAddition => "IDE_ENV_VARS",
            Self::BashRemoval => "API_ROUTING_ENV_VARS",
        }
    }

    /// The `diff` direction emitted by the installer: `<` is the Rust side,
    /// `>` is the Bash array side.
    fn installer_diff_marker(self) -> &'static str {
        match self {
            Self::RustAddition | Self::BashRemoval => "<",
            Self::RustRemoval | Self::BashAddition => ">",
        }
    }

    fn all() -> [Self; 4] {
        [
            Self::RustAddition,
            Self::RustRemoval,
            Self::BashAddition,
            Self::BashRemoval,
        ]
    }
}

fn mutate_rust_constants(src: &str, mutation: DrillMutation) -> String {
    let (old, replacement) = match mutation {
        DrillMutation::RustAddition => (
            "    \"VSCODE_CWD\",\n];",
            "    \"VSCODE_CWD\",\n    \"VSCODE_DRILL_PROBE\",\n];",
        ),
        DrillMutation::RustRemoval => ("    \"VSCODE_CWD\",\n", ""),
        DrillMutation::BashAddition | DrillMutation::BashRemoval => return src.to_string(),
    };
    assert_eq!(
        src.matches(old).count(),
        1,
        "Rust drill anchor is not unique"
    );
    src.replacen(old, replacement, 1)
}

fn mutate_installer_source(src: &str, mutation: DrillMutation) -> String {
    let edit = match mutation {
        DrillMutation::BashAddition => Mutation::Extra {
            array: "RULE3_IDE_VARS",
            var: "VSCODE_DRILL_PROBE",
        },
        DrillMutation::BashRemoval => Mutation::Missing {
            array: "RULE5_API_VARS",
            var: "ANTHROPIC_SMALL_FAST_MODEL",
        },
        DrillMutation::RustAddition | DrillMutation::RustRemoval => return src.to_string(),
    };
    let array = mutation.array();
    let declaration = extract_array(src, array);
    let mutated = mutate_decl(&declaration, &edit, array);
    assert_eq!(
        src.matches(&declaration).count(),
        1,
        "Bash drill anchor is not unique"
    );
    src.replacen(&declaration, &mutated, 1)
}

fn write_file(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// Materialize enough of the repository to run the real Rust unit gate and
/// the real installer. No mutation is ever applied to the test checkout.
fn write_drill_copy(root: &Path, mutation: DrillMutation) {
    write_file(&root.join("Cargo.toml"), CARGO_TOML).unwrap();
    write_file(&root.join("Cargo.lock"), CARGO_LOCK).unwrap();
    write_file(
        &root.join("src/adapter_verify.rs"),
        &mutate_rust_constants(ADAPTER_VERIFY_SRC, mutation),
    )
    .unwrap();
    for (relative, contents) in LIB_SOURCES {
        if *relative == "src/adapter_verify.rs" {
            continue;
        }
        write_file(&root.join(relative), contents).unwrap();
    }
    write_file(&root.join("config/governor.yaml"), CONFIG_GOVERNOR_YAML).unwrap();

    let installer = mutate_installer_source(INSTALLER_SH, mutation);
    write_file(&root.join(INSTALLER), &installer).unwrap();

    // The committed adapters name a machine-global absolute binary. Point
    // the isolated copy at a path inside this temp directory so even the
    // installer's symlink-establishment step cannot write outside it.
    let safe_binary = root.join("adapter-bin/claude-print");
    let safe_binary = safe_binary.to_str().expect("temp path is valid UTF-8");
    for (relative, contents) in [
        (
            "deploy/needle-adapters/claude-print-opus.yaml",
            ADAPTER_OPUS_YAML,
        ),
        (
            "deploy/needle-adapters/claude-print-fable.yaml",
            ADAPTER_FABLE_YAML,
        ),
    ] {
        let isolated = contents.replace("/home/coding/.local/bin/claude-print", safe_binary);
        write_file(&root.join(relative), &isolated).unwrap();
    }
}

fn command_text(output: std::process::Output) -> (bool, String) {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn run_cargo_sync_gate(root: &Path) -> (bool, String) {
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "test",
            "--lib",
            "adapter_verify::tests::installer_bash_variable_lists_match_the_rust_constants",
            "--",
            "--exact",
        ])
        .output()
        .unwrap_or_else(|e| panic!("{}: could not run cargo sync gate: {}", root.display(), e));
    command_text(output)
}

fn make_fake_claude_print(home: &Path) -> PathBuf {
    let binary = home.join(".cargo/bin/claude-print");
    write_file(
        &binary,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo claude-print-drill\n  exit 0\nfi\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).unwrap();
    }
    binary
}

fn run_installer_sync_gate(root: &Path) -> (bool, String) {
    let home = root.join("home");
    let fake_binary = make_fake_claude_print(&home);
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!(
        "{}:{}",
        fake_binary.parent().unwrap().display(),
        inherited_path
    );
    let output = Command::new("bash")
        .current_dir(root)
        .arg(root.join(INSTALLER))
        .arg("--skip-live")
        .env("HOME", &home)
        .env("PATH", path)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "{}: could not run installer sync gate: {}",
                root.display(),
                e
            )
        });
    command_text(output)
}

#[test]
fn four_way_adapter_drift_drill_runs_both_real_gates_in_isolated_copies() {
    for mutation in DrillMutation::all() {
        let temp = TempDir::new().unwrap();
        write_drill_copy(temp.path(), mutation);

        let (cargo_ok, cargo_output) = run_cargo_sync_gate(temp.path());
        assert!(
            !cargo_ok,
            "{} cargo gate unexpectedly passed:\n{}",
            mutation.label(),
            cargo_output
        );
        assert!(
            cargo_output.contains("installer_bash_variable_lists_match_the_rust_constants"),
            "{} cargo output did not identify the synchronization test:\n{}",
            mutation.label(),
            cargo_output
        );
        assert!(
            cargo_output.contains(mutation.array()) && cargo_output.contains(mutation.rust_name()),
            "{} cargo diagnostic omitted the drifted list pair:\n{}",
            mutation.label(),
            cargo_output
        );
        assert!(
            cargo_output.contains(mutation.variable()),
            "{} cargo diagnostic omitted {}:\n{}",
            mutation.label(),
            mutation.variable(),
            cargo_output
        );

        let (installer_ok, installer_output) = run_installer_sync_gate(temp.path());
        assert!(
            !installer_ok,
            "{} installer gate unexpectedly passed:\n{}",
            mutation.label(),
            installer_output
        );
        assert!(
            installer_output.contains("Install incomplete."),
            "{} installer output omitted the failing-install diagnostic:\n{}",
            mutation.label(),
            installer_output
        );
        assert!(
            installer_output.contains("variable-list drift")
                && installer_output.contains(mutation.array())
                && installer_output.contains(mutation.rust_name())
                && installer_output.contains(mutation.variable()),
            "{} installer diagnostic omitted the drift details:\n{}",
            mutation.label(),
            installer_output
        );
        assert!(
            installer_output.contains(&format!(
                "{} {}",
                mutation.installer_diff_marker(),
                mutation.variable()
            )),
            "{} installer diagnostic omitted the expected diff direction:\n{}",
            mutation.label(),
            installer_output
        );
    }
}

#[test]
fn clean_tree_sync_gate_passes() {
    let tmp = TempDir::new().unwrap();
    let sandbox = write_sandbox(tmp.path(), &Mutation::Clean);
    let (ok, out) = run_gate(&sandbox);
    assert!(
        ok,
        "check_var_list_sync failed on the committed fragments — either the \
         arrays really drift from src/adapter_verify.rs or the extraction is \
         stale:\n{}",
        out
    );
    assert!(
        out.contains(OK_MARKER),
        "harness output lacked {}:\n{}",
        OK_MARKER,
        out
    );
}

#[test]
fn sync_gate_flags_an_extra_bash_variable() {
    // One variable present in the bash mirror but not the Rust constants —
    // exactly what a copy-paste edit to one side produces. If the gate ever
    // stops flagging this, it no longer detects drift at all.
    let tmp = TempDir::new().unwrap();
    let sandbox = write_sandbox(
        tmp.path(),
        &Mutation::Extra {
            array: "RULE3_IDE_VARS",
            var: "CGOV_DRIFT_EXTRA",
        },
    );
    let (ok, out) = run_gate(&sandbox);
    assert!(
        !ok,
        "check_var_list_sync passed an array carrying an extra variable — \
         drift is no longer detected:\n{}",
        out
    );
    assert!(
        out.contains("CGOV_DRIFT_EXTRA"),
        "drift report did not name the extra variable:\n{}",
        out
    );
    assert!(
        out.contains("RULE3_IDE_VARS"),
        "drift report did not name the drifted array:\n{}",
        out
    );
}

#[test]
fn sync_gate_flags_a_removed_bash_variable() {
    // The mirror-image edit: a variable dropped from the bash side only. A
    // gate that only compared sizes (or short-circuited on the first match)
    // would pass this.
    let tmp = TempDir::new().unwrap();
    let sandbox = write_sandbox(
        tmp.path(),
        &Mutation::Missing {
            array: "RULE5_API_VARS",
            var: "ANTHROPIC_SMALL_FAST_MODEL",
        },
    );
    let (ok, out) = run_gate(&sandbox);
    assert!(
        !ok,
        "check_var_list_sync passed an array missing a required variable — \
         drift is no longer detected:\n{}",
        out
    );
    assert!(
        out.contains("ANTHROPIC_SMALL_FAST_MODEL"),
        "drift report did not name the removed variable:\n{}",
        out
    );
    assert!(
        out.contains("RULE5_API_VARS"),
        "drift report did not name the drifted array:\n{}",
        out
    );
}
