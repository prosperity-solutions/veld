#!/usr/bin/env bash
# Veld installer — detects OS/arch and installs the latest release.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/prosperity-solutions/veld/main/install.sh | bash
#
# Options (via env vars):
#   VELD_VERSION=1.0.0    Install a specific version (default: latest)
#   VELD_INSTALL_DIR=/usr/local/bin   Where to put the veld binary (default: /usr/local/bin)

set -euo pipefail

REPO="prosperity-solutions/veld"
INSTALL_DIR="${VELD_INSTALL_DIR:-/usr/local/bin}"
LIB_DIR="/usr/local/lib/veld"

# --- Detect platform ---

detect_os() {
  case "$(uname -s)" in
    Darwin) echo "macos" ;;
    Linux)  echo "linux" ;;
    *)      echo "unsupported"; return 1 ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64)   echo "amd64" ;;
    arm64|aarch64)   echo "arm64" ;;
    *)               echo "unsupported"; return 1 ;;
  esac
}

OS="$(detect_os)"
ARCH="$(detect_arch)"
SUFFIX="${OS}-${ARCH}"

echo "Detected platform: ${SUFFIX}"

# --- Resolve version ---

if [ -n "${VELD_VERSION:-}" ]; then
  VERSION="$VELD_VERSION"
  TAG="v${VERSION}"
else
  echo "Fetching latest release..."
  TAG="$(curl -fsSL -H "Accept: application/json" "https://api.github.com/repos/${REPO}/releases/latest" | grep -o '"tag_name":"[^"]*"' | cut -d'"' -f4)"
  VERSION="${TAG#v}"
fi

if [ -z "$VERSION" ]; then
  echo "Error: could not determine version"
  exit 1
fi

echo "Installing veld ${VERSION}..."

# --- Download and extract ---

TARBALL="veld-${VERSION}-${SUFFIX}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${TARBALL}"
TMP_DIR="$(mktemp -d)"

cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

echo "Downloading ${URL}..."
curl -fSL -o "${TMP_DIR}/${TARBALL}" "$URL"

echo "Extracting..."
tar xzf "${TMP_DIR}/${TARBALL}" -C "$TMP_DIR"

# --- Install ---

# Check if we need sudo
NEED_SUDO=""
if [ ! -w "$INSTALL_DIR" ] 2>/dev/null; then
  NEED_SUDO="sudo"
fi

echo "Installing binaries..."
$NEED_SUDO mkdir -p "$INSTALL_DIR"
$NEED_SUDO mkdir -p "$LIB_DIR"

# veld CLI goes to INSTALL_DIR (on PATH)
$NEED_SUDO cp "${TMP_DIR}/veld" "${INSTALL_DIR}/veld"
$NEED_SUDO chmod +x "${INSTALL_DIR}/veld"

# Helper and daemon go to LIB_DIR
for bin in veld-helper veld-daemon; do
  if [ -f "${TMP_DIR}/${bin}" ]; then
    $NEED_SUDO cp "${TMP_DIR}/${bin}" "${LIB_DIR}/${bin}"
    $NEED_SUDO chmod +x "${LIB_DIR}/${bin}"
  fi
done

# --- macOS: remove quarantine attribute ---

if [ "$OS" = "macos" ]; then
  echo "Removing macOS quarantine attribute..."
  $NEED_SUDO xattr -dr com.apple.quarantine "${INSTALL_DIR}/veld" 2>/dev/null || true
  for bin in veld-helper veld-daemon; do
    if [ -f "${LIB_DIR}/${bin}" ]; then
      $NEED_SUDO xattr -dr com.apple.quarantine "${LIB_DIR}/${bin}" 2>/dev/null || true
    fi
  done
fi

echo ""
echo "veld ${VERSION} installed successfully!"
echo ""
echo "  veld binary:   ${INSTALL_DIR}/veld"
echo "  veld-helper:   ${LIB_DIR}/veld-helper"
echo "  veld-daemon:   ${LIB_DIR}/veld-daemon"
echo ""
echo "Run 'veld setup' to complete one-time system configuration."
