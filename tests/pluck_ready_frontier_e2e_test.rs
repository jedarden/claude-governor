//! End-to-end Pluck ready-frontier regression tests (claudego-9a2205c5).
//!
//! Pins the frontier behaviors the neighbouring suites left unpinned, each
//! against the real `bead` CLI and a real seeded store:
//!
//! - **manual blocks** — `bead update --status blocked` is the CLI surface
//!   behind Pluck's `AND i.manual_blocked = 0` clause, which
//!   `tests/pluck_db_test.rs` so far only asserted on a *simulated* query
//!   string. Pinned here end to end: a manually blocked bead leaves
//!   `--ready` while its base status stays `open`, the JSONL carries the
//!   `status` / `effective_status` / `manual_blocked` distinction, the
//!   effective-status listing still surfaces it, and `--status open`
//!   restores it to the frontier.
//! - **stale assignments** — an assignee that never existed (the stale
//!   extreme; nothing to wait for, no liveness check) holds the bead out of
//!   `--ready`, and the recovery path is fail-closed: both `release` and a
//!   bare `--clear-assignee` refuse with the lease-conflict exit code until
//!   the claim epoch projected by `bead show --json` is passed as
//!   `--fencing-token`. bead 0.2.6 fences assignment claims — the
//!   fencing half of the recovery recipe the org docs document is easy to
//!   miss, and it is exactly what makes a dead worker's claim safe to clear.
//! - **absolute workspace resolution** — Pluck renders
//!   `(cd {workspace} && bead list --ready --json --limit 999999)`; the
//!   absolute path is the whole resolution mechanism, because store
//!   discovery is purely cwd-based. Pinned with a barrier store: the
//!   command executed where the rendered absolute path points serves the
//!   seeded store, while the identical command from a cwd above it meets
//!   the barrier instead — the workspace-not-absolute starvation root cause
//!   (docs/research/pluck-filter-root-cause.md), now as assertions.
//! - **ready-frontier gates and checkpoint publication** — the raw bead-rs
//!   frontier requires open, unassigned, unblocked beads with no unfinished
//!   `blocks` dependency; Pluck's label exclusion remains an exact,
//!   case-sensitive post-filter, so `documentation` is inert while `human`
//!   is excluded and `Human` survives. A suppressed auto-flush is then made
//!   durable by the explicit `bead sync flush-only` command.
//!
//! Supersedes three dead diagnostic tests replaced by asserting versions
//! (they printed analysis and never asserted, read the live shared store,
//! and ran bf-era `status` SQL that errors against the bead-rs schema —
//! `memory/pluck-config-investigation.md` already flagged the class as
//! "should be migrated ... before being used as regression gates"):
//! `tests/test_workspace_path_formats.rs`,
//! `tests/pluck_workspace_mismatch_test.rs` and
//! `tests/pluck_filter_combinations_test.rs`. Their findings survive in
//! docs/research/pluck-filter-root-cause.md; the clauses they only printed
//! are asserted here and in `tests/pluck_db_test.rs`.
//!
//! Division of labor: `bead_rs_contract_test.rs` owns the version pins and
//! the store / JSONL / dependency contract; `pluck_db_test.rs` owns the
//! adapter's query construction and label exclusion. This file adds only
//! the behaviors above and deliberately re-runs no version pin — it
//! rides the same `bead` binary that file pins (0.2.6; behaviors here
//! observed live against it, 2026-09-27).
//!
//! Every store touched is seeded inside an isolated tempdir with the same
//! barrier geometry as the neighbouring suites: only a `config.json`
//! fingerprint stops workspace discovery deterministically, and this host
//! keeps a live store at /tmp/.beads that stray discovery would otherwise
//! adopt (claudego-fb95927b).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Workspace identity planted before the first `bead` call — the same
/// fresh-clone shape the neighbouring suites seed (committed `config.json`,
/// gitignored database absent; `bead init` rebuilds around it).
const PREFIX: &str = "pfront";
const SEEDED_IDENTITY: &str = r#"{"created_at":"2026-09-27T00:00:00Z","prefix":"pfront","uuid":"00000000-0000-4000-8000-0000000000f1","version":1}"#;
const BARRIER_IDENTITY: &str = r#"{"created_at":"2026-09-27T00:00:00Z","prefix":"pbarrier","uuid":"00000000-0000-4000-8000-0000000000f2","version":1}"#;

/// The backend command Pluck renders for this deployment, in content pinned
/// by `construct_pluck_invocation` in tests/pluck_db_test.rs. Rendered here
/// so the resolution test executes exactly its shape.
const RENDERED_COMMAND: &[&str] = &["list", "--ready", "--json", "--limit", "999999"];

/// A bead workspace isolated inside its own tempdir.
///
/// Same geometry as `bead_rs_contract_test.rs`: a barrier fingerprint at the
/// tempdir root stops discovery from ever reaching whatever the host keeps
/// above `$TMPDIR`, and the seeded workspace's own fingerprint makes
/// `bead init` rebuild *here*.
struct FrontierWorkspace {
    dir: TempDir,
    path: PathBuf,
}

fn bead_output(dir: &Path, args: &[&str]) -> Output {
    Command::new("bead")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("bead CLI must be on PATH")
}

/// Run `bead` asserting success, returning stdout.
fn bead_ok(dir: &Path, args: &[&str]) -> String {
    let output = bead_output(dir, args);
    assert!(
        output.status.success(),
        "bead {} failed: {}",
        args.first().unwrap_or(&"<none>"),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// IDs from one JSONL listing — every non-empty line is one bead object.
fn jsonl_ids(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|e| panic!("listing stdout must be pure JSONL, got {line:?}: {e}"))
        })
        .map(|bead| {
            bead["id"]
                .as_str()
                .expect("every listed bead carries an id")
                .to_string()
        })
        .collect()
}

impl FrontierWorkspace {
    /// Barrier + seeded fingerprint + `bead init`, then prove minting stays
    /// inside. Every test gets its own instance; nothing here touches the
    /// live store.
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir for frontier workspace");
        let root = dir.path();

        fs::create_dir_all(root.join(".beads")).expect("create barrier .beads");
        fs::write(root.join(".beads/config.json"), BARRIER_IDENTITY)
            .expect("write barrier config.json");

        let path = root.join("ws");
        fs::create_dir_all(path.join(".beads")).expect("create seeded .beads");
        fs::write(path.join(".beads/config.json"), SEEDED_IDENTITY)
            .expect("write seeded config.json");

        bead_ok(&path, &["init"]);
        assert!(
            path.join(".beads/beads.db").is_file(),
            "bead init must build the store inside the seeded workspace, not adopt one above it"
        );

        let ws = FrontierWorkspace { dir, path };
        let first = ws.create("frontier tripwire bead", &[]);
        assert!(
            first.starts_with(&format!("{PREFIX}-")),
            "seeding escaped the isolated workspace: {first} was not minted under {PREFIX}-"
        );
        ws
    }

    /// The tempdir root — the cwd above the seeded workspace where the
    /// barrier store lives. A command run here resolves to the barrier,
    /// never to the workspace below it.
    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Create one bead and return its printed ID.
    fn create(&self, title: &str, labels: &[&str]) -> String {
        let mut args = vec!["create", "--title", title];
        for label in labels {
            args.push("--label");
            args.push(label);
        }
        let id = bead_ok(&self.path, &args)
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .expect("bead create must print the new issue ID")
            .to_string();
        assert!(!id.is_empty(), "bead create must print the new issue ID");
        id
    }

    /// Ready IDs from the frontier, in output order.
    fn ready_ids(&self) -> Vec<String> {
        jsonl_ids(&bead_ok(&self.path, RENDERED_COMMAND))
    }

    /// Full objects from the raw bead-rs ready frontier. Label exclusion is
    /// deliberately not performed by `bead list --ready`; it is Pluck's
    /// exact-match post-filter over these objects.
    fn ready_beads(&self) -> Vec<Value> {
        bead_ok(&self.path, RENDERED_COMMAND)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("ready stdout must be JSONL, got {line:?}: {e}"))
            })
            .collect()
    }

    /// Create a mutation while deliberately suppressing the automatic
    /// checkpoint publication, leaving `sync status` with a dirty frontier
    /// for the explicit flush assertion below.
    fn create_without_auto_flush(&self, title: &str, labels: &[&str]) -> String {
        let mut args = vec!["--no-auto-flush", "create", "--title", title];
        for label in labels {
            args.push("--label");
            args.push(label);
        }
        bead_ok(&self.path, &args)
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .expect("bead create must print the new issue ID")
            .to_string()
    }

    /// Machine-readable checkpoint freshness, which is the durable commit
    /// gate bead-rs exposes after a mutation.
    fn checkpoint_status(&self) -> Value {
        let output = bead_ok(&self.path, &["sync", "status", "--format", "json"]);
        serde_json::from_str(output.trim())
            .expect("bead sync status --format json must emit one JSON object")
    }

    /// One bead's full JSON object (`bead show ID --json` is an array —
    /// pinned by the contract suite).
    fn show(&self, id: &str) -> Value {
        let output = bead_ok(&self.path, &["show", id, "--json"]);
        let parsed: Value =
            serde_json::from_str(output.trim()).expect("show --json must emit JSON");
        parsed
            .as_array()
            .expect("show --json emits an array")
            .first()
            .expect("show --json array must carry the requested bead")
            .clone()
    }
}

/// A manually blocked bead is held out of `--ready` without changing its
/// base status — the row shape Pluck's `manual_blocked = 0` clause trusts.
#[test]
fn a_manually_blocked_bead_is_held_out_of_the_ready_frontier() {
    let ws = FrontierWorkspace::new();
    let id = ws.create("manual block frontier bead", &[]);
    assert!(
        ws.ready_ids().contains(&id),
        "a fresh unassigned open bead must start on the ready frontier"
    );

    // The only CLI surface behind Pluck's `AND i.manual_blocked = 0` clause.
    bead_ok(&ws.path, &["update", &id, "--status", "blocked"]);
    assert!(
        !ws.ready_ids().contains(&id),
        "a manually blocked bead must be held out of --ready"
    );

    // A manual block is neither a state change nor a label: base status is
    // retained, the block rides its own column, and the labels array stays
    // empty. This is the distinction that keeps manual blocks from being
    // confounded with label exclusion or an in-progress claim.
    let bead = ws.show(&id);
    assert_eq!(
        bead["status"].as_str(),
        Some("open"),
        "blocking must retain base status"
    );
    assert_eq!(
        bead["effective_status"].as_str(),
        Some("blocked"),
        "the blocked state must surface as the effective status"
    );
    assert_eq!(
        bead["manual_blocked"].as_bool(),
        Some(true),
        "the manual_blocked column must carry the block"
    );
    assert!(bead["assignee"].is_null(), "blocking must not assign");
    assert!(
        bead["labels"]
            .as_array()
            .is_some_and(|labels| labels.is_empty()),
        "a manual block must bite with no label attached"
    );

    // The blocked bead is still listable under its effective status —
    // held, not hidden: operators can see what they parked.
    let blocked = bead_ok(
        &ws.path,
        &["list", "--status", "blocked", "--json", "--limit", "999999"],
    );
    assert!(
        jsonl_ids(&blocked).contains(&id),
        "the effective-status listing must still surface the manually blocked bead"
    );

    // The documented restore path promotes it back.
    bead_ok(&ws.path, &["update", &id, "--status", "open"]);
    assert!(
        ws.ready_ids().contains(&id),
        "clearing the manual block must return the bead to --ready"
    );
    let bead = ws.show(&id);
    assert_eq!(
        bead["manual_blocked"].as_bool(),
        Some(false),
        "--status open must clear the manual block"
    );
    assert_eq!(
        bead["effective_status"].as_str(),
        Some("open"),
        "the effective status must return to open"
    );
}

/// A stale assignment — an assignee nothing has ever answered for — holds
/// the bead out of `--ready`, and clearing it is fail-closed: both recovery
/// verbs refuse until the claim epoch projected by `bead show --json` is
/// presented as `--fencing-token`.
#[test]
fn a_stale_assignment_holds_the_bead_until_the_claim_epoch_credentials_the_clear() {
    let ws = FrontierWorkspace::new();
    let id = ws.create("stale assignment frontier bead", &[]);

    // A worker name nothing has ever held. The frontier must hold the bead
    // on assignment alone — there is no liveness check to wait for, which
    // is exactly what makes an assignment stale-safe.
    bead_ok(
        &ws.path,
        &["update", &id, "--assignee", "ghost-worker-gone"],
    );
    assert!(
        !ws.ready_ids().contains(&id),
        "an assigned open bead must stay out of --ready even when the assignee is a name nothing ever held"
    );

    // bead 0.2.6 fences assignment claims: recovery without the credential
    // fails closed with the lease-conflict exit code, naming the flag that
    // clears it.
    let release = bead_output(&ws.path, &["release", &id]);
    assert_eq!(
        release.status.code(),
        Some(4),
        "release on an assigned-open bead must refuse with the lease-conflict exit code, got stderr: {}",
        String::from_utf8_lossy(&release.stderr).trim()
    );
    let bare = bead_output(&ws.path, &["update", &id, "--clear-assignee"]);
    assert_eq!(
        bare.status.code(),
        Some(4),
        "a bare --clear-assignee must refuse the same way, got stderr: {}",
        String::from_utf8_lossy(&bare.stderr).trim()
    );
    for refusal in [&release, &bare] {
        let stderr = String::from_utf8_lossy(&refusal.stderr);
        assert!(
            stderr.contains("fencing-token"),
            "the refusal must name the recovery flag, got: {stderr}"
        );
    }

    // The current epoch is projected by `bead show --json` — the dynamic
    // half of the recovery recipe the refusal points at.
    let epoch = ws.show(&id)["claim_epoch"]
        .as_u64()
        .expect("an assigned bead must project its claim_epoch");
    assert!(epoch >= 1, "a claim epoch must be positive, got {epoch}");

    // The credentialed clear is the one recovery path, and it returns the
    // bead to the frontier.
    bead_ok(
        &ws.path,
        &[
            "update",
            &id,
            "--clear-assignee",
            "--fencing-token",
            &epoch.to_string(),
        ],
    );
    assert!(
        ws.ready_ids().contains(&id),
        "the fenced clear must return the stale-assigned bead to --ready"
    );
}

/// Pluck's rendered invocation resolves the workspace by absolute path, and
/// that absoluteness is load-bearing: store discovery is purely cwd-based,
/// so the identical command from a cwd above the workspace meets a
/// different store — the workspace-not-absolute starvation root cause.
#[test]
fn the_rendered_invocation_resolves_the_workspace_by_absolute_path() {
    let ws = FrontierWorkspace::new();
    let id = ws.create("workspace resolution frontier bead", &[]);

    // The template Pluck renders: absolute workspace, inner command exact.
    let rendered = format!(
        "(cd {} && bead {})",
        ws.path.display(),
        RENDERED_COMMAND.join(" ")
    );
    assert!(
        rendered.starts_with("(cd /"),
        "the rendered workspace must be absolute, got: {rendered}"
    );

    // What the rendered form does, by construction of its cd: the command
    // runs where the absolute path points, whatever the invoking directory
    // was. The seeded store serves its own beads.
    let served = jsonl_ids(&bead_ok(&ws.path, RENDERED_COMMAND));
    assert!(
        served.contains(&id),
        "the command the absolute render produces must serve the seeded workspace's frontier"
    );

    // The cwd-dependence that makes absoluteness load-bearing: the identical
    // command from the cwd above the workspace stops discovery at the
    // barrier store there — it must error (the barrier is an uninitialized
    // fingerprint) and must never serve the workspace below it.
    let output = bead_output(ws.root(), RENDERED_COMMAND);
    assert!(
        !output.status.success(),
        "the same command from above the workspace must not silently succeed against the barrier"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !jsonl_ids(&stdout).contains(&id),
        "the barrier-root invocation must never serve the seeded workspace's beads, got: {stdout}"
    );
}

/// The raw bead-rs frontier owns state, assignment, manual-block, and
/// unfinished-dependency readiness. Pluck then applies exact label exclusion
/// to that raw JSONL; labels are not a positive readiness requirement.
#[test]
fn the_ready_frontier_requires_all_gates_and_uses_exact_label_exclusion() {
    const EXCLUDED_LABELS: &[&str] = &["deferred", "human", "blocked", "escalation", "alert"];

    let ws = FrontierWorkspace::new();
    let clean = ws.create("all frontier gates pass", &[]);
    let documentation = ws.create("documentation label is inert", &["documentation"]);
    let exact_excluded = ws.create("exact excluded label", &["human"]);
    let case_variant = ws.create("case variant is not excluded", &["Human"]);
    let glob_shaped = ws.create("glob-shaped label is not excluded", &["hum*"]);
    let assigned = ws.create("assigned open is not ready", &[]);
    let in_progress = ws.create("in-progress is not ready", &[]);
    let manually_blocked = ws.create("manual block is not ready", &[]);
    let blocker = ws.create("unfinished dependency blocker", &[]);
    let dependency_blocked = ws.create("unfinished dependency is not ready", &[]);
    let documentation_in_progress =
        ws.create("documentation does not override status", &["documentation"]);

    bead_ok(
        &ws.path,
        &["update", &assigned, "--assignee", "frontier-worker"],
    );
    bead_ok(
        &ws.path,
        &["update", &in_progress, "--status", "in_progress"],
    );
    bead_ok(
        &ws.path,
        &["update", &manually_blocked, "--status", "blocked"],
    );
    bead_ok(
        &ws.path,
        &[
            "update",
            &documentation_in_progress,
            "--status",
            "in_progress",
        ],
    );
    bead_ok(&ws.path, &["dep", "add", &dependency_blocked, &blocker]);

    let raw_beads = ws.ready_beads();
    let raw_ids: Vec<String> = raw_beads
        .iter()
        .map(|bead| {
            bead["id"]
                .as_str()
                .expect("every ready bead must carry an id")
                .to_string()
        })
        .collect();

    // These are the raw bead-rs gates. Labels do not make a bead ready or
    // unready: the documentation bead is present, while the documentation
    // bead with an in-progress status is absent.
    for ready in [
        &clean,
        &documentation,
        &exact_excluded,
        &case_variant,
        &glob_shaped,
    ] {
        assert!(
            raw_ids.contains(ready),
            "an open, unassigned, unblocked bead with no unfinished blocks dependency must be raw-ready: {ready}"
        );
    }
    for held in [
        &assigned,
        &in_progress,
        &manually_blocked,
        &dependency_blocked,
        &documentation_in_progress,
    ] {
        assert!(
            !raw_ids.contains(held),
            "a bead failing a readiness gate must not be raw-ready: {held}"
        );
    }

    // This is the adapter-side Pluck rule: an exact excluded label drops a
    // raw-ready bead, but case variants and wildcard-looking literals remain.
    let pluck_ids: Vec<String> = raw_beads
        .into_iter()
        .filter(|bead| {
            !bead["labels"].as_array().is_some_and(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|label| EXCLUDED_LABELS.contains(&label))
            })
        })
        .map(|bead| {
            bead["id"]
                .as_str()
                .expect("every ready bead must carry an id")
                .to_string()
        })
        .collect();

    assert!(
        !pluck_ids.contains(&exact_excluded),
        "the exact excluded label must be filtered by Pluck"
    );
    for survives in [&clean, &documentation, &case_variant, &glob_shaped] {
        assert!(
            pluck_ids.contains(survives),
            "non-excluded labels must survive Pluck's exact matching: {survives}"
        );
    }
}

/// Checkpoint publication is part of the bead-rs mutation contract: a
/// mutation suppressed with `--no-auto-flush` is not commit-ready until an
/// explicit `sync flush-only` covers its live sequence.
#[test]
fn explicit_checkpoint_flush_publishes_suppressed_mutations() {
    let ws = FrontierWorkspace::new();
    let mutation = ws.create_without_auto_flush("checkpoint mutation", &[]);

    let dirty = ws.checkpoint_status();
    assert_eq!(
        dirty["dirty"], true,
        "the suppressed mutation must dirty the checkpoint"
    );
    assert_eq!(dirty["relationship"], "behind");
    assert_eq!(dirty["ready_to_commit"], false);
    assert!(
        dirty["live_sequence"].as_u64() > dirty["covered_sequence"].as_u64(),
        "the live event sequence must be ahead of the checkpoint after suppression"
    );

    bead_ok(&ws.path, &["sync", "flush-only"]);

    let clean = ws.checkpoint_status();
    assert_eq!(
        clean["dirty"], false,
        "flush-only must clear checkpoint dirtiness"
    );
    assert_eq!(clean["relationship"], "aligned");
    assert_eq!(clean["ready_to_commit"], true);
    assert_eq!(
        clean["live_sequence"], clean["covered_sequence"],
        "the flushed checkpoint must cover every live mutation"
    );

    let root_path = clean["root_path"]
        .as_str()
        .expect("checkpoint status must report the active root path");
    assert!(
        ws.path.join(".beads/checkpoint").join(root_path).is_file(),
        "the status root must point to a published checkpoint object"
    );
    let forensic = fs::read_to_string(ws.path.join(".beads/checkpoint/forensic.jsonl"))
        .expect("flush-only must publish the forensic checkpoint view");
    assert!(
        forensic.contains(&mutation),
        "the published checkpoint must contain the mutation {mutation}"
    );
}
