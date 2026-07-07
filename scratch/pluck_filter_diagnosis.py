#!/usr/bin/env python3
"""
Comprehensive diagnostic script to test Pluck filter combinations.
This helps identify what's causing Pluck to return 0 beads in some cases.
"""

import sqlite3
import os
from typing import List, Dict, Any, Set
from collections import defaultdict

# Default exclude labels from PluckStrand
DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked", "starvation-alert"]

def get_bead_store_path(workspace: str) -> str:
    """Get the path to the bead store database."""
    return os.path.join(workspace, ".beads", "beads.db")

def get_labels_for_issue(conn: sqlite3.Connection, issue_id: str) -> List[str]:
    """Get all labels for a specific issue."""
    cursor = conn.cursor()
    cursor.execute("SELECT label FROM labels WHERE issue_id = ?", (issue_id,))
    return [row[0] for row in cursor.fetchall()]

def has_excluded_label(labels: List[str], excluded: List[str]) -> bool:
    """Check if any excluded label is present."""
    return bool(set(labels) & set(excluded))

def query_beads_with_filters(
    workspace: str,
    exclude_labels: List[str] = None,
    include_in_progress: bool = False,
    require_no_assignee: bool = False
) -> List[Dict[str, Any]]:
    """Query beads with various filter combinations."""
    if exclude_labels is None:
        exclude_labels = DEFAULT_EXCLUDE_LABELS

    db_path = get_bead_store_path(workspace)
    if not os.path.exists(db_path):
        return []

    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Base query for open beads
    query = """
        SELECT id, title, description, status, assignee, priority, created_at
        FROM issues
        WHERE status = 'open'
        ORDER BY priority ASC, created_at ASC, id ASC
    """

    cursor.execute(query)
    beads = cursor.fetchall()

    # Apply filtering
    filtered = []
    for bead in beads:
        bead_dict = dict(bead)
        labels = get_labels_for_issue(conn, bead_dict['id'])

        # Filter by excluded labels
        if has_excluded_label(labels, exclude_labels):
            continue

        # Filter out InProgress status (unless explicitly included)
        if not include_in_progress and bead_dict["status"] == "in_progress":
            continue

        # Filter by assignee (if required)
        if require_no_assignee and bead_dict["assignee"] is not None:
            continue

        bead_dict['labels'] = labels
        filtered.append(bead_dict)

    conn.close()
    return filtered

def analyze_label_distribution(workspace: str) -> Dict[str, Set[str]]:
    """Analyze which labels are present on open beads."""
    db_path = get_bead_store_path(workspace)
    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()

    cursor.execute("""
        SELECT issues.id, labels.label
        FROM issues
        LEFT JOIN labels ON issues.id = labels.issue_id
        WHERE issues.status = 'open'
    """)

    label_to_beads = defaultdict(set)
    for row in cursor.fetchall():
        bead_id, label = row
        if label:
            label_to_beads[label].add(bead_id)
        else:
            label_to_beads["(no labels)"].add(bead_id)

    conn.close()
    return dict(label_to_beads)

def main():
    workspace = "/home/coding/claude-governor"

    print("=" * 80)
    print("PLUCK FILTER DIAGNOSIS")
    print("=" * 80)
    print(f"Workspace: {workspace}")
    print()

    # Test 1: Default Pluck configuration
    print("TEST 1: DEFAULT PLUCK CONFIGURATION")
    print("-" * 80)
    print("Exclude labels: DEFAULT_EXCLUDE_LABELS")
    print("Include in_progress: False")
    print("Require no assignee: False")
    print()

    claimable = query_beads_with_filters(workspace)
    print(f"Result: {len(claimable)} claimable beads")
    if claimable:
        print("First 5 claimable beads:")
        for bead in claimable[:5]:
            print(f"  [{bead['id']}] {bead['title']}")
            print(f"    Labels: {', '.join(bead['labels']) if bead['labels'] else '(none)'}")
            print(f"    Assignee: {bead['assignee']}")
    print()

    # Test 2: No exclude labels
    print("TEST 2: NO EXCLUDE LABELS")
    print("-" * 80)
    print("Exclude labels: []")
    print("Include in_progress: False")
    print("Require no assignee: False")
    print()

    no_exclude = query_beads_with_filters(workspace, exclude_labels=[])
    print(f"Result: {len(no_exclude)} beads")
    print(f"Difference from default: {len(no_exclude) - len(claimable)} additional beads")
    print()

    # Test 3: Each exclude label individually
    print("TEST 3: INDIVIDUAL EXCLUDE LABELS")
    print("-" * 80)
    for label in DEFAULT_EXCLUDE_LABELS:
        result = query_beads_with_filters(workspace, exclude_labels=[label])
        print(f"Excluding only '{label}': {len(result)} beads")
    print()

    # Test 4: Include in-progress
    print("TEST 4: INCLUDE IN-PROGRESS BEADS")
    print("-" * 80)
    print("Exclude labels: DEFAULT_EXCLUDE_LABELS")
    print("Include in_progress: True")
    print("Require no assignee: False")
    print()

    with_progress = query_beads_with_filters(workspace, include_in_progress=True)
    print(f"Result: {len(with_progress)} beads")
    print(f"Difference from default: {len(with_progress) - len(claimable)} additional beads")
    print()

    # Test 5: Require no assignee
    print("TEST 5: REQUIRE NO ASSIGNEE")
    print("-" * 80)
    print("Exclude labels: DEFAULT_EXCLUDE_LABELS")
    print("Include in_progress: False")
    print("Require no assignee: True")
    print()

    no_assignee = query_beads_with_filters(workspace, require_no_assignee=True)
    print(f"Result: {len(no_assignee)} beads")
    print(f"Difference from default: {len(no_assignee) - len(claimable)} fewer beads")
    print()

    # Test 6: Label distribution analysis
    print("TEST 6: LABEL DISTRIBUTION ANALYSIS")
    print("-" * 80)
    label_dist = analyze_label_distribution(workspace)
    print("Labels found on open beads:")
    for label, bead_ids in sorted(label_dist.items(), key=lambda x: len(x[1]), reverse=True):
        print(f"  {label}: {len(bead_ids)} beads")
    print()

    # Test 7: Beads with specific problematic labels
    print("TEST 7: EXCLUDED LABELS BREAKDOWN")
    print("-" * 80)
    for label in DEFAULT_EXCLUDE_LABELS:
        if label in label_dist:
            print(f"  {label}: {len(label_dist[label])} beads excluded")
    print()

    # Summary
    print("SUMMARY")
    print("=" * 80)
    print(f"Total open beads: {sum(len(beads) for beads in label_dist.values())}")
    print(f"Default Pluck would find: {len(claimable)} beads")
    print(f"Blocked by exclude_labels: {len(no_exclude) - len(claimable)} beads")
    print(f"Most common label: {max(label_dist.items(), key=lambda x: len(x[1]))[0]}")
    print()

    # Diagnostic conclusions
    print("DIAGNOSTIC CONCLUSIONS")
    print("-" * 80)
    if len(claimable) == 0:
        print("⚠️  STARVATION CONFIRMED: Pluck finds 0 beads with default configuration!")
        print()
        print("Possible causes:")
        print("  1. All open beads have excluded labels")
        print("  2. Workspace path mismatch")
        print("  3. Database corruption")
        print("  4. All beads are assigned or in-progress")
    else:
        print(f"✓ Pluck would find {len(claimable)} beads - NO starvation detected")
        print()
        print("If NEEDLE still reports 0 beads, check:")
        print("  1. NEEDLE workspace configuration matches this path")
        print("  2. NEEDLE is using default exclude_labels")
        print("  3. No runtime filters beyond what we tested")

if __name__ == "__main__":
    main()
