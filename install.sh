#!/bin/bash
# Claude Governor Installation Script
#
# Provenance
# ----------
# Source of truth:   Forgejo — https://git.ardenone.com/jedarden/claude-governor
# Release artifacts: built by the `cgov-ci` Argo Workflow (iad-ci cluster) from a
#                    fresh Forgejo clone — the v<VERSION> tag is pushed to Forgejo
#                    before the release is cut — then published to GitHub Releases
#                    with one sha256 sidecar per binary.
#
# Forgejo is private, so anonymous installs fetch the artifact from the public
# GitHub release. That makes the mirror's possible lag irrelevant rather than
# trusted: a vX.Y.Z asset and its sidecar digest are immutable once published,
# and this script verifies the digest and only then installs. Nothing is
# written to the install dir until verification passes.
#
# Usage
#   curl -fsSL <raw-url>/install.sh | bash                        # latest, digest-verified
#   curl -fsSL <raw-url>/install.sh | CGOV_VERSION=v0.1.1 bash    # pinned release
#   curl -fsSL <raw-url>/install.sh | CGOV_SHA256=<64-hex> bash   # pinned digest
#
# Flags (each may also be given as an environment variable):
#   --version <vX.Y.Z>    install this release instead of latest    [CGOV_VERSION]
#   --sha256 <64-hex>     require this exact sha256 digest          [CGOV_SHA256]
#   --install-dir <dir>   target directory (default ~/.local/bin)   [CGOV_INSTALL_DIR]

set -euo pipefail

REPO="jedarden/claude-governor"
BINARY_NAME="cgov"
RELEASES_BASE="https://github.com/${REPO}/releases"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}$*${NC}"; }
fail() { echo -e "${RED}$*${NC}" >&2; exit 1; }

usage() {
    sed -n '2,34p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

VERSION="${CGOV_VERSION:-}"
EXPECT_SHA="${CGOV_SHA256:-}"
INSTALL_DIR="${CGOV_INSTALL_DIR:-${HOME}/.local/bin}"

while [ $# -gt 0 ]; do
    case "$1" in
        --version)     [ $# -ge 2 ] || fail "--version requires a value"; VERSION="$2"; shift 2 ;;
        --sha256)      [ $# -ge 2 ] || fail "--sha256 requires a value"; EXPECT_SHA="$2"; shift 2 ;;
        --install-dir) [ $# -ge 2 ] || fail "--install-dir requires a value"; INSTALL_DIR="$2"; shift 2 ;;
        --help|-h)     usage ;;
        *)             fail "Unknown argument: $1 (try --help)" ;;
    esac
done

if [ -n "${EXPECT_SHA}" ] && ! [[ "${EXPECT_SHA}" =~ ^[0-9a-fA-F]{64}$ ]]; then
    fail "CGOV_SHA256 must be a 64-character hex sha256 digest (got: ${EXPECT_SHA})"
fi

# A pinned version must be release-tag shaped; accept 1.2.3 and normalize to v1.2.3.
if [ -n "${VERSION}" ]; then
    case "${VERSION}" in v*) ;; *) VERSION="v${VERSION}" ;; esac
    ASSET_BASE="${RELEASES_BASE}/download/${VERSION}"
else
    ASSET_BASE="${RELEASES_BASE}/latest/download"
fi

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
    Linux*)
        ;;
    Darwin*)
        echo -e "${YELLOW}macOS is not yet supported. Please build from source:${NC}"
        echo "  git clone https://git.ardenone.com/jedarden/claude-governor.git"
        echo "  cd claude-governor && cargo build --release"
        exit 1
        ;;
    *)
        fail "Unsupported OS: ${OS}"
        ;;
esac

case "${ARCH}" in
    x86_64|amd64)
        PLATFORM="linux-amd64"
        ;;
    aarch64|arm64)
        PLATFORM="linux-arm64"
        ;;
    *)
        fail "Unsupported architecture: ${ARCH}
Only x86_64 and aarch64 are currently supported."
        ;;
esac

ARTIFACT="${BINARY_NAME}-${PLATFORM}"

# Resolve which release "latest" actually points at, for a transparent log line.
# Best effort only — the download itself does not depend on this.
RESOLVED_TAG=""
if [ -z "${VERSION}" ]; then
    LOCATION="$(curl -fsSI -o /dev/null -w '%{redirect_url}' "${ASSET_BASE}/${ARTIFACT}" 2>/dev/null || true)"
    RESOLVED_TAG="$(printf '%s' "${LOCATION}" | sed -n 's#.*/releases/[a-z]*/\(v[0-9A-Za-z.-]*\)/.*#\1#p')"
fi

info "Claude Governor Installer"
echo "=========================="
echo "Platform:    ${PLATFORM}"
echo "Release:     ${RESOLVED_TAG:-${VERSION:-latest}}"
echo "Install dir: ${INSTALL_DIR}"
[ -n "${EXPECT_SHA}" ] && echo "Digest pin:  caller-supplied (CGOV_SHA256)"
echo ""

fetch() { # fetch <url> <dest>
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --progress-bar -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --show-progress -O "$2" "$1"
    else
        fail "Error: Neither curl nor wget is available"
    fi
}

# Download into a temp dir and verify there. The install dir is only touched
# after the checksum passes, so a failed/corrupted download can never clobber
# an existing working binary.
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT
ARTIFACT_PATH="${WORK_DIR}/${ARTIFACT}"

echo "Downloading ${ARTIFACT}..."
fetch "${ASSET_BASE}/${ARTIFACT}" "${ARTIFACT_PATH}" || fail "Download failed: ${ASSET_BASE}/${ARTIFACT}"

if [ -n "${EXPECT_SHA}" ]; then
    ACTUAL_SHA="$(sha256sum "${ARTIFACT_PATH}" | awk '{print $1}')"
    if [ "${ACTUAL_SHA}" != "$(printf '%s' "${EXPECT_SHA}" | tr 'A-Z' 'a-z')" ]; then
        fail "Checksum MISMATCH for ${ARTIFACT}
  expected (CGOV_SHA256): ${EXPECT_SHA}
  actual:                 ${ACTUAL_SHA}
Refusing to install. Nothing was written to ${INSTALL_DIR}."
    fi
    echo "Checksum OK (caller-supplied digest): ${ACTUAL_SHA}"
else
    echo "Downloading ${ARTIFACT}.sha256 (published digest)..."
    fetch "${ASSET_BASE}/${ARTIFACT}.sha256" "${ARTIFACT_PATH}.sha256" \
        || fail "Digest sidecar download failed: ${ASSET_BASE}/${ARTIFACT}.sha256
(If this release predates sidecar publication, pin the digest yourself: CGOV_SHA256=<64-hex>)"
    (cd "${WORK_DIR}" && sha256sum -c "${ARTIFACT}.sha256") \
        || fail "Checksum verification FAILED for ${ARTIFACT} — refusing to install.
Nothing was written to ${INSTALL_DIR}."
    echo "Checksum OK (published sidecar): $(awk '{print $1}' "${ARTIFACT_PATH}.sha256")"
fi
echo ""

mkdir -p "${INSTALL_DIR}"
install -m 0755 "${ARTIFACT_PATH}" "${INSTALL_DIR}/${BINARY_NAME}"
BINARY_PATH="${INSTALL_DIR}/${BINARY_NAME}"

info "✓ ${BINARY_NAME} installed to ${BINARY_PATH}"
echo "Installed: $("${BINARY_PATH}" --version 2>/dev/null || echo '(version check failed)')"
echo ""

# Run cgov init if this is a fresh install
if [ -t 1 ] && [ "${CI:-}" != "true" ]; then
    echo "Running cgov init..."
    "${BINARY_PATH}" init
    echo ""
    info "Installation complete!"
    echo ""
    echo "Quickstart:"
    echo "  1. Edit configuration: cgov config --edit"
    echo "  2. Check status:       cgov status"
    echo "  3. Enable services:    cgov enable"
else
    info "Installation complete!"
    echo "Run 'cgov init' to initialize configuration."
fi
