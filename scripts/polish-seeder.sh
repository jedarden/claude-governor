#!/usr/bin/env bash
#
# polish-seeder.sh — keep the cgov polish queue topped up with generation meta-beads.
#
# For each target repo it creates a "Polish-gen: <repo>" meta-bead in the polish
# queue ONLY IF:
#   (a) no such meta-bead is already pending (open/in_progress) in the queue, AND
#   (b) no such meta-bead completed inside the cooldown window, AND
#   (c) the target repo's own ready-bead backlog is below LOW_WATER.
#
# (c) ties generation to consumption (two-tank): when a repo already has plenty of
# unworked polish beads, don't generate more — wait until they drain. This makes the
# loop converge instead of endlessly re-polishing the same repo.
#
# Targets can come from either a static file or live discovery of active, non-fork
# public repositories owned by a GitHub user. GitHub discovery is deliberately
# fail-closed: an API failure never falls back to a stale list that might include a
# repository which has since become private.
#
# The meta-bead's description IS the generator prompt (self-contained: the worker cd's
# into the target repo, audits its authoritative docs for verifiable polish within
# scope, creates ≤5 beads IN THE TARGET repo, then closes the meta-bead in the queue).
#
# Usage:
#   polish-seeder.sh              # one pass (idempotent; safe to run repeatedly)
#   polish-seeder.sh --loop [secs]  # run forever, every [secs] (default 1800)
#   polish-seeder.sh --list-targets # inspect discovery/eligibility without mutation
#
# Config (env-overridable):
#   CGOV_POLISH_QUEUE      queue workspace       (default ~/cgov-polish-queue)
#   CGOV_POLISH_LOW_WATER  backlog threshold     (default 5)
#   CGOV_POLISH_TARGETS    target-repo list file (default ~/.config/claude-governor/polish-targets.txt)
#   CGOV_POLISH_GITHUB_OWNER discover current public repos for this owner (default: disabled)
#   CGOV_POLISH_REPO_ROOT   local clone root for GitHub discovery (default: ~)
#   CGOV_POLISH_COOLDOWN_HOURS minimum delay after a completed pass (default: 168)
#   CGOV_POLISH_MAX_SEED_PER_PASS cap new meta-beads per pass (default: 8)
#   BEAD                   bead binary           (default: bead on PATH)
#   CURL                   curl binary            (default: curl on PATH)
#
set -euo pipefail

QUEUE="${CGOV_POLISH_QUEUE:-$HOME/cgov-polish-queue}"
LOW_WATER="${CGOV_POLISH_LOW_WATER:-5}"
TARGETS_FILE="${CGOV_POLISH_TARGETS:-$HOME/.config/claude-governor/polish-targets.txt}"
GITHUB_OWNER="${CGOV_POLISH_GITHUB_OWNER:-}"
REPO_ROOT="${CGOV_POLISH_REPO_ROOT:-$HOME}"
GITHUB_API_URL="${CGOV_POLISH_GITHUB_API_URL:-https://api.github.com}"
GITHUB_PER_PAGE="${CGOV_POLISH_GITHUB_PER_PAGE:-100}"
GITHUB_MAX_PAGES="${CGOV_POLISH_GITHUB_MAX_PAGES:-10}"
COOLDOWN_HOURS="${CGOV_POLISH_COOLDOWN_HOURS:-168}"
MAX_SEED_PER_PASS="${CGOV_POLISH_MAX_SEED_PER_PASS:-8}"
BEAD="${BEAD:-bead}"
CURL="${CURL:-curl}"
GIT_PUSH="${CGOV_POLISH_PUSH:-0}"    # 1 = also push bead commits (best-effort); 0 = local commit only

log() { echo "[polish-seeder $(date -u +%H:%M:%S)] $*" >&2; }

# Discover active, source (non-fork), public GitHub repositories and map each
# repository name to its existing local clone under REPO_ROOT. The GitHub API is
# used only for visibility/discovery; all git writes still go to each clone's
# configured origin (Forgejo in this environment).
discover_github_targets() {
  case "$GITHUB_OWNER" in
    ''|*[!A-Za-z0-9-]*)
      log "invalid GitHub owner: $GITHUB_OWNER"
      return 1
      ;;
  esac
  case "$GITHUB_PER_PAGE:$GITHUB_MAX_PAGES" in
    *[!0-9:]*|0:*|*:0)
      log "invalid GitHub pagination: per_page=$GITHUB_PER_PAGE max_pages=$GITHUB_MAX_PAGES"
      return 1
      ;;
  esac

  local page=1 body count
  while [ "$page" -le "$GITHUB_MAX_PAGES" ]; do
    if ! body=$("$CURL" -fsSL --connect-timeout 10 --max-time 30 \
        -H 'Accept: application/vnd.github+json' \
        -H 'X-GitHub-Api-Version: 2022-11-28' \
        -H 'User-Agent: claude-governor-polish-seeder' \
        "$GITHUB_API_URL/users/$GITHUB_OWNER/repos?type=owner&sort=full_name&direction=asc&per_page=$GITHUB_PER_PAGE&page=$page"); then
      log "GitHub discovery failed for $GITHUB_OWNER (page $page); refusing stale fallback"
      return 1
    fi

    if ! count=$(printf '%s' "$body" | PYTHONNOUSERSITE=1 python3 -c '
import json, re, sys

owner, root = sys.argv[1:3]
try:
    repos = json.load(sys.stdin)
except Exception as exc:
    print(f"invalid GitHub response: {exc}", file=sys.stderr)
    raise SystemExit(2)
if not isinstance(repos, list):
    print("invalid GitHub response: expected a repository array", file=sys.stderr)
    raise SystemExit(2)
for repo in repos:
    if not isinstance(repo, dict):
        continue
    login = ((repo.get("owner") or {}).get("login") or "")
    name = repo.get("name") or ""
    if login.casefold() != owner.casefold():
        continue
    if repo.get("private") or repo.get("archived") or repo.get("disabled") or repo.get("fork"):
        continue
    if not re.fullmatch(r"[A-Za-z0-9._-]+", name):
        continue
    print(f"{root.rstrip(chr(47))}/{name}")
print(f"__COUNT__={len(repos)}")
' "$GITHUB_OWNER" "$REPO_ROOT"); then
      log "GitHub discovery returned malformed data for $GITHUB_OWNER (page $page)"
      return 1
    fi
    printf '%s\n' "$count" | sed '/^__COUNT__=/d'
    count=$(printf '%s\n' "$count" | sed -n 's/^__COUNT__=//p')
    [ "${count:-0}" -lt "$GITHUB_PER_PAGE" ] && return 0
    page=$((page + 1))
  done

  log "GitHub discovery hit the safety limit of $GITHUB_MAX_PAGES pages"
  return 1
}

# One absolute repo path per line. When a GitHub owner is configured, current
# public visibility is authoritative and the static file is intentionally ignored.
read_targets() {
  if [ -n "$GITHUB_OWNER" ]; then
    discover_github_targets | sort -u
    return
  fi
  [ -f "$TARGETS_FILE" ] || { log "no targets file: $TARGETS_FILE"; return 0; }
  grep -vE '^[[:space:]]*(#|$)' "$TARGETS_FILE" | sort -u || true
}

# Number of pending (open/in_progress) "Polish-gen: <name>" meta-beads in the queue.
# A target is suppressed while a matching meta-bead is pending, and for a
# cooldown after a completed pass. Without the completed-pass cooldown, a repo
# where an audit correctly found zero issues would be re-audited every 30 minutes.
suppressed_meta() {
  local name="$1"
  {
    ( cd "$QUEUE" && "$BEAD" list --status open --json --limit 999999 2>/dev/null )
    ( cd "$QUEUE" && "$BEAD" list --status in_progress --json --limit 999999 2>/dev/null )
    ( cd "$QUEUE" && "$BEAD" list --status closed --json --limit 999999 2>/dev/null )
  } | PYTHONNOUSERSITE=1 python3 -c "
import datetime, re, sys, json
# bead-rs --json is not one consistent shape: a populated workspace emits NDJSON
# (one object per line) while an EMPTY one emits a JSON array ('[]'). Accept
# both — assuming NDJSON crashed the seeder on a fresh queue, which is exactly
# the state it has to handle first (claudego-bc842506).
name, cooldown_hours = sys.argv[1], int(sys.argv[2])
raw = sys.stdin.read().strip()
items = []
if raw:
    try:
        doc = json.loads(raw)
        items = doc if isinstance(doc, list) else [doc]
    except Exception:
        for line in raw.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                items.append(json.loads(line))
            except Exception:
                continue
cutoff = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=cooldown_hours)
suppressed = False
for item in items:
    if not isinstance(item, dict) or item.get('title', '') != 'Polish-gen: ' + name:
        continue
    if item.get('status') in ('open', 'in_progress'):
        suppressed = True
        break
    if item.get('status') != 'closed' or cooldown_hours == 0:
        continue
    value = item.get('updated_at') or ''
    try:
        # Rust emits nanoseconds; datetime accepts microseconds. Truncate only the
        # fractional component while preserving the timezone suffix.
        match = re.match(r'^(.*\.)(\d+)(Z|[+-]\d\d:\d\d)$', value)
        if match:
            value = match.group(1) + match.group(2)[:6].ljust(6, '0') + match.group(3)
        updated = datetime.datetime.fromisoformat(value.replace('Z', '+00:00'))
    except (TypeError, ValueError):
        continue
    if updated >= cutoff:
        suppressed = True
        break
print(1 if suppressed else 0)
" "$name" "$COOLDOWN_HOURS"
}

target_skip_reason() {
  local repo="$1"
  if [ ! -e "$repo/.git" ]; then echo "no-local-clone"; return 0; fi
  if [ ! -f "$repo/.beads/config.json" ]; then
    if [ -f "$repo/.beads/config.yaml" ]; then echo "non-bead-rs-workspace"; else echo "no-bead-rs-workspace"; fi
    return 0
  fi
  echo ""
}

# Ready-bead count in a repo. `bead list --ready` prints one "ID: <id>" line per
# issue, so count those rather than grepping for an id prefix — prefixes are
# per-workspace (claudepr-, needle-, armor-, ...), and the old `grep -c 'bf-'`
# matched none of them and silently returned 0 for every repo, which defeated
# the two-tank gate below (claudego-bc842506).
#
# `grep -c` exits 1 when the count is 0 while still printing "0", so capture the
# substitution and normalise — a bare `|| echo 0` appends a SECOND value.
ready_count() {
  local n
  n=$( cd "$1" 2>/dev/null && "$BEAD" list --ready 2>/dev/null | grep -c '^ID:' ) || n=0
  echo "${n:-0}"
}

# Flush any runner-produced beads to the JSONL checkpoint and commit them to git so
# they survive a fresh clone / db rebuild and are visible to other hosts. bead writes
# to the live SQLite store (gitignored); the checkpoint under .beads/checkpoint/ is
# what git tracks.
# Commits ONLY the .beads/ pathspec, so an otherwise-dirty working tree is left alone.
# Push is opt-in (CGOV_POLISH_PUSH=1) and best-effort — a rejected push just leaves the
# beads committed locally.
sync_beads_git() {
  local repo="$1" label="$2"
  ( cd "$repo" || exit 0
    "$BEAD" sync flush-only >/dev/null 2>&1 || true
    [ -d .git ] || exit 0
    [ -n "$(git status --porcelain -- .beads 2>/dev/null)" ] || exit 0
    git add .beads >/dev/null 2>&1 || true
    if git -c user.email=github@jedarden.com -c user.name=jedarden \
        commit -q -m "chore(beads): sync polish-loop beads [$label]" -- .beads 2>/dev/null; then
      log "committed beads: $label"
      if [ "$GIT_PUSH" = "1" ]; then
        if GIT_TERMINAL_PROMPT=0 git push origin >/dev/null 2>&1; then
          log "pushed beads: $label"
        else
          log "push failed (committed locally): $label"
        fi
      fi
    fi
  )
}

# The generator prompt for a target repo. $repo and $QUEUE expand; backticks and
# <ID> are kept literal for the worker to fill in.
meta_prompt() {
  local repo="$1"
  cat <<PROMPT
POLISH-GENERATION PASS. Target repo: $repo. Your ONLY output is new beads in the TARGET repo. Do NOT modify code, commit, or push.
STEPS:
1. cd $repo
2. Read every applicable AGENTS.md plus README.md. Read the authoritative project plan if present (prefer docs/plan/plan.md, then plan.md). Treat the existing documented project scope as a CEILING; if there is no plan, README.md and AGENTS.md define that ceiling. Skim the implementation only after understanding those documents.
   If the repo has a DEPLOYED web frontend/app, you have ADB access to a Pixel 6 over Tailscale (run adb-check first): open the deployed URL in Chrome and screenshot it to audit the REAL deployed artifact's UI/UX, not just the source — adb shell am start -a android.intent.action.VIEW -d '<url>' com.android.chrome ; sleep 2 ; adb shell screencap -p > /tmp/polish-view.png ; then read /tmp/polish-view.png.
3. Run \`bead list --ready\` and \`bead list\` there; do NOT duplicate anything already tracked.
4. Find real, concrete, VERIFIABLE polish opportunities WITHIN existing scope ONLY: bugs, stubs/TODOs, silently-swallowed errors, missing edge cases, test gaps, impl diverging from plan.md. NOT new features.
5. Adversarial self-check each candidate: real defect at a SPECIFIC file:line? fix objectively verifiable? If subjective/speculative/uncertain -> DISCARD.
6. For each survivor (AT MOST 5; fewer is better; zero is fine), create a bead IN THE TARGET repo:
   (cd $repo && bead create --issue-type task --priority 2 --title "<specific>" --description "<repo-relative file:line. what is wrong & why. ACCEPTANCE CRITERIA: an objective check a verifier can confirm.>")
7. FINAL STEP: close THIS meta-bead in the queue store. Your bead id is in the task header shown as [needle:...:<ID>:...]. A close reason is REQUIRED. Run:
   (cd $QUEUE && bead close <ID> --reason "polish pass complete: created N beads in $repo")
PROMPT
}

seed_once() {
  [ -d "$QUEUE/.beads" ] || { log "queue not initialised: $QUEUE (run: cd $QUEUE && bead init --prefix polishq --skip-foreign-workspace)"; return 1; }
  case "$COOLDOWN_HOURS:$MAX_SEED_PER_PASS" in
    *[!0-9:]*|*:0) log "invalid limits: cooldown_hours=$COOLDOWN_HOURS max_seed_per_pass=$MAX_SEED_PER_PASS"; return 1 ;;
  esac
  local seeded=0 targets repo name suppressed ready reason
  if ! targets="$(read_targets)"; then
    log "target discovery failed; seeded nothing"
    return 1
  fi
  while IFS= read -r repo; do
    [ -n "$repo" ] || continue
    reason="$(target_skip_reason "$repo")"
    if [ -n "$reason" ]; then log "skip ($reason): $repo"; continue; fi
    name="$(basename "$repo")"
    sync_beads_git "$repo" "$name"    # commit any beads runners produced in this repo
    suppressed="$(suppressed_meta "$name")"
    if [ "${suppressed:-0}" -ge 1 ]; then log "skip (meta pending/recent): $name"; continue; fi
    ready="$(ready_count "$repo")"
    if [ "${ready:-0}" -ge "$LOW_WATER" ]; then log "skip (backlog ${ready}>=${LOW_WATER}): $name"; continue; fi
    ( cd "$QUEUE" && "$BEAD" create --issue-type task --priority 1 \
        --title "Polish-gen: $name" \
        --description "$(meta_prompt "$repo")" >/dev/null )
    log "seeded meta-bead: $name (ready=${ready})"
    seeded=$((seeded + 1))
    if [ "$seeded" -ge "$MAX_SEED_PER_PASS" ]; then
      log "reached per-pass seed cap: $MAX_SEED_PER_PASS"
      break
    fi
  done <<< "$targets"
  sync_beads_git "$QUEUE" "queue"
  log "pass complete: seeded ${seeded}"
}

list_targets() {
  local targets repo reason eligible=0 skipped=0
  if ! targets="$(read_targets)"; then return 1; fi
  while IFS= read -r repo; do
    [ -n "$repo" ] || continue
    reason="$(target_skip_reason "$repo")"
    if [ -n "$reason" ]; then
      printf 'skip\t%s\t%s\n' "$reason" "$repo"
      skipped=$((skipped + 1))
    else
      printf 'eligible\t-\t%s\n' "$repo"
      eligible=$((eligible + 1))
    fi
  done <<< "$targets"
  log "target inventory: eligible=$eligible skipped=$skipped"
}

case "${1:-}" in
  --loop)
    interval="${2:-1800}"
    log "loop mode: every ${interval}s (queue=$QUEUE, low_water=$LOW_WATER)"
    while true; do seed_once || true; sleep "$interval"; done
    ;;
  --list-targets)
    list_targets
    ;;
  -h | --help)
    sed -n '2,42p' "$0"
    ;;
  *)
    seed_once
    ;;
esac
