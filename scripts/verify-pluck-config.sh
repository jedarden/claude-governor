#!/usr/bin/env bash
# Authoritative verification of the current Pluck + bead-rs configuration.
#
# One command answers every "which config path / default workspace / backend /
# store layout / CLI contract is current?" question this repository's docs
# used to disagree about (docs/notes/bf-2bxsv.md records the superseded
# claims). Run it instead of trusting any doc snapshot:
#
#   scripts/verify-pluck-config.sh [workspace]
#
#   scripts/verify-pluck-config.sh [workspace]
#
# Read-only: it only reads config files and runs `bead list` / `bead show`.
# Exit 0 = every check passed. Exit 1 = at least one check failed; reconcile
# the documentation against this output, never from memory.
#
#   scripts/verify-pluck-config.sh --self-test
#
# --self-test is a static contract check over this script's own text: the
# declared check inventory must still match the `# N.` check sections in the
# file and the six-part list both docs bequeath it (see CHECK_INVENTORY
# below). It runs nothing, reads no config, and exits non-zero on drift —
# so a check dropped, renamed or renumbered here fails loudly instead of
# silently weakening the authoritative verification while the docs keep
# claiming six checks. tests/pluck_config_selftest_test.rs drills this mode
# with mutated copies and pins the inventory against the docs themselves.

set -u

WORKSPACE="${1:-/home/coding/claude-governor}"
CONFIG_PATH="$HOME/.config/needle/config.yaml"
LEGACY_CONFIG_PATH="$HOME/.needle/config.yaml"
STATE_DIR="$HOME/.needle"
pass=0
fail=0

ok()   { echo "PASS: $*"; pass=$((pass + 1)); }
bad()  { echo "FAIL: $*"; fail=$((fail + 1)); }
note() { echo "note: $*"; }

# Self-declared check inventory: one "N:area-id" entry per numbered check
# section below, in order. The first six ids are the documented six-part
# inventory — docs/bead-visibility-quickref.md and
# docs/bead-visibility-troubleshooting.md both say the script checks "the
# backend binding, the active config path, the resolved default workspace,
# the bead-rs store layout, the CLI output contract, and the live
# strands.pluck values, and exits non-zero on any mismatch". Entries after
# those are supplementary sections: they must be named "<area>-supplementary"
# and carry no failure path, because the documented non-zero exit promise
# covers the six documented checks only. --self-test pins this registry
# against the actual "# N." section headers; the cargo test
# tests/pluck_config_selftest_test.rs pins it against the docs' own text.
CHECK_INVENTORY=(
    "1:backend-binding"
    "2:active-config-path"
    "3:default-workspace"
    "4:store-layout"
    "5:cli-contract"
    "6:strands-pluck"
    "7:diagnostics-supplementary"
)
DOCUMENTED_AREAS=(
    backend-binding
    active-config-path
    default-workspace
    store-layout
    cli-contract
    strands-pluck
)

# Static contract check over this script's own text: parse the declared
# inventory and the check sections actually present in $1 and fail loudly on
# any divergence. Deliberately offline — no CLI is run, no config is read,
# nothing outside $1 is opened — so it is safe to invoke anywhere.
self_test() {
    local file="$1"
    local failures=0

    st_fail() {
        echo "self-test: FAIL: $*" >&2
        failures=$((failures + 1))
    }

    # Every inventory entry is "i:<area-id>" with i its 1-based position —
    # a dropped or renumbered registry line breaks this immediately.
    local pos=0 entry
    for entry in "${CHECK_INVENTORY[@]}"; do
        pos=$((pos + 1))
        case "$entry" in
            "$pos:"*) ;;
            *) st_fail "inventory entry $pos is '$entry', expected '$pos:<area-id>'" ;;
        esac
    done

    # Area ids are unique — a renamed duplicate would otherwise slip past the
    # position check.
    local seen=" " id
    for entry in "${CHECK_INVENTORY[@]}"; do
        id="${entry#*:}"
        case "$seen" in
            *" $id "*) st_fail "inventory id '$id' declared more than once" ;;
        esac
        seen="$seen$id "
    done

    # The file's "# N." check-section headers must be exactly 1..N and as
    # numerous as the registry: this is the check the docs' six-part claim
    # is pinned to, so a section deleted without renumbering, or a new
    # section added without a registry entry, fails here.
    local -a header_nums=()
    local num
    while IFS= read -r num; do
        [ -n "$num" ] && header_nums+=("$num")
    done < <(sed -n 's/^# \([0-9][0-9]*\)\. .*/\1/p' "$file")
    if [ "${#header_nums[@]}" -ne "${#CHECK_INVENTORY[@]}" ]; then
        st_fail "found ${#header_nums[@]} '# N.' check sections (${header_nums[*]:-none}) but CHECK_INVENTORY declares ${#CHECK_INVENTORY[@]} — a check section was dropped or added without updating the inventory"
    fi
    local slot
    for slot in "${!header_nums[@]}"; do
        if [ "${header_nums[$slot]}" != "$((slot + 1))" ]; then
            st_fail "check section '${header_nums[$slot]}' found at position $((slot + 1)) — numbering is not contiguous; a check was dropped, renumbered or added"
        fi
    done

    # The documented six: exactly six areas, equal to the first six registry
    # ids in order. Anything else means the documented list and the script's
    # real inventory have drifted apart.
    if [ "${#DOCUMENTED_AREAS[@]}" -ne 6 ]; then
        st_fail "DOCUMENTED_AREAS has ${#DOCUMENTED_AREAS[@]} entries; the docs' inventory has exactly six"
    fi
    local slot2
    for slot2 in "${!DOCUMENTED_AREAS[@]}"; do
        entry="${CHECK_INVENTORY[$slot2]:-<missing>}"
        id="${entry#*:}"
        if [ "${DOCUMENTED_AREAS[$slot2]}" != "$id" ]; then
            st_fail "DOCUMENTED_AREAS[$slot2] is '${DOCUMENTED_AREAS[$slot2]}' but inventory entry $((slot2 + 1)) is '$entry' — the documented list and the check inventory disagree"
        fi
    done

    # Sections beyond the documented six are supplementary: named
    # "<area>-supplementary" and free of failure paths, since the docs'
    # "exits non-zero" promise covers the six documented checks only.
    local sup_pos="$(( ${#DOCUMENTED_AREAS[@]} + 1 ))"
    local sup_ids="" start_line section_bad
    while [ "$sup_pos" -le "${#CHECK_INVENTORY[@]}" ]; do
        entry="${CHECK_INVENTORY[$((sup_pos - 1))]}"
        id="${entry#*:}"
        case "$id" in
            *-supplementary) ;;
            *) st_fail "supplementary inventory entry $sup_pos is '$id'; sections beyond the documented six must be named '<area>-supplementary'" ;;
        esac
        sup_ids="$sup_ids $id"
        start_line="$(grep -n "^# ${sup_pos}\. " "$file" | head -1 | cut -d: -f1)"
        if [ -z "$start_line" ]; then
            st_fail "supplementary section $sup_pos ('$id') has no '# ${sup_pos}.' header in the file"
        else
            section_bad="$(sed -n "${start_line},\$p" "$file" | grep -E -c '^[[:space:]]*bad[[:space:]]+"' || true)"
            if [ "$section_bad" -gt 0 ]; then
                st_fail "supplementary section $sup_pos ('$id') contains a 'bad' call — sections beyond the documented six must stay note-or-pass only"
            fi
        fi
        sup_pos=$((sup_pos + 1))
    done

    if [ "$failures" -gt 0 ]; then
        echo "self-test: $failures inventory contract failure(s) in $file" >&2
        return 1
    fi
    echo "self-test: OK: ${#CHECK_INVENTORY[@]} check sections declared and contiguous"
    echo "self-test: OK: documented six = ${DOCUMENTED_AREAS[*]}"
    echo "self-test: OK: supplementary sections (fail-free):${sup_ids:- none}"
    return 0
}

if [ "${1:-}" = "--self-test" ]; then
    self_test "$0"
    exit $?
fi

echo "== Pluck / bead-rs configuration verification =="
echo "workspace: $WORKSPACE"
echo

# 1. Backend binding: the workspace declares bead-rs (not bead-forge).
if grep -q 'backend: *bead-rs' "$WORKSPACE/.needle.yaml" 2>/dev/null; then
    ok "backend: $WORKSPACE/.needle.yaml declares 'backend: bead-rs'"
else
    bad "backend: $WORKSPACE/.needle.yaml does not declare 'backend: bead-rs'"
fi

# 2. NEEDLE config path: the v2 loader file exists and resolves; the legacy
#    v1 file, when present, must carry the marker that the loader ignores it.
if [ -f "$CONFIG_PATH" ]; then
    ok "config: active config at $CONFIG_PATH"
else
    bad "config: $CONFIG_PATH missing"
fi
if [ -f "$LEGACY_CONFIG_PATH" ]; then
    if grep -q 'v2 loader does NOT read it' "$LEGACY_CONFIG_PATH"; then
        note "$LEGACY_CONFIG_PATH exists and carries the legacy-v1 marker; the v2 loader does not read it"
    else
        bad "config: $LEGACY_CONFIG_PATH exists without the legacy-v1 marker; confirm it cannot be misread as live"
    fi
fi
if needle config --get workspace.default >/dev/null 2>&1; then
    ok "config: 'needle config --get workspace.default' resolves (v2 loader active)"
else
    bad "config: 'needle config --get workspace.default' failed"
fi

# 3. Default workspace resolution.
resolved="$(needle config --get workspace.default 2>/dev/null | tr -d '[:space:]')"
if [ "$resolved" = "$WORKSPACE" ]; then
    ok "workspace: workspace.default resolves to $WORKSPACE"
else
    bad "workspace: workspace.default is '$resolved', expected '$WORKSPACE'"
fi

# 4. Store layout: bead-rs layout, not the bf-era flat JSONL store.
if [ -f "$WORKSPACE/.beads/beads.db" ]; then
    ok "store: $WORKSPACE/.beads/beads.db present (bead-rs SQLite live store)"
else
    bad "store: $WORKSPACE/.beads/beads.db missing"
fi
if [ -d "$WORKSPACE/.beads/checkpoint" ]; then
    ok "store: $WORKSPACE/.beads/checkpoint/ present (durable checkpoint dir)"
else
    bad "store: $WORKSPACE/.beads/checkpoint/ missing"
fi
if [ -f "$WORKSPACE/.beads/config.json" ]; then
    ok "store: $WORKSPACE/.beads/config.json present (workspace identity)"
else
    bad "store: $WORKSPACE/.beads/config.json missing"
fi
if [ -e "$WORKSPACE/.beads/issues.jsonl" ]; then
    bad "store: $WORKSPACE/.beads/issues.jsonl exists — bf-era flat store; bead-rs uses beads.db + checkpoint/"
else
    ok "store: no bf-era flat .beads/issues.jsonl"
fi

# 5. CLI contract.
if command -v bead >/dev/null 2>&1; then
    ok "cli: bead on PATH at $(command -v bead)"
else
    bad "cli: bead not on PATH"
fi
if command -v bf >/dev/null 2>&1; then
    note "cli: bf is also on PATH at $(command -v bf) — deprecated; never run it against a bead-rs store"
fi
if command -v jq >/dev/null 2>&1; then
    ok "cli: jq available"
else
    bad "cli: jq missing"
fi

# `bead list --json` emits JSONL: the first non-empty line is one object,
# not a top-level array.
first_line="$( (cd "$WORKSPACE" && bead list --ready --json --limit 1 2>/dev/null) | grep -m1 . || true )"
if [ -n "$first_line" ] && printf '%s' "$first_line" | jq -e 'type == "object"' >/dev/null 2>&1; then
    ok "cli: 'bead list --ready --json' emits JSONL (line parses as one JSON object)"
else
    bad "cli: 'bead list --ready --json' first line is not a JSON object: ${first_line:-<empty>}"
fi

# `bead show ID --json` emits a JSON array (pipeline it through jq '.[0]').
first_id="$(printf '%s' "$first_line" | jq -r '.id // empty' 2>/dev/null || true)"
if [ -z "$first_id" ]; then
    first_id="$( (cd "$WORKSPACE" && bead list --status open --json --limit 1 2>/dev/null) | grep -m1 . | jq -r '.id // empty' 2>/dev/null || true )"
fi
if [ -n "$first_id" ]; then
    if (cd "$WORKSPACE" && bead show "$first_id" --json 2>/dev/null) | jq -e 'type == "array"' >/dev/null 2>&1; then
        ok "cli: 'bead show ID --json' emits a JSON array"
    else
        bad "cli: 'bead show $first_id --json' did not emit a JSON array"
    fi
else
    note "cli: no open/ready bead available to exercise 'bead show --json'"
fi

# 6. Live strands.pluck values — the section the historical pages misquoted.
if [ -f "$CONFIG_PATH" ] && grep -q '^  pluck:' "$CONFIG_PATH"; then
    pluck_block="$(sed -n '/^  pluck:/,/^  [a-z_]*:/p' "$CONFIG_PATH")"
    ok "pluck: strands.pluck section present in $CONFIG_PATH"
    if printf '%s\n' "$pluck_block" | grep -q 'exclude_labels: *\[\]'; then
        note "pluck: exclude_labels is [] — PluckStrand substitutes its built-in default set (see docs/bead-visibility-troubleshooting.md)"
    else
        printf '%s\n' "$pluck_block" | sed -n '/exclude_labels:/,/^[^ ]/p' | sed '$d' | head -12
    fi
    for key in split_after_failures persistent_starvation_records; do
        value="$(printf '%s\n' "$pluck_block" | sed -n "s/^ *$key: *//p" | tail -1 | tr -d '[:space:]')"
        if [ -n "$value" ]; then
            ok "pluck: $key = $value"
        else
            note "pluck: $key not explicitly set (built-in default applies)"
        fi
    done
else
    note "pluck: no strands.pluck section in $CONFIG_PATH — PluckConfig defaults apply"
fi

# 7. Durable no-candidate diagnostics. The setting name is historical; the
#    file is starvation_events.jsonl and lives under the state dir, never in
#    a target workspace's .beads store.
if [ -f "$STATE_DIR/state/starvation_events.jsonl" ]; then
    ok "diagnostics: $STATE_DIR/state/starvation_events.jsonl present"
else
    note "diagnostics: $STATE_DIR/state/starvation_events.jsonl absent (no snapshot written yet, or persistent_starvation_records disabled)"
fi

echo
echo "== $pass passed, $fail failed =="
if [ "$fail" -gt 0 ]; then
    echo "Reconcile the documentation against this output; see docs/bead-visibility-troubleshooting.md"
    exit 1
fi
echo "Current-state snapshot verified; documentation claims must match this output."
exit 0
