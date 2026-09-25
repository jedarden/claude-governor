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
# Read-only: it only reads config files and runs `bead list` / `bead show`.
# Exit 0 = every check passed. Exit 1 = at least one check failed; reconcile
# the documentation against this output, never from memory.

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
