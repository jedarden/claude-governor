#!/usr/bin/env python3
"""
Comprehensive diagnosis of Pluck configuration filter and exclude_labels issues.
This tests the exact behavior with different configuration scenarios.
"""

import sqlite3
import os
from typing import List, Dict, Any

# Configuration from NEEDLE .needle.yaml
NEEDLE_PLUCK_EXCLUDE_LABELS = []  # Pluck is configured with empty exclude list!
DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked", "starvation-alert"]
DEFAULT_WORKSPACE = "/home/coding/NEEDLE"
CURRENT_WORKSPACE = "/home/coding/claude-governor"

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

def simulate_pluck_query(workspace: str, agent_id: str = None,
                        exclude_labels: List[str] = None) -> Dict[str, Any]:
    """
    Simulate exactly what Pluck does with different configurations.
    """
    if exclude_labels is None:
        exclude_labels = NEEDLE_PLUCK_EXCLUDE_LABELS

    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return {"claimable": [], "total_open": 0, "filtered_out": [], "config": {}}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Step 1: Store-level query (what Pluck requests from the bead store)
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
        if exclude_labels and has_excluded_label(labels, exclude_labels):
            excluded_labels = set(labels) & set(exclude_labels)
            reasons.append(f"excluded_labels: {excluded_labels}")

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
                "labels": labels,
                "assignee": bead_dict['assignee']
            })

    conn.close()
    return {
        "claimable": claimable,
        "total_open": total_open,
        "filtered_out": filtered_out,
        "config": {
            "workspace": workspace,
            "agent_id": agent_id,
            "exclude_labels": exclude_labels
        }
    }

def analyze_assignment_patterns(workspace: str):
    """Analyze current assignment patterns in the workspace."""
    db_path = get_bead_store_path(workspace)

    if not os.path.exists(db_path):
        return {}

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Check assignees on open beads
    cursor.execute("""
        SELECT assignee, COUNT(*) as count
        FROM issues
        WHERE status = 'open' AND assignee IS NOT NULL AND assignee != ''
        GROUP BY assignee
        ORDER BY count DESC
    """)

    real_assignees = {row['assignee']: row['count'] for row in cursor.fetchall()}

    # Check beads with NULL or empty assignee
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'open' AND (assignee IS NULL OR assignee = '')")
    unassigned_count = cursor.fetchone()[0]

    # Check beads with empty string specifically
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'open' AND assignee = ''")
    empty_assignee_count = cursor.fetchone()[0]

    # Check beads with NULL specifically
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'open' AND assignee IS NULL")
    null_assignee_count = cursor.fetchone()[0]

    conn.close()

    return {
        "real_assignees": real_assignees,
        "unassigned_count": unassigned_count,
        "empty_assignee_count": empty_assignee_count,
        "null_assignee_count": null_assignee_count
    }

def main():
    print("=" * 80)
    print("PLUCK CONFIGURATION DIAGNOSIS")
    print("=" * 80)
    print()

    print("CONFIGURATION ANALYSIS")
    print("-" * 80)
    print(f"NEEDLE config exclude_labels: {NEEDLE_PLUCK_EXCLUDE_LABELS}")
    print(f"Default exclude_labels (code): {DEFAULT_EXCLUDE_LABELS}")
    print(f"Default workspace (NEEDLE): {DEFAULT_WORKSPACE}")
    print(f"Current workspace: {CURRENT_WORKSPACE}")
    print()

    # Test different configuration scenarios
    scenarios = [
        {
            "name": "Scenario 1: Pluck with NEEDLE config (empty exclude_labels) + agent_id",
            "workspace": CURRENT_WORKSPACE,
            "agent_id": "claude-code-glm-4.7",
            "exclude_labels": NEEDLE_PLUCK_EXCLUDE_LABELS
        },
        {
            "name": "Scenario 2: Pluck with NEEDLE config (empty exclude_labels) + NO agent_id",
            "workspace": CURRENT_WORKSPACE,
            "agent_id": None,
            "exclude_labels": NEEDLE_PLUCK_EXCLUDE_LABELS
        },
        {
            "name": "Scenario 3: Pluck with DEFAULT exclude_labels + agent_id",
            "workspace": CURRENT_WORKSPACE,
            "agent_id": "claude-code-glm-4.7",
            "exclude_labels": DEFAULT_EXCLUDE_LABELS
        },
        {
            "name": "Scenario 4: Pluck with DEFAULT exclude_labels + NO agent_id",
            "workspace": CURRENT_WORKSPACE,
            "agent_id": None,
            "exclude_labels": DEFAULT_EXCLUDE_LABELS
        },
    ]

    for scenario in scenarios:
        print("=" * 80)
        print(scenario["name"])
        print("-" * 80)

        result = simulate_pluck_query(
            scenario["workspace"],
            scenario["agent_id"],
            scenario["exclude_labels"]
        )

        config = result["config"]
        print(f"Config: workspace={config['workspace']}, agent_id={config['agent_id']}, exclude_labels={config['exclude_labels']}")
        print(f"Total open beads: {result['total_open']}")
        print(f"Claimable beads: {len(result['claimable'])}")
        print(f"Filtered out: {len(result['filtered_out'])}")

        if result['claimable']:
            print(f"\n✅ First claimable bead: [{result['claimable'][0]['id']}] {result['claimable'][0]['title'][:60]}...")
            print(f"   Assignee: {result['claimable'][0]['assignee']}")
            print(f"   Labels: {result['claimable'][0]['labels']}")
        else:
            print(f"\n⚠️  STARVATION: No claimable beads found!")
            if result['total_open'] == 0:
                print("   CAUSE: No open beads match the agent_id filter in the database query")
            elif result['filtered_out']:
                print("   All beads were filtered out by exclude_labels")

        print()

    # Analyze assignment patterns
    print("=" * 80)
    print("ASSIGNMENT PATTERN ANALYSIS")
    print("=" * 80)
    print()

    patterns = analyze_assignment_patterns(CURRENT_WORKSPACE)

    print(f"Beads with real assignees: {len(patterns['real_assignees'])}")
    for assignee, count in patterns['real_assignees'].items():
        print(f"  {assignee}: {count} beads")

    print(f"\nBeads with NULL assignee: {patterns['null_assignee_count']}")
    print(f"Beads with empty string assignee: {patterns['empty_assignee_count']}")
    print(f"Beads with NULL or empty assignee: {patterns['unassigned_count']}")

    print()
    print("=" * 80)
    print("ROOT CAUSE ANALYSIS")
    print("=" * 80)
    print()

    print("🔍 KEY FINDINGS:")
    print("1. Pluck is configured with exclude_labels: [] (empty array)")
    print("2. This means NO beads are filtered out by labels")
    print("3. The problem is NOT exclude_labels - it's the agent_id query condition")
    print()
    print("🎯 ROOT CAUSE:")
    print("When Pluck runs with agent_id='claude-code-glm-4.7', it queries:")
    print("  WHERE status = 'open' AND assignee = 'claude-code-glm-4.7'")
    print()
    print("But the database contains:")
    print(f"  - {patterns['null_assignee_count']} beads with NULL assignee")
    print(f"  - {patterns['empty_assignee_count']} beads with empty string assignee")
    print(f"  - {len(patterns['real_assignees'])} beads with actual assignee values")
    print()
    print("⚠️  NONE of the beads match the agent_id condition, so Pluck returns 0 beads.")
    print()
    print("✅ SOLUTION:")
    print("The exclude_labels configuration is NOT the problem.")
    print("The problem is that Pluck's agent-specific query assumes beads are")
    print("explicitly assigned to the agent, but most beads have NULL/empty assignees.")

if __name__ == "__main__":
    main()
