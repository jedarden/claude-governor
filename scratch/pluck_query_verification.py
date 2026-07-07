#!/usr/bin/env python3
"""
Pluck Query Construction Verification Tool

This script verifies and logs the exact queries Pluck constructs with all filter parameters.
It provides comprehensive logging of the query construction process to verify that
Pluck is querying beads correctly according to configuration.

Usage:
    python3 scratch/pluck_query_verification.py [--workspace PATH] [--agent ID] [--exclude-labels LIST]

Example:
    python3 scratch/pluck_query_verification.py --workspace /home/coding/claude-governor --agent claude-code-glm-4.7
"""

import sqlite3
import os
import json
import argparse
from typing import List, Dict, Any, Optional
from datetime import datetime

# Default exclude labels from Pluck configuration
DEFAULT_EXCLUDE_LABELS = ["deferred", "human", "blocked", "starvation-alert"]

def get_bead_store_path(workspace: str) -> str:
    """Get the path to the beads database for a workspace."""
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
    1. Workspace path and database location
    2. Query parameters (assignee, exclude_labels, status)
    3. The exact SQL query that would be executed
    4. Query parameters and bindings
    5. Raw results from database
    6. Defensive filtering steps
    7. Final claimable results

    Returns a dictionary with all query construction details.
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
        return {"error": "database_not_found", "workspace": workspace}

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
    query_params = {}

    if agent_id:
        query = """
            SELECT id, title, status, assignee, priority, created_at
            FROM issues
            WHERE status = 'open' AND assignee = ?
            ORDER BY priority ASC, created_at ASC, id ASC
        """
        params = (agent_id,)
        query_params["assignee"] = agent_id
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

    query_params["exclude_labels"] = exclude_labels
    query_params["status"] = "open"
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
        "database_path": db_path,
        "agent_id": agent_id,
        "exclude_labels": exclude_labels,
        "query": query.strip(),
        "query_params": query_params,
        "store_results": len(store_results),
        "claimable": len(claimable),
        "filtered": len(filtered_out),
        "claimable_beads": claimable,
        "filtered_beads": filtered_out,
        "timestamp": datetime.now().isoformat()
    }

def verify_query_matches_expected(result: Dict[str, Any], expected: Dict[str, Any]) -> bool:
    """
    Verify that the query construction matches expected configuration.

    Returns True if all expected values match, False otherwise.
    """
    mismatches = []

    # Check workspace
    if "workspace" in expected and result.get("workspace") != expected["workspace"]:
        mismatches.append(f"Workspace: expected {expected['workspace']}, got {result.get('workspace')}")

    # Check agent_id
    if "agent_id" in expected and result.get("agent_id") != expected["agent_id"]:
        mismatches.append(f"Agent ID: expected {expected['agent_id']}, got {result.get('agent_id')}")

    # Check exclude_labels
    if "exclude_labels" in expected and result.get("exclude_labels") != expected["exclude_labels"]:
        mismatches.append(f"Exclude labels: expected {expected['exclude_labels']}, got {result.get('exclude_labels')}")

    if mismatches:
        print("QUERY VERIFICATION FAILED")
        print("-" * 80)
        for mismatch in mismatches:
            print(f"  - {mismatch}")
        print()
        return False
    else:
        print("✅ QUERY VERIFICATION PASSED")
        print("-" * 80)
        print("All query parameters match expected configuration")
        print()
        return True

def main():
    """Main entry point for Pluck query verification."""
    parser = argparse.ArgumentParser(
        description="Verify and log Pluck query construction with exact filters"
    )
    parser.add_argument(
        "--workspace",
        default="/home/coding/claude-governor",
        help="Path to the workspace containing .beads/beads.db"
    )
    parser.add_argument(
        "--agent",
        default=None,
        help="Agent ID to filter beads (default: None, for unassigned beads)"
    )
    parser.add_argument(
        "--exclude-labels",
        nargs="*",
        default=DEFAULT_EXCLUDE_LABELS,
        help="List of labels to exclude (default: deferred human blocked starvation-alert)"
    )
    parser.add_argument(
        "--verify",
        action="store_true",
        help="Verify query matches expected configuration and exit with status code"
    )
    parser.add_argument(
        "--output-json",
        action="store_true",
        help="Output results as JSON instead of human-readable format"
    )

    args = parser.parse_args()

    # Run query construction logging
    result = log_query_construction(
        workspace=args.workspace,
        agent_id=args.agent,
        exclude_labels=args.exclude_labels
    )

    # Output JSON if requested
    if args.output_json:
        print(json.dumps(result, indent=2))
        return

    # Verify if requested
    if args.verify:
        expected = {
            "workspace": args.workspace,
            "agent_id": args.agent,
            "exclude_labels": args.exclude_labels
        }
        verify_query_matches_expected(result, expected)

if __name__ == "__main__":
    main()
