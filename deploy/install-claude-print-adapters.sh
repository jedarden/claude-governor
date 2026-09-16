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
#
# Beyond the --version check, this script now runs the two authoritative
# verifications CLAUDE.md prescribes (claudego-b03e5c39), because
# `needle test-agent` cannot catch either failure class:
#   4. a static check that each template still unsets the rule-3 (inherited
#      IDE env) and rule-5 (inherited API-routing env) variable sets
#   5. a live run of each template's invoke_template verbatim with a trivial
#      prompt and a poisoned scrub env, requiring exit 0 — one trivial
#      subscription call per adapter
# Use --skip-live to run only the static checks (offline, or conserving
# subscription quota). The Rust twin of these checks lives in
# src/adapter_verify.rs and runs as `cgov doctor`'s claude_print_adapters
# check; keep the variable lists in the two files in sync.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

SKIP_LIVE=0
for arg in "$@"; do
    case "${arg}" in
        --skip-live) SKIP_LIVE=1 ;;
        -h|--help)
            sed -n '2,23p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: ${arg}" >&2
            echo "usage: $(basename "${BASH_SOURCE[0]}") [--skip-live]" >&2
            exit 2
            ;;
    esac
done

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADAPTER_SRC="${REPO_DIR}/deploy/needle-adapters"
ADAPTER_DST="${HOME}/.config/needle/adapters"

# Rule-3 and rule-5 variable sets that every template must unset. Keep in
# sync with IDE_ENV_VARS / API_ROUTING_ENV_VARS in src/adapter_verify.rs and
# the adapter rules in the repo CLAUDE.md.
RULE3_IDE_VARS=(CLAUDECODE CLAUDE_CODE_SSE_PORT VSCODE_IPC_HOOK_CLI VSCODE_GIT_IPC_HANDLE VSCODE_GIT_ASKPASS_NODE VSCODE_GIT_ASKPASS_MAIN VSCODE_GIT_ASKPASS_EXTRA_ARGS VSCODE_INJECTION VSCODE_NONCE VSCODE_PID VSCODE_CWD)
RULE5_API_VARS=(ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL ANTHROPIC_MODEL ANTHROPIC_SMALL_FAST_MODEL ANTHROPIC_DEFAULT_OPUS_MODEL ANTHROPIC_DEFAULT_SONNET_MODEL ANTHROPIC_DEFAULT_HAIKU_MODEL CLAUDE_CODE_SUBAGENT_MODEL)

# Cap on the live probe. A trivial prompt normally finishes in well under a
# minute; the cap only bites on a hung dispatch, which is the failure class
# the probe exists to surface.
PROBE_TIMEOUT_SECS=240
# Stable probe workspace (not a fresh temp dir per run) so --pretrust-cwd
# writes its trust entry once. Same directory the Rust side uses.
PROBE_DIR="${XDG_CACHE_HOME:-${HOME}/.cache}/claude-print-adapter-verify"

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

# ── helpers for the template checks ─────────────────────────────────────────

# First top-level scalar for `key` in an adapter YAML, unquoted.
yaml_scalar() {  # yaml_scalar <file> <key>
    sed -n "s/^${2}:[[:space:]]*//p" "${1}" | head -1 | sed 's/^"//; s/"[[:space:]]*$//'
}

# Variables an invoke_template actually unsets: the templates are flat
# one-liners (`cd {workspace} && unset V1 V2 … && /path/…`), so split on &&
# and read the segments that begin with `unset`. Trim each record first —
# every segment after the first `&&` starts with a space, and split() makes
# that leading separator an empty w[1], which would hide the `unset`. awk
# RS='&&' is a gawk extension; mawk falls back to RS='&', which yields the
# same segments plus empty ones this test discards.
template_unset_vars() {  # stdin: invoke_template; stdout: one var per line
    awk -v RS='&&' '{
        sub(/^[ \t\n]+/, "")
        n = split($0, w, /[ \t\n]+/)
        if (w[1] == "unset") for (i = 2; i <= n; i++) print w[i]
    }'
}

# Required rule-3/rule-5 variables the template does NOT unset.
scrub_missing() {  # scrub_missing <invoke_template>
    comm -23 \
        <(printf '%s\n' "${RULE3_IDE_VARS[@]}" "${RULE5_API_VARS[@]}" | sort -u) \
        <(printf '%s' "${1}" | template_unset_vars | sort -u)
}

# ── 4. Static scrub check (rules 3 and 5) ───────────────────────────────────
# A template that stops unsetting the IDE env (rule 3) or the API-routing env
# (rule 5) hangs or misroutes dispatches launched from interactive shells —
# 46% of the fleet's dispatch volume at the time rule 3 was written down.
echo ""
echo "Scrub check (rule-3 IDE env, rule-5 API-routing env):"
scrub_fail=0
PROBE_TEMPLATES=()
PROBE_MODELS=()
PROBE_NAMES=()
for yaml_file in "${ADAPTER_DST}"/claude-print-*.yaml; do
    name="$(yaml_scalar "${yaml_file}" name)"
    tmpl="$(yaml_scalar "${yaml_file}" invoke_template)"
    model="$(yaml_scalar "${yaml_file}" model)"
    if [ -z "${tmpl}" ]; then
        echo -e "  ${RED}✗ ${name:-$(basename "${yaml_file}")}: no invoke_template${NC}"
        scrub_fail=1
        continue
    fi
    missing="$(scrub_missing "${tmpl}")"
    if [ -n "${missing}" ]; then
        echo -e "  ${RED}✗ ${name}: invoke_template stops unsetting $(echo "${missing}" | tr '\n' ' ')${NC}"
        scrub_fail=1
        continue
    fi
    echo -e "  ${GREEN}✓${NC} ${name} unsets all rule-3 and rule-5 variables"
    PROBE_TEMPLATES+=("${tmpl}")
    PROBE_MODELS+=("${model}")
    PROBE_NAMES+=("${name}")
done

# ── 5. Live invoke_template probe ───────────────────────────────────────────
# The exit-0 bar CLAUDE.md prescribes, run against the installed template
# verbatim. The probe poisons every rule-3/rule-5 variable in the child env:
# if the template scrubs properly the call reaches the real subscription
# endpoint; if a regression drops an unset, the poisoned value (closed port,
# bogus model/auth) makes the failure deterministic instead of silently
# passing because this shell happens to be clean. Values are dummies — the
# worst a leak can leak is the poison itself.
live_fail=0
if [ "${SKIP_LIVE}" -eq 1 ]; then
    echo ""
    echo "Live dispatch probe: skipped (--skip-live); static checks only."
elif [ "${scrub_fail}" -ne 0 ]; then
    echo ""
    echo "Live dispatch probe: skipped — fix the scrub regressions first."
elif [ ${#PROBE_TEMPLATES[@]} -eq 0 ]; then
    echo ""
    echo "Live dispatch probe: skipped — no templates to probe."
else
    echo ""
    echo "Live dispatch probe (trivial prompt, poisoned scrub env, exit-0 + output required):"
    mkdir -p "${PROBE_DIR}"
    PROMPT_FILE="${PROBE_DIR}/probe-prompt.txt"
    printf '%s\n' \
        "Adapter verification probe from install-claude-print-adapters.sh: reply with the single word OK and nothing else. Do not use any tools." \
        > "${PROMPT_FILE}"

    for i in "${!PROBE_TEMPLATES[@]}"; do
        rendered="${PROBE_TEMPLATES[$i]//'{workspace}'/${PROBE_DIR}}"
        rendered="${rendered//'{model}'/${PROBE_MODELS[$i]}}"
        rendered="${rendered//'{prompt_file}'/${PROMPT_FILE}}"

        out_file="$(mktemp)"
        err_file="$(mktemp)"
        rc=0
        (
            set +e
            export CLAUDECODE=1 CLAUDE_CODE_SSE_PORT=0
            for v in "${RULE3_IDE_VARS[@]}" "${RULE5_API_VARS[@]}"; do
                case "${v}" in
                    CLAUDECODE|CLAUDE_CODE_SSE_PORT) ;;
                    ANTHROPIC_BASE_URL) export "${v}=http://127.0.0.1:9" ;;
                    ANTHROPIC_*) export "${v}=cgov-adapter-probe-poison" ;;
                    *) export "${v}=/nonexistent-cgov-adapter-probe" ;;
                esac
            done
            timeout "${PROBE_TIMEOUT_SECS}" bash -c "${rendered}"
        ) >"${out_file}" 2>"${err_file}" || rc=$?

        if [ "${rc}" -eq 0 ]; then
            if [ -s "${out_file}" ]; then
                echo -e "  ${GREEN}✓${NC} ${PROBE_NAMES[$i]} — invoke_template exit 0 ($(wc -c <"${out_file}") bytes out)"
            else
                echo -e "  ${RED}✗ ${PROBE_NAMES[$i]} — exit 0 but empty output (the silent-empty-output signature)${NC}"
                live_fail=1
            fi
        elif [ "${rc}" -eq 124 ]; then
            echo -e "  ${RED}✗ ${PROBE_NAMES[$i]} — timed out after ${PROBE_TIMEOUT_SECS}s (hung dispatch — the IDE/API-routing env signature)${NC}"
            live_fail=1
        else
            echo -e "  ${RED}✗ ${PROBE_NAMES[$i]} — invoke_template exited ${rc}${NC}"
            echo "    stderr tail: $(tail -c 300 "${err_file}" | tr '\n' ' ')"
            live_fail=1
        fi
        rm -f "${out_file}" "${err_file}"
    done
fi

if [ "${fail}" -ne 0 ] || [ "${scrub_fail}" -ne 0 ] || [ "${live_fail}" -ne 0 ]; then
    echo -e "${RED}Install incomplete.${NC}"
    exit 1
fi

echo ""
echo -e "${GREEN}Done.${NC} Adapters installed, and their invoke_templates verified."
echo "Note: 'needle test-agent claude-print-opus' can report READY even when"
echo "dispatch is broken — the checks above (and cgov doctor's"
echo "claude_print_adapters check) are the authoritative ones."
