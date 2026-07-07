#!/usr/bin/env python3
"""
Reproduce the EXACT starvation condition: "Pluck returns 0 beads when 37 are open"

This identifies the specific query condition that causes starvation.
"""

import sqlite3
import os
from typing import List, Dict, Any

DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked", "starvation-alert"]

def get_bead_store_path(workspace: str) -> str:
    return os.path.join(workspace, ".beads", "beads.db")

def get_labels_for_issue(conn: sqlite3.Connection, issue_id: str) -> List[str]:
    """Get all labels for a specific issue."""
    cursor = conn.cursor()
    cursor.execute("SELECT label FROM labels WHERE issue_id = ?", (issue_id,))
    return [row[0] for row in cursor.fetchall()]

def has_excluded_label(labels: List[str], excluded: List[str]) -> bool:
    """Check if any excluded label is present."""
    return bool(set(labels) & set(excluded))

def diagnose_starvation_conditions(workspace: str) -> Dict[str, Any]:
    """
    Diagnose the exact conditions that would cause Pluck to return 0 beads
    when there are actually N open beads.
    """
    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return {"error": "Database not found"}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Get total open beads
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'open'")
    total_open = cursor.fetchone()[0]

    # Get the in-progress bead for test-pluck-debug
    cursor.execute("""
        SELECT id, title, assignee, status
        FROM issues
        WHERE assignee = 'claude-code-glm47-test-pluck-debug'
        AND status = 'in_progress'
    """)
    in_progress_bead = cursor.fetchone()

    # Simulate what happens when test-pluck-debug tries to claim a bead
    # Step 1: Query beads assigned to this agent
    cursor.execute("""
        SELECT id, title, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open' AND assignee = 'claude-code-glm47-test-pluck-debug'
        ORDER BY priority ASC, created_at ASC, id ASC
    """)
    assigned_open = cursor.fetchall()

    # Step 2: Apply exclude_labels filter
    claimable = []
    filtered_out = []

    for bead in assigned_open:
        bead_dict = dict(bead)
        labels = get_labels_for_issue(conn, bead_dict['id'])

        if has_excluded_label(labels, DEFAULT_EXCLUDE_LABELS):
            excluded_labels = set(labels) & set(DEFAULT_EXCLUDE_LABELS)
            filtered_out.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "reason": f"excluded_labels: {excluded_labels}",
                "labels": labels
            })
        else:
            claimable.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "labels": labels
            })

    # Now test: what if we query WITHOUT agent assignment?
    cursor.execute("""
        SELECT id, title, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open'
        ORDER BY priority ASC, created_at ASC, id ASC
    """)
    all_open = cursor.fetchall()

    # Apply filtering to all open beads
    claimable_all = []
    filtered_all = []

    for bead in all_open:
        bead_dict = dict(bead)
        labels = get_labels_for_issue(conn, bead_dict['id'])

        reasons = []
        if has_excluded_label(labels, DEFAULT_EXCLUDE_LABELS):
            excluded_labels = set(labels) & set(DEFAULT_EXCLUDE_LABELS)
            reasons.append(f"excluded_labels: {excluded_labels}")

        if bead_dict.get("assignee") and bead_dict["assignee"] != "claude-code-glm47-test-pluck-debug":
            reasons.append(f"assigned_to: {bead_dict['assignee']}")

        if reasons:
            filtered_all.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "reasons": reasons,
                "assignee": bead_dict.get('assignee'),
                "labels": labels
            })
        else:
            claimable_all.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "assignee": bead_dict.get('assignee'),
                "labels": labels
            })

    conn.close()

    return {
        "total_open": total_open,
        "in_progress_bead": dict(in_progress_bead) if in_progress_bead else None,
        "agent_query": {
            "assigned_to_agent": len(assigned_open),
            "claimable": len(claimable),
            "filtered_out": len(filtered_out),
            "claimable_beads": claimable,
            "filtered_beads": filtered_out
        },
        "unassigned_query": {
            "total_open_unfiltered": len(all_open),
            "claimable": len(claimable_all),
            "filtered": len(filtered_all),
            "claimable_beads": claimable_all[:5],  # First 5
            "filtered_beads": filtered_all[:5]  # First 5
        }
    }

def main():
    workspace = "/home/coding/claude-governor"

    print("=" * 80)
    print("PLUCK STARVATION ROOT CAUSE DIAGNOSIS")
    print("=" * 80)
    print(f"Workspace: {workspace}")
    print(f"Agent: claude-code-glm47-test-pluck-debug")
    print()

    result = diagnose_starvation_conditions(workspace)

    if "error" in result:
        print(f"ERROR: {result['error']}")
        return

    print("OVERVIEW")
    print("-" * 80)
    print(f"Total open beads in workspace: {result['total_open']}")
    print()

    if result['in_progress_bead']:
        print(f"Agent currently working on: [{result['in_progress_bead']['id']}] {result['in_progress_bead']['title'][:60]}...")
        print()

    print("QUERY SCENARIO 1: WITH AGENT ASSIGNMENT")
    print("-" * 80)
    print("Query: SELECT * FROM issues WHERE status='open' AND assignee='claude-code-glm47-test-pluck-debug'")
    print()
    print(f"Open beads assigned to this agent: {result['agent_query']['assigned_to_agent']}")
    print(f"After exclude_labels filter: {result['agent_query']['claimable']} claimable")
    print(f"Filtered out: {result['agent_query']['filtered_out']}")
    print()

    if result['agent_query']['claimable'] == 0:
        print("⚠️  STARVATION CONDITION DETECTED!")
        print()
        print("Explanation:")
        print("  1. Agent queries for beads assigned to itself")
        print("  2. No open beads are assigned to this agent")
        print("  3. Result: 0 claimable beads")
        print()
        print("This is the ROOT CAUSE of '0 beads when N are open'")
    else:
        print(f"✓ Agent would claim: {result['agent_query']['claimable_beads'][0]['id']}")

    print()
    print("QUERY SCENARIO 2: WITHOUT AGENT ASSIGNMENT (Unassigned Pluck)")
    print("-" * 80)
    print("Query: SELECT * FROM issues WHERE status='open'")
    print()
    print(f"Total open beads: {result['unassigned_query']['total_open_unfiltered']}")
    print(f"After exclude_labels filter: {result['unassigned_query']['claimable']} claimable")
    print(f"Filtered out: {result['unassigned_query']['filtered']}")
    print()

    if result['unassigned_query']['claimable']:
        print("First 5 claimable beads (unassigned):")
        for bead in result['unassigned_query']['claimable_beads']:
            print(f"  [{bead['id']}] {bead['title'][:60]}...")
            print(f"    Assignee: {bead['assignee'] if bead['assignee'] else '(none)'}")
            print(f"    Labels: {', '.join(bead['labels']) if bead['labels'] else '(none)'}")

    print()
    print("ROOT CAUSE SUMMARY")
    print("=" * 80)
    print()
    print("The starvation occurs because:")
    print()
    print("  1. NEEDLE workers query beads WHERE assignee = <their_agent_id>")
    print("  2. Most beads have assignee = NULL or empty string")
    print("  3. The query returns 0 rows")
    print("  4. Worker thinks there are no beads available")
    print()
    print("SOLUTION:")
    print("  - Workers should query for assignee IS NULL when unassigned")
    print("  - OR workers should use a separate 'unassigned' query mode")
    print("  - OR beads should be pre-assigned to agents")
    print()

if __name__ == "__main__":
    main()
