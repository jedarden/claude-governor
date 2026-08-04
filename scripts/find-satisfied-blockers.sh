#!/bin/bash
# Script to identify blocked beads with satisfied dependencies
# A blocked bead is "satisfied" if it has no blockers OR all blockers are closed

set -euo pipefail

BLOCKED_BEADS=()
SATISFIED_BEADS=()

# Get all blocked bead IDs
mapfile -t BLOCKED_BEADS < <(bf list --status blocked --json | jq -r '.id')

echo "Found ${#BLOCKED_BEADS[@]} blocked beads"

for bead_id in "${BLOCKED_BEADS[@]}"; do
  # Get the list of blockers
  blockers=$(bf dep list "$bead_id" 2>/dev/null || echo "")

  if [ -z "$blockers" ]; then
    # No blockers at all - satisfied
    SATISFIED_BEADS+=("$bead_id (no blockers)")
    echo "$bead_id: satisfied (no blockers)"
  else
    # Check if all blockers are closed
    all_closed=true
    while IFS= read -r blocker_id; do
      if [ -n "$blocker_id" ]; then
        blocker_status=$(bf show "$blocker_id" --json 2>/dev/null | jq -r '.status // "unknown"' || echo "unknown")
        if [ "$blocker_status" != "closed" ]; then
          all_closed=false
          break
        fi
      fi
    done <<< "$blockers"

    if [ "$all_closed" = true ]; then
      SATISFIED_BEADS+=("$bead_id (all blockers closed)")
      echo "$bead_id: satisfied (all blockers closed)"
    else
      echo "$bead_id: still blocked (has open blockers)"
    fi
  fi
done

echo ""
echo "=== SUMMARY ==="
echo "Total blocked beads: ${#BLOCKED_BEADS[@]}"
echo "Satisfied (ready for reconciliation): ${#SATISFIED_BEADS[@]}"
echo ""
echo "=== SATISFIED BEADS ==="
printf '%s\n' "${SATISFIED_BEADS[@]}"
