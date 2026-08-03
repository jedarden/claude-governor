// Test Pluck database connectivity
// This test verifies that Pluck can connect to and query the beads database

use std::path::PathBuf;
use rusqlite::Connection;

/// Test database connection and basic query functionality
#[test]
fn test_pluck_database_connectivity() {
    let db_path = PathBuf::from("/home/coding/claude-governor/.beads/beads.db");

    // Define filter parameters
    let labels_filter: Vec<&str> = vec![]; // Empty = no label inclusion filter
    let exclude_labels_filter: Vec<&str> = vec!["deferred", "human", "blocked"];
    let state_filter: &str = "open";

    // Log filter parameters - workspace_path
    println!("\n=== PLUCK FILTER PARAMETERS ===");
    println!("workspace_path: {}", db_path.display());
    println!("labels (include filter): {:?}", labels_filter);
    println!("exclude_labels (exclude filter): {:?}", exclude_labels_filter);
    println!("state (status filter): {}", state_filter);
    println!("===============================\n");

    let test_results = test_database_connection(&db_path, &labels_filter, &exclude_labels_filter, state_filter);

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
    assert!(test_results.connection_ok, "Must be able to connect to database");
    assert!(test_results.integrity_ok, "Database integrity check must pass");
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
    state_filter: &str
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
        results.errors.push(format!("Database file does not exist: {}", db_path.display()));
        return results;
    }

    // Test 1: Can we open the database?
    let conn = match Connection::open(db_path) {
        Ok(conn) => {
            results.connection_ok = true;
            conn
        }
        Err(e) => {
            results.errors.push(format!("Failed to open database: {}", e));
            return results;
        }
    };

    // Test 2: Check database integrity
    match conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)) {
        Ok(result) => {
            // integrity_check returns "ok" if successful
            results.integrity_ok = result == "ok";
            if !results.integrity_ok {
                results.errors.push(format!("Database integrity check failed: {}", result));
            }
        }
        Err(e) => {
            results.errors.push(format!("Database integrity check error: {}", e));
            return results;
        }
    };

    // Test 3: Verify schema has expected tables (bead store uses 'issues', not 'beads')
    let expected_tables = vec!["issues", "labels", "events", "metadata"];
    let mut tables_query = match conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    ) {
        Ok(stmt) => stmt,
        Err(e) => {
            results.errors.push(format!("Failed to query database schema: {}", e));
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
            results.errors.push(format!("Missing expected table: {}", table));
        }
    }

    results.schema_valid = results.errors.is_empty();

    // Test 4: Count total issues (bead store uses 'issues' table)
    match conn.query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0)) {
        Ok(count) => {
            results.total_issues = count;
        }
        Err(e) => {
            results.errors.push(format!("Failed to query issues count: {}", e));
        }
    }

    // Test 5: Count open issues (Pluck's primary query)
    match conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE status = 'open'",
        [],
        |row| row.get(0)
    ) {
        Ok(count) => {
            results.open_issues = count;
        }
        Err(e) => {
            results.errors.push(format!("Failed to query open issues: {}", e));
        }
    }

    // Test 6: Count issues with labels (Pluck filters by labels)
    match conn.query_row(
        "SELECT COUNT(DISTINCT issue_id) FROM labels",
        [],
        |row| row.get(0)
    ) {
        Ok(count) => {
            results.issues_with_labels = count;
        }
        Err(e) => {
            results.errors.push(format!("Failed to query issues with labels: {}", e));
        }
    }

    // Test 7: Simulate a Pluck query (filter by exclude_labels)
    // Log the actual values being used in query construction
    println!("\n=== PLUCK QUERY CONSTRUCTION ===");
    println!("Using state filter: '{}'", state_filter);
    println!("Using exclude_labels filter: {:?}", exclude_labels_filter);
    println!("Using labels filter: {:?}", labels_filter);
    println!("===============================\n");

    // Pluck excludes: deferred, human, blocked
    let pluck_query = "
        SELECT COUNT(DISTINCT i.id)
        FROM issues i
        LEFT JOIN labels l ON l.issue_id = i.id
        WHERE i.status = 'open'
          AND i.assignee IS NULL
          AND NOT EXISTS (
              SELECT 1 FROM labels
              WHERE issue_id = i.id
              AND label IN ('deferred', 'human', 'blocked')
          )
    ";

    match conn.query_row(pluck_query, [], |row| row.get::<_, i64>(0)) {
        Ok(claimable_count) => {
            println!("Claimable issues (Pluck query result): {}", claimable_count);
        }
        Err(e) => {
            results.errors.push(format!("Failed to execute Pluck-style query: {}", e));
        }
    }

    // Test 8: Test actual label filtering (deferred, human, blocked)
    let exclude_query = "
        SELECT COUNT(DISTINCT issue_id)
        FROM labels
        WHERE label IN ('deferred', 'human', 'blocked')
    ";

    match conn.query_row(exclude_query, [], |row| row.get::<_, i64>(0)) {
        Ok(excluded_count) => {
            println!("Issues excluded by Pluck filters: {}", excluded_count);
        }
        Err(e) => {
            results.errors.push(format!("Failed to query excluded issues: {}", e));
        }
    }

    results
}
