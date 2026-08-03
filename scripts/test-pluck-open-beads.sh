#!/bin/bash
# Test Pluck query for open beads
# Comprehensive test of Pluck's ability to discover and filter open beads

set -e

DB_PATH="/home/coding/claude-governor/.beads/beads.db"
echo "=== Pluck Open Beads Query Test ==="
echo "Database: $DB_PATH"
echo "Date: $(date)"
echo ""

# Check if database exists
if [ ! -f "$DB_PATH" ]; then
    echo "ERROR: Database file not found: $DB_PATH"
    exit 1
fi

echo "1. DATABASE OVERVIEW"
echo "===================="
echo ""

# Total issues
TOTAL=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues;")
echo "Total issues in database: $TOTAL"
echo ""

# Open issues
OPEN_TOTAL=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues WHERE status = 'open';")
echo "Total open issues: $OPEN_TOTAL"
echo ""

# Issues by status
echo "Issues by status:"
sqlite3 -column "$DB_PATH" <<EOF
SELECT status, COUNT(*) as count
FROM issues
GROUP BY status
ORDER BY count DESC;
EOF
echo ""

echo "2. EXCLUDED LABELS ANALYSIS"
echo "============================"
echo ""

# Open issues with excluded labels
EXCLUDED_LABELS="('deferred', 'human', 'blocked')"
EXCLUDED_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues WHERE status = 'open' AND id IN (SELECT issue_id FROM labels WHERE label IN $EXCLUDED_LABELS);")
echo "Open issues with excluded labels: $EXCLUDED_COUNT"
echo ""

# List excluded open issues
echo "Open issues with excluded labels:"
sqlite3 -column -header "$DB_PATH" <<EOF
SELECT i.id, substr(i.title, 1, 60), i.priority, GROUP_CONCAT(l.label, ', ')
FROM issues i
JOIN labels l ON i.id = l.issue_id
WHERE i.status = 'open'
AND l.label IN $EXCLUDED_LABELS
GROUP BY i.id
ORDER BY i.id;
EOF
echo ""

echo "3. BLOCKED ISSUES CACHE"
echo "========================"
echo ""

# Blocked open issues
BLOCKED_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT issue_id) FROM blocked_issues_cache WHERE issue_id IN (SELECT id FROM issues WHERE status = 'open');")
echo "Open issues in blocked cache: $BLOCKED_COUNT"
echo ""

# Sample of blocked issues
echo "Sample of blocked open issues:"
sqlite3 -column -header "$DB_PATH" <<EOF
SELECT bic.issue_id, substr(i.title, 1, 50), bic.blocked_by
FROM blocked_issues_cache bic
JOIN issues i ON bic.issue_id = i.id
WHERE i.status = 'open'
LIMIT 5;
EOF
echo ""

echo "4. READY BEADS QUERY (Expected Pluck Behavior)"
echo "=============================================="
echo ""

# What SHOULD be returned by bf ready (all filters applied)
READY_SQL=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues WHERE status = 'open' AND ephemeral = 0 AND pinned = 0 AND is_template = 0 AND id NOT IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked')) AND id NOT IN (SELECT issue_id FROM blocked_issues_cache);")
echo "Expected ready beads (SQL query): $READY_SQL"
echo ""

echo "Expected ready beads:"
sqlite3 -column -header "$DB_PATH" <<EOF
SELECT id, substr(title, 1, 70), priority
FROM issues
WHERE status = 'open'
AND ephemeral = 0
AND pinned = 0
AND is_template = 0
AND id NOT IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked'))
AND id NOT IN (SELECT issue_id FROM blocked_issues_cache)
ORDER BY priority DESC, id;
EOF
echo ""

echo "5. ACTUAL BF READY OUTPUT"
echo "=========================="
echo ""

# Run bf ready and count
BF_READY_COUNT=$(bf ready --limit 0 | grep -c "^\[bf-" || echo "0")
echo "Actual bf ready count: $BF_READY_COUNT"
echo ""

echo "Actual bf ready output:"
bf ready --limit 0
echo ""

echo "6. COMPARISON ANALYSIS"
echo "======================"
echo ""

# Check for discrepancies
if [ "$READY_SQL" -eq "$BF_READY_COUNT" ]; then
    echo "✅ MATCH: SQL query ($READY_SQL) matches bf ready ($BF_READY_COUNT)"
else
    echo "❌ MISMATCH: SQL query ($READY_SQL) does NOT match bf ready ($BF_READY_COUNT)"
    echo "Difference: $((READY_SQL - BF_READY_COUNT))"
fi
echo ""

echo "7. FILTER BREAKDOWN"
echo "===================="
echo ""

# Show the filtering cascade
echo "Starting with: $OPEN_TOTAL open issues"
echo ""

# Step 1: Filter out excluded labels
AFTER_LABELS=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues WHERE status = 'open' AND id NOT IN (SELECT issue_id FROM labels WHERE label IN $EXCLUDED_LABELS);")
echo "After excluding labels ($EXCLUDED_COUNT excluded): $AFTER_LABELS"
echo ""

# Step 2: Filter out blocked
FINAL_READY=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues WHERE status = 'open' AND id NOT IN (SELECT issue_id FROM labels WHERE label IN ('deferred', 'human', 'blocked')) AND id NOT IN (SELECT issue_id FROM blocked_issues_cache);")
BLOCKED_AFTER_LABELS=$((AFTER_LABELS - FINAL_READY))
echo "After removing blocked ($BLOCKED_AFTER_LABELS blocked): $FINAL_READY"
echo ""

echo "8. ISSUE WITH BF-156NN7"
echo "========================"
echo ""

# Check the specific issue with bf-156nn7
echo "bf-156nn7 has 'deferred' label but appears in bf ready:"
BF_156NN7_READY=$(bf ready --limit 0 | grep -c "bf-156nn7" || echo "0")
echo "Appears in bf ready: $BF_156NN7_READY"
echo ""

echo "Details:"
sqlite3 -column "$DB_PATH" <<EOF
SELECT id, substr(title, 1, 60), status, priority, ephemeral, pinned, is_template
FROM issues
WHERE id = 'bf-156nn7';
EOF
echo ""

echo "Labels:"
sqlite3 -column "$DB_PATH" "SELECT label FROM labels WHERE issue_id = 'bf-156nn7';"
echo ""

echo "In blocked cache:"
sqlite3 -column "$DB_PATH" "SELECT COUNT(*) FROM blocked_issues_cache WHERE issue_id = 'bf-156nn7';"
echo ""

echo "=== Test Complete ==="
echo ""
echo "Summary:"
echo "- Total open issues: $OPEN_TOTAL"
echo "- Excluded by labels: $EXCLUDED_COUNT"
echo "- Expected ready beads: $READY_SQL"
echo "- Actual bf ready count: $BF_READY_COUNT"
echo "- Match: $([ "$READY_SQL" -eq "$BF_READY_COUNT" ] && echo "YES" || echo "NO")"
