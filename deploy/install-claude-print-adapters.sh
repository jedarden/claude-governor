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
# a dispatched strand died at exit 127 while `needle test-agent` still
# reported READY.
#
# Beyond the --version check, this script now runs the authoritative
# verifications CLAUDE.md prescribes (claudego-b03e5c39), because
# `needle test-agent` cannot catch these failure classes:
#   1. a static check that each template still unsets the rule-3 (inherited
#      IDE env) and rule-5 (inherited API-routing env) variable sets
#   2. a static check that each template still carries the full invoke
#      contract — the `< {prompt_file}` redirection, `--pretrust-cwd`,
#      `--output-format stream-json`, `--no-inherit-hooks` — and still pins
#      `timeout_secs` to its documented per-adapter value (opus 1200s, fable
#      600s). A trivial live probe exits 0 with output through every one of
#      these regressions; production dispatch does not survive them.
#   3. a live run of each template's invoke_template verbatim with a trivial
#      prompt and a poisoned scrub env, requiring exit 0 — one trivial
#      subscription call per adapter
# Use --skip-live to run only the static checks (offline, or conserving
# subscription quota). The Rust twin of these checks lives in
# src/adapter_verify.rs and runs as `cgov doctor`'s claude_print_adapters
# check. Its rule-3/rule-5 variable lists, invoke flags and timeout pins are
# mirrored in bash below, and the mirror is enforced, not manual: this script
# cross-checks the Rust constants (section 4) before trusting its own lists,
# and a cargo-test parse of this file fails on the same drift — add new
# variables, flags or pins to both copies.

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
            sed -n '2,36p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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

# Rule-3 and rule-5 variable sets that every template must unset — the bash
# mirror of IDE_ENV_VARS / API_ROUTING_ENV_VARS in src/adapter_verify.rs (the
# canonical copy). The sync check in section 4 fails this install if the two
# copies diverge, and a cargo-test parse of this file fails the same way, so
# drift cannot go silent: add new variables to BOTH lists.
RULE3_IDE_VARS=(CLAUDECODE CLAUDE_CODE_SSE_PORT VSCODE_IPC_HOOK_CLI VSCODE_GIT_IPC_HANDLE VSCODE_GIT_ASKPASS_NODE VSCODE_GIT_ASKPASS_MAIN VSCODE_GIT_ASKPASS_EXTRA_ARGS VSCODE_INJECTION VSCODE_NONCE VSCODE_PID VSCODE_CWD)
RULE5_API_VARS=(ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL ANTHROPIC_MODEL ANTHROPIC_SMALL_FAST_MODEL ANTHROPIC_DEFAULT_OPUS_MODEL ANTHROPIC_DEFAULT_SONNET_MODEL ANTHROPIC_DEFAULT_HAIKU_MODEL CLAUDE_CODE_SUBAGENT_MODEL)

# The invoke tokens and per-adapter timeout_secs pins every template must
# carry — the bash mirror of REQUIRED_INVOKE_FLAGS / ADAPTER_TIMEOUT_PINS in
# src/adapter_verify.rs (the canonical copy). Same rule as the lists above:
# the sync check in section 4 fails this install if the copies diverge, and a
# cargo-test parse of this file fails the same drift — add new flags or pins
# to BOTH lists. Flags are matched as literal substrings of the template; a
# pin is `adapter-name=seconds`, exact (not a ceiling): a raised value wedges
# a worker that much longer on a hung strand, a lowered one kills legitimate
# strands, and NEEDLE's stuck-detection is blind during a run, so
# timeout_secs is the only killer of a hung strand.
REQUIRED_INVOKE_FLAGS=("< {prompt_file}" "--pretrust-cwd" "--output-format stream-json" "--no-inherit-hooks")
ADAPTER_TIMEOUT_PINS=("claude-print-fable=600" "claude-print-opus=1200")

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

# ── variable-list sync helpers ──────────────────────────────────────────────
# The arrays above are a bash mirror of the canonical Rust constants; nothing
# used to fail when they drifted, and a drifted list silently makes this
# script's scrub check cover a different variable set than `cgov doctor`'s.
# rust_rule_vars reads the constants straight out of the Rust source so this
# script can fail its own install on divergence.

rust_rule_vars() {  # rust_rule_vars <const-name> — one var per line, sorted
    sed -n "/pub const ${1}:/,/^\];$/p" "${REPO_DIR}/src/adapter_verify.rs" \
        | grep -oE '"[^"]+"' | tr -d '"' | sort -u
}

bash_rule_vars() {  # bash_rule_vars <array-name> — one var per line, sorted
    local -n array_ref="${1}"
    printf '%s\n' "${array_ref[@]}" | sort -u
}

# ── invoke-contract sync helpers ────────────────────────────────────────────
# REQUIRED_INVOKE_FLAGS / ADAPTER_TIMEOUT_PINS above are a bash mirror of the
# canonical Rust constants, under the same two-gate rule as the variable
# lists: this script fails its own install on divergence, and a cargo-test
# parse of this file (installer_bash_flag_and_timeout_pins_match_the_rust_
# constants) fails the same drift.

rust_invoke_flags() {  # one flag per line, sorted
    sed -n "/pub const REQUIRED_INVOKE_FLAGS:/,/^\];$/p" "${REPO_DIR}/src/adapter_verify.rs" \
        | grep -oE '"[^"]+"' | tr -d '"' | sort -u
}

bash_invoke_flags() {  # one flag per line, sorted
    printf '%s\n' "${REQUIRED_INVOKE_FLAGS[@]}" | sort -u
}

rust_timeout_pins() {  # one name=value pin per line, sorted
    sed -n "/pub const ADAPTER_TIMEOUT_PINS:/,/^\];$/p" "${REPO_DIR}/src/adapter_verify.rs" \
        | grep -oE '\("[^"]+", [0-9]+\)' | sed 's/^("\([^"]*\)", \([0-9]*\))$/\1=\2/' | sort -u
}

bash_timeout_pins() {  # one name=value pin per line, sorted
    printf '%s\n' "${ADAPTER_TIMEOUT_PINS[@]}" | sort -u
}

check_flag_and_timeout_sync() {
    local sync_fail=0 drift
    drift="$(diff <(rust_invoke_flags) <(bash_invoke_flags) || true)"
    if [ -n "${drift}" ]; then
        echo -e "  ${RED}✗ invoke-flag drift: REQUIRED_INVOKE_FLAGS (this script) vs src/adapter_verify.rs${NC}"
        sed 's/^/      /' <<<"${drift}"
        sync_fail=1
    fi
    drift="$(diff <(rust_timeout_pins) <(bash_timeout_pins) || true)"
    if [ -n "${drift}" ]; then
        echo -e "  ${RED}✗ timeout-pin drift: ADAPTER_TIMEOUT_PINS (this script) vs src/adapter_verify.rs${NC}"
        sed 's/^/      /' <<<"${drift}"
        sync_fail=1
    fi
    return "${sync_fail}"
}

check_var_list_sync() {
    local rust_src="${REPO_DIR}/src/adapter_verify.rs"
    if [ ! -f "${rust_src}" ]; then
        echo -e "  ${RED}✗ ${rust_src} not found — cannot cross-check the rule-3/rule-5 lists${NC}"
        return 1
    fi
    local sync_fail=0 pair rust_name bash_name drift
    for pair in "IDE_ENV_VARS RULE3_IDE_VARS" "API_ROUTING_ENV_VARS RULE5_API_VARS"; do
        rust_name="${pair%% *}"
        bash_name="${pair##* }"
        drift="$(diff <(rust_rule_vars "${rust_name}") <(bash_rule_vars "${bash_name}") || true)"
        if [ -n "${drift}" ]; then
            echo -e "  ${RED}✗ variable-list drift: ${bash_name} (this script) vs ${rust_name} (src/adapter_verify.rs)${NC}"
            sed 's/^/      /' <<<"${drift}"
            sync_fail=1
        fi
    done
    return "${sync_fail}"
}

# ── 4. Sync check (installer arrays vs src/adapter_verify.rs) ───────────────
# The static checks below are only as good as the lists they check against;
# fail the install rather than verify against sets nobody else enforces.
echo ""
echo "Variable-list and invoke-contract sync check (installer arrays vs src/adapter_verify.rs):"
sync_fail=0
if ! check_var_list_sync; then
    sync_fail=1
else
    echo -e "  ${GREEN}✓${NC} rule-3 (RULE3_IDE_VARS) and rule-5 (RULE5_API_VARS) match the Rust constants"
fi
if ! check_flag_and_timeout_sync; then
    sync_fail=1
else
    echo -e "  ${GREEN}✓${NC} REQUIRED_INVOKE_FLAGS and ADAPTER_TIMEOUT_PINS match the Rust constants"
fi

# ── 5. Static template contract (scrub rules 3/5, invoke flags, timeout) ────
# A template that stops unsetting the IDE env (rule 3) or the API-routing env
# (rule 5) hangs or misroutes dispatches launched from interactive shells —
# 46% of the fleet's dispatch volume at the time rule 3 was written down. A
# template that drops an invoke flag or drifts timeout_secs off its pin is
# the same shape of silent: the live probe in section 6 still sees exit 0
# with output; only production dispatch breaks.
echo ""
echo "Static template contract (scrub rules 3/5, invoke flags, timeout_secs pins):"
scrub_fail=0
contract_fail=0
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

    missing_flags=""
    for flag in "${REQUIRED_INVOKE_FLAGS[@]}"; do
        case "${tmpl}" in
            *"${flag}"*) ;;
            *) missing_flags="${missing_flags}${flag}, " ;;
        esac
    done
    if [ -n "${missing_flags}" ]; then
        echo -e "  ${RED}✗ ${name}: invoke_template drops required dispatch flags: ${missing_flags%, } — a trivial live probe still exits 0 without them${NC}"
        contract_fail=1
        continue
    fi

    pin=""
    for entry in "${ADAPTER_TIMEOUT_PINS[@]}"; do
        if [ "${entry%%=*}" = "${name}" ]; then
            pin="${entry##*=}"
            break
        fi
    done
    tmpl_secs="$(yaml_scalar "${yaml_file}" timeout_secs)"
    if [ -z "${pin}" ]; then
        echo -e "  ${RED}✗ ${name}: no timeout_secs pin for this adapter name — add it to ADAPTER_TIMEOUT_PINS in this script and src/adapter_verify.rs (both copies)${NC}"
        contract_fail=1
        continue
    elif [ -z "${tmpl_secs}" ]; then
        echo -e "  ${RED}✗ ${name}: omits timeout_secs — NEEDLE's stuck-detection is blind during a run, so timeout_secs is the only killer of a hung strand; the pin is ${pin}s${NC}"
        contract_fail=1
        continue
    elif [ "${tmpl_secs}" != "${pin}" ]; then
        echo -e "  ${RED}✗ ${name}: timeout_secs is ${tmpl_secs}s, pinned at ${pin}s — edit both copies to change a pin${NC}"
        contract_fail=1
        continue
    fi

    echo -e "  ${GREEN}✓${NC} ${name} unsets all rule-3 and rule-5 variables, carries every required invoke flag, pins timeout_secs at ${pin}s"
    PROBE_TEMPLATES+=("${tmpl}")
    PROBE_MODELS+=("${model}")
    PROBE_NAMES+=("${name}")
done

# ── 6. Live invoke_template probe ───────────────────────────────────────────
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
elif [ "${scrub_fail}" -ne 0 ] || [ "${contract_fail}" -ne 0 ]; then
    echo ""
    echo "Live dispatch probe: skipped — fix the static template regressions first."
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

if [ "${fail}" -ne 0 ] || [ "${scrub_fail}" -ne 0 ] || [ "${contract_fail}" -ne 0 ] || [ "${live_fail}" -ne 0 ] || [ "${sync_fail}" -ne 0 ]; then
    echo -e "${RED}Install incomplete.${NC}"
    exit 1
fi

echo ""
echo -e "${GREEN}Done.${NC} Adapters installed, and their invoke_templates verified."
echo "Note: 'needle test-agent claude-print-opus' can report READY even when"
echo "dispatch is broken — the checks above (and cgov doctor's"
echo "claude_print_adapters check) are the authoritative ones."
