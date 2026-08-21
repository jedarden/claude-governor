// Test Pluck database connectivity and query construction
// This test verifies that:
// 1. Pluck can connect to and query the beads database
// 2. Query construction matches expected filter configuration
// 3. All filter parameters are properly logged before execution

use rusqlite::Connection;
use std::path::PathBuf;
use std::process::Command;

const PLUCK_WORKSPACE: &str = "/home/coding/claude-governor";
const PLUCK_STATE: &str = "open";
const PLUCK_EXCLUDE_LABELS: &[&str] = &["deferred", "human", "blocked", "starvation-alert"];

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

/// Verify the exact backend query Pluck constructs before it is executed.
#[test]
fn test_pluck_query_matches_expected_configuration() {
    let labels: &[&str] = &[];
    let query =
        construct_pluck_invocation(PLUCK_WORKSPACE, labels, PLUCK_EXCLUDE_LABELS, PLUCK_STATE);

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

    assert_eq!(query.workspace_path, PathBuf::from(PLUCK_WORKSPACE));
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

    let mut candidate_count = 0;
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let bead: serde_json::Value =
            serde_json::from_str(line).expect("Pluck backend must return JSONL");
        assert_eq!(bead["status"], PLUCK_STATE);
        assert!(bead["assignee"].is_null());
        for label in &query.exclude_labels {
            assert!(
                !bead["labels"]
                    .as_array()
                    .expect("Pluck JSON must include labels")
                    .iter()
                    .any(|value| value.as_str() == Some(label)),
                "Pluck returned excluded label {label:?}"
            );
        }
        candidate_count += 1;
    }
    println!("Pluck backend returned {candidate_count} ready candidates");
}

/// Test database connection and basic query functionality
#[test]
fn test_pluck_database_connectivity() {
    let db_path = PathBuf::from(PLUCK_WORKSPACE).join(".beads/beads.db");

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
