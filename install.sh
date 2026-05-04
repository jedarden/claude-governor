#!/bin/bash
# Claude Governor Installation Script
# Downloads and installs the cgov binary from GitHub releases

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# GitHub repo settings
REPO="jedarden/claude-governor"
BINARY_NAME="cgov"

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
    Linux*)
        ;;
    Darwin*)
        echo -e "${YELLOW}macOS is not yet supported. Please build from source:${NC}"
        echo "  cargo build --release"
        exit 1
        ;;
    *)
        echo -e "${RED}Unsupported OS: ${OS}${NC}"
        exit 1
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
        echo -e "${RED}Unsupported architecture: ${ARCH}${NC}"
        echo "Only x86_64 and aarch64 are currently supported."
        exit 1
        ;;
esac

# Installation paths
INSTALL_DIR="${HOME}/.local/bin"
BINARY_PATH="${INSTALL_DIR}/${BINARY_NAME}"

# Get latest release download URL
LATEST_URL="https://github.com/${REPO}/releases/latest/download/${BINARY_NAME}-${PLATFORM}"

echo -e "${GREEN}Claude Governor Installer${NC}"
echo "=========================="
echo "Platform: ${PLATFORM}"
echo "Install dir: ${INSTALL_DIR}"
echo ""

# Create install directory if it doesn't exist
if [ ! -d "${INSTALL_DIR}" ]; then
    echo "Creating install directory..."
    mkdir -p "${INSTALL_DIR}"
fi

# Download binary
echo "Downloading ${BINARY_NAME}-${PLATFORM} from GitHub releases..."
if command -v wget >/dev/null 2>&1; then
    wget -q --show-progress -O "${BINARY_PATH}" "${LATEST_URL}"
elif command -v curl >/dev/null 2>&1; then
    curl -fsSL --progress-bar -o "${BINARY_PATH}" "${LATEST_URL}"
else
    echo -e "${RED}Error: Neither wget nor curl is available${NC}"
    exit 1
fi

# Make binary executable
chmod +x "${BINARY_PATH}"

echo -e "${GREEN}✓ Binary installed to ${BINARY_PATH}${NC}"
echo ""

# Run cgov init if this is a fresh install
if [ -t 1 ] && [ "${CI:-}" != "true" ]; then
    echo "Running cgov init..."
    "${BINARY_PATH}" init
    echo ""
    echo -e "${GREEN}Installation complete!${NC}"
    echo ""
    echo "Quickstart:"
    echo "  1. Edit configuration: cgov config --edit"
    echo "  2. Check status:       cgov status"
    echo "  3. Enable services:    cgov enable"
else
    echo -e "${GREEN}Installation complete!${NC}"
    echo "Run 'cgov init' to initialize configuration."
fi
