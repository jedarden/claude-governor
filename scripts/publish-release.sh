#!/usr/bin/env bash
#
# Release publication gate (claudego-78c221be).
#
# The single fail-closed path between "artifacts are built" and "a GitHub
# release is public". Every check below runs BEFORE anything is published;
# any failure exits 1 with nothing uploaded:
#
#   1. PROVENANCE — the release dir is a clone of the Forgejo repo, the tag
#      exists, points exactly at the built HEAD, and Forgejo itself resolves
#      that tag to the same commit. A release is only ever published from
#      the commit its Forgejo tag names; the GitHub mirror is never trusted.
#   2. ARTIFACTS — every architecture install.sh supports
#      (cgov-linux-amd64, cgov-linux-arm64) is present. A missing
#      architecture is a failure, not a partial release.
#   3. STATIC — every artifact passes scripts/verify-release-static.sh (the
#      zero-runtime-dependency promise) on EACH architecture, not just the
#      build host's; foreign-arch artifacts keep the full linkage checks.
#   4. SIDECARS — every artifact has an <artifact>.sha256 sidecar in exactly
#      the `sha256sum -c` format install.sh consumes, whose digest equals the
#      artifact's actual digest. Sidecars are validated here, never
#      generated: a published digest is immutable once the release is cut,
#      so it is checked, not rewritten.
#
# Only after all four pass: `gh release create` publishes BOTH binaries and
# BOTH sidecars, then the published asset list is re-fetched and required to
# pair every binary with its sidecar. An unpaired asset in either direction
# fails the run (exit 1) so the cut release can never pass silently.
#
# --dry-run runs phases 1-4 and stops before any gh call (CI rehearsal).
#
# Usage:
#   scripts/publish-release.sh --version v0.1.2 [--release-dir DIR] [--dry-run]
#
# Exit: 0 = all checks passed (and published, unless --dry-run);
#       1 = any validation or post-publish pairing check failed;
#       2 = usage error.
#
# Environment (sandbox/test hooks; production CI sets neither):
#   CGOV_FORGEJO_URL  Forgejo repo URL provenance is proven against
#                     (default: the source-of-truth repo below)
#   CGOV_GH_REPO      GitHub slug the release is published to
#                     (default: jedarden/claude-governor)

set -euo pipefail

FORGEJO_URL="${CGOV_FORGEJO_URL:-https://git.ardenone.com/jedarden/claude-governor.git}"
GH_REPO="${CGOV_GH_REPO:-jedarden/claude-governor}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATIC_CHECK="${SCRIPT_DIR}/verify-release-static.sh"
ARTIFACTS=(cgov-linux-amd64 cgov-linux-arm64)

die()  { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'PASS: %s\n' "$*"; }
note() { printf 'NOTE: %s\n' "$*"; }

usage() {
    sed -n '2,43p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

# Compare two git remote URLs as "the same repository": strip a trailing
# .git/slash and reduce the scp-like ssh form to host/path so the https
# clone the CI uses and an ssh clone agree.
canon_repo_url() {
    local u="$1"
    u="${u%.git}"
    u="${u%/}"
    case "$u" in
        https://*|http://*|ssh://*) u="${u#*://}"; u="${u#git@}";;
        git@*) u="${u#git@}"; u="${u/://}";;
    esac
    printf '%s' "$u"
}

VERSION=""
RELEASE_DIR=""
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version)     [ $# -ge 2 ] || die "--version requires a value"; VERSION="$2"; shift 2 ;;
        --release-dir) [ $# -ge 2 ] || die "--release-dir requires a value"; RELEASE_DIR="$2"; shift 2 ;;
        --dry-run)     DRY_RUN=1; shift ;;
        --help|-h)     usage ;;
        *)             usage ;;
    esac
done

# Accept 1.2.3 and normalize to v1.2.3, like install.sh does.
case "${VERSION}" in
    v*) ;;
    "") usage ;;
    *)  VERSION="v${VERSION}" ;;
esac
if ! [[ "${VERSION}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
    die "--version must be a vX.Y.Z release tag (got: ${VERSION})"
fi

[ -f "${STATIC_CHECK}" ] || die "static validator missing: ${STATIC_CHECK}"

if [ -z "${RELEASE_DIR}" ]; then
    RELEASE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
fi
[ -d "${RELEASE_DIR}" ] || die "--release-dir does not exist: ${RELEASE_DIR}"

echo "Claude Governor release publication gate"
echo "========================================"
echo "Tag:         ${VERSION}"
echo "Release dir: ${RELEASE_DIR}"
echo "Forgejo:     ${FORGEJO_URL}"
echo "GitHub repo: ${GH_REPO}"
[ "${DRY_RUN}" = 1 ] && echo "Mode:        dry-run (no publish)"
echo ""

# ---------------------------------------------------------------------------
# Phase 1: provenance — built from the Forgejo tag
# ---------------------------------------------------------------------------
git -C "${RELEASE_DIR}" rev-parse --git-dir >/dev/null 2>&1 \
    || die "provenance: ${RELEASE_DIR} is not a git checkout"

origin="$(git -C "${RELEASE_DIR}" remote get-url origin 2>/dev/null)" \
    || die "provenance: ${RELEASE_DIR} has no origin remote"
if [ "$(canon_repo_url "${origin}")" != "$(canon_repo_url "${FORGEJO_URL}")" ]; then
    die "provenance: origin is ${origin}, not the Forgejo source of truth (${FORGEJO_URL})"
fi
pass "provenance: origin is the Forgejo source of truth"

head_commit="$(git -C "${RELEASE_DIR}" rev-parse HEAD)"
tag_commit="$(git -C "${RELEASE_DIR}" rev-parse --verify --quiet "refs/tags/${VERSION}^{commit}" 2>/dev/null || true)"
[ -n "${tag_commit}" ] || die "provenance: tag ${VERSION} does not exist in the checkout"
[ "${tag_commit}" = "${head_commit}" ] \
    || die "provenance: tag ${VERSION} points at ${tag_commit}, but the artifacts were built from HEAD ${head_commit}"
pass "provenance: tag ${VERSION} points at the built commit ${head_commit:0:12}"

# The tag must be resolvable ON Forgejo — a purely local tag proves nothing.
REMOTE_LIST="$(mktemp)"
trap 'rm -f "${REMOTE_LIST}"' EXIT
git -C "${RELEASE_DIR}" ls-remote "${FORGEJO_URL}" \
    "refs/tags/${VERSION}" "refs/tags/${VERSION}^{}" >"${REMOTE_LIST}" 2>/dev/null \
    || die "provenance: cannot reach Forgejo at ${FORGEJO_URL}"
raw_sha="" peeled_sha=""
while read -r sha ref; do
    [ -n "$sha" ] || continue
    case "$ref" in
        "refs/tags/${VERSION}^{}") peeled_sha="$sha" ;;
        "refs/tags/${VERSION}")    raw_sha="$sha" ;;
    esac
done <"${REMOTE_LIST}"
if [ -n "${peeled_sha}" ]; then
    remote_commit="$peeled_sha"    # annotated tag: the peeled entry is the commit
elif [ -n "${raw_sha}" ]; then
    remote_commit="$raw_sha"       # lightweight tag: the entry IS the commit
else
    die "provenance: tag ${VERSION} is not on Forgejo (${FORGEJO_URL}) — push it before publishing"
fi
[ "${remote_commit}" = "${head_commit}" ] \
    || die "provenance: Forgejo's ${VERSION} points at ${remote_commit}, not the built commit ${head_commit}"
pass "provenance: Forgejo resolves ${VERSION} to the built commit"

# ---------------------------------------------------------------------------
# Phase 2: every architecture's artifact exists
# ---------------------------------------------------------------------------
for artifact in "${ARTIFACTS[@]}"; do
    [ -f "${RELEASE_DIR}/${artifact}" ] \
        || die "artifact: ${artifact} is missing — every supported architecture must be built before publishing"
    pass "artifact: ${artifact}"
done

# ---------------------------------------------------------------------------
# Phase 3: static validation, per architecture
# ---------------------------------------------------------------------------
for artifact in "${ARTIFACTS[@]}"; do
    bash "${STATIC_CHECK}" "${RELEASE_DIR}/${artifact}" \
        || die "static: ${artifact} failed scripts/verify-release-static.sh — refusing to publish a non-static artifact"
    pass "static: ${artifact} passes the zero-runtime-dependency checks"
done

# ---------------------------------------------------------------------------
# Phase 4: sidecar validation — checked, never generated
# ---------------------------------------------------------------------------
for artifact in "${ARTIFACTS[@]}"; do
    sidecar="${RELEASE_DIR}/${artifact}.sha256"
    [ -f "${sidecar}" ] || die "sidecar: ${sidecar} is missing — every artifact must ship an immutable digest sidecar"

    read -r digest name extra <"${sidecar}" || true
    [ -n "${digest:-}" ] && [ -n "${name:-}" ] && [ -z "${extra:-}" ] \
        || die "sidecar: ${sidecar} is not one '<digest>  <artifact>' line in sha256sum -c format"
    [[ "${digest}" =~ ^[0-9a-f]{64}$ ]] \
        || die "sidecar: ${sidecar} does not carry a 64-hex lowercase sha256 digest (got: ${digest})"
    [ "${name}" = "${artifact}" ] \
        || die "sidecar: ${sidecar} names '${name}', not ${artifact} — install.sh runs sha256sum -c from the download dir and would fail"
    actual="$(sha256sum "${RELEASE_DIR}/${artifact}" | awk '{print $1}')"
    [ "${digest}" = "${actual}" ] \
        || die "sidecar: ${sidecar} digest ${digest} != actual artifact digest ${actual}"
    pass "sidecar: ${artifact}.sha256 matches the artifact digest"
done

# ---------------------------------------------------------------------------
# Publish — every check above has passed
# ---------------------------------------------------------------------------
if [ "${DRY_RUN}" = 1 ]; then
    echo ""
    echo "dry-run OK: all four phases passed; no gh call was made."
    exit 0
fi

command -v gh >/dev/null 2>&1 || die "publish: gh (GitHub CLI) is required"
echo ""
echo "Publishing ${VERSION} to ${GH_REPO}..."
gh release create "${VERSION}" \
    --repo "${GH_REPO}" \
    --title "Claude Governor ${VERSION}" \
    --notes "Release ${VERSION}

Built from Forgejo commit: ${head_commit} (tag ${VERSION}).

Assets: one statically-linked binary per architecture, each with its
.sha256 digest sidecar (verified by install.sh before it installs)." \
    "${RELEASE_DIR}/cgov-linux-amd64" \
    "${RELEASE_DIR}/cgov-linux-amd64.sha256" \
    "${RELEASE_DIR}/cgov-linux-arm64" \
    "${RELEASE_DIR}/cgov-linux-arm64.sha256" \
    || die "publish: gh release create failed for ${VERSION}"

pass "publish: release ${VERSION} created"

# Post-publish: the release's own asset list must pair every binary with its
# sidecar — a missing upload (network, quota, renames) must fail the CI run
# even though the release now exists.
ASSETS_JSON="$(gh release view "${VERSION}" --repo "${GH_REPO}" --json assets)" \
    || die "post-publish: cannot list assets of ${VERSION}"
mapfile -t asset_names < <(grep -o '"name":"[^"]*"' <<<"${ASSETS_JSON}" | cut -d'"' -f4 | sed '/^$/d')
[ "${#asset_names[@]}" -ge 1 ] || die "post-publish: release ${VERSION} has no assets"

unpaired=0
for name in "${asset_names[@]}"; do
    case "$name" in
        *.sha256)
            peer="${name%.sha256}"
            if ! printf '%s\n' "${asset_names[@]}" | grep -qxF "$peer"; then
                printf 'FAIL: post-publish: sidecar %s has no %s asset\n' "$name" "$peer"
                unpaired=$((unpaired + 1))
            fi
            ;;
        *)
            if ! printf '%s\n' "${asset_names[@]}" | grep -qxF "${name}.sha256"; then
                printf 'FAIL: post-publish: asset %s has no %s.sha256 sidecar\n' "$name" "$name"
                unpaired=$((unpaired + 1))
            fi
            ;;
    esac
done
[ "$unpaired" -eq 0 ] || die "post-publish: ${unpaired} unpaired asset(s) on ${VERSION} — release is live but incomplete; fix the asset list"

for artifact in "${ARTIFACTS[@]}"; do
    pass "post-publish: ${artifact} and ${artifact}.sha256 are both published"
done

echo ""
echo "publish-release: OK — ${VERSION} published with paired digest sidecars"
