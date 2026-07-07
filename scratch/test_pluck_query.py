#!/usr/bin/env python3
"""
Test script to simulate Pluck's bead query and see what beads it would return.
This replicates the logic from NEEDLE's PluckStrand.
"""

import sqlite3
import os
from typing import List, Dict, Any

# Default exclude labels from PluckStrand
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
        print(f"ERROR: Bead store not found at {db_path}")
        return []

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Query for open beads (similar to Pluck's store query)
    query = """
        SELECT id, title, description, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open'
        ORDER BY priority ASC, created_at ASC, id ASC
    """

    cursor.execute(query)
    beads = cursor.fetchall()

    # Apply defensive filtering (like PluckStrand does)
    claimable = []
    for bead in beads:
        bead_dict = dict(bead)

        # Get labels from the labels table
        labels = get_labels_for_issue(conn, bead_dict['id'])

        # Filter by excluded labels
        if has_excluded_label(labels, exclude_labels):
            continue

        # Filter out InProgress status
        if bead_dict["status"] == "in_progress":
            continue

        # Add labels to the bead dict for display
        bead_dict['labels'] = ', '.join(labels) if labels else 'none'

        # Note: Pluck also filters by stale assignee, but for this test we'll just check basic claimability
        claimable.append(bead_dict)

    conn.close()
    return claimable

def main():
    workspace = "/home/coding/claude-governor"

    print("=" * 60)
    print("Pluck Bead Query Test")
    print("=" * 60)
    print(f"Workspace: {workspace}")
    print(f"Exclude labels: {DEFAULT_EXCLUDE_LABELS}")
    print()

    claimable = query_claimable_beads(workspace)

    print(f"Total claimable beads found: {len(claimable)}")
    print()

    if claimable:
        print("First 10 claimable beads:")
        print("-" * 60)
        for bead in claimable[:10]:
            print(f"[{bead['id']}] {bead['title']}")
            print(f"  Priority: {bead['priority']}")
            print(f"  Labels: {bead['labels']}")
            print(f"  Assignee: {bead['assignee']}")
            print()
    else:
        print("⚠️  STARVATION DETECTED: Pluck found 0 claimable beads!")
        print()
        print("This confirms the starvation issue - Pluck cannot find work")
        print("even though claimable beads exist in the database.")

if __name__ == "__main__":
    main()
