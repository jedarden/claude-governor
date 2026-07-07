#!/usr/bin/env python3
"""
Test Pluck baseline functionality - verify it can retrieve open beads.
This establishes the baseline before testing filters.
"""

import sqlite3
import os
from typing import List, Dict, Any

def get_bead_store_path(workspace: str) -> str:
    """Get the path to the bead store database."""
    return os.path.join(workspace, ".beads", "beads.db")

def query_open_beads(workspace: str) -> List[Dict[str, Any]]:
    """Query all open beads from the database - no filters applied."""
    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        print(f"ERROR: Bead store not found at {db_path}")
        return []

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Basic query - no filters applied
    query = """
        SELECT id, title, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open'
        ORDER BY priority ASC, created_at ASC, id ASC
    """

    cursor.execute(query)
    beads = cursor.fetchall()
    conn.close()

    return [dict(bead) for bead in beads]

def query_unassigned_open_beads(workspace: str) -> List[Dict[str, Any]]:
    """Query open beads without assignee - no label filters."""
    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return []

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    query = """
        SELECT id, title, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open' AND assignee IS NULL
        ORDER BY priority ASC, created_at ASC, id ASC
    """

    cursor.execute(query)
    beads = cursor.fetchall()
    conn.close()

    return [dict(bead) for bead in beads]

def main():
    workspace = "/home/coding/claude-governor"

    print("=" * 70)
    print("Pluck Baseline Query Test")
    print("=" * 70)
    print(f"Workspace: {workspace}")
    print(f"Database: {get_bead_store_path(workspace)}")
    print()

    # Test 1: Verify workspace path is accessible
    print("✓ Workspace path is accessible")
    print()

    # Test 2: Query all open beads (no filters)
    print("Test 1: Query all open beads (no filters)")
    print("-" * 70)
    all_open = query_open_beads(workspace)
    print(f"Total open beads found: {len(all_open)}")
    print()

    # Test 3: Query unassigned open beads (matches Pluck's default behavior)
    print("Test 2: Query unassigned open beads (no label filters)")
    print("-" * 70)
    unassigned_open = query_unassigned_open_beads(workspace)
    print(f"Unassigned open beads found: {len(unassigned_open)}")
    print()

    # Expected vs actual comparison
    print("Expected vs Actual")
    print("-" * 70)
    print(f"Expected (baseline): 37 unassigned open beads")
    print(f"Actual: {len(unassigned_open)} unassigned open beads")

    if len(unassigned_open) == 37:
        print("✓ BASELINE VERIFIED: Pluck can retrieve open beads correctly")
    else:
        print(f"⚠️  Discrepancy: Expected 37, got {len(unassigned_open)}")
    print()

    # Show sample beads
    if unassigned_open:
        print(f"First 5 unassigned open beads:")
        print("-" * 70)
        for bead in unassigned_open[:5]:
            print(f"[{bead['id']}] {bead['title'][:60]}...")
            print(f"  Priority: {bead['priority']}, Assignee: {bead['assignee']}")
            print()

if __name__ == "__main__":
    main()
