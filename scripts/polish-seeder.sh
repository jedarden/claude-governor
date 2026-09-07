#!/usr/bin/env bash
#
# polish-seeder.sh — keep the cgov polish queue topped up with generation meta-beads.
#
# For each target repo it creates a "Polish-gen: <repo>" meta-bead in the polish
# queue ONLY IF:
#   (a) no such meta-bead is already pending (open/in_progress) in the queue, AND
#   (b) the target repo's own ready-bead backlog is below LOW_WATER.
#
# (b) ties generation to consumption (two-tank): when a repo already has plenty of
# unworked polish beads, don't generate more — wait until they drain. This makes the
# loop converge instead of endlessly re-polishing the same repo.
#
# The meta-bead's description IS the generator prompt (self-contained: the worker cd's
# into the target repo, audits plan.md for verifiable polish within scope, creates ≤5
# beads IN THE TARGET repo, then closes the meta-bead in the queue).
#
# Usage:
#   polish-seeder.sh              # one pass (idempotent; safe to run repeatedly)
#   polish-seeder.sh --loop [secs]  # run forever, every [secs] (default 1800)
#
# Config (env-overridable):
#   CGOV_POLISH_QUEUE      queue workspace       (default ~/cgov-polish-queue)
#   CGOV_POLISH_LOW_WATER  backlog threshold     (default 5)
#   CGOV_POLISH_TARGETS    target-repo list file (default ~/.config/claude-governor/polish-targets.txt)
#   BEAD                   bead binary           (default: bead on PATH)
#
set -euo pipefail

QUEUE="${CGOV_POLISH_QUEUE:-$HOME/cgov-polish-queue}"
LOW_WATER="${CGOV_POLISH_LOW_WATER:-5}"
TARGETS_FILE="${CGOV_POLISH_TARGETS:-$HOME/.config/claude-governor/polish-targets.txt}"
BEAD="${BEAD:-bead}"
GIT_PUSH="${CGOV_POLISH_PUSH:-0}"    # 1 = also push bead commits (best-effort); 0 = local commit only

log() { echo "[polish-seeder $(date -u +%H:%M:%S)] $*"; }

# One absolute repo path per line; blank lines and '#' comments ignored.
read_targets() {
  [ -f "$TARGETS_FILE" ] || { log "no targets file: $TARGETS_FILE"; return 0; }
  grep -vE '^[[:space:]]*(#|$)' "$TARGETS_FILE" || true
}

# Number of pending (open/in_progress) "Polish-gen: <name>" meta-beads in the queue.
pending_meta() {
  local name="$1"
  ( cd "$QUEUE" && "$BEAD" list --json 2>/dev/null ) | PYTHONNOUSERSITE=1 python3 -c "
import sys, json
# bead-rs --json is not one consistent shape: a populated workspace emits NDJSON
# (one object per line) while an EMPTY one emits a JSON array ('[]'). Accept
# both — assuming NDJSON crashed the seeder on a fresh queue, which is exactly
# the state it has to handle first (claudego-bc842506).
name = sys.argv[1]
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
print(sum(1 for d in items
          if isinstance(d, dict)
          and d.get('status') in ('open', 'in_progress')
          and d.get('title', '') == 'Polish-gen: ' + name))
" "$name"
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
        if git push >/dev/null 2>&1; then log "pushed beads: $label"; else log "push failed (committed locally): $label"; fi
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
2. Read docs/plan/plan.md (treat as scope CEILING) and skim src/.
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
  local seeded=0
  while IFS= read -r repo; do
    [ -n "$repo" ] || continue
    if [ ! -d "$repo" ]; then log "skip (missing): $repo"; continue; fi
    local name pend ready
    name="$(basename "$repo")"
    sync_beads_git "$repo" "$name"    # commit any beads runners produced in this repo
    pend="$(pending_meta "$name")"
    if [ "${pend:-0}" -ge 1 ]; then log "skip (meta pending): $name"; continue; fi
    ready="$(ready_count "$repo")"
    if [ "${ready:-0}" -ge "$LOW_WATER" ]; then log "skip (backlog ${ready}>=${LOW_WATER}): $name"; continue; fi
    ( cd "$QUEUE" && "$BEAD" create --issue-type task --priority 1 \
        --title "Polish-gen: $name" \
        --description "$(meta_prompt "$repo")" >/dev/null )
    log "seeded meta-bead: $name (ready=${ready})"
    seeded=$((seeded + 1))
  done < <(read_targets)
  sync_beads_git "$QUEUE" "queue"
  log "pass complete: seeded ${seeded}"
}

case "${1:-}" in
  --loop)
    interval="${2:-1800}"
    log "loop mode: every ${interval}s (queue=$QUEUE, low_water=$LOW_WATER)"
    while true; do seed_once || true; sleep "$interval"; done
    ;;
  -h | --help)
    sed -n '2,30p' "$0"
    ;;
  *)
    seed_once
    ;;
esac
