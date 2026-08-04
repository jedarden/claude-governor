// Systematic Pluck filter combination test
// Tests each filter individually and in combination to identify blocking conditions

use std::path::PathBuf;
use rusqlite::Connection;

#[test]
fn test_pluck_filter_combinations() {
    let db_path = PathBuf::from("/home/coding/claude-governor/.beads/beads.db");

    println!("\n=== SYSTEMATIC PLUCK FILTER COMBINATION TEST ===");
    println!("Database path: {}", db_path.display());
    println!("================================================\n");

    let conn = match Connection::open(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            panic!("Failed to open database: {}", e);
        }
    };

    // Define test scenarios
    let scenarios = vec![
        ("BASE - No filters",
         "SELECT COUNT(DISTINCT i.id) FROM issues i LEFT JOIN labels l ON l.issue_id = i.id",
         vec![]),

        ("Only state = 'open'",
         "SELECT COUNT(DISTINCT i.id) FROM issues i WHERE status = 'open'",
         vec!["state=open"]),

        ("Only assignee IS NULL",
         "SELECT COUNT(DISTINCT i.id) FROM issues i WHERE assignee IS NULL",
         vec!["assignee=NULL"]),

        ("Only exclude_labels (deferred, human, blocked)",
         "SELECT COUNT(DISTINCT i.id) FROM issues i WHERE NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'human', 'blocked'))",
         vec!["exclude_labels"]),

        ("state + assignee",
         "SELECT COUNT(DISTINCT i.id) FROM issues i WHERE status = 'open' AND assignee IS NULL",
         vec!["state=open", "assignee=NULL"]),

        ("state + exclude_labels",
         "SELECT COUNT(DISTINCT i.id) FROM issues i WHERE status = 'open' AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'human', 'blocked'))",
         vec!["state=open", "exclude_labels"]),

        ("assignee + exclude_labels",
         "SELECT COUNT(DISTINCT i.id) FROM issues i WHERE assignee IS NULL AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'human', 'blocked'))",
         vec!["assignee=NULL", "exclude_labels"]),

        ("FULL QUERY - state + assignee + exclude_labels",
         "SELECT COUNT(DISTINCT i.id) FROM issues i WHERE status = 'open' AND assignee IS NULL AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'human', 'blocked'))",
         vec!["state=open", "assignee=NULL", "exclude_labels"]),
    ];

    // Test each scenario
    let mut results = Vec::new();
    for (name, query, filters) in scenarios {
        match conn.query_row(query, [], |row| row.get::<_, i64>(0)) {
            Ok(count) => {
                println!("✓ {}: {} issues", name, count);
                results.push((name, count, filters.clone()));

                // Highlight blocking conditions
                if count == 0 {
                    println!("  ⚠️  BLOCKING CONDITION FOUND!");
                    println!("  Filters applied: {:?}", filters);
                }
            }
            Err(e) => {
                println!("✗ {}: Query failed - {}", name, e);
            }
        }
    }

    println!("\n=== ANALYSIS ===");
    println!("Total scenarios tested: {}", results.len());
    let blocking: Vec<_> = results.iter().filter(|(_, count, _)| *count == 0).collect();
    println!("Blocking conditions found: {}", blocking.len());

    for (name, count, filters) in &results {
        if *count == 0 {
            println!("  - {} (filters: {:?})", name, filters);
        }
    }

    // Find the delta - what each filter removes
    println!("\n=== FILTER IMPACT ANALYSIS ===");
    let base_count = results[0].1;  // BASE - No filters
    println!("Base count (no filters): {}", base_count);

    for (name, count, _) in &results {
        let delta = base_count - count;
        let percentage = if base_count > 0 {
            (delta as f64 / base_count as f64) * 100.0
        } else {
            0.0
        };
        println!("{}: Δ={} ({}% reduction)", name, delta, percentage);
    }
    println!("=========================\n");

    // Find the transition point - where it first becomes 0
    println!("=== BLOCKING CONDITION IDENTIFICATION ===");
    let mut prev_count = base_count;
    for (name, count, filters) in &results {
        if count == &0 && prev_count > 0 {
            println!("⚠️  FIRST BLOCKING CONDITION: {}", name);
            println!("   Previous count: {}", prev_count);
            println!("   Current count: {}", count);
            println!("   Filters: {:?}", filters);
        }
        prev_count = *count;
    }
    println!("=========================================\n");
}
