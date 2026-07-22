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
#   BF                     bf binary             (default: bf on PATH)
#
set -euo pipefail

QUEUE="${CGOV_POLISH_QUEUE:-$HOME/cgov-polish-queue}"
LOW_WATER="${CGOV_POLISH_LOW_WATER:-5}"
TARGETS_FILE="${CGOV_POLISH_TARGETS:-$HOME/.config/claude-governor/polish-targets.txt}"
BF="${BF:-bf}"

log() { echo "[polish-seeder $(date -u +%H:%M:%S)] $*"; }

# One absolute repo path per line; blank lines and '#' comments ignored.
read_targets() {
  [ -f "$TARGETS_FILE" ] || { log "no targets file: $TARGETS_FILE"; return 0; }
  grep -vE '^[[:space:]]*(#|$)' "$TARGETS_FILE" || true
}

# Number of pending (open/in_progress) "Polish-gen: <name>" meta-beads in the queue.
pending_meta() {
  local name="$1"
  ( cd "$QUEUE" && "$BF" list --json 2>/dev/null ) | PYTHONNOUSERSITE=1 python3 -c "
import sys, json
name = sys.argv[1]
count = 0
for line in sys.stdin:                       # bf list --json is NDJSON (one object per line)
    try:
        d = json.loads(line)
    except Exception:
        continue
    if d.get('status') in ('open', 'in_progress') and d.get('title', '') == 'Polish-gen: ' + name:
        count += 1
print(count)
" "$name"
}

# Ready-bead count in a repo (bf ready is plain text with bf- ids).
ready_count() {
  ( cd "$1" && "$BF" ready 2>/dev/null | grep -c 'bf-' ) || echo 0
}

# The generator prompt for a target repo. $repo and $QUEUE expand; backticks and
# <ID> are kept literal for the worker to fill in.
meta_prompt() {
  local repo="$1"
  cat <<PROMPT
POLISH-GENERATION PASS. Target repo: $repo. Your ONLY output is new bf beads in the TARGET repo. Do NOT modify code, commit, or push.
STEPS:
1. cd $repo
2. Read docs/plan/plan.md (treat as scope CEILING) and skim src/.
3. Run \`bf ready\` and \`bf list\` there; do NOT duplicate anything already tracked.
4. Find real, concrete, VERIFIABLE polish opportunities WITHIN existing scope ONLY: bugs, stubs/TODOs, silently-swallowed errors, missing edge cases, test gaps, impl diverging from plan.md. NOT new features.
5. Adversarial self-check each candidate: real defect at a SPECIFIC file:line? fix objectively verifiable? If subjective/speculative/uncertain -> DISCARD.
6. For each survivor (AT MOST 5; fewer is better; zero is fine), create a bead IN THE TARGET repo:
   (cd $repo && bf create --type task --priority 2 --title "<specific>" --description "<repo-relative file:line. what is wrong & why. ACCEPTANCE CRITERIA: an objective check a verifier can confirm.>")
7. FINAL STEP: close THIS meta-bead in the queue store. Your bead id is in the task header shown as [needle:...:<ID>:...]. Run:
   (cd $QUEUE && bf batch --json '[{"op":"close","id":"<ID>"}]')
PROMPT
}

seed_once() {
  [ -d "$QUEUE/.beads" ] || { log "queue not initialised: $QUEUE (run: cd $QUEUE && bf init)"; return 1; }
  local seeded=0
  while IFS= read -r repo; do
    [ -n "$repo" ] || continue
    if [ ! -d "$repo" ]; then log "skip (missing): $repo"; continue; fi
    local name pend ready
    name="$(basename "$repo")"
    pend="$(pending_meta "$name")"
    if [ "${pend:-0}" -ge 1 ]; then log "skip (meta pending): $name"; continue; fi
    ready="$(ready_count "$repo")"
    if [ "${ready:-0}" -ge "$LOW_WATER" ]; then log "skip (backlog ${ready}>=${LOW_WATER}): $name"; continue; fi
    ( cd "$QUEUE" && "$BF" create --type task --priority 1 \
        --title "Polish-gen: $name" \
        --description "$(meta_prompt "$repo")" >/dev/null )
    log "seeded meta-bead: $name (ready=${ready})"
    seeded=$((seeded + 1))
  done < <(read_targets)
  ( cd "$QUEUE" && "$BF" sync --flush-only >/dev/null 2>&1 ) || true
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
