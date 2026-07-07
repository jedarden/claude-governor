#!/usr/bin/env python3
"""
Test to replicate EXACTLY what Pluck does when querying for beads.
This simulates the exact query conditions and filtering that Pluck applies.
"""

import sqlite3
import os
from typing import List, Dict, Any, Set

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

def simulate_pluck_query(workspace: str, agent_id: str = None) -> Dict[str, Any]:
    """
    Simulate exactly what PluckStrand does when querying for beads.
    This replicates the logic from NEEDLE's PluckStrand::run() method.
    """
    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return {"claimable": [], "total_open": 0, "filtered_out": []}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Step 1: Store-level query (what Pluck requests from the bead store)
    # Pluck requests open beads, optionally filtered by assignee
    if agent_id:
        store_query = """
            SELECT id, title, status, assignee, priority, created_at
            FROM issues
            WHERE status = 'open' AND assignee = ?
            ORDER BY priority ASC, created_at ASC, id ASC
        """
        cursor.execute(store_query, (agent_id,))
    else:
        store_query = """
            SELECT id, title, status, assignee, priority, created_at
            FROM issues
            WHERE status = 'open'
            ORDER BY priority ASC, created_at ASC, id ASC
        """
        cursor.execute(store_query)

    store_results = cursor.fetchall()
    total_open = len(store_results)

    # Step 2: Apply Pluck's defensive filtering
    claimable = []
    filtered_out = []

    for bead in store_results:
        bead_dict = dict(bead)
        reasons = []

        # Get labels
        labels = get_labels_for_issue(conn, bead_dict['id'])

        # Filter by excluded labels (defensive)
        if has_excluded_label(labels, DEFAULT_EXCLUDE_LABELS):
            excluded_labels = set(labels) & set(DEFAULT_EXCLUDE_LABELS)
            reasons.append(f"excluded_labels: {excluded_labels}")

        # Filter out InProgress status
        if bead_dict["status"] == "in_progress":
            reasons.append("status: in_progress")

        # Filter by stale assignee (simplified - normally checks TTL)
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

    conn.close()
    return {
        "claimable": claimable,
        "total_open": total_open,
        "filtered_out": filtered_out
    }

def test_agent_scenarios():
    """Test different agent assignment scenarios."""
    workspace = "/home/coding/claude-governor"

    print("=" * 80)
    print("Pluck Agent Assignment Diagnosis")
    print("=" * 80)
    print(f"Workspace: {workspace}")
    print()

    # Scenario 1: No specific agent (Pluck's default behavior)
    print("Scenario 1: Pluck without agent assignment")
    print("-" * 80)
    result = simulate_pluck_query(workspace, agent_id=None)
    print(f"Total open beads: {result['total_open']}")
    print(f"Claimable beads: {len(result['claimable'])}")
    print(f"Filtered out: {len(result['filtered_out'])}")

    if result['claimable']:
        print(f"\nFirst claimable bead: [{result['claimable'][0]['id']}] {result['claimable'][0]['title'][:60]}...")
    else:
        print("\n⚠️  STARVATION: No claimable beads found!")
        if result['filtered_out']:
            print("\nFirst few filtered beads:")
            for bead in result['filtered_out'][:5]:
                print(f"  [{bead['id']}] {bead['title'][:50]}...")
                for reason in bead['reasons']:
                    print(f"    - {reason}")

    print()

    # Scenario 2: With specific agent assignment
    test_agents = [
        "claude-code-glm-4.7",
        "claude-code-glm47-test-pluck-debug",
        "claude-anthropic-sonnet"
    ]

    for agent_id in test_agents:
        print(f"Scenario 2: Pluck with agent assignment: {agent_id}")
        print("-" * 80)
        result = simulate_pluck_query(workspace, agent_id=agent_id)
        print(f"Total open beads: {result['total_open']}")
        print(f"Claimable beads: {len(result['claimable'])}")
        print(f"Filtered out: {len(result['filtered_out'])}")

        if result['claimable']:
            print(f"\nFirst claimable bead: [{result['claimable'][0]['id']}] {result['claimable'][0]['title'][:60]}...")
        else:
            print("\n⚠️  STARVATION: No claimable beads for this agent!")

        print()

def analyze_assignment_patterns():
    """Analyze current assignment patterns in the workspace."""
    workspace = "/home/coding/claude-governor"
    db_path = get_bead_store_path(workspace)

    if not os.path.exists(db_path):
        return

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    print("=" * 80)
    print("Assignment Pattern Analysis")
    print("=" * 80)
    print()

    # Check assignees on open beads
    cursor.execute("""
        SELECT assignee, COUNT(*) as count
        FROM issues
        WHERE status = 'open' AND assignee IS NOT NULL
        GROUP BY assignee
        ORDER BY count DESC
    """)

    print("Current assignees on open beads:")
    for row in cursor.fetchall():
        print(f"  {row['assignee']}: {row['count']} beads")

    print()

    # Check beads without assignees
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'open' AND assignee IS NULL")
    unassigned_count = cursor.fetchone()[0]
    print(f"Open beads without assignee: {unassigned_count}")

    # Check if there are any in_progress beads
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'in_progress'")
    in_progress_count = cursor.fetchone()[0]
    print(f"Beads in progress: {in_progress_count}")

    conn.close()

if __name__ == "__main__":
    test_agent_scenarios()
    print()
    analyze_assignment_patterns()
