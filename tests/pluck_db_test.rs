// Test Pluck database connectivity and query construction
// This test verifies that:
// 1. Pluck can connect to and query the beads database
// 2. Query construction matches expected filter configuration
// 3. All filter parameters are properly logged before execution
//
// The suite is hermetic: every store it touches is seeded inside an isolated
// tempdir workspace via the real `bead` CLI. It never reads the live shared
// bead store — that frontier legitimately holds ready beads carrying
// excluded labels (`human` is how operators park beads out of Pluck's
// reach), so asserting on it made the suite fail on environment races
// instead of code (claudego-6003fe75). The seeded store deliberately
// contains a ready bead for every excluded label, so the adapter's
// exclusion contract is pinned under exactly the condition that used to
// break the suite, deterministically.
//
// Hermeticity is structural, not flag-based. The `bead` CLI discovers its
// store by walking up from the cwd to the FIRST `.beads` directory, and a
// valid bead-rs store above the tempdir stops that walk: `bead init` then
// exits 0 having done nothing ("Workspace already exists at: <ancestor>")
// and every later command mints into the foreign store — `bead init` in a
// bare /tmp tempdir is exit-0-no-op against this host's live /tmp/.beads.
// Five runs of this suite planted 80 beads into that store that way on
// 2026-09-24 before anyone noticed (claudego-fb95927b). The seeding below
// therefore builds a geometry the walk cannot escape: a config.json barrier
// `.beads` at the tempdir root (a fingerprint is the only thing that stops
// the walk deterministically — `--skip-foreign-workspace` exists to walk
// *past* config-less `.beads` dirs) and a planted identity fingerprint in
// the seeded workspace itself, so the first init rebuilds around it instead
// of discovering anything above. A prefix tripwire on the first minted bead
// fires if that ever regresses, on any host, poison or no poison.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const PLUCK_STATE: &str = "open";
const PLUCK_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];

/// Workspace identity planted before the first `bead` call.
///
/// Exactly the fresh-clone shape — committed `config.json`, gitignored
/// database absent — that `bead init` rebuilds a store around, preserving
/// the recorded identity. Deterministic values: every run gets its own
/// tempdir, so the identities never meet across runs.
const SEED_PREFIX: &str = "pluckseed";
const SEEDED_IDENTITY: &str = r#"{"created_at":"2026-09-24T00:00:00Z","prefix":"pluckseed","uuid":"00000000-0000-4000-8000-000000000001","version":1}"#;
const BARRIER_IDENTITY: &str = r#"{"created_at":"2026-09-24T00:00:00Z","prefix":"pluckbarrier","uuid":"00000000-0000-4000-8000-000000000002","version":1}"#;

/// A bead workspace seeded inside its own tempdir.
///
/// `_dir` must stay alive for the test's duration — dropping it removes the
/// store. `path` is the workspace root the Pluck backend command runs in: a
/// `ws/` directory inside the tempdir, sitting above the tempdir's barrier
/// `.beads` so no invocation made anywhere under it can discover a store
/// outside the tempdir.
struct SeededWorkspace {
    _dir: TempDir,
    path: PathBuf,
    /// Ready beads carrying no excluded label — Pluck's candidate set.
    clean_ids: Vec<String>,
    /// Ready beads carrying an excluded label — present in the raw frontier,
    /// dropped by the adapter's label exclusion.
    excluded_ids: Vec<String>,
}

/// Run the real `bead` CLI inside `dir`, asserting success and returning stdout.
///
/// Hermeticity comes from the seeded geometry (`seed_workspace`), not from
/// this flag: workspace discovery stops at the first `.beads` on the
/// walk-up, and only a `config.json` fingerprint stops it — a *valid* store
/// above `dir` adopts the run silently even with the override set.
/// `--skip-foreign-workspace` remains as defense in depth for the config-less
/// case (a foreign `.beads` such as the traces directory the CLI itself
/// leaves in /tmp would otherwise fail the command closed from inside `dir`).
fn run_bead(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("bead")
        .arg("--skip-foreign-workspace")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("bead CLI must be executable");
    assert!(
        output.status.success(),
        "bead {} failed: {}",
        args.first().unwrap_or(&"<none>"),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Create one bead in the seeded workspace and return its printed ID.
fn create_bead(dir: &Path, title: &str, labels: &[&str]) -> String {
    let mut args = vec!["create", "--title", title];
    for label in labels {
        args.push("--label");
        args.push(label);
    }
    let stdout = run_bead(dir, &args);
    let id = stdout.trim().to_string();
    assert!(!id.is_empty(), "bead create must print the new issue ID");
    id
}

/// Seed an isolated workspace exercising every frontier shape the adapter
/// must distinguish: clean labeled/unlabelled candidates, one ready bead per
/// excluded label, an assigned-open bead, and an in-progress bead.
fn seed_workspace() -> SeededWorkspace {
    let dir = TempDir::new().expect("tempdir for seeded bead workspace");
    let root = dir.path();

    // Barrier: a bare fingerprint at the tempdir root. Discovery stops at the
    // FIRST `.beads` on the walk-up and only a `config.json` fingerprint stops
    // it deterministically, so no `bead` invocation made anywhere under the
    // tempdir can discover a workspace outside it — whatever the host keeps
    // above $TMPDIR (this box keeps a live store at /tmp/.beads). The barrier
    // itself is an uninitialized fingerprint: stray discovery that lands on
    // it reports that path rather than adopting anything real.
    std::fs::create_dir_all(root.join(".beads")).expect("create barrier .beads");
    std::fs::write(root.join(".beads/config.json"), BARRIER_IDENTITY)
        .expect("write barrier config.json");

    // The seeded workspace carries its fingerprint before the first `bead`
    // call. Without it the first init is the poisoned step: a valid store
    // above the tempdir makes `bead init` exit 0 having done nothing, and
    // every later command mints into that store.
    let path = root.join("ws");
    std::fs::create_dir_all(path.join(".beads")).expect("create seeded .beads");
    std::fs::write(path.join(".beads/config.json"), SEEDED_IDENTITY)
        .expect("write seeded config.json");

    run_bead(&path, &["init"]);
    assert!(
        path.join(".beads/beads.db").exists(),
        "bead init must build the store inside the seeded workspace, not adopt one above it"
    );

    let clean_labeled = create_bead(&path, "clean labeled candidate", &["codinghome"]);
    // Tripwire: the minted prefix proves which store served the create. If
    // discovery ever escapes again, the ID carries the foreign store's
    // prefix and this fires before any assertion runs against the wrong
    // frontier.
    assert!(
        clean_labeled.starts_with(&format!("{SEED_PREFIX}-")),
        "seeding escaped the isolated workspace: {clean_labeled} was not minted under {SEED_PREFIX}-"
    );
    let clean_unlabelled = create_bead(&path, "clean unlabelled candidate", &[]);
    let human = create_bead(
        &path,
        "human-parked candidate",
        &["codinghome", "human", "quota"],
    );
    let deferred = create_bead(&path, "deferred-label candidate", &["deferred"]);
    let blocked = create_bead(&path, "blocked-label candidate", &["blocked"]);
    let starved = create_bead(&path, "starvation-alert candidate", &["starvation-alert"]);
    let assigned = create_bead(&path, "assigned open candidate", &[]);
    let in_progress = create_bead(&path, "in-progress candidate", &[]);
    run_bead(
        &path,
        &["update", assigned.as_str(), "--assignee", "worker-x"],
    );
    run_bead(
        &path,
        &["update", in_progress.as_str(), "--status", "in_progress"],
    );

    SeededWorkspace {
        _dir: dir,
        path,
        clean_ids: vec![clean_labeled, clean_unlabelled],
        excluded_ids: vec![human, deferred, blocked, starved],
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PluckInvocation {
    workspace_path: PathBuf,
    labels: Vec<String>,
    exclude_labels: Vec<String>,
    state: String,
    command: Vec<String>,
}

fn construct_pluck_invocation(
    workspace_path: &str,
    labels: &[&str],
    exclude_labels: &[&str],
    state: &str,
) -> PluckInvocation {
    PluckInvocation {
        workspace_path: PathBuf::from(workspace_path),
        labels: labels.iter().map(|label| (*label).to_string()).collect(),
        exclude_labels: exclude_labels
            .iter()
            .map(|label| (*label).to_string())
            .collect(),
        state: state.to_string(),
        // Pluck passes these filters to the bead store. The bead-rs backend
        // expresses the open/unassigned/ready state as this command; label
        // exclusion is applied to the returned JSON by the store adapter.
        command: vec![
            "bead".to_string(),
            "list".to_string(),
            "--ready".to_string(),
            "--json".to_string(),
            "--limit".to_string(),
            "999999".to_string(),
        ],
    }
}

fn render_pluck_invocation(query: &PluckInvocation) -> String {
    format!(
        "(cd {} && {})",
        query.workspace_path.display(),
        query.command.join(" ")
    )
}

/// Verify the exact backend query Pluck constructs before it is executed,
/// against a seeded isolated workspace — never the live shared store.
#[test]
fn test_pluck_query_matches_expected_configuration() {
    let ws = seed_workspace();
    let labels: &[&str] = &[];
    let query = construct_pluck_invocation(
        ws.path.to_str().expect("tempdir path is UTF-8"),
        labels,
        PLUCK_EXCLUDE_LABELS,
        PLUCK_STATE,
    );

    println!("\n=== PLUCK QUERY PARAMETERS ===");
    println!("workspace_path: {}", query.workspace_path.display());
    println!("labels: {:?}", query.labels);
    println!("exclude_labels: {:?}", query.exclude_labels);
    println!("state: {:?}", query.state);
    println!(
        "final query before execution: {}",
        render_pluck_invocation(&query)
    );
    println!("===============================\n");

    assert_eq!(query.workspace_path, ws.path);
    assert!(
        query.labels.is_empty(),
        "Pluck does not configure include labels"
    );
    assert_eq!(
        query.exclude_labels,
        PLUCK_EXCLUDE_LABELS
            .iter()
            .map(|label| (*label).to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(query.state, PLUCK_STATE);
    assert_eq!(
        query.command,
        ["bead", "list", "--ready", "--json", "--limit", "999999"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    );

    let output = Command::new(&query.command[0])
        .args(&query.command[1..])
        .current_dir(&query.workspace_path)
        .output()
        .expect("Pluck backend command must be executable");
    assert!(
        output.status.success(),
        "Pluck backend query failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    // The raw backend output is the dependency-safe frontier, and label
    // exclusion is applied to the returned JSON by the store adapter (see
    // construct_pluck_invocation's command comment). The seeded frontier
    // deliberately carries a ready bead for every excluded label — the live
    // condition that used to fail this suite — so what is asserted here is
    // both halves of the contract, deterministically: `--ready` surfaces
    // exactly the seeded unassigned open beads, and the adapter's exclusion
    // reduces them to the clean candidates.
    let mut raw_candidates = 0;
    let mut candidates = Vec::new();
    let mut excluded = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let bead: serde_json::Value =
            serde_json::from_str(line).expect("Pluck backend must return JSONL");
        assert_eq!(bead["status"], PLUCK_STATE);
        assert!(bead["assignee"].is_null());
        assert!(bead["labels"].is_array(), "Pluck JSON must include labels");
        let id = bead["id"].as_str().unwrap_or("<unknown id>").to_string();
        if carries_excluded_label(&bead, PLUCK_EXCLUDE_LABELS) {
            excluded.push(id);
        } else {
            candidates.push(id);
        }
        raw_candidates += 1;
    }

    let mut raw_ids: Vec<String> = candidates.iter().chain(excluded.iter()).cloned().collect();
    raw_ids.sort();
    let mut expected_frontier: Vec<String> = ws
        .clean_ids
        .iter()
        .chain(ws.excluded_ids.iter())
        .cloned()
        .collect();
    expected_frontier.sort();
    assert_eq!(
        raw_ids, expected_frontier,
        "--ready must return exactly the seeded unassigned open beads; assigned and in-progress beads stay hidden"
    );

    let mut sorted_candidates = candidates;
    sorted_candidates.sort();
    let mut expected_candidates = ws.clean_ids.clone();
    expected_candidates.sort();
    assert_eq!(
        sorted_candidates, expected_candidates,
        "adapter label exclusion must reduce the frontier to the clean candidates"
    );

    let mut sorted_excluded = excluded;
    sorted_excluded.sort();
    let mut expected_excluded = ws.excluded_ids.clone();
    expected_excluded.sort();
    assert_eq!(
        sorted_excluded, expected_excluded,
        "the beads dropped by label exclusion must be exactly the seeded excluded-label beads"
    );

    println!(
        "Pluck backend returned {raw_candidates} ready candidates; adapter label exclusion drops {sorted_excluded:?}"
    );
}

/// Whether `bead` carries any label in `exclude_labels`.
///
/// Static form of the exclusion decision the store adapter applies to the
/// backend's returned JSONL (NEEDLE `bead_store::excluded_by_labels`, without
/// the expired-quarantine exception). Shared by the seeded-frontier check above
/// and the fixture test below so the two cannot drift.
fn carries_excluded_label(bead: &serde_json::Value, exclude_labels: &[&str]) -> bool {
    bead["labels"]
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .filter_map(|value| value.as_str())
                .any(|label| exclude_labels.contains(&label))
        })
        .unwrap_or(false)
}

/// Label exclusion is deterministic JSONL filtering, independent of any
/// particular frontier: every configured exclude label drops its bead, and
/// only a bead carrying none of them reaches Pluck's candidate set.
/// Exercises all four configured labels plus multi-label mixes so the
/// contract is pinned without needing a store at all.
#[test]
fn test_label_exclusion_drops_excluded_label_candidates() {
    let candidates: &[(&str, &[&str])] = &[
        ("claudego-clean", &["codinghome", "fleet-control"]),
        ("claudego-deferred", &["deferred"]),
        ("claudego-human", &["codinghome", "human", "quota"]),
        ("claudego-blocked", &["blocked"]),
        ("claudego-starved", &["starvation-alert"]),
        (
            "claudego-every-exclusion",
            &["deferred", "human", "blocked", "starvation-alert"],
        ),
    ];

    let survivors: Vec<&str> = candidates
        .iter()
        .filter(|(_, labels)| {
            let bead = serde_json::json!({ "id": "unused", "labels": labels });
            !carries_excluded_label(&bead, PLUCK_EXCLUDE_LABELS)
        })
        .map(|(id, _)| *id)
        .collect();

    assert_eq!(survivors, ["claudego-clean"]);

    // The adapter's parse yields a labels array for every bead; a missing one
    // (malformed JSON) is treated as unlabelled and kept, not a crash.
    let unlabelled = serde_json::json!({ "id": "claudego-unlabelled" });
    assert!(!carries_excluded_label(&unlabelled, PLUCK_EXCLUDE_LABELS));
}

/// Test database connection and basic query functionality against a seeded
/// isolated workspace — the counts asserted below are properties of the
/// seed, not of whatever the live shared store happens to hold.
#[test]
fn test_pluck_database_connectivity() {
    let ws = seed_workspace();
    let db_path = ws.path.join(".beads/beads.db");

    // Define filter parameters
    let labels_filter: Vec<&str> = vec![]; // Empty = no label inclusion filter
    let exclude_labels_filter: Vec<&str> = PLUCK_EXCLUDE_LABELS.to_vec();
    let state_filter: &str = "open";

    // Log filter parameters - workspace_path
    println!("\n=== PLUCK FILTER PARAMETERS ===");
    println!("Workspace path: {}", db_path.display());
    println!("State filter: '{}'", state_filter);
    println!(
        "Labels (include filter): {:?} ({} labels)",
        labels_filter,
        labels_filter.len()
    );
    println!(
        "Exclude labels (exclude filter): {:?} ({} labels)",
        exclude_labels_filter,
        exclude_labels_filter.len()
    );
    println!("Assignee filter: 'IS NULL' (always filters unassigned issues)");
    println!("===============================\n");

    let test_results = test_database_connection(
        &db_path,
        &labels_filter,
        &exclude_labels_filter,
        state_filter,
    );

    // Print results for visibility
    println!("\n=== PLUCK DATABASE CONNECTIVITY TEST RESULTS ===");
    println!("Database path: {}", db_path.display());
    println!("File exists: {}", test_results.file_exists);
    println!("Connection successful: {}", test_results.connection_ok);
    println!("Database integrity check: {}", test_results.integrity_ok);
    println!("Database schema valid: {}", test_results.schema_valid);
    println!("Total issues in database: {}", test_results.total_issues);
    println!("Open issues: {}", test_results.open_issues);
    println!("Issues with labels: {}", test_results.issues_with_labels);
    println!(
        "Claimable (constructed Pluck query): {:?}",
        test_results.claimable_count
    );
    println!("Excluded by labels: {:?}", test_results.excluded_by_labels);

    let error_string = if test_results.errors.is_empty() {
        "None".to_string()
    } else {
        test_results.errors.join("; ")
    };
    println!("Test errors: {}", error_string);
    println!("===================================================\n");

    // Assertions for acceptance criteria
    assert!(test_results.file_exists, "Database file must exist");
    assert!(
        test_results.connection_ok,
        "Must be able to connect to database"
    );
    assert!(
        test_results.integrity_ok,
        "Database integrity check must pass"
    );
    assert!(test_results.schema_valid, "Database schema must be valid");

    // Deterministic seeded-store counts. The seed holds 2 clean candidates,
    // 4 excluded-label beads, 1 assigned-open bead, and 1 in-progress bead.
    let seeded_total = (ws.clean_ids.len() + ws.excluded_ids.len() + 2) as i64;
    assert_eq!(
        test_results.total_issues, seeded_total,
        "seeded store must hold every created bead"
    );
    assert_eq!(
        test_results.open_issues,
        seeded_total - 1,
        "only the in-progress bead leaves base_status='open'"
    );
    assert_eq!(
        test_results.issues_with_labels,
        ws.excluded_ids.len() as i64 + 1,
        "only the labeled clean bead and the excluded-label beads carry labels"
    );
    assert_eq!(
        test_results.claimable_count,
        Some(ws.clean_ids.len() as i64),
        "constructed Pluck query must count exactly the clean candidates"
    );
    assert_eq!(
        test_results.excluded_by_labels,
        Some(ws.excluded_ids.len() as i64),
        "label exclusion must cover exactly the seeded excluded-label beads"
    );

    // If we have errors, report them but don't fail on minor issues
    if !test_results.errors.is_empty() {
        eprintln!("WARNING: Database connectivity issues detected:");
        for error in &test_results.errors {
            eprintln!("  - {}", error);
        }
    }
}

struct DatabaseTestResults {
    file_exists: bool,
    connection_ok: bool,
    integrity_ok: bool,
    schema_valid: bool,
    total_issues: i64,
    open_issues: i64,
    issues_with_labels: i64,
    claimable_count: Option<i64>,
    excluded_by_labels: Option<i64>,
    errors: Vec<String>,
}

fn test_database_connection(
    db_path: &PathBuf,
    labels_filter: &[&str],
    exclude_labels_filter: &[&str],
    state_filter: &str,
) -> DatabaseTestResults {
    let mut results = DatabaseTestResults {
        file_exists: db_path.exists(),
        connection_ok: false,
        integrity_ok: false,
        schema_valid: false,
        total_issues: 0,
        open_issues: 0,
        issues_with_labels: 0,
        claimable_count: None,
        excluded_by_labels: None,
        errors: Vec::new(),
    };

    if !results.file_exists {
        results.errors.push(format!(
            "Database file does not exist: {}",
            db_path.display()
        ));
        return results;
    }

    // Test 1: Can we open the database?
    let conn = match Connection::open(db_path) {
        Ok(conn) => {
            results.connection_ok = true;
            conn
        }
        Err(e) => {
            results
                .errors
                .push(format!("Failed to open database: {}", e));
            return results;
        }
    };

    // Test 2: Check database integrity
    match conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
        Ok(result) => {
            // integrity_check returns "ok" if successful
            results.integrity_ok = result == "ok";
            if !results.integrity_ok {
                results
                    .errors
                    .push(format!("Database integrity check failed: {}", result));
            }
        }
        Err(e) => {
            results
                .errors
                .push(format!("Database integrity check error: {}", e));
            return results;
        }
    };

    // Test 3: Verify schema has expected tables (bead store uses 'issues', not 'beads').
    // bead-rs schema: 'metadata' no longer exists; status lives in 'base_status'.
    let expected_tables = vec!["issues", "labels", "events", "dependencies"];
    let mut tables_query = match conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
    {
        Ok(stmt) => stmt,
        Err(e) => {
            results
                .errors
                .push(format!("Failed to query database schema: {}", e));
            return results;
        }
    };

    let existing_tables: Vec<String> = tables_query
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    for table in &expected_tables {
        if !existing_tables.contains(&table.to_string()) {
            results
                .errors
                .push(format!("Missing expected table: {}", table));
        }
    }

    results.schema_valid = results.errors.is_empty();

    // Test 4: Count total issues (bead store uses 'issues' table)
    match conn.query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0)) {
        Ok(count) => {
            results.total_issues = count;
        }
        Err(e) => {
            results
                .errors
                .push(format!("Failed to query issues count: {}", e));
        }
    }

    // Test 5: Count open issues (Pluck's primary query)
    match conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE base_status = 'open'",
        [],
        |row| row.get(0),
    ) {
        Ok(count) => {
            results.open_issues = count;
        }
        Err(e) => {
            results
                .errors
                .push(format!("Failed to query open issues: {}", e));
        }
    }

    // Test 6: Count issues with labels (Pluck filters by labels)
    match conn.query_row("SELECT COUNT(DISTINCT issue_id) FROM labels", [], |row| {
        row.get(0)
    }) {
        Ok(count) => {
            results.issues_with_labels = count;
        }
        Err(e) => {
            results
                .errors
                .push(format!("Failed to query issues with labels: {}", e));
        }
    }

    // Test 7: Simulate a Pluck query (filter by exclude_labels)
    // Construct the exact query that Pluck would build
    let (query_string, query_params) =
        construct_pluck_query(db_path, labels_filter, exclude_labels_filter, state_filter);

    // Log the complete query construction
    println!("\n=== PLUCK QUERY CONSTRUCTION ===");
    println!("Workspace path: {}", db_path.display());
    println!("State filter: '{}'", state_filter);
    println!("Labels filter (include): {:?}", labels_filter);
    println!("Exclude labels filter: {:?}", exclude_labels_filter);
    println!("--- CONSTRUCTED QUERY ---");
    println!("{}", query_string);
    if !query_params.is_empty() {
        println!("Query parameters: {:?}", query_params);
    }
    println!("===============================\n");

    let pluck_query = &query_string;

    // Verify query matches expected configuration before execution
    println!("=== QUERY VERIFICATION ===");
    println!("✓ Query constructed from provided filter parameters");
    println!("✓ Workspace path: {}", db_path.display());
    println!("✓ State filter: '{}'", state_filter);
    println!(
        "✓ Exclude labels: {:?} ({} labels)",
        exclude_labels_filter,
        exclude_labels_filter.len()
    );
    println!(
        "✓ Include labels: {:?} ({} labels)",
        labels_filter,
        labels_filter.len()
    );
    println!("✓ Assignee filter: always applied (IS NULL)");
    println!("✓ Manual blocked filter: always applied (= 0)");
    println!("========================\n");

    // Verify query structure
    println!("=== QUERY STRUCTURE VERIFICATION ===");
    assert!(
        query_string.contains("SELECT COUNT(DISTINCT i.id)"),
        "Query must select distinct issue IDs"
    );
    assert!(
        query_string.contains("FROM issues i"),
        "Query must use issues table"
    );
    assert!(
        query_string.contains("LEFT JOIN labels"),
        "Query must join labels table"
    );
    assert!(
        query_string.contains(&format!("WHERE i.base_status = '{}'", state_filter)),
        "Query must filter by state"
    );
    assert!(
        query_string.contains("AND i.assignee IS NULL"),
        "Query must filter unassigned issues"
    );
    assert!(
        query_string.contains("AND i.manual_blocked = 0"),
        "Query must filter manually blocked issues"
    );
    if !exclude_labels_filter.is_empty() {
        assert!(
            query_string.contains("AND NOT EXISTS"),
            "Query must exclude specified labels"
        );
    }
    println!("✓ Query structure is valid");
    println!("✓ All expected clauses present");
    println!("==================================\n");

    match conn.query_row(pluck_query, [], |row| row.get::<_, i64>(0)) {
        Ok(claimable_count) => {
            println!("=== QUERY EXECUTION RESULTS ===");
            println!("Claimable issues (Pluck query result): {}", claimable_count);
            println!("✓ Query executed successfully");
            println!("==============================\n");

            // Log query execution summary
            println!("=== QUERY EXECUTION SUMMARY ===");
            println!("✓ Query constructed and verified");
            println!("✓ Database: {}", db_path.display());
            println!("✓ Result: {} claimable issues", claimable_count);
            println!("✓ Filters applied:");
            println!("    - State: '{}'", state_filter);
            println!("    - Assignee: IS NULL");
            println!("    - Manual blocked: = 0");
            if !exclude_labels_filter.is_empty() {
                println!("    - Excluded labels: {:?}", exclude_labels_filter);
            }
            if !labels_filter.is_empty() {
                println!("    - Required labels: {:?}", labels_filter);
            }
            println!("================================\n");

            results.claimable_count = Some(claimable_count);
        }
        Err(e) => {
            results
                .errors
                .push(format!("Failed to execute Pluck-style query: {}", e));
            eprintln!("ERROR: Query execution failed - check query construction above");
        }
    }

    // Test 8: Test actual label filtering.
    let exclude_query = "
        SELECT COUNT(DISTINCT issue_id)
        FROM labels
        WHERE label IN ('deferred', 'human', 'blocked', 'starvation-alert')
    ";

    match conn.query_row(exclude_query, [], |row| row.get::<_, i64>(0)) {
        Ok(excluded_count) => {
            println!("Issues excluded by Pluck filters: {}", excluded_count);
            results.excluded_by_labels = Some(excluded_count);
        }
        Err(e) => {
            results
                .errors
                .push(format!("Failed to query excluded issues: {}", e));
        }
    }

    results
}

/// Constructs the exact Pluck query with all filter parameters
/// Returns the SQL query string and its parameters for logging and verification
/// Note: Uses hardcoded values in query (not parameter binding) to match Pluck's actual behavior
fn construct_pluck_query(
    db_path: &PathBuf,
    labels_filter: &[&str],
    exclude_labels_filter: &[&str],
    state_filter: &str,
) -> (String, Vec<String>) {
    let mut query_parts = Vec::new();
    let mut params = Vec::new();
    let mut construction_log = Vec::new();

    // Log initial parameters
    construction_log.push(format!("=== QUERY CONSTRUCTION START ==="));
    construction_log.push(format!("Workspace: {}", db_path.display()));
    construction_log.push(format!("Initial parameters provided:"));
    construction_log.push(format!("  - state_filter: '{}'", state_filter));
    construction_log.push(format!("  - labels_filter: {} labels", labels_filter.len()));
    construction_log.push(format!(
        "  - exclude_labels_filter: {} labels",
        exclude_labels_filter.len()
    ));

    // Step 1: Base query structure
    query_parts.push("SELECT COUNT(DISTINCT i.id)".to_string());
    construction_log.push(format!(
        "✓ Added SELECT clause for counting distinct issue IDs"
    ));

    query_parts.push("FROM issues i".to_string());
    construction_log.push(format!("✓ Added FROM clause (issues table aliased as 'i')"));

    query_parts.push("LEFT JOIN labels l ON l.issue_id = i.id".to_string());
    construction_log.push(format!("✓ Added LEFT JOIN for labels table"));

    // Step 2: State filter (WHERE clause)
    // bead-rs stores status in the 'base_status' column ('open', 'in_progress', 'deferred', 'closed')
    let where_clause = format!("WHERE i.base_status = '{}'", state_filter);
    query_parts.push(where_clause);
    params.push(format!("state:{}", state_filter));
    construction_log.push(format!(
        "✓ Added WHERE clause with state filter: '{}'",
        state_filter
    ));

    // Step 3: Assignee filter (always applied by Pluck)
    query_parts.push("AND i.assignee IS NULL".to_string());
    params.push("assignee:NULL".to_string());
    construction_log.push(format!(
        "✓ Added assignee filter: IS NULL (Pluck always filters unassigned issues)"
    ));

    // Step 3b: Manual-blocked filter (bead-rs ready frontier excludes manually blocked issues)
    query_parts.push("AND i.manual_blocked = 0".to_string());
    params.push("manual_blocked:0".to_string());
    construction_log.push(format!("✓ Added manual_blocked filter: = 0 (bead-rs ready frontier excludes manually blocked issues)"));

    // Step 4: Exclude labels filter (NOT EXISTS clause)
    if !exclude_labels_filter.is_empty() {
        let labels_list = exclude_labels_filter
            .iter()
            .map(|l| format!("'{}'", l))
            .collect::<Vec<_>>()
            .join(", ");

        let exclude_clause = format!(
            "AND NOT EXISTS (\
                SELECT 1 FROM labels \
                WHERE issue_id = i.id \
                AND label IN ({}) \
            )",
            labels_list
        );
        query_parts.push(exclude_clause);

        construction_log.push(format!("✓ Added exclude_labels filter (NOT EXISTS):"));
        construction_log.push(format!("    Excluded labels: {:?}", exclude_labels_filter));
        for label in exclude_labels_filter {
            params.push(format!("exclude:{}", label));
        }
    } else {
        construction_log.push(format!("○ No exclude_labels filter (empty)"));
    }

    // Step 5: Include labels filter (EXISTS clause)
    if !labels_filter.is_empty() {
        let labels_list = labels_filter
            .iter()
            .map(|l| format!("'{}'", l))
            .collect::<Vec<_>>()
            .join(", ");

        let include_clause = format!(
            "AND EXISTS (\
                SELECT 1 FROM labels \
                WHERE issue_id = i.id \
                AND label IN ({}) \
            )",
            labels_list
        );
        query_parts.push(include_clause);

        construction_log.push(format!("✓ Added labels filter (EXISTS):"));
        construction_log.push(format!("    Included labels: {:?}", labels_filter));
        for label in labels_filter {
            params.push(format!("include:{}", label));
        }
    } else {
        construction_log.push(format!(
            "○ No labels filter (empty - no label inclusion requirement)"
        ));
    }

    let query = query_parts.join("\n  ");

    // Final verification summary
    construction_log.push(format!("=== QUERY CONSTRUCTION COMPLETE ==="));
    construction_log.push(format!("Total query components: {}", query_parts.len()));
    construction_log.push(format!("Total filter parameters tracked: {}", params.len()));
    construction_log.push(format!("Final query parameters: {:?}", params));
    construction_log.push(format!("==============================="));

    // Print construction log
    println!("\n--- QUERY CONSTRUCTION LOG ---");
    for log_entry in construction_log {
        println!("{}", log_entry);
    }
    println!("-------------------------------\n");

    (query, params)
}
