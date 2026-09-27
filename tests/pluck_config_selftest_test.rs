//! Static contract pin for the check inventory of
//! `scripts/verify-pluck-config.sh` and its `--self-test` mode
//! (claudego-250ae860).
//!
//! docs/bead-visibility-quickref.md and docs/bead-visibility-troubleshooting.md
//! both designate the script as the one authoritative workflow and both list
//! the same six-part inventory: the backend binding, the active config path,
//! the resolved default workspace, the bead-rs store layout, the CLI output
//! contract, and the live `strands.pluck` values — with the promise that it
//! "exits non-zero on any mismatch". Nothing pinned the script itself: a
//! check dropped, renamed or renumbered there would silently weaken the
//! authoritative verification while both docs keep claiming six checks.
//!
//! Two gates keep the script's inventory from drifting, in the same shape as
//! the installer's two sync gates (tests/adapter_var_sync.rs):
//!
//! * `--self-test` — the script's own static contract check — pins the
//!   declared `CHECK_INVENTORY` registry against the `# N.` check sections
//!   actually present in the file, against the documented six-part list, and
//!   against the rule that sections beyond the documented six stay
//!   note-or-pass only.
//! * the tests here pin `--self-test` itself: the shipped script passes, the
//!   registry parsed out of the script equals the canonical constants below,
//!   both docs still name all six documented areas, and — via mutated
//!   sandbox copies — the self-test exits non-zero and names the drift for
//!   every way the inventory can silently break.
//!
//! Entirely static and offline: the script is embedded at compile time
//! (close-gate binaries are reused from a shared cache across extractions,
//! so a runtime read via `CARGO_MANIFEST_DIR` can point at a deleted
//! checkout), runs only in `--self-test` mode with `HOME` pointed at a
//! nonexistent sandbox path, and never invokes `bead`, `needle`, or any of
//! the script's live checks. The script's behavior against real workspaces
//! is already pinned hermetically by tests/pluck_config_verification_test.rs.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// The shipped validator, embedded at compile time.
const VERIFY_SH: &str = include_str!("../scripts/verify-pluck-config.sh");

/// The two docs whose six-part claim is the contract. Embedded, not read
/// from disk, for the same reason as the script.
const QUICKREF_MD: &str = include_str!("../docs/bead-visibility-quickref.md");
const TROUBLESHOOTING_MD: &str = include_str!("../docs/bead-visibility-troubleshooting.md");

/// The canonical six-part inventory, in the order both docs list it. The
/// script's `DOCUMENTED_AREAS` registry must equal this — that equality is
/// asserted twice: by the script's own `--self-test` at run time, and here
/// at parse level.
const DOCUMENTED_AREAS: [&str; 6] = [
    "backend-binding",
    "active-config-path",
    "default-workspace",
    "store-layout",
    "cli-contract",
    "strands-pluck",
];

/// The prose phrase naming each documented area in both docs (matched
/// whitespace-tolerantly, so re-wrapping a paragraph does not break the pin
/// — rewording it does).
const DOCUMENTED_PHRASES: [(&str, &str); 6] = [
    ("backend-binding", "backend binding"),
    ("active-config-path", "active config path"),
    ("default-workspace", "resolved default workspace"),
    ("store-layout", "store layout"),
    ("cli-contract", "CLI output contract"),
    ("strands-pluck", "strands.pluck"),
];

/// The sections the script may carry beyond the documented six. Each must be
/// named "<area>-supplementary" in the registry and must contain no failure
/// path: the docs' "exits non-zero on any mismatch" promise covers the six
/// documented checks only.
const SUPPLEMENTARY_SECTIONS: [&str; 1] = ["diagnostics-supplementary"];

// -- fixtures ----------------------------------------------------------------

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
}

/// Materialize `contents` as the script under its own `scripts/` path shape.
fn materialize(dir: &Path, contents: &str) -> PathBuf {
    let scripts = dir.join("scripts");
    fs::create_dir_all(&scripts).expect("scripts dir");
    let path = scripts.join("verify-pluck-config.sh");
    write_executable(&path, contents.as_bytes());
    path
}

/// Run the materialized script in `--self-test` mode with `HOME` pointed at
/// a path that does not exist: the mode is a static check over the file's
/// own text, so any dependence on live machine state fails here loudly
/// instead of passing through hidden coupling.
fn run_self_test(script: &Path) -> Run {
    let out = Command::new("bash")
        .arg(script)
        .arg("--self-test")
        .env("HOME", script.parent().unwrap().parent().unwrap().join("no-home"))
        .output()
        .expect("spawn the script with --self-test");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Run {
        code: out.status.code(),
        out: text,
    }
}

struct Run {
    code: Option<i32>,
    out: String,
}

impl Run {
    fn expect_exit(&self, code: i32) {
        assert_eq!(
            self.code,
            Some(code),
            "exit code mismatch\nfull output:\n{}",
            self.out
        );
    }

    fn expect_failure_naming(&self, fragment: &str) {
        assert_eq!(
            self.code,
            Some(1),
            "the mutated script must exit 1\nfull output:\n{}",
            self.out
        );
        assert!(
            self.out.contains("self-test: FAIL"),
            "the self-test's own diagnostics must fire:\n{}",
            self.out
        );
        assert!(
            self.out.contains(fragment),
            "the diagnostic must name the drift (`{fragment}`):\n{}",
            self.out
        );
    }
}

/// Replace `from` with `to`, asserting the anchor occurs exactly once — a
/// mutation against a non-unique anchor would silently corrupt an unrelated
/// part of the script and prove nothing.
fn replace_exactly_once(src: &str, from: &str, to: &str, label: &str) -> String {
    let occurrences = src.matches(from).count();
    assert_eq!(
        occurrences, 1,
        "{label} anchor is not unique in the script ({occurrences} occurrences)"
    );
    src.replacen(from, to, 1)
}

/// Lines from the first line declaring `name=(` through the line closing the
/// array, with one entry per line — the shape of `CHECK_INVENTORY` and
/// `DOCUMENTED_AREAS` in the script.
fn extract_array(src: &str, name: &str) -> Vec<String> {
    let anchor = format!("{name}=(");
    let mut body: Vec<String> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if body.is_empty() {
            if trimmed.starts_with(&anchor) {
                body.push(trimmed.trim_start_matches(&anchor).to_string());
            }
        } else {
            if trimmed == ")" {
                break;
            }
            body.push(trimmed.to_string());
        }
    }
    assert!(
        !body.is_empty(),
        "no {name}=( declaration found in the script"
    );
    body.into_iter()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_matches('"').to_string())
        .collect()
}

/// Collapse whitespace runs to single spaces, so a phrase pinned in the docs
/// survives paragraph re-wrapping (quickref wraps "resolved default
/// workspace" across lines; troubleshooting wraps "backend binding").
fn unwrapped(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// -- the shipped script passes its own contract ------------------------------

#[test]
fn the_shipped_script_passes_its_own_self_test() {
    let dir = TempDir::new().expect("sandbox tmpdir");
    let run = run_self_test(&materialize(dir.path(), VERIFY_SH));

    run.expect_exit(0);
    assert!(
        run.out.contains(&format!(
            "self-test: OK: {} check sections declared and contiguous",
            DOCUMENTED_AREAS.len() + SUPPLEMENTARY_SECTIONS.len()
        )),
        "the summary must declare the full section count:\n{}",
        run.out
    );
    assert!(
        run.out.contains(&format!("documented six = {}", DOCUMENTED_AREAS.join(" "))),
        "the summary must name the documented six in order:\n{}",
        run.out
    );
    assert!(
        run.out.contains(&format!(
            "supplementary sections (fail-free): {}",
            SUPPLEMENTARY_SECTIONS.join(" ")
        )),
        "the summary must name the supplementary sections:\n{}",
        run.out
    );
}

// -- the script's registry mirrors the canonical inventory --------------------

#[test]
fn the_script_registry_matches_the_canonical_inventory() {
    let inventory = extract_array(VERIFY_SH, "CHECK_INVENTORY");
    let documented = extract_array(VERIFY_SH, "DOCUMENTED_AREAS");

    assert_eq!(
        inventory.len(),
        DOCUMENTED_AREAS.len() + SUPPLEMENTARY_SECTIONS.len(),
        "CHECK_INVENTORY in the script must declare every canonical section"
    );
    for (position, entry) in inventory.iter().enumerate() {
        let expected_prefix = format!("{}:", position + 1);
        assert!(
            entry.starts_with(&expected_prefix),
            "CHECK_INVENTORY entry {position} is `{entry}`, expected `{expected_prefix}<area-id>`"
        );
    }

    let ids: Vec<&str> = inventory
        .iter()
        .map(|e| e.split_once(':').expect("N:id entry").1)
        .collect();
    assert_eq!(
        &ids[..DOCUMENTED_AREAS.len()],
        DOCUMENTED_AREAS,
        "the script's first sections must be the documented six, in order"
    );
    assert_eq!(
        &ids[DOCUMENTED_AREAS.len()..],
        SUPPLEMENTARY_SECTIONS,
        "the script's extra sections must be exactly the registered supplementary set"
    );
    assert_eq!(
        documented, DOCUMENTED_AREAS,
        "the script's DOCUMENTED_AREAS registry must equal the canonical list"
    );
}

// -- the docs still claim the inventory ---------------------------------------

#[test]
fn both_docs_still_name_the_six_part_inventory() {
    let quickref = unwrapped(QUICKREF_MD);
    let troubleshooting = unwrapped(TROUBLESHOOTING_MD);

    for (area, phrase) in DOCUMENTED_PHRASES {
        assert!(
            quickref.contains(phrase),
            "docs/bead-visibility-quickref.md no longer names the `{area}` check \
             (`{phrase}`) — the doc's inventory claim has drifted from the pin"
        );
        assert!(
            troubleshooting.contains(phrase),
            "docs/bead-visibility-troubleshooting.md no longer names the `{area}` \
             check (`{phrase}`) — the doc's inventory claim has drifted from the pin"
        );
    }

    // The authoritative designation and the non-zero promise — the two
    // halves of the claim that make the inventory a contract at all.
    for phrase in ["scripts/verify-pluck-config.sh", "exits non-zero"] {
        assert!(
            quickref.contains(phrase) && troubleshooting.contains(phrase),
            "both docs must keep designating the script as authoritative \
             (`{phrase}` missing)"
        );
    }
}

// -- mutation drills: every silent way the inventory can break -----------------

/// Drop the entire check-4 block (header through the code before check 5):
/// the store-layout check is gone while the registry still declares it.
#[test]
fn a_dropped_check_section_is_detected() {
    let start = VERIFY_SH
        .find("# 4. Store layout:")
        .expect("check-4 header anchor");
    let end = VERIFY_SH
        .find("# 5. CLI contract.")
        .expect("check-5 header anchor");
    assert!(start < end, "section anchors out of order");
    let mutated = format!("{}{}", &VERIFY_SH[..start], &VERIFY_SH[end..]);

    let dir = TempDir::new().expect("sandbox tmpdir");
    let run = run_self_test(&materialize(dir.path(), &mutated));
    run.expect_failure_naming("a check section was dropped");
}

/// A new check section appears in the code without a registry entry — the
/// exact "silently added seventh check" shape the docs would not know about.
#[test]
fn an_unregistered_added_section_is_detected() {
    let mutated = replace_exactly_once(
        VERIFY_SH,
        "# 7. Durable no-candidate diagnostics.",
        "# 8. Extra synthetic check.\n# 7. Durable no-candidate diagnostics.",
        "added-section",
    );

    let dir = TempDir::new().expect("sandbox tmpdir");
    let run = run_self_test(&materialize(dir.path(), &mutated));
    run.expect_failure_naming("a check section was dropped or added");
}

/// A documented area id is renamed in the registry while DOCUMENTED_AREAS
/// still carries the old name: the documented list and the inventory
/// disagree.
#[test]
fn a_renamed_documented_area_is_detected() {
    let mutated = replace_exactly_once(
        VERIFY_SH,
        "\"6:strands-pluck\"",
        "\"6:pluck-config-section\"",
        "renamed-area",
    );

    let dir = TempDir::new().expect("sandbox tmpdir");
    let run = run_self_test(&materialize(dir.path(), &mutated));
    run.expect_failure_naming("the documented list and the check inventory disagree");
}

/// A registry entry disappears: the remaining entries no longer line up
/// with their positions, and the header count no longer matches.
#[test]
fn a_dropped_registry_entry_is_detected() {
    let mutated = replace_exactly_once(
        VERIFY_SH,
        "    \"5:cli-contract\"\n",
        "",
        "dropped-registry-entry",
    );

    let dir = TempDir::new().expect("sandbox tmpdir");
    let run = run_self_test(&materialize(dir.path(), &mutated));
    run.expect_failure_naming("expected '5:<area-id>'");
}

/// The supplementary diagnostics section grows a failure path: the docs
/// promise non-zero exit for the six documented checks only, so a `bad`
/// call beyond them silently widens the contract.
#[test]
fn a_failure_path_in_a_supplementary_section_is_detected() {
    let mutated = replace_exactly_once(
        VERIFY_SH,
        "if [ -f \"$STATE_DIR/state/starvation_events.jsonl\" ]; then",
        "bad \"diagnostics: synthetic failure path\"\nif [ -f \"$STATE_DIR/state/starvation_events.jsonl\" ]; then",
        "supplementary-failure-path",
    );

    let dir = TempDir::new().expect("sandbox tmpdir");
    let run = run_self_test(&materialize(dir.path(), &mutated));
    run.expect_failure_naming("must stay note-or-pass only");
}

/// A supplementary section sheds its `-supplementary` naming: the registry
/// no longer distinguishes documented checks from extras.
#[test]
fn an_unnamed_supplementary_section_is_detected() {
    let mutated = replace_exactly_once(
        VERIFY_SH,
        "\"7:diagnostics-supplementary\"",
        "\"7:diagnostics\"",
        "supplementary-naming",
    );

    let dir = TempDir::new().expect("sandbox tmpdir");
    let run = run_self_test(&materialize(dir.path(), &mutated));
    run.expect_failure_naming("must be named '<area>-supplementary'");
}
