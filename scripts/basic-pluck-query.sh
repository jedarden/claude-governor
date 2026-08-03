#!/bin/bash
# Basic Pluck query without filters
# This script demonstrates querying the Pluck database directly without label filters

set -e

DB_PATH="/home/coding/claude-governor/.beads/beads.db"

echo "=== Basic Pluck Query (No Filters) ==="
echo "Database: $DB_PATH"
echo ""

# Check if database exists
if [ ! -f "$DB_PATH" ]; then
    echo "ERROR: Database file not found: $DB_PATH"
    exit 1
fi

echo "Running basic queries..."
echo ""

# Query 1: Total issues
echo "1. Total issues (no filter):"
sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues;"
echo ""

# Query 2: All open issues (no label filter)
echo "2. Open issues (no label filter):"
sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM issues WHERE status = 'open';"
echo ""

# Query 3: Sample of 5 open issues with basic info
echo "3. Sample of 5 open issues (no filter):"
sqlite3 -column -header "$DB_PATH" <<EOF
SELECT id, title, status, priority, issue_type
FROM issues
WHERE status = 'open'
LIMIT 5;
EOF
echo ""

# Query 4: Issues by status
echo "4. Issues by status:"
sqlite3 -column "$DB_PATH" <<EOF
SELECT status, COUNT(*) as count
FROM issues
GROUP BY status;
EOF
echo ""

# Query 5: Issues by type
echo "5. Issues by issue_type:"
sqlite3 -column "$DB_PATH" <<EOF
SELECT issue_type, COUNT(*) as count
FROM issues
GROUP BY issue_type;
EOF
echo ""

# Query 6: Issues by priority
echo "6. Issues by priority:"
sqlite3 -column "$DB_PATH" <<EOF
SELECT priority, COUNT(*) as count
FROM issues
GROUP BY priority
ORDER BY priority DESC;
EOF
echo ""

echo "=== Query Complete ==="
