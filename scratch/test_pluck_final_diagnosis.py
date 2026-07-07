#!/usr/bin/env python3
"""
Final comprehensive diagnosis of the Pluck configuration filter issue.
This tests the ACTUAL behavior based on NEEDLE code analysis.
"""

import sqlite3
import os
from typing import List, Dict, Any

# From NEEDLE/src/strand/pluck.rs lines 28-36:
# When exclude_labels config is empty [], PluckStrand uses DEFAULT_EXCLUDE_LABELS!
DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked", "starvation-alert"]

# From NEEDLE/.needle.yaml line 29:
NEEDLE_CONFIG_EXCLUDE_LABELS = []

# From NEEDLE/src/strand/pluck.rs line 106:
# PluckStrand ALWAYS queries with assignee: None
# Then filters out beads where b.assignee.is_some() (line 132)

CURRENT_WORKSPACE = "/home/coding/claude-governor"

def get_bead_store_path(workspace: str) -> str:
    return os.path.join(workspace, ".beads", "beads.db")

def get_labels_for_issue(conn: sqlite3.Connection, issue_id: str) -> List[str]:
    cursor = conn.cursor()
    cursor.execute("SELECT label FROM labels WHERE issue_id = ?", (issue_id,))
    return [row[0] for row in cursor.fetchall()]

def simulate_actual_pluck_behavior(workspace: str) -> Dict[str, Any]:
    """
    Simulate EXACTLY what PluckStrand does based on code analysis.
    """
    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return {"claimable": [], "total_open": 0, "filtered_out": []}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Step 1: Query with assignee: None (line 106)
    # This queries for ALL open beads, not filtered by assignee
    store_query = """
        SELECT id, title, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open'
        ORDER BY priority ASC, created_at ASC, id ASC
    """
    cursor.execute(store_query)
    all_candidates = cursor.fetchall()

    # Step 2: Apply the ACTUAL filters from PluckStrand
    claimable = []
    filtered_out = []

    for bead in all_candidates:
        bead_dict = dict(bead)
        reasons = []
        labels = get_labels_for_issue(conn, bead_dict['id'])

        # Filter 1: Exclude labels (line 125)
        # candidates.retain(|b| !b.labels.iter().any(|l| self.exclude_labels.contains(l)));
        if any(label in DEFAULT_EXCLUDE_LABELS for label in labels):
            reasons.append(f"excluded_label: {[l for l in labels if l in DEFAULT_EXCLUDE_LABELS]}")

        # Filter 2: Remove beads with assignee.is_some() (line 132)
        # This filters out beads where assignee is NOT None AND NOT empty string
        if bead_dict.get('assignee') and bead_dict['assignee'].strip():
            reasons.append(f"has_assignee: {bead_dict['assignee']}")

        if reasons:
            filtered_out.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "reasons": reasons,
                "labels": labels,
                "assignee": bead_dict['assignee']
            })
        else:
            claimable.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "priority": bead_dict['priority'],
                "labels": labels,
                "assignee": bead_dict['assignee']
            })

    conn.close()
    return {
        "claimable": claimable,
        "total_open": len(all_candidates),
        "filtered_out": filtered_out
    }

def analyze_empty_vs_null_assignees(workspace: str):
    """
    Analyze the difference between NULL and empty string assignees.
    This is critical because Pluck filters assignee.is_some() which allows
    empty strings but not NULL values.
    """
    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return {}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Count NULL vs empty string assignees
    cursor.execute("""
        SELECT
            COUNT(CASE WHEN assignee IS NULL THEN 1 END) as null_count,
            COUNT(CASE WHEN assignee = '' THEN 1 END) as empty_count,
            COUNT(CASE WHEN assignee IS NOT NULL AND assignee != '' THEN 1 END) as real_count
        FROM issues
        WHERE status = 'open'
    """)

    row = cursor.fetchone()
    result = {
        "null_assignees": row['null_count'],
        "empty_assignees": row['empty_count'],
        "real_assignees": row['real_count']
    }

    # Get examples of each type
    cursor.execute("""
        SELECT id, title, assignee
        FROM issues
        WHERE status = 'open' AND assignee IS NULL
        LIMIT 3
    """)
    result['null_examples'] = [dict(row) for row in cursor.fetchall()]

    cursor.execute("""
        SELECT id, title, assignee
        FROM issues
        WHERE status = 'open' AND assignee = ''
        LIMIT 3
    """)
    result['empty_examples'] = [dict(row) for row in cursor.fetchall()]

    conn.close()
    return result

def main():
    print("=" * 80)
    print("PLUCK CONFIGURATION FILTER DIAGNOSIS - FINAL ANALYSIS")
    print("=" * 80)
    print()

    print("🔍 CODE ANALYSIS FINDINGS:")
    print("-" * 80)
    print("From NEEDLE/src/strand/pluck.rs:")
    print("  Line 28-36: When config exclude_labels is [], USES DEFAULTS!")
    print(f"    DEFAULT_EXCLUDE_LABELS = {DEFAULT_EXCLUDE_LABELS}")
    print()
    print("From NEEDLE/.needle.yaml:")
    print(f"  Line 29: strands.pluck.exclude_labels = {NEEDLE_CONFIG_EXCLUDE_LABELS}")
    print()
    print("From NEEDLE/src/strand/pluck.rs:")
    print("  Line 106: Queries with assignee: None (no agent filtering)")
    print("  Line 132: Filters out beads where assignee.is_some()")
    print()
    print("🎯 KEY INSIGHT:")
    print("  The config has exclude_labels: [] (empty)")
    print("  But PluckStrand code interprets empty as 'use defaults'!")
    print(f"  So ACTUAL exclude_labels = {DEFAULT_EXCLUDE_LABELS}")
    print()

    print("=" * 80)
    print("SIMULATING ACTUAL PLUCK BEHAVIOR")
    print("=" * 80)
    print()

    result = simulate_actual_pluck_behavior(CURRENT_WORKSPACE)

    print(f"Total open beads: {result['total_open']}")
    print(f"Claimable beads: {len(result['claimable'])}")
    print(f"Filtered out: {len(result['filtered_out'])}")
    print()

    if result['claimable']:
        print("✅ First 5 claimable beads:")
        for bead in result['claimable'][:5]:
            print(f"  [{bead['id']}] {bead['title'][:60]}...")
            print(f"    Assignee: {repr(bead['assignee'])}")
            print(f"    Labels: {bead['labels']}")
            print()

    if result['filtered_out']:
        print("🚫 Filtered out beads (first 5):")
        for bead in result['filtered_out'][:5]:
            print(f"  [{bead['id']}] {bead['title'][:60]}...")
            print(f"    Reasons: {', '.join(bead['reasons'])}")
            print(f"    Assignee: {repr(bead['assignee'])}")
            print(f"    Labels: {bead['labels']}")
            print()

    print("=" * 80)
    print("ASSIGNEE ANALYSIS")
    print("=" * 80)
    print()

    assignee_data = analyze_empty_vs_null_assignees(CURRENT_WORKSPACE)

    print(f"Beads with NULL assignee: {assignee_data['null_assignees']}")
    if assignee_data['null_examples']:
        print("  Examples:")
        for ex in assignee_data['null_examples'][:2]:
            print(f"    [{ex['id']}] {ex['title'][:50]}... (assignee={repr(ex['assignee'])})")

    print()
    print(f"Beads with empty string assignee: {assignee_data['empty_assignees']}")
    if assignee_data['empty_examples']:
        print("  Examples:")
        for ex in assignee_data['empty_examples'][:2]:
            print(f"    [{ex['id']}] {ex['title'][:50]}... (assignee={repr(ex['assignee'])})")

    print()
    print(f"Beads with real assignee: {assignee_data['real_assignees']}")

    print()
    print("=" * 80)
    print("ROOT CAUSE DETERMINATION")
    print("=" * 80)
    print()

    if len(result['claimable']) == 0:
        print("❌ PATTERN: Pluck returns 0 beads (STARVATION)")
        print()
        print("🔍 ROOT CAUSE:")
        print("  1. Config has exclude_labels: [] (empty array)")
        print("  2. PluckStrand code interprets [] as 'use defaults'")
        print(f"  3. DEFAULT_EXCLUDE_LABELS = {DEFAULT_EXCLUDE_LABELS}")
        print("  4. Many beads have 'deferred' label (see test output)")
        print("  5. All these beads get filtered out")
        print()
        print("✅ SOLUTION:")
        print("  Option 1: Remove 'deferred' labels from beads that are ready to process")
        print("  Option 2: Change NEEDLE config to explicitly exclude only specific labels:")
        print("            strands.pluck.exclude_labels: ['human', 'blocked', 'starvation-alert']")
        print("            (Note: empty list [] USES DEFAULTS, must list labels to exclude)")
        print()
    else:
        print(f"✅ Pluck would find {len(result['claimable'])} claimable beads")
        print()
        print("This suggests the configuration is working correctly.")
        print()

if __name__ == "__main__":
    main()
