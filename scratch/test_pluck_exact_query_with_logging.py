#!/usr/bin/env python3
"""
Comprehensive logging of Pluck query construction and execution.
This captures every filter parameter and the exact query being executed.
"""

import sqlite3
import os
import json
from typing import List, Dict, Any, Set, Optional
from datetime import datetime

# Default exclude labels from Pluck configuration
DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked", "starvation-alert"]

def get_bead_store_path(workspace: str) -> str:
    return os.path.join(workspace, ".beads", "beads.db")

def get_labels_for_issue(conn: sqlite3.Connection, issue_id: str) -> List[str]:
    """Get all labels for a specific issue."""
    cursor = conn.cursor()
    cursor.execute("SELECT label FROM labels WHERE issue_id = ?", (issue_id,))
    return [row[0] for row in cursor.fetchall()]

def log_query_construction(workspace: str, agent_id: Optional[str] = None,
                            exclude_labels: List[str] = None) -> Dict[str, Any]:
    """
    Log the complete query construction process that Pluck uses.

    This captures:
    1. Workspace path
    2. Query parameters (assignee, exclude_labels, status)
    3. The SQL query that would be executed
    4. Defensive filtering steps
    5. Final results
    """

    if exclude_labels is None:
        exclude_labels = DEFAULT_EXCLUDE_LABELS.copy()

    print("=" * 80)
    print("PLUCK QUERY CONSTRUCTION LOG")
    print("=" * 80)
    print(f"Timestamp: {datetime.now().isoformat()}")
    print()

    # Log workspace configuration
    print("1. WORKSPACE CONFIGURATION")
    print("-" * 80)
    print(f"Workspace path: {workspace}")
    db_path = get_bead_store_path(workspace)
    print(f"Database path: {db_path}")
    print(f"Database exists: {os.path.exists(db_path)}")
    print()

    if not os.path.exists(db_path):
        print("ERROR: Database not found")
        return {"error": "database_not_found"}

    # Log filter parameters
    print("2. FILTER PARAMETERS")
    print("-" * 80)
    print(f"Assignee filter: {agent_id if agent_id else 'None (unassigned beads only)'}")
    print(f"Exclude labels: {exclude_labels}")
    print(f"Status filter: open")
    print()

    # Log the SQL query construction
    print("3. SQL QUERY CONSTRUCTION")
    print("-" * 80)

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Build the query exactly as Pluck would
    if agent_id:
        query = """
            SELECT id, title, status, assignee, priority, created_at
            FROM issues
            WHERE status = 'open' AND assignee = ?
            ORDER BY priority ASC, created_at ASC, id ASC
        """
        params = (agent_id,)
        print(f"Query: {query.strip()}")
        print(f"Parameters: {params}")
    else:
        query = """
            SELECT id, title, status, assignee, priority, created_at
            FROM issues
            WHERE status = 'open'
            ORDER BY priority ASC, created_at ASC, id ASC
        """
        params = ()
        print(f"Query: {query.strip()}")
        print(f"Parameters: (none)")

    print()

    # Execute the query
    print("4. QUERY EXECUTION")
    print("-" * 80)
    cursor.execute(query, params)
    store_results = cursor.fetchall()
    print(f"Raw results from database: {len(store_results)} beads")
    print()

    # Log store-level results (before defensive filtering)
    print("5. STORE-LEVEL RESULTS (before defensive filtering)")
    print("-" * 80)
    for i, bead in enumerate(store_results[:5], 1):  # Show first 5
        bead_dict = dict(bead)
        labels = get_labels_for_issue(conn, bead_dict['id'])
        print(f"{i}. [{bead_dict['id']}] {bead_dict['title'][:60]}...")
        print(f"   Status: {bead_dict['status']}, Assignee: {bead_dict['assignee']}, Priority: {bead_dict['priority']}")
        print(f"   Labels: {labels}")

    if len(store_results) > 5:
        print(f"   ... and {len(store_results) - 5} more")
    print()

    # Log defensive filtering steps
    print("6. DEFENSIVE FILTERING (PluckStrand)")
    print("-" * 80)

    claimable = []
    filtered_out = []

    for bead in store_results:
        bead_dict = dict(bead)
        reasons = []
        labels = get_labels_for_issue(conn, bead_dict['id'])

        # Filter by excluded labels (defensive)
        if any(label in exclude_labels for label in labels):
            excluded = [l for l in labels if l in exclude_labels]
            reasons.append(f"excluded_labels: {excluded}")

        # Filter out InProgress status
        if bead_dict["status"] == "in_progress":
            reasons.append("status: in_progress")

        # Filter by stale assignee
        if bead_dict.get("assignee") and bead_dict["assignee"] != agent_id:
            reasons.append(f"assignee: {bead_dict['assignee']}")

        if reasons:
            filtered_out.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "reasons": reasons,
                "labels": labels
            })
        else:
            claimable.append({
                "id": bead_dict['id'],
                "title": bead_dict['title'],
                "priority": bead_dict['priority'],
                "labels": labels
            })

    print(f"Total store results: {len(store_results)}")
    print(f"After defensive filtering: {len(claimable)} claimable, {len(filtered_out)} filtered")
    print()

    # Log filtered beads with reasons
    if filtered_out:
        print("7. FILTERED BEADS (with reasons)")
        print("-" * 80)
        for i, bead in enumerate(filtered_out[:10], 1):  # Show first 10
            print(f"{i}. [{bead['id']}] {bead['title'][:60]}...")
            for reason in bead['reasons']:
                print(f"   - {reason}")
            print(f"   Labels: {bead['labels']}")

        if len(filtered_out) > 10:
            print(f"   ... and {len(filtered_out) - 10} more")
        print()

    # Log final claimable beads
    print("8. FINAL CLAIMABLE BEADS")
    print("-" * 80)
    if claimable:
        for i, bead in enumerate(claimable[:10], 1):  # Show first 10
            print(f"{i}. [{bead['id']}] Priority {bead['priority']} - {bead['title'][:70]}...")
            print(f"   Labels: {bead['labels']}")

        if len(claimable) > 10:
            print(f"   ... and {len(claimable) - 10} more")
    else:
        print("⚠️  STARVATION: No claimable beads found!")
    print()

    # Log summary
    print("9. QUERY SUMMARY")
    print("-" * 80)
    print(f"Workspace: {workspace}")
    print(f"Assignee filter: {agent_id if agent_id else 'None'}")
    print(f"Exclude labels: {exclude_labels}")
    print(f"Store results: {len(store_results)}")
    print(f"Claimable beads: {len(claimable)}")
    print(f"Filtered beads: {len(filtered_out)}")
    print()

    conn.close()

    return {
        "workspace": workspace,
        "agent_id": agent_id,
        "exclude_labels": exclude_labels,
        "store_results": len(store_results),
        "claimable": len(claimable),
        "filtered": len(filtered_out),
        "claimable_beads": claimable,
        "filtered_beads": filtered_out
    }

def main():
    """Test different Pluck query scenarios."""
    workspace = "/home/coding/claude-governor"

    # Test 1: Default Pluck query (no assignee, default exclude labels)
    print("\n\n")
    print("#" * 80)
    print("# TEST 1: DEFAULT PLUCK QUERY (no assignee, default exclude labels)")
    print("#" * 80)
    result1 = log_query_construction(workspace, agent_id=None, exclude_labels=DEFAULT_EXCLUDE_LABELS)

    # Test 2: Pluck query with specific agent
    print("\n\n")
    print("#" * 80)
    print("# TEST 2: PLUCK QUERY WITH AGENT")
    print("#" * 80)
    result2 = log_query_construction(
        workspace,
        agent_id="claude-code-glm47-test-pluck-debug",
        exclude_labels=DEFAULT_EXCLUDE_LABELS
    )

    # Test 3: Pluck query with custom exclude labels
    print("\n\n")
    print("#" * 80)
    print("# TEST 3: PLUCK QUERY WITH CUSTOM EXCLUDE LABELS")
    print("#" * 80)
    result3 = log_query_construction(
        workspace,
        agent_id=None,
        exclude_labels=["deferred", "human"]
    )

    # Test 4: Pluck query with no exclude labels
    print("\n\n")
    print("#" * 80)
    print("# TEST 4: PLUCK QUERY WITH NO EXCLUDE LABELS")
    print("#" * 80)
    result4 = log_query_construction(
        workspace,
        agent_id=None,
        exclude_labels=[]
    )

    # Final summary
    print("\n\n")
    print("=" * 80)
    print("FINAL SUMMARY - ALL TESTS")
    print("=" * 80)
    print(f"Test 1 (default): {result1['store_results']} store → {result1['claimable']} claimable")
    print(f"Test 2 (with agent): {result2['store_results']} store → {result2['claimable']} claimable")
    print(f"Test 3 (custom excludes): {result3['store_results']} store → {result3['claimable']} claimable")
    print(f"Test 4 (no excludes): {result4['store_results']} store → {result4['claimable']} claimable")
    print()

if __name__ == "__main__":
    main()
