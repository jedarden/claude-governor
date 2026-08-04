#!/usr/bin/env bash
# Script to find blocked beads whose dependencies are already satisfied

set -euo pipefail

echo "=== Checking blocked beads for satisfied dependencies ===" >&2
echo "Format: BEAD_ID | blocker_count | should_unblock" >&2
echo ""

# Get all blocked beads - bf list outputs JSONL (one JSON object per line)
blocked_beads=$(bf list --status blocked --json | jq -r '.id' 2>/dev/null || echo "")

if [ -z "$blocked_beads" ]; then
    echo "No blocked beads found." >&2
    exit 0
fi

stale_count=0

for bead in $blocked_beads; do
    # Get the bead's current status to confirm it's actually blocked
    status=$(bf show "$bead" --json 2>/dev/null | jq -r '.status // "unknown"' || echo "unknown")

    if [ "$status" != "blocked" ]; then
        continue
    fi

    # Get blocking dependencies (text format)
    dep_output=$(bf dep list "$bead" 2>/dev/null || echo "")

    # Parse blockers from text format
    if echo "$dep_output" | grep -q "depends on"; then
        # Extract blocker IDs using grep
        blocker_ids=$(echo "$dep_output" | grep -oE 'bf-[a-z0-9]+' | sort -u || echo "")
        blocker_count=$(echo "$blocker_ids" | grep -c 'bf-' || echo "0")

        if [ "$blocker_count" -eq 0 ]; then
            # No blockers - should be open
            echo "$bead | 0 | YES (no blockers)"
            ((stale_count++))
            continue
        fi

        # Check if all blockers are closed
        all_closed=true
        for blocker in $blocker_ids; do
            blocker_status=$(bf show "$blocker" --json 2>/dev/null | jq -r '.status // "unknown"' || echo "unknown")
            if [ "$blocker_status" != "closed" ]; then
                all_closed=false
                break
            fi
        done

        if [ "$all_closed" = "true" ]; then
            echo "$bead | $blocker_count | YES (all $blocker_count blockers closed)"
            ((stale_count++))
        fi
    else
        # No dependency line found - treat as no blockers
        echo "$bead | 0 | YES (no dependency info)"
        ((stale_count++))
    fi
done | sort -t '|' -k1

echo ""
echo "=== Found $stale_count stale blocked beads ===" >&2
