#!/usr/bin/env bash
#
# Verify the README's zero-runtime-dependency promise:
#   "Zero runtime dependencies — Single statically-linked binary"
#
# For each binary, FAILS unless all of these hold:
#   1. it is an ELF executable for x86-64 or AArch64 (the platforms
#      install.sh supports);
#   2. it has no PT_INTERP program header — no dynamic loader is requested;
#   3. it has no NEEDED entry in its dynamic section — no shared library is
#      required (checks 2+3 are the authoritative definition of static);
#   4. file(1) never reports it as dynamically linked;
#   5. `--version` and `--help` exit 0 with output when run under `env -i`
#      from an empty working directory with PATH pointed at an empty
#      directory — no environment, no libraries to resolve at runtime, no
#      project files to read.
# The probe always runs for this machine's architecture. A foreign-
# architecture binary runs it too whenever a way to execute it exists: a
# qemu-user emulator on PATH (the -static builds preferred), or an enabled
# binfmt_misc registration the kernel itself would use. Only with neither
# is check 5 skipped, with a printed note — that artifact ships with the
# linkage checks alone (readelf/file are cross-arch); every other check
# still applies.
#
# Usage:
#   scripts/verify-release-static.sh [binary ...]
#   scripts/verify-release-static.sh          # auto-discover repo artifacts
#
# Exit 0 = every binary verified; 1 = at least one check failed, no artifact
# found, or a required tool is missing.

set -u

failures=0

die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'PASS: %s\n' "$*"; }
note() { printf 'NOTE: %s\n' "$*"; }

command -v readelf >/dev/null 2>&1 || die "readelf (binutils) is required"
# file(1) is an optional cross-check only — hosts without it (e.g. the debian
# build container installs no file package) still get the full guarantee from
# the readelf INTERP/NEEDED checks below.
HAVE_FILE="$(command -v file || true)"

# Canonical machine name of an ELF, restricted to the supported platforms.
elf_machine_of() {
    local h
    h="$(readelf -h "$1" 2>/dev/null)" || { echo other; return; }
    if grep -qi 'Machine:.*X86-64' <<<"$h"; then echo x86_64
    elif grep -qi 'Machine:.*AArch64' <<<"$h"; then echo aarch64
    else echo other; fi
}

host_machine() {
    case "$(uname -m)" in
        x86_64|amd64) echo x86_64 ;;
        aarch64|arm64) echo aarch64 ;;
        *) echo other ;;
    esac
}

# Absolute path of a user-mode emulator able to run an ELF of machine $1,
# or "" when none is on PATH. The -static builds are preferred: the probe
# runs under env -i, and a static emulator cannot itself need libraries.
emulator_for() {
    local m="$1" e
    case "$m" in
        x86_64)  set -- qemu-x86_64-static  qemu-x86_64  ;;
        aarch64) set -- qemu-aarch64-static qemu-aarch64 ;;
        *) echo ""; return ;;
    esac
    for e in "$@"; do
        if command -v "$e" >/dev/null 2>&1; then
            command -v "$e"
            return
        fi
    done
    echo ""
}

# True when the kernel would exec an ELF of machine $1 itself: an enabled
# binfmt_misc registration whose magic carries $1's ET_EXEC + e_machine
# pair — exactly what the qemu binfmt registrations match (their mask also
# admits ET_DYN, so this stays a conservative "would exec" test).
# BINFMT_MISC_DIR overrides the mountpoint so tests can pin both cases
# regardless of the host's real registrations.
binfmt_registered_for() {
    local m="$1" dir f magic
    case "$m" in
        x86_64)  magic=02003e ;;  # ET_EXEC(02 00), EM_X86_64(3e 00), little-endian
        aarch64) magic=0200b7 ;;  # ET_EXEC(02 00), EM_AARCH64(b7 00)
        *) return 1 ;;
    esac
    dir="${BINFMT_MISC_DIR:-/proc/sys/fs/binfmt_misc}"
    [ -d "$dir" ] || return 1
    for f in "$dir"/*; do
        [ -f "$f" ] || continue
        case "${f##*/}" in register|status) continue ;; esac
        [ "$(head -1 "$f" 2>/dev/null)" = "enabled" ] || continue
        if grep -q "^magic [0-9a-f]*$magic" "$f" 2>/dev/null; then
            return 0
        fi
    done
    return 1
}

# Existing release artifacts this repo produces, deduped by realpath.
# The cargo target dir is resolved via `cargo metadata` so a configured
# target-dir (fleet boxes redirect it off-repo) is honored; falls back to
# $CARGO_TARGET_DIR and ./target.
discover_artifacts() {
    local td="" p rp seen
    if command -v cargo >/dev/null 2>&1; then
        td="$(cargo metadata --no-deps --offline --format-version 1 2>/dev/null \
              | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1)"
    fi
    [ -n "$td" ] || td="${CARGO_TARGET_DIR:-$PWD/target}"
    local candidates=()
    for p in "$td"/*-musl/release/cgov \
             "$PWD/cgov-linux-amd64" "$PWD/cgov-linux-arm64"; do
        [ -f "$p" ] || continue
        rp="$(readlink -f "$p")"
        seen=""
        for p in "${candidates[@]:-}"; do
            [ "$p" = "$rp" ] && { seen=1; break; }
        done
        [ -n "$seen" ] || candidates+=("$rp")
    done
    printf '%s\n' "${candidates[@]:-}"
}

verify_one() {
    local bin="$1"

    if [ ! -f "$bin" ] || [ ! -x "$bin" ]; then
        printf 'FAIL: %s: not an executable regular file\n' "$bin"
        failures=$((failures + 1))
        return
    fi

    local h type_
    h="$(readelf -h "$bin" 2>/dev/null)" || {
        printf 'FAIL: %s: readelf could not parse it as ELF\n' "$bin"
        failures=$((failures + 1))
        return
    }
    type_="$(awk '/^  Type:/{print $2}' <<<"$h")"
    case "$type_" in
        EXEC|DYN) ;;  # DYN with no INTERP below = static-pie, also fine
        *)
            printf 'FAIL: %s: unexpected ELF type %q (want EXEC or DYN)\n' "$bin" "$type_"
            failures=$((failures + 1))
            return
            ;;
    esac

    local machine
    machine="$(elf_machine_of "$bin")"
    case "$machine" in
        x86_64|aarch64) pass "$bin: ELF $type_ for $machine" ;;
        *)
            printf 'FAIL: %s: unsupported ELF machine (install.sh supports x86-64/AArch64 only)\n' "$bin"
            failures=$((failures + 1))
            return
            ;;
    esac

    if readelf -l "$bin" 2>/dev/null | grep -q 'INTERP'; then
        printf 'FAIL: %s: has a PT_INTERP segment (requests a dynamic loader)\n' "$bin"
        failures=$((failures + 1))
    else
        pass "$bin: no PT_INTERP (no dynamic loader)"
    fi

    if readelf -d "$bin" 2>/dev/null | grep -q 'NEEDED'; then
        printf 'FAIL: %s: has NEEDED shared-library entries:\n' "$bin"
        readelf -d "$bin" 2>/dev/null | grep 'NEEDED' | sed 's/^/      /'
        failures=$((failures + 1))
    else
        pass "$bin: no NEEDED entries (no shared libraries)"
    fi

    if [ -z "$HAVE_FILE" ]; then
        note "$bin: file(1) not available — skipping the file(1) cross-check (readelf checks are authoritative)"
    elif file -b "$bin" 2>/dev/null | grep -qi 'dynamically linked'; then
        printf 'FAIL: %s: file(1) reports a dynamically linked binary\n' "$bin"
        failures=$((failures + 1))
    else
        pass "$bin: file(1): $(file -b "$bin" 2>/dev/null | cut -c1-96)"
    fi

    # Execution probe. This machine's architecture runs natively. A foreign
    # architecture runs under a user-mode emulator when one is on PATH, or
    # directly when the kernel itself would exec it (an enabled binfmt_misc
    # registration) — either is a genuine run of this binary's instructions.
    # Only with neither is the probe skipped, loudly: that artifact ships
    # with linkage checks alone.
    local emu="" how=""
    if [ "$machine" != "$(host_machine)" ]; then
        emu="$(emulator_for "$machine")"
        if [ -n "$emu" ]; then
            how="under ${emu##*/} "
        elif binfmt_registered_for "$machine"; then
            how="under binfmt_misc "
        else
            note "$bin: foreign architecture ($machine host $(uname -m)) — linkage verified, but no $machine emulator on PATH (e.g. qemu-$machine-static) and no binfmt_misc registration: EXECUTION PROBE SKIPPED, this artifact ships with linkage checks only"
            return
        fi
    fi
    local tmp out rc probe abs
    tmp="$(mktemp -d)" || {
        printf 'FAIL: %s: mktemp failed for execution probe\n' "$bin"
        failures=$((failures + 1))
        return
    }
    abs="$(readlink -f "$bin")"
    for probe in --version --help; do
        if [ -n "$emu" ]; then
            out="$(cd "$tmp" && env -i PATH="$tmp" HOME="$tmp" "$emu" "$abs" "$probe" 2>&1)"
        else
            out="$(cd "$tmp" && env -i PATH="$tmp" HOME="$tmp" "$abs" "$probe" 2>&1)"
        fi
        rc=$?
        if [ "$rc" -eq 0 ] && [ -n "$out" ]; then
            pass "$bin: $probe ${how}in env -i, empty cwd, empty PATH (exit 0: $(head -1 <<<"$out"))"
        else
            printf 'FAIL: %s: %s %sin env -i, empty cwd, empty PATH: exit=%s output=%.200q\n' \
                "$bin" "$probe" "$how" "$rc" "$out"
            failures=$((failures + 1))
        fi
    done
    rm -rf -- "$tmp"
}

main() {
    local -a args=("$@")
    if [ "${#args[@]}" -eq 0 ]; then
        local -a found=()
        mapfile -t found < <(discover_artifacts)
        if [ "${#found[@]}" -eq 0 ]; then
            die "no release artifacts found (build one with: make verify-release)"
        fi
        note "auto-discovered artifact(s): ${found[*]}"
        args=("${found[@]}")
    fi
    local b
    for b in "${args[@]}"; do
        verify_one "$b"
    done
    if [ "$failures" -gt 0 ]; then
        printf 'verify-release-static: FAILED (%d check(s) failed)\n' "$failures"
        exit 1
    fi
    printf 'verify-release-static: OK — all binaries statically linked and runnable in a minimal environment\n'
}

main "$@"
