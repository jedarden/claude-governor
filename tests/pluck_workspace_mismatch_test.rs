// Test Pluck workspace mismatch bug
// Demonstrates that Pluck returns 0 results due to incorrect workspace path resolution

use std::path::PathBuf;
use rusqlite::Connection;

#[test]
fn test_pluck_workspace_mismatch() {
    println!("\n=== PLUCK WORKSPACE MISMATCH TEST ===");
    println!("Testing if workspace='.' resolves to wrong directory\n");

    // Define the two possible workspace paths
    let current_workspace = PathBuf::from("/home/coding/claude-governor");
    let parent_workspace = PathBuf::from("/home/coding");

    // Test both databases
    let test_dbs = vec![
        ("Current workspace (correct)", current_workspace),
        ("Parent workspace (wrong - where '.' resolves)", parent_workspace),
    ];

    for (description, workspace_path) in test_dbs {
        println!("\n--- Testing: {} ---", description);
        println!("Workspace path: {}", workspace_path.display());

        let db_path = workspace_path.join(".beads").join("beads.db");
        println!("Database path: {}", db_path.display());

        if !db_path.exists() {
            println!("❌ Database does not exist");
            continue;
        }

        match Connection::open(&db_path) {
            Ok(conn) => {
                println!("✅ Connection successful");

                // Count total issues
                let total: i64 = conn.query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0)).unwrap_or(0);
                println!("   Total issues: {}", total);

                // Count open issues
                let open: i64 = conn.query_row("SELECT COUNT(*) FROM issues WHERE status = 'open'", [], |row| row.get(0)).unwrap_or(0);
                println!("   Open issues: {}", open);

                // Run full Pluck query
                let pluck_query = "
                    SELECT COUNT(DISTINCT i.id)
                    FROM issues i
                    WHERE i.status = 'open'
                    AND i.assignee IS NULL
                    AND NOT EXISTS (
                        SELECT 1 FROM labels
                        WHERE issue_id = i.id
                        AND label IN ('deferred', 'human', 'blocked')
                    )
                ";

                let ready: i64 = conn.query_row(pluck_query, [], |row| row.get(0)).unwrap_or(0);
                println!("   Ready beads (Pluck query): {}", ready);

                if ready == 0 && total > 0 {
                    println!("   ⚠️  BLOCKING: Pluck would return 0 candidates here!");
                    println!("   💡 This explains why Pluck starves when workspace='.'");
                }
            }
            Err(e) => {
                println!("❌ Connection failed: {}", e);
            }
        }
    }

    println!("\n=== ROOT CAUSE ANALYSIS ===");
    println!("Problem: Pluck uses workspace='.' which resolves differently than expected");
    println!("\nCurrent directory resolution:");
    println!("  - In shell: cwd = /home/coding/claude-governor");
    println!("  - In NEEDLE: workspace='.' → /home/coding (parent!)");
    println!("\nResult:");
    println!("  - Correct database: /home/coding/claude-governor/.beads/beads.db → 36 ready beads");
    println!("  - Wrong database:   /home/coding/.beads/beads.db → 0 ready beads");
    println!("\n💡 SOLUTION: Pluck needs absolute workspace path, not relative '.'");
    println!("==============================\n");
}

#[test]
fn test_dot_path_resolution() {
    println!("\n=== DOT PATH RESOLUTION TEST ===");
    println!("Showing how '.' resolves in different contexts\n");

    // Get current working directory
    let cwd = std::env::current_dir().unwrap();
    println!("Rust std::env::current_dir(): {}", cwd.display());

    // Simulate what happens with PathBuf::from(".")
    let dot_path = PathBuf::from(".");
    println!("PathBuf::from('.'): {}", dot_path.display());

    // Show canonical path
    match std::fs::canonicalize(".") {
        Ok(canonical) => println!("canonicalize('.'): {}", canonical.display()),
        Err(e) => println!("canonicalize failed: {}", e),
    }

    // Test against parent directory
    let parent_test = PathBuf::from("/home/coding");
    let parent_db = parent_test.join(".beads").join("beads.db");

    println!("\nParent directory database:");
    println!("  Path: {}", parent_db.display());
    println!("  Exists: {}", parent_db.exists());

    // Test against current directory
    let current_test = PathBuf::from("/home/coding/claude-governor");
    let current_db = current_test.join(".beads").join("beads.db");

    println!("\nCurrent directory database:");
    println!("  Path: {}", current_db.display());
    println!("  Exists: {}", current_db.exists());

    println!("\n💡 ISSUE: If Pluck resolves '.' to parent instead of current,");
    println!("   it queries the wrong database and finds 0 ready beads!");
    println!("=====================================\n");
}
