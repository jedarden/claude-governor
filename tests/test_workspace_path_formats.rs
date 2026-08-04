// Test Pluck workspace path filter formats
// Tests which path format correctly returns beads from the database

use std::path::PathBuf;

#[test]
fn test_workspace_path_formats() {
    println!("\n=== Pluck Workspace Path Format Test ===\n");

    // Test different path formats
    let test_paths = vec![
        ("Absolute path (correct)", "/home/coding/claude-governor"),
        ("Parent path (incorrect)", "/home/coding"),
        ("Relative path (dot)", "."),
        ("Relative path (dot-slash)", "./"),
        ("Workspace name only", "claude-governor"),
        ("User home tilde", "~/claude-governor"),
        ("Trailing slash", "/home/coding/claude-governor/"),
        ("Double slash", "/home/coding//claude-governor"),
        ("Non-existent path", "/nonexistent/path"),
    ];

    let mut successful_format = None;

    for (description, path_str) in test_paths {
        println!("\n--- Testing: {} ---", description);
        println!("Path string: '{}'", path_str);

        let db_path = PathBuf::from(path_str).join(".beads").join("beads.db");
        println!("Resolved DB path: '{}'", db_path.display());

        if test_path_format(&db_path, description) {
            successful_format = Some(description.to_string());
        }
    }

    println!("\n=== Test Complete ===");

    if let Some(format) = successful_format {
        println!("\n✅ SUCCESSFUL FORMAT: {}", format);
    } else {
        println!("\n❌ No format successfully returned beads");
    }
}

fn test_path_format(db_path: &PathBuf, description: &str) -> bool {
    // Check if path exists
    let exists = db_path.exists();
    println!("File exists: {}", exists);

    if !exists {
        println!("❌ {} - Path does not exist", description);
        return false;
    }

    // Try to connect and count beads
    match rusqlite::Connection::open(db_path) {
        Ok(conn) => {
            println!("✅ {} - Connection successful", description);

            // Count total beads
            match conn.query_row("SELECT COUNT(*) FROM issues", [], |row| row.get::<_, i64>(0)) {
                Ok(count) => println!("   Total beads: {}", count),
                Err(e) => println!("   Query failed: {}", e),
            }

            // Count open beads
            match conn.query_row(
                "SELECT COUNT(*) FROM issues WHERE status = 'open'",
                [],
                |row| row.get::<_, i64>(0)
            ) {
                Ok(count) => println!("   Open beads: {}", count),
                Err(e) => println!("   Query failed: {}", e),
            }

            // Simulate Pluck query (ready beads)
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
                Ok(count) => {
                    println!("   Ready beads (Pluck query): {}", count);
                    // Return true if we found beads
                    count > 0
                }
                Err(e) => {
                    println!("   Query failed: {}", e);
                    false
                }
            }
        }
        Err(e) => {
            println!("❌ {} - Connection failed: {}", description, e);
            false
        }
    }
}
