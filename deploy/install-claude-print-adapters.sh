#!/usr/bin/env bash
# Install the claude-print NEEDLE adapters and guarantee the absolute binary
# path they call actually exists.
#
# Why this exists (claudego-49195ba4): the adapters call claude-print by
# absolute path on purpose — NEEDLE's dispatch shell PATH is not the
# interactive shell's, so a bare `claude-print` is "command not found" and
# produces silent empty output. But nothing established that path. On
# codinghome it was simply absent (the working binary lives at
# ~/.cargo/bin/claude-print, where `cargo install` puts it), so every
# polish-opus dispatch died at exit 127 while `needle test-agent` still
# reported READY.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADAPTER_SRC="${REPO_DIR}/deploy/needle-adapters"
ADAPTER_DST="${HOME}/.config/needle/adapters"

echo -e "${GREEN}claude-print adapter installer${NC}"
echo "=============================="

# ── 1. Install the adapter YAMLs ────────────────────────────────────────────
# The live directory is ~/.config/needle/adapters, NOT ~/.needle/agents — the
# latter is a stale staging path the current needle binary does not read.
mkdir -p "${ADAPTER_DST}"
for src in "${ADAPTER_SRC}"/claude-print-*.yaml; do
    install -m 644 "${src}" "${ADAPTER_DST}/$(basename "${src}")"
    echo "  installed $(basename "${src}") -> ${ADAPTER_DST}/"
done

# ── 2. Establish every absolute path the templates actually call ────────────
# Read the path out of the adapters rather than hardcoding it here, so this
# script cannot drift from the templates it is meant to satisfy.
mapfile -t WANTED < <(
    grep -ho '/[^ ]*/claude-print' "${ADAPTER_DST}"/claude-print-*.yaml | sort -u
)

if [ ${#WANTED[@]} -eq 0 ]; then
    echo -e "${RED}✗ No absolute claude-print path found in the adapters.${NC}"
    echo "  Expected an invoke_template calling claude-print by absolute path."
    exit 1
fi

# Locate a real claude-print to point at, preferring one already on PATH.
SOURCE_BIN=""
for candidate in \
    "$(command -v claude-print 2>/dev/null || true)" \
    "${HOME}/.cargo/bin/claude-print" \
    "${HOME}/.local/bin/claude-print"
do
    if [ -n "${candidate}" ] && [ -x "${candidate}" ] && [ ! -L "${candidate}" ]; then
        SOURCE_BIN="${candidate}"
        break
    fi
done

if [ -z "${SOURCE_BIN}" ]; then
    echo -e "${RED}✗ No claude-print binary found.${NC}"
    echo "  Build and install it from the claude-print repo first:"
    echo "      cd ~/claude-print && cargo build --release && cargo install --path ."
    exit 1
fi
echo "  source binary: ${SOURCE_BIN}"

for want in "${WANTED[@]}"; do
    if [ "${want}" = "${SOURCE_BIN}" ]; then
        continue
    fi
    if [ -x "${want}" ]; then
        echo "  ok: ${want} already present"
        continue
    fi
    mkdir -p "$(dirname "${want}")"
    ln -sfn "${SOURCE_BIN}" "${want}"
    echo -e "  ${YELLOW}linked${NC} ${want} -> ${SOURCE_BIN}"
done

# ── 3. Verify what dispatch will actually run ───────────────────────────────
# `needle test-agent` is NOT sufficient on its own: it resolves `agent_cli`
# through PATH while dispatch runs `invoke_template`, so it reports READY for
# an adapter whose template names a binary that does not exist (NEEDLE bead
# needle-adef2ccd). Check the template's own path directly.
echo ""
echo "Verifying:"
fail=0
for want in "${WANTED[@]}"; do
    if [ ! -x "${want}" ]; then
        echo -e "  ${RED}✗ ${want} is not executable${NC}"
        fail=1
        continue
    fi
    if ! ver="$("${want}" --version 2>&1)"; then
        echo -e "  ${RED}✗ ${want} --version failed: ${ver}${NC}"
        fail=1
        continue
    fi
    echo -e "  ${GREEN}✓${NC} ${want} — ${ver}"
done

if [ "${fail}" -ne 0 ]; then
    echo -e "${RED}Install incomplete.${NC}"
    exit 1
fi

echo ""
echo -e "${GREEN}Done.${NC} Adapters installed and their binary paths verified."
echo "Note: 'needle test-agent claude-print-opus' can report READY even when"
echo "dispatch is broken — the check above is the authoritative one."
