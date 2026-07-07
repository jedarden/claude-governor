#!/usr/bin/env python3
"""
Test to understand which workspace Pluck is operating on.
This simulates what happens when Pluck runs with different workspace configurations.
"""

import sqlite3
import os
from typing import List, Dict, Any

DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked", "starvation-alert"]

def get_bead_store_path(workspace: str) -> str:
    """Get the path to the bead store database."""
    return os.path.join(workspace, ".beads", "beads.db")

def parse_labels(labels_str: str) -> List[str]:
    """Parse labels from database format."""
    if not labels_str or labels_str == "none":
        return []
    return [label.strip() for label in labels_str.split(",") if label.strip()]

def has_excluded_label(labels: List[str], excluded: List[str]) -> bool:
    """Check if any excluded label is present."""
    return bool(set(labels) & set(excluded))

def get_labels_for_issue(conn: sqlite3.Connection, issue_id: str) -> List[str]:
    """Get all labels for a specific issue."""
    cursor = conn.cursor()
    cursor.execute("SELECT label FROM labels WHERE issue_id = ?", (issue_id,))
    return [row[0] for row in cursor.fetchall()]

def query_claimable_beads(workspace: str, exclude_labels: List[str] = None) -> List[Dict[str, Any]]:
    """Query beads that Pluck would consider claimable."""
    if exclude_labels is None:
        exclude_labels = DEFAULT_EXCLUDE_LABELS

    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return []

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    query = """
        SELECT id, title, description, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open'
        ORDER BY priority ASC, created_at ASC, id ASC
    """

    cursor.execute(query)
    beads = cursor.fetchall()

    claimable = []
    for bead in beads:
        bead_dict = dict(bead)
        labels = get_labels_for_issue(conn, bead_dict['id'])

        if has_excluded_label(labels, exclude_labels):
            continue

        if bead_dict["status"] == "in_progress":
            continue

        bead_dict['labels'] = ', '.join(labels) if labels else 'none'
        claimable.append(bead_dict)

    conn.close()
    return claimable

def test_all_workspaces():
    """Test Pluck behavior across all known workspaces."""
    workspaces = [
        "/home/coding/claude-governor",
        "/home/coding/NEEDLE",
        "/home/coding/AgentScribe",
        "/home/coding/telegram-claude-bridge",
        "/home/coding/Research/weather-fast",
    ]

    print("=" * 80)
    print("Pluck Workspace Path Diagnosis")
    print("=" * 80)
    print(f"Default workspace from config: /home/coding/telegram-claude-bridge")
    print(f"Current workspace: /home/coding/claude-governor")
    print(f"Exclude labels: {DEFAULT_EXCLUDE_LABELS}")
    print()

    total_claimable = 0
    workspace_results = []

    for workspace in workspaces:
        db_path = get_bead_store_path(workspace)

        if not os.path.exists(db_path):
            print(f"❌ {workspace}")
            print(f"   No bead store found")
            print()
            continue

        claimable = query_claimable_beads(workspace)
        total_claimable += len(claimable)
        workspace_results.append((workspace, claimable))

        print(f"{'✅' if len(claimable) > 0 else '⚠️'} {workspace}")
        print(f"   Claimable beads: {len(claimable)}")

        if claimable:
            print(f"   First bead: [{claimable[0]['id']}] {claimable[0]['title'][:60]}...")
        else:
            print(f"   ⚠️  STARVATION: No claimable beads found!")

        print()

    print("=" * 80)
    print(f"Total claimable beads across all workspaces: {total_claimable}")
    print("=" * 80)

    return workspace_results

def analyze_filter_effects():
    """Analyze how different filter combinations affect results."""
    workspace = "/home/coding/claude-governor"
    db_path = get_bead_store_path(workspace)

    if not os.path.exists(db_path):
        print(f"No bead store at {db_path}")
        return

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    print("=" * 80)
    print("Filter Analysis for " + workspace)
    print("=" * 80)
    print()

    # Total open beads
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'open'")
    total_open = cursor.fetchone()[0]
    print(f"Total open beads: {total_open}")

    # Beads with assignees
    cursor.execute("SELECT COUNT(*) FROM issues WHERE status = 'open' AND assignee IS NOT NULL")
    with_assignee = cursor.fetchone()[0]
    print(f"Open beads with assignees: {with_assignee}")

    # Beads without assignees
    without_assignee = total_open - with_assignee
    print(f"Open beads without assignees: {without_assignee}")

    print()
    print("Label distribution on open beads:")
    cursor.execute("""
        SELECT label, COUNT(DISTINCT issue_id) as count
        FROM labels
        WHERE issue_id IN (SELECT id FROM issues WHERE status = 'open')
        GROUP BY label
        ORDER BY count DESC
    """)

    for row in cursor.fetchall():
        label = row[0]
        count = row[1]
        excluded = "🚫" if label in DEFAULT_EXCLUDE_LABELS else ""
        print(f"  {label}: {count} {excluded}")

    print()
    print("Excluded label effects:")
    for excluded_label in DEFAULT_EXCLUDE_LABELS:
        cursor.execute(f"""
            SELECT COUNT(DISTINCT i.id)
            FROM issues i
            WHERE i.status = 'open'
            AND i.id IN (SELECT issue_id FROM labels WHERE label = '{excluded_label}')
        """)
        count = cursor.fetchone()[0]
        print(f"  {excluded_label}: {count} beads excluded")

    conn.close()

if __name__ == "__main__":
    test_all_workspaces()
    print()
    analyze_filter_effects()
