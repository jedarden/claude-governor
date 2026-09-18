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
//! * one removed variable fails the gate the same way.
//!
//! The installer script itself is NEVER executed — it writes
//! `~/.config/needle/adapters` and links binaries, which a test must not do.
//! Extraction is anchor-based, not line-number-based, so edits elsewhere in
//! the script do not break the pin.

use std::fmt::Write as _;
use std::fs;
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
    assert!(!lines.is_empty(), "no {}=( declaration found in {}", name, INSTALLER);
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
    assert!(!lines.is_empty(), "no {}() {{ definition found in {}", name, INSTALLER);
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
    Extra { array: &'static str, var: &'static str },
    /// Drop `var` from one array's elements — the other side of the same edit.
    Missing { array: &'static str, var: &'static str },
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
    let open = base
        .find('(')
        .unwrap_or_else(|| panic!("{} declaration has no opening paren: {:?}", this_array, decl));
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
    writeln!(&mut script, "# Sandbox assembled by tests/adapter_var_sync.rs from fragments of").unwrap();
    writeln!(&mut script, "# {} — extraction only; the installer itself never runs.", INSTALLER).unwrap();
    writeln!(&mut script, "REPO_DIR='{}'", repo_dir).unwrap();
    writeln!(&mut script, "{}", extract_color_vars(&installer)).unwrap();
    writeln!(
        &mut script,
        "{}",
        mutate_decl(&extract_array(&installer, "RULE3_IDE_VARS"), mutation, "RULE3_IDE_VARS")
    )
    .unwrap();
    writeln!(
        &mut script,
        "{}",
        mutate_decl(&extract_array(&installer, "RULE5_API_VARS"), mutation, "RULE5_API_VARS")
    )
    .unwrap();
    writeln!(&mut script, "{}", extract_function(&installer, "rust_rule_vars")).unwrap();
    writeln!(&mut script, "{}", extract_function(&installer, "bash_rule_vars")).unwrap();
    writeln!(&mut script, "{}", extract_function(&installer, "check_var_list_sync")).unwrap();

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
    assert!(out.contains(OK_MARKER), "harness output lacked {}:\n{}", OK_MARKER, out);
}

#[test]
fn sync_gate_flags_an_extra_bash_variable() {
    // One variable present in the bash mirror but not the Rust constants —
    // exactly what a copy-paste edit to one side produces. If the gate ever
    // stops flagging this, it no longer detects drift at all.
    let tmp = TempDir::new().unwrap();
    let sandbox = write_sandbox(
        tmp.path(),
        &Mutation::Extra { array: "RULE3_IDE_VARS", var: "CGOV_DRIFT_EXTRA" },
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
        &Mutation::Missing { array: "RULE5_API_VARS", var: "ANTHROPIC_SMALL_FAST_MODEL" },
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
