#!/bin/bash
# Test exclude_labels filter in isolation
# This script tests different exclude_labels configurations to document behavior

set -e

WORKSPACE="/home/coding/claude-governor"
DB_PATH="$WORKSPACE/.beads/beads.db"

echo "=== EXCLUDE_LABELS FILTER TEST ==="
echo "Workspace: $WORKSPACE"
echo "Date: $(date)"
echo ""

# Function to count beads with specific SQL WHERE clause
count_beads() {
    local where_clause="$1"
    sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT i.id) FROM issues i LEFT JOIN labels l ON i.id = l.issue_id WHERE $where_clause;"
}

# Function to list beads with specific SQL WHERE clause
list_beads() {
    local where_clause="$1"
    sqlite3 "$DB_PATH" "SELECT DISTINCT i.id FROM issues i LEFT JOIN labels l ON i.id = l.issue_id WHERE $where_clause;"
}

echo "=== BASELINE: ALL OPEN BEADS ==="
baseline_count=$(count_beads "i.status = 'open'")
echo "Total open beads: $baseline_count"
echo "Bead IDs: $(list_beads "i.status = 'open'")"
echo ""

echo "=== TEST 1: NO EXCLUDE_LABELS (empty array) ==="
# SQL equivalent: no NOT EXISTS clause
test1_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0")
echo "Beads with no exclude_labels: $test1_count"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0")"
echo ""

echo "=== TEST 2: EXCLUDE_LABELS = ['deferred'] ==="
# SQL equivalent: NOT EXISTS with 'deferred'
test2_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'deferred')")
echo "Beads excluding 'deferred': $test2_count"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'deferred')")"
echo "Excluded beads: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'deferred')")"
echo ""

echo "=== TEST 3: EXCLUDE_LABELS = ['human'] ==="
test3_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'human')")
echo "Beads excluding 'human': $test3_count"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'human')")"
echo "Excluded beads: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'human')")"
echo ""

echo "=== TEST 4: EXCLUDE_LABELS = ['blocked'] ==="
test4_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'blocked')")
echo "Beads excluding 'blocked': $test4_count"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'blocked')")"
echo "Excluded beads: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'blocked')")"
echo ""

echo "=== TEST 5: EXCLUDE_LABELS = ['deferred', 'human', 'blocked'] (DEFAULT) ==="
test5_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'human', 'blocked'))")
echo "Beads with default exclude_labels: $test5_count"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'human', 'blocked'))")"
echo "Excluded beads: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'human', 'blocked'))")"
echo ""

echo "=== TEST 6: EXCLUDE_LABELS = ['deferred', 'split-child'] ==="
test6_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'split-child'))")
echo "Beads excluding 'deferred' and 'split-child': $test6_count"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'split-child'))")"
echo "Excluded beads: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label IN ('deferred', 'split-child'))")"
echo ""

echo "=== TEST 7: EXCLUDE_LABELS = ['umbrella'] ==="
test7_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'umbrella')")
echo "Beads excluding 'umbrella': $test7_count"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'umbrella')")"
echo "Excluded beads: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label = 'umbrella')")"
echo ""

echo "=== TEST 8: EXCLUDE_LABELS WILDCARD PATTERN ('deferred%') ==="
# This tests if the filter supports wildcards (it shouldn't, but let's verify)
test8_count=$(count_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label LIKE 'deferred%')")
echo "Beads excluding 'deferred%' (wildcard): $test8_count"
echo "Note: This uses LIKE pattern matching, NOT standard exclude_labels behavior"
echo "Bead IDs: $(list_beads "i.status = 'open' AND i.assignee IS NULL AND i.ephemeral = 0 AND i.pinned = 0 AND i.is_template = 0 AND NOT EXISTS (SELECT 1 FROM labels WHERE issue_id = i.id AND label LIKE 'deferred%')")"
echo ""

echo "=== SUMMARY ==="
echo "Baseline (no label filters): $test1_count"
echo "Exclude 'deferred' only: $test2_count (filtered: $((test1_count - test2_count)))"
echo "Exclude 'human' only: $test3_count (filtered: $((test1_count - test3_count)))"
echo "Exclude 'blocked' only: $test4_count (filtered: $((test1_count - test4_count)))"
echo "Exclude ['deferred', 'human', 'blocked'] (default): $test5_count (filtered: $((test1_count - test5_count)))"
echo "Exclude ['deferred', 'split-child']: $test6_count (filtered: $((test1_count - test6_count)))"
echo "Exclude 'umbrella' only: $test7_count (filtered: $((test1_count - test7_count)))"
echo ""
echo "=== CONCLUSION ==="
echo "Default exclude_labels ['deferred', 'human', 'blocked'] filters: $((test1_count - test5_count)) beads"
echo "This represents: $(awk "BEGIN {printf \"%.1f\", ($((test1_count - test5_count)) * 100.0 / $test1_count}")}% of unassigned open beads"
