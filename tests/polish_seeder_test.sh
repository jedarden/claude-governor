#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SEEDER="$ROOT/scripts/polish-seeder.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "not ok: $*" >&2
  exit 1
}

assert_contains() {
  local haystack="$1" needle="$2"
  [[ "$haystack" == *"$needle"* ]] || fail "expected output to contain: $needle"
}

assert_not_contains() {
  local haystack="$1" needle="$2"
  [[ "$haystack" != *"$needle"* ]] || fail "expected output not to contain: $needle"
}

mkdir -p "$TMP/bin" "$TMP/repos/alpha/.git" "$TMP/repos/alpha/.beads"
mkdir -p "$TMP/repos/beta/.git" "$TMP/repos/beta/.beads"
mkdir -p "$TMP/repos/static-only/.git" "$TMP/repos/static-only/.beads"
touch "$TMP/repos/alpha/.beads/config.json"
touch "$TMP/repos/beta/.beads/config.json"
touch "$TMP/repos/static-only/.beads/config.json"

cat >"$TMP/bin/mock-curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [ "${MOCK_CURL_FAIL:-0}" = "1" ]; then
  exit 22
fi
url="${*: -1}"
case "$url" in
  *page=1)
    cat <<'JSON'
[
  {"name":"alpha","private":false,"archived":false,"disabled":false,"fork":false,"owner":{"login":"jedarden"}},
  {"name":"forked","private":false,"archived":false,"disabled":false,"fork":true,"owner":{"login":"jedarden"}},
  {"name":"archived","private":false,"archived":true,"disabled":false,"fork":false,"owner":{"login":"jedarden"}}
]
JSON
    ;;
  *page=2)
    cat <<'JSON'
[
  {"name":"beta","private":false,"archived":false,"disabled":false,"fork":false,"owner":{"login":"JEDARDEN"}},
  {"name":"private-repo","private":true,"archived":false,"disabled":false,"fork":false,"owner":{"login":"jedarden"}}
]
JSON
    ;;
  *) exit 23 ;;
esac
MOCK
chmod +x "$TMP/bin/mock-curl"

printf '%s\n' "$TMP/repos/static-only" >"$TMP/targets.txt"

inventory=$(CGOV_POLISH_GITHUB_OWNER=jedarden \
  CGOV_POLISH_REPO_ROOT="$TMP/repos" \
  CGOV_POLISH_GITHUB_PER_PAGE=3 \
  CGOV_POLISH_GITHUB_MAX_PAGES=3 \
  CGOV_POLISH_TARGETS="$TMP/targets.txt" \
  CURL="$TMP/bin/mock-curl" \
  "$SEEDER" --list-targets 2>"$TMP/inventory.err")

assert_contains "$inventory" $'eligible\t-\t'"$TMP/repos/alpha"
assert_contains "$inventory" $'eligible\t-\t'"$TMP/repos/beta"
assert_not_contains "$inventory" "forked"
assert_not_contains "$inventory" "archived"
assert_not_contains "$inventory" "private-repo"
assert_not_contains "$inventory" "static-only"
assert_contains "$(<"$TMP/inventory.err")" "eligible=2 skipped=0"

if MOCK_CURL_FAIL=1 \
  CGOV_POLISH_GITHUB_OWNER=jedarden \
  CGOV_POLISH_REPO_ROOT="$TMP/repos" \
  CGOV_POLISH_TARGETS="$TMP/targets.txt" \
  CURL="$TMP/bin/mock-curl" \
  "$SEEDER" --list-targets >"$TMP/fail.out" 2>"$TMP/fail.err"; then
  fail "GitHub API failure should fail closed"
fi
assert_contains "$(<"$TMP/fail.err")" "refusing stale fallback"
assert_not_contains "$(<"$TMP/fail.out")" "static-only"

mkdir -p "$TMP/queue/.beads"
cat >"$TMP/bin/mock-bead" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
args="$*"
case "$args" in
  "list --status open"*) echo '[]' ;;
  "list --status in_progress"*) echo '[]' ;;
  "list --status closed"*)
    printf '{"title":"Polish-gen: alpha","status":"closed","updated_at":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)"
    printf '{"title":"Polish-gen: beta","status":"closed","updated_at":"%s"}\n' "$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)"
    ;;
  "list --ready"*) : ;;
  "create "*) printf 'create:%s\n' "$args" >>"$MOCK_BEAD_LOG" ;;
  "sync flush-only"*) : ;;
  *) : ;;
esac
MOCK
chmod +x "$TMP/bin/mock-bead"

MOCK_BEAD_LOG="$TMP/bead.log" \
CGOV_POLISH_QUEUE="$TMP/queue" \
CGOV_POLISH_GITHUB_OWNER=jedarden \
CGOV_POLISH_REPO_ROOT="$TMP/repos" \
CGOV_POLISH_GITHUB_PER_PAGE=3 \
CGOV_POLISH_GITHUB_MAX_PAGES=3 \
CGOV_POLISH_COOLDOWN_HOURS=168 \
CGOV_POLISH_MAX_SEED_PER_PASS=8 \
BEAD="$TMP/bin/mock-bead" \
CURL="$TMP/bin/mock-curl" \
"$SEEDER" >"$TMP/seed.out" 2>"$TMP/seed.err"

assert_contains "$(<"$TMP/seed.err")" "skip (meta pending/recent): alpha"
if [ -s "$TMP/bead.log" ]; then
  fail "recently completed target should not create another meta-bead"
fi

: >"$TMP/bead.log"
MOCK_BEAD_LOG="$TMP/bead.log" \
CGOV_POLISH_QUEUE="$TMP/queue" \
CGOV_POLISH_GITHUB_OWNER=jedarden \
CGOV_POLISH_REPO_ROOT="$TMP/repos" \
CGOV_POLISH_GITHUB_PER_PAGE=3 \
CGOV_POLISH_GITHUB_MAX_PAGES=3 \
CGOV_POLISH_COOLDOWN_HOURS=0 \
CGOV_POLISH_MAX_SEED_PER_PASS=1 \
BEAD="$TMP/bin/mock-bead" \
CURL="$TMP/bin/mock-curl" \
"$SEEDER" >"$TMP/uncapped.out" 2>"$TMP/uncapped.err"

assert_contains "$(<"$TMP/bead.log")" "--title Polish-gen: alpha"
assert_not_contains "$(<"$TMP/bead.log")" "Polish-gen: beta"
assert_contains "$(<"$TMP/uncapped.err")" "reached per-pass seed cap: 1"

echo "ok: polish seeder discovery, filtering, fail-closed behavior, cooldown, and pass cap"
