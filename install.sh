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
CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${TAG}/checksums.txt"
TMP_DIR="$(mktemp -d)"

cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

echo "Downloading ${URL}..."
curl -fSL -o "${TMP_DIR}/${TARBALL}" "$URL"

echo "Downloading checksums..."
curl -fSL -o "${TMP_DIR}/checksums.txt" "$CHECKSUMS_URL"

# --- Verify SHA-256 checksum ---

echo "Verifying checksum..."
EXPECTED_HASH="$(grep "${TARBALL}" "${TMP_DIR}/checksums.txt" | awk '{print $1}')"

if [ -z "$EXPECTED_HASH" ]; then
  echo "Error: checksum for ${TARBALL} not found in checksums.txt"
  exit 1
fi

if [ "$OS" = "macos" ]; then
  ACTUAL_HASH="$(shasum -a 256 "${TMP_DIR}/${TARBALL}" | awk '{print $1}')"
else
  ACTUAL_HASH="$(sha256sum "${TMP_DIR}/${TARBALL}" | awk '{print $1}')"
fi

if [ "$EXPECTED_HASH" != "$ACTUAL_HASH" ]; then
  echo "Error: checksum verification failed"
  echo "  Expected: ${EXPECTED_HASH}"
  echo "  Actual:   ${ACTUAL_HASH}"
  exit 1
fi

echo "Checksum verified."

# --- Extract ---

echo "Extracting..."
tar xzf "${TMP_DIR}/${TARBALL}" -C "$TMP_DIR"

# --- Determine install directories ---

INSTALL_DIR="${VELD_INSTALL_DIR:-/usr/local/bin}"
LIB_DIR="/usr/local/lib/veld"
NEED_SUDO=""
USED_FALLBACK=""

if [ ! -w "$INSTALL_DIR" ] 2>/dev/null; then
  if sudo -n true 2>/dev/null; then
    NEED_SUDO="sudo"
  else
    # Fallback to ~/.local/bin and ~/.local/lib/veld
    INSTALL_DIR="$HOME/.local/bin"
    LIB_DIR="$HOME/.local/lib/veld"
    USED_FALLBACK="1"
  fi
fi

# --- Install ---

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

# --- Auto-run veld setup in interactive mode ---

if [ -t 1 ]; then
  echo ""
  echo "Running veld setup..."
  "${INSTALL_DIR}/veld" setup
else
  echo ""
  echo "Non-interactive mode detected — skipping 'veld setup'."
  echo "Run it manually after install:"
  echo "  veld setup"
fi

# --- Print success and next steps ---

echo ""
echo "veld ${VERSION} installed successfully!"
echo ""
echo "  veld binary:   ${INSTALL_DIR}/veld"
echo "  veld-helper:   ${LIB_DIR}/veld-helper"
echo "  veld-daemon:   ${LIB_DIR}/veld-daemon"

if [ -n "$USED_FALLBACK" ]; then
  echo ""
  echo "Note: Installed to ${INSTALL_DIR} because /usr/local/bin is not writable."
  echo "Make sure it is on your PATH:"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

echo ""
if [ -t 1 ]; then
  echo "Next steps:"
  echo "  Run 'veld init' in a project to get started."
else
  echo "Next steps:"
  echo "  1. Run 'veld setup' to complete one-time system configuration."
  echo "  2. Run 'veld init' in a project to get started."
fi
