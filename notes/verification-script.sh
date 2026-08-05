#!/bin/bash
# Verification script to reproduce the Pluck filtering bug

echo "=== VERIFICATION: Pluck Filtering Bug ==="
echo "Date: $(date)"
echo ""

echo "1. Expected ready beads (correct SQL query):"
sqlite3 .beads/beads.db "SELECT id FROM issues WHERE status = 'open' AND ephemeral = 0 AND pinned = 0 AND is_template = 0 AND id NOT IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked')) AND id NOT IN (SELECT issue_id FROM blocked_issues_cache);" > /tmp/expected-ids.txt
cat /tmp/expected-ids.txt
echo ""

echo "2. Actual ready beads returned by bf ready (buggy):"
bf ready --limit 0 --format json | jq -r '.[].id' > /tmp/actual-ids.txt
cat /tmp/actual-ids.txt
echo ""

echo "3. Beads incorrectly included (in actual but not expected):"
comm -13 /tmp/expected-ids.txt /tmp/actual-ids.txt
echo ""

echo "4. Verify bf-156nn7 has 'deferred' label:"
sqlite3 .beads/beads.db "SELECT label FROM labels WHERE issue_id = 'bf-156nn7';"
echo ""

echo "5. Summary:"
EXPECTED_COUNT=$(wc -l < /tmp/expected-ids.txt | tr -d ' ')
ACTUAL_COUNT=$(wc -l < /tmp/actual-ids.txt | tr -d ' ')
echo "Expected: $EXPECTED_COUNT beads"
echo "Actual: $ACTUAL_COUNT beads"
echo "Extra beads incorrectly included: $((ACTUAL_COUNT - EXPECTED_COUNT))"
echo ""

if comm -12 /tmp/expected-ids.txt /tmp/actual-ids.txt | grep -q "bf-156nn7"; then
    echo "❌ BUG CONFIRMED: bf-156nn7 is in both lists despite having 'deferred' label"
else
    if comm -13 /tmp/expected-ids.txt /tmp/actual-ids.txt | grep -q "bf-156nn7"; then
        echo "✅ BUG CONFIRMED: bf-156nn7 appears in actual output but not expected (has 'deferred' label)"
    else
        echo "⚠️  Inconsistent state - run queries again"
    fi
fi
