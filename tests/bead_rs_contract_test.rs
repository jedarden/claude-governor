//! NEEDLE / bead-rs compatibility contract (claudego-643e8336).
//!
//! Pins the real `needle` and `bead` CLI behaviors that Pluck and
//! `scripts/verify-pluck-config.sh` depend on, against the real binaries and
//! real stores — so a NEEDLE or bead-rs upgrade that changes any of them fails
//! here, at review time, instead of silently changing what Pluck dispatches.
//!
//! Division of labor with the neighbouring suites:
//!
//! - `pluck_config_verification_test.rs` pins the *script's detection logic*
//!   using stub doubles whose canned output each scenario controls. It proves
//!   the script notices a broken input; it cannot prove the real binaries
//!   emit what the script expects in the first place. This file does.
//! - `pluck_db_test.rs` pins the *adapter's* query construction and label
//!   exclusion against a seeded store. This file pins the CLI contract those
//!   queries ride on.
//!
//! Pinned surface:
//!
//! - backend selection — this repo's `.needle.yaml` binds `bead-rs`
//!   (via `include_str!`, so the tested text cannot drift from the shipped one)
//! - store layout — `bead init` builds `beads.db` + `checkpoint/` +
//!   `config.json`, never the bf-era flat `issues.jsonl`
//! - workspace resolution — the cwd's nearest `.beads` store serves minting;
//!   a barrier proves discovery never escapes the tempdir
//! - JSONL output — `bead list --json` stdout is one object per line and
//!   `--limit` is honored; `bead show ID --json` is an array
//! - dependency readiness — a `blocks` edge holds a bead out of `--ready`
//!   while its blocker is open, closing the blocker promotes it, and a
//!   `relates_to` edge is inert; the ready frontier is open + unassigned only
//! - exclusion labels — the exact built-in default set
//!   (`deferred, human, blocked, escalation, alert`), plus version tripwires
//!   so a needle or bead upgrade forces this file to be re-verified
//! - version tripwires resolve the deployed binaries (`~/.local/bin` first,
//!   PATH fallback) — cargo puts its own bin dir on spawned PATHs, and on
//!   this host that dir carries a stale needle build
//! - label case — labels round-trip the ready JSONL byte-exact, the CLI fact
//!   NEEDLE's case-sensitive exclusion matching rides on

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use tempfile::TempDir;

/// The pinned contract: the version each behavior was verified against and
/// the exact label set Pluck excludes by default. Every `bead`/`needle`
/// behavior asserted below was confirmed live against these versions; bump
/// them only after re-verifying, never to make a test go green.
struct PinnedContract;

impl PinnedContract {
    /// `needle --version` this contract was verified against (the default
    /// exclusion set below is `DEFAULT_EXCLUDE_LABELS` in NEEDLE
    /// `src/strand/pluck.rs` at this version).
    ///
    /// 0.6.14 → 0.6.16 (claudego-fddf5c3f, 2026-09-26): re-verified from the
    /// deployed binary's exact release commit (3eaf0e83, the v0.6.16 bump) —
    /// `DEFAULT_EXCLUDE_LABELS` is still the same five labels.
    const NEEDLE: &'static str = "0.6.16";

    /// `bead --version` this contract was verified against (store layout,
    /// JSONL shapes, dep/readiness semantics).
    const BEAD: &'static str = "0.2.6";

    /// The built-in exclusion set PluckStrand substitutes when the configured
    /// `exclude_labels` is empty or omitted — this deployment's live case
    /// (`strands.pluck.exclude_labels: []`). Needle 0.6.16
    /// `DEFAULT_EXCLUDE_LABELS`, `src/strand/pluck.rs`. A non-empty
    /// configured list *replaces* this set rather than merging with it.
    ///
    /// Needle nuance (documented, not CLI-observable from here): if the whole
    /// `strands.pluck` section is absent, `PluckConfig::default()` supplies
    /// only the first four — without `alert`.
    const DEFAULT_EXCLUDE_LABELS: &[&str] = &[
        "deferred", "human", "blocked", "escalation", "alert",
    ];
}

/// Workspace identity planted before the first `bead` call — the fresh-clone
/// shape (`config.json` present, database absent) that `bead init` rebuilds a
/// store around, preserving the recorded identity.
const PREFIX: &str = "bcontract";
const SEEDED_IDENTITY: &str = r#"{"created_at":"2026-09-25T00:00:00Z","prefix":"bcontract","uuid":"00000000-0000-4000-8000-00000000000b","version":1}"#;
const BARRIER_IDENTITY: &str = r#"{"created_at":"2026-09-25T00:00:00Z","prefix":"bbarrier","uuid":"00000000-0000-4000-8000-00000000000c","version":1}"#;

/// A bead workspace isolated inside its own tempdir.
///
/// Same geometry as `pluck_db_test.rs`: a barrier fingerprint at the tempdir
/// root stops discovery from ever reaching whatever the host keeps above
/// `$TMPDIR` (this box keeps a live store at `/tmp/.beads`), and the seeded
/// workspace's own fingerprint makes `bead init` rebuild *here*.
struct ContractWorkspace {
    _dir: TempDir,
    path: PathBuf,
}

fn run_bead(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("bead")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("bead CLI must be on PATH");
    assert!(
        output.status.success(),
        "bead {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Last non-empty stdout line — `bead create` prints the minted ID there.
fn last_line(output: &str) -> String {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .expect("command must print something")
        .to_string()
}

impl ContractWorkspace {
    /// Barrier + seeded fingerprint + `bead init`, then prove minting stays
    /// inside. Every test gets its own instance; nothing here touches the
    /// live store.
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir for contract workspace");
        let root = dir.path();

        fs::create_dir_all(root.join(".beads")).expect("create barrier .beads");
        fs::write(root.join(".beads/config.json"), BARRIER_IDENTITY)
            .expect("write barrier config.json");

        let path = root.join("ws");
        fs::create_dir_all(path.join(".beads")).expect("create seeded .beads");
        fs::write(path.join(".beads/config.json"), SEEDED_IDENTITY)
            .expect("write seeded config.json");

        run_bead(&path, &["init"]);
        assert!(
            path.join(".beads/beads.db").is_file(),
            "bead init must build the store inside the seeded workspace, not adopt one above it"
        );

        let ws = ContractWorkspace { _dir: dir, path };
        let first = ws.create("contract tripwire bead", &[]);
        assert!(
            first.starts_with(&format!("{PREFIX}-")),
            "seeding escaped the isolated workspace: {first} was not minted under {PREFIX}-"
        );
        ws
    }

    /// Create one bead and return its printed ID.
    fn create(&self, title: &str, labels: &[&str]) -> String {
        let mut args = vec!["create", "--title", title];
        for label in labels {
            args.push("--label");
            args.push(label);
        }
        let id = last_line(&run_bead(&self.path, &args));
        assert!(!id.is_empty(), "bead create must print the new issue ID");
        id
    }

    /// Ready IDs from the frontier, in output order.
    fn ready_ids(&self) -> Vec<String> {
        let output = run_bead(
            &self.path,
            &["list", "--ready", "--json", "--limit", "999999"],
        );
        ready_ids_from_jsonl(&output)
    }

    /// IDs from a state listing (the ready frontier's superset for open).
    fn status_ids(&self, state: &str) -> Vec<String> {
        let output = run_bead(
            &self.path,
            &["list", "--status", state, "--json", "--limit", "999999"],
        );
        ready_ids_from_jsonl(&output)
    }
}

/// Parse the JSONL the ready-frontier contract promises: every non-empty
/// line is one JSON object carrying an `id`.
fn ready_ids_from_jsonl(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let bead: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("list --json stdout must be pure JSONL, got {line:?}: {e}"));
            bead["id"]
                .as_str()
                .expect("every listed bead carries an id")
                .to_string()
        })
        .collect()
}

/// The repo's backend binding, pinned by content: this is the declaration
/// `scripts/verify-pluck-config.sh` check 1 greps for, and the one that makes
/// every NEEDLE strand in this workspace talk to the bead-rs store.
const REPO_NEEDLE_YAML: &str = include_str!("../.needle.yaml");

#[test]
fn repo_backend_binding_declares_bead_rs() {
    assert!(
        REPO_NEEDLE_YAML.contains("bead_cli:"),
        ".needle.yaml must declare a bead_cli section, got: {REPO_NEEDLE_YAML:?}"
    );
    assert!(
        REPO_NEEDLE_YAML.contains("backend: bead-rs"),
        ".needle.yaml must bind backend: bead-rs (the bead-forge/bf backend is retired); got: {REPO_NEEDLE_YAML:?}"
    );
    assert!(
        !REPO_NEEDLE_YAML.contains("backend: bf"),
        ".needle.yaml must not bind the retired bf backend anywhere"
    );
}

#[test]
fn init_builds_the_bead_rs_store_layout() {
    let ws = ContractWorkspace::new();
    let beads_dir = ws.path.join(".beads");

    // The live store is SQLite, not the bf-era flat JSONL file.
    assert!(
        beads_dir.join("beads.db").is_file(),
        ".beads/beads.db must be the live store (bead-rs SQLite)"
    );
    assert!(
        !beads_dir.join("issues.jsonl").exists(),
        ".beads/issues.jsonl must not exist — that is the bf-era flat store; bead-rs uses beads.db + checkpoint/"
    );

    // Workspace identity survives the rebuild.
    let identity: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(beads_dir.join("config.json"))
            .expect(".beads/config.json must exist (workspace identity)"),
    )
    .expect(".beads/config.json must be valid JSON");
    assert_eq!(
        identity["prefix"].as_str(),
        Some(PREFIX),
        "bead init must preserve the committed workspace identity prefix"
    );

    // The durable checkpoint: SQLite is the live store, checkpoint/ is what
    // a fresh clone rebuilds from. current.json + objects/ are the pieces
    // `bead sync` and the recovery recipes depend on.
    let checkpoint = beads_dir.join("checkpoint");
    assert!(checkpoint.is_dir(), ".beads/checkpoint/ must exist");
    assert!(
        checkpoint.join("current.json").is_file(),
        "checkpoint/current.json must exist (durable checkpoint head)"
    );
    assert!(
        checkpoint.join("objects").is_dir(),
        "checkpoint/objects/ must exist (checkpoint object store)"
    );
}

#[test]
fn commands_mint_into_the_nearest_cwd_store() {
    let ws = ContractWorkspace::new();

    // From a nested directory, discovery walks up to the nearest `.beads` —
    // the same store, proven by the shared prefix. This is the resolution
    // Pluck relies on when it renders `(cd {workspace} && bead list ...)`.
    let nested = ws.path.join("nested/deeper");
    std::fs::create_dir_all(&nested).expect("create nested dir");
    let output = Command::new("bead")
        .args(["create", "--title", "minted from a nested cwd"])
        .current_dir(&nested)
        .output()
        .expect("bead CLI must be on PATH");
    assert!(output.status.success());
    let id = last_line(&String::from_utf8_lossy(&output.stdout));
    assert!(
        id.starts_with(&format!("{PREFIX}-")),
        "a bead minted from a nested cwd must carry the nearest store's prefix {PREFIX}-, got {id}"
    );
    assert!(
        ws.ready_ids().iter().any(|ready| ready == &id),
        "a bead created from a nested cwd must land in the enclosing workspace's store"
    );
}

#[test]
fn list_json_stdout_is_pure_jsonl_and_limit_is_honored() {
    let ws = ContractWorkspace::new();
    let _a = ws.create("jsonl contract a", &[]);
    let _b = ws.create("jsonl contract b", &[]);
    let _c = ws.create("jsonl contract c", &[]);

    let output = run_bead(
        &ws.path,
        &["list", "--ready", "--json", "--limit", "2"],
    );

    // Exactly `--limit` objects, one per line — never a top-level array.
    // (verify-pluck-config.sh pipes the first line through `jq -e 'type ==
    // "object"'` and Pluck parses line-per-bead; an array-shaped regression
    // must fail here, at the CLI, not only in the script's detection.)
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        2,
        "--limit 2 must yield exactly two JSONL lines, got {lines:?}"
    );
    for line in &lines {
        let bead: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("each list line must be one JSON object, got {line:?}: {e}"));
        assert!(
            bead.is_object(),
            "list --json emits JSONL objects, not array elements: {line:?}"
        );

        // The fields Pluck's adapter reads on every candidate.
        assert!(bead["id"].is_string(), "listed bead must carry id: {line:?}");
        assert!(
            bead["status"].is_string(),
            "listed bead must carry status: {line:?}"
        );
        assert!(
            bead["assignee"].is_null(),
            "ready beads are unassigned; assignee must be null: {line:?}"
        );
        assert!(
            bead["labels"].is_array(),
            "listed bead must carry a labels array (the adapter's exclusion input): {line:?}"
        );
    }
}

#[test]
fn show_json_is_an_array_of_objects() {
    let ws = ContractWorkspace::new();
    let id = ws.create("show shape contract", &[]);

    let output = run_bead(&ws.path, &["show", &id, "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(output.trim())
        .unwrap_or_else(|e| panic!("show --json must emit JSON, got {output:?}: {e}"));
    assert!(
        parsed.is_array(),
        "show --json emits a JSON array (pipelines use jq '.[0]'), got: {output:?}"
    );
    assert_eq!(
        parsed[0]["id"].as_str(),
        Some(id.as_str()),
        "show --json array's first element must be the requested bead"
    );
}

#[test]
fn a_blocks_dependency_holds_a_bead_out_of_the_ready_frontier() {
    let ws = ContractWorkspace::new();
    let blocker = ws.create("contract blocker", &[]);
    let blocked = ws.create("contract blocked", &[]);
    let related = ws.create("contract related", &[]);
    let target = ws.create("contract related target", &[]);

    // Blocked-first, per `bead dep add --help`: BLOCKER must close before
    // BLOCKED can become ready.
    run_bead(&ws.path, &["dep", "add", &blocked, &blocker]);
    // relates_to edges are informational only — they must never gate readiness.
    run_bead(
        &ws.path,
        &["dep", "add", &related, &target, "--kind", "relates_to"],
    );

    // The blocked bead is open and unassigned — every state filter Pluck
    // checks passes — yet the blocks edge holds it out of the frontier.
    assert!(
        !ws.ready_ids().iter().any(|ready| ready == &blocked),
        "a bead with an open blocker must not appear in --ready"
    );
    assert!(
        ws.status_ids("open").iter().any(|open| open == &blocked),
        "a dependency does not change state: the blocked bead must still list as open"
    );
    assert!(
        ws.ready_ids().iter().any(|ready| ready == &related),
        "a relates_to edge is informational only and must not hold a bead out of --ready"
    );

    // Closing the blocker promotes the dependent onto the frontier — the
    // transition Pluck's readiness assumption rests on.
    run_bead(&ws.path, &["close", &blocker, "--reason", "contract: done"]);
    assert!(
        ws.ready_ids().iter().any(|ready| ready == &blocked),
        "closing the blocker must promote the blocked bead into --ready"
    );
}

#[test]
fn the_ready_frontier_is_open_and_unassigned_only() {
    let ws = ContractWorkspace::new();
    let assigned = ws.create("assigned open bead", &[]);
    let in_progress = ws.create("in-progress bead", &[]);

    run_bead(&ws.path, &["update", &assigned, "--assignee", "worker-x"]);
    run_bead(
        &ws.path,
        &["update", &in_progress, "--status", "in_progress"],
    );

    let ready = ws.ready_ids();
    assert!(
        !ready.iter().any(|ready| ready == &assigned),
        "an assigned open bead must stay out of --ready (Pluck always dispatches unassigned work)"
    );
    assert!(
        !ready.iter().any(|ready| ready == &in_progress),
        "an in-progress bead must stay out of --ready"
    );
}

#[test]
fn the_default_exclusion_label_set_is_pinned() {
    // Membership, not order: exclusion is string membership in the adapter,
    // so a NEEDLE-internal reorder is not a behavior change. Any addition,
    // removal, or rename IS — it silently changes what Pluck dispatches
    // under this deployment's `exclude_labels: []`.
    let mut pinned: Vec<&str> = PinnedContract::DEFAULT_EXCLUDE_LABELS.to_vec();
    pinned.sort_unstable();
    let mut expected = vec![
        "alert", "blocked", "deferred", "escalation", "human",
    ];
    expected.sort_unstable();
    assert_eq!(
        pinned, expected,
        "DEFAULT_EXCLUDE_LABELS changed. The new set decides which ready beads this \
         deployment's Pluck strand can never see (exclude_labels: [] substitutes the \
         built-in defaults). Re-verify against NEEDLE src/strand/pluck.rs, then re-pin \
         PinnedContract::DEFAULT_EXCLUDE_LABELS here and the label lists in \
         tests/pluck_db_test.rs and docs/bead-visibility-troubleshooting.md."
    );
}

/// The CLI fact NEEDLE's exact, case-sensitive label matching rides on:
/// labels round-trip the store and the ready JSONL byte-exact. A bead-rs
/// release that normalized case (or expanded glob-shaped labels) would turn a
/// case variant of an exclusion label into an exact match, and Pluck would
/// silently start dropping those beads — that drift must fail here, at review
/// time, not in production dispatch. The matching rule itself is pinned on
/// the adapter side in `tests/pluck_db_test.rs`; this pins the CLI contract
/// that rule trusts.
#[test]
fn exclusion_labels_round_trip_byte_exact_through_the_ready_jsonl() {
    let ws = ContractWorkspace::new();
    // A case variant of every default exclusion label, plus glob-shaped
    // literals: all documented as inert (docs/bead-visibility-quickref.md —
    // "exact, case-sensitive ... no globs, `%`, regular expressions, or
    // prefix matching"), and all dangerous to normalize away.
    let variants = [
        "Deferred", "HUMAN", "Blocked", "Escalation", "Alert", //
        "defer*", "human?", "aler.*",
    ];
    let id = ws.create("label-case contract bead", &variants);

    let output = run_bead(
        &ws.path,
        &["list", "--ready", "--json", "--limit", "999999"],
    );
    let bead = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("ready stdout must be pure JSONL, got {line:?}: {e}"))
        })
        .find(|bead| bead["id"].as_str() == Some(id.as_str()))
        .expect("the case-variant bead must surface in --ready: the backend ready query has no label filter — label exclusion is NEEDLE's post-query job");

    let labels: Vec<&str> = bead["labels"]
        .as_array()
        .expect("ready JSONL must carry a labels array")
        .iter()
        .map(|value| value.as_str().expect("every label is a string"))
        .collect();
    for variant in variants {
        assert!(
            labels.contains(&variant),
            "label {variant:?} must round-trip byte-exact through the ready JSONL, \
             got {labels:?} — case-normalizing it would turn it into an exact \
             exclusion match and silently hide the bead from Pluck"
        );
    }
    assert!(
        !labels
            .iter()
            .any(|label| PinnedContract::DEFAULT_EXCLUDE_LABELS.contains(label)),
        "no label may have been normalized into a default exclusion label's exact \
         form ({labels:?}) — NEEDLE's case-sensitive match would then drop this \
         bead from the candidate set"
    );
}

/// Resolve a fleet CLI the way the deployment installs it, not by whatever
/// PATH the invoking context happens to carry. This host keeps two `needle`
/// binaries: the deployed one at `~/.local/bin/needle` and a stale
/// 2026-08-29 build at `~/.cargo/bin/needle` — and cargo puts its own bin
/// dir on the PATH of every process it spawns, so a test run under a
/// non-interactive PATH (systemd unit, the NEEDLE close gate) resolves the
/// stale one while an interactive shell resolves the deployed one. The
/// version pins must measure the binary production actually runs, so prefer
/// the fleet's install location and fall back to PATH where no deployed
/// copy exists (e.g. `bead` ships only in `~/.cargo/bin` today).
fn fleet_cli(name: &str) -> Command {
    if let Some(home) = std::env::var_os("HOME") {
        let deployed = Path::new(&home).join(".local/bin").join(name);
        if deployed.is_file() {
            return Command::new(deployed);
        }
    }
    Command::new(name)
}

#[test]
fn needle_version_pin_is_current() {
    let version = fleet_cli("needle")
        .arg("--version")
        .output()
        .expect("needle CLI must be resolvable (deployed ~/.local/bin or PATH)");
    let stdout = String::from_utf8_lossy(&version.stdout).into_owned();
    let current = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or("<unparsable>")
        .to_string();

    assert!(
        semver(current.trim()) == semver(PinnedContract::NEEDLE),
        "needle is now {current:?}; this contract was verified against {}. \
         A NEEDLE upgrade can change the pluck strand's default exclusion set, the \
         config loader, or the query template. Re-verify DEFAULT_EXCLUDE_LABELS \
         (NEEDLE src/strand/pluck.rs) and the behaviors pinned in this file, then \
         re-pin PinnedContract::NEEDLE and the version mentions in \
         docs/bead-visibility-troubleshooting.md, docs/bead-visibility-quickref.md, \
         and docs/pluck-query-results.md.",
        PinnedContract::NEEDLE
    );
}

#[test]
fn bead_version_pin_is_current() {
    let version = fleet_cli("bead")
        .arg("--version")
        .output()
        .expect("bead CLI must be resolvable (deployed ~/.local/bin or PATH)");
    let stdout = String::from_utf8_lossy(&version.stdout).into_owned();
    let current = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or("<unparsable>")
        .to_string();

    assert!(
        semver(current.trim()) == semver(PinnedContract::BEAD),
        "bead is now {current:?}; this contract was verified against {}. \
         A bead-rs upgrade can change the store layout, the JSONL output shapes, \
         or dependency-readiness semantics that Pluck rides on. Re-verify the \
         behaviors pinned in this file against the new binary, then re-pin \
         PinnedContract::BEAD.",
        PinnedContract::BEAD
    );
}

/// Compare versions by their numeric segments so a build metadata suffix can
/// never trip the pin (`0.6.14` == `0.6.14`, `0.6.14-rc.1` != `0.6.14`).
fn semver(version: &str) -> Vec<u64> {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    core.split('.')
        .map(|part| {
            u64::from_str(part)
                .unwrap_or_else(|_| panic!("version {version:?} is not semver-shaped"))
        })
        .collect()
}
