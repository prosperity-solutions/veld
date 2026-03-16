#!/usr/bin/env bash
# Veld installer — detects OS/arch and installs the latest release.
#
# Usage:
#   curl -fsSL https://veld.oss.life.li/get | bash
#
# Options (via env vars):
#   VELD_VERSION=1.0.0    Install a specific version (default: latest)
#   VELD_INSTALL_DIR=$HOME/.local/bin   Where to put the veld binary

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
  TAG="$(curl -fsSL -H "Accept: application/json" "https://api.github.com/repos/${REPO}/releases/latest" | grep -o '"tag_name": *"[^"]*"' | cut -d'"' -f4)"
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
if curl -fSL -o "${TMP_DIR}/checksums.txt" "$CHECKSUMS_URL" 2>/dev/null; then
  EXPECTED_HASH="$(grep -F " ${TARBALL}" "${TMP_DIR}/checksums.txt" | awk '{print $1}')"

  if [ -n "$EXPECTED_HASH" ]; then
    echo "Verifying checksum..."
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
  else
    echo "Warning: checksum for ${TARBALL} not found in checksums.txt, skipping verification"
  fi
else
  echo "Warning: checksums.txt not available, skipping verification"
fi

# --- Extract ---

echo "Extracting..."
# Verify tarball only contains expected files before extracting.
EXPECTED_BINS="veld veld-helper veld-daemon"
TAR_CONTENTS="$(tar -tzf "${TMP_DIR}/${TARBALL}")"
for entry in $TAR_CONTENTS; do
  entry="${entry#./}"
  case "$entry" in
    veld|veld-helper|veld-daemon|"") ;;
    *) echo "Error: unexpected file in tarball: ${entry}"; exit 1 ;;
  esac
done
tar xzf "${TMP_DIR}/${TARBALL}" -C "$TMP_DIR"

# --- Determine install directories ---

# Default to user-level paths (no sudo required).
NEED_SUDO=""

EXISTING_VELD="$(command -v veld 2>/dev/null || true)"
if [ -n "$EXISTING_VELD" ] && [ -z "${VELD_INSTALL_DIR:-}" ]; then
  EXISTING_DIR="$(dirname "$EXISTING_VELD")"
  # If the existing install is under /usr/local, ask for sudo to update there.
  case "$EXISTING_DIR" in
    /usr/local/*)
      echo "Existing veld found at ${EXISTING_VELD} (system path)."
      if sudo -n true 2>/dev/null; then
        NEED_SUDO="sudo"
        INSTALL_DIR="$EXISTING_DIR"
      else
        printf "Sudo is needed to update ${EXISTING_DIR}. Grant access? [Y/n] "
        read -r answer < /dev/tty 2>/dev/null || answer="n"
        answer="${answer:-y}"
        if [ "$answer" = "y" ] || [ "$answer" = "Y" ]; then
          if sudo true </dev/tty; then
            NEED_SUDO="sudo"
            INSTALL_DIR="$EXISTING_DIR"
          else
            echo "Sudo failed. Installing to user paths instead."
            INSTALL_DIR="${VELD_INSTALL_DIR:-$HOME/.local/bin}"
          fi
        else
          echo "Installing to user paths instead."
          INSTALL_DIR="${VELD_INSTALL_DIR:-$HOME/.local/bin}"
        fi
      fi
      ;;
    *)
      INSTALL_DIR="$EXISTING_DIR"
      echo "Existing veld found at ${EXISTING_VELD}, updating in place."
      ;;
  esac
else
  INSTALL_DIR="${VELD_INSTALL_DIR:-$HOME/.local/bin}"
fi

# Determine lib directory based on install dir.
if [[ "$INSTALL_DIR" == /usr/local/* ]] || [[ "$INSTALL_DIR" == /usr/* ]]; then
  LIB_DIR="/usr/local/lib/veld"
else
  LIB_DIR="$HOME/.local/lib/veld"
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

# --- Download Caddy with replace-response plugin ---
echo "Installing Caddy..."
if [ ! -f "${LIB_DIR}/caddy" ]; then
  CADDY_OS="$OS"
  [ "$CADDY_OS" = "macos" ] && CADDY_OS="darwin"
  CADDY_URL="https://caddyserver.com/api/download?os=${CADDY_OS}&arch=${ARCH}&p=github.com/caddyserver/replace-response"
  curl -fSL -o "${TMP_DIR}/caddy" "$CADDY_URL"
  $NEED_SUDO cp "${TMP_DIR}/caddy" "${LIB_DIR}/caddy"
  $NEED_SUDO chmod +x "${LIB_DIR}/caddy"

  # macOS: clear xattrs and re-sign
  if [ "$OS" = "macos" ]; then
    $NEED_SUDO xattr -cr "${LIB_DIR}/caddy" 2>/dev/null || true
    $NEED_SUDO codesign --force --sign - "${LIB_DIR}/caddy" 2>/dev/null || true
  fi
  echo "Caddy installed."
else
  echo "Caddy already installed."
fi

# --- Restart running services (picks up new binaries) ---

# Detect install mode from setup.json to determine how to restart the helper.
SETUP_JSON="$HOME/.veld/setup.json"
PRIVILEGED_MODE=""
if [ -f "$SETUP_JSON" ]; then
  if grep -q '"mode"' "$SETUP_JSON" 2>/dev/null; then
    MODE_VALUE="$(grep -o '"mode" *: *"[^"]*"' "$SETUP_JSON" | cut -d'"' -f4)"
    if [ "$MODE_VALUE" = "privileged" ]; then
      PRIVILEGED_MODE="1"
    fi
  fi
fi

if [ "$OS" = "macos" ]; then
  if [ -n "$PRIVILEGED_MODE" ]; then
    # Privileged mode: helper runs as a system LaunchDaemon.
    HELPER_PLIST="/Library/LaunchDaemons/dev.veld.helper.plist"
    if [ -f "$HELPER_PLIST" ]; then
      echo "Restarting veld-helper service (privileged)..."
      sudo launchctl bootout system/dev.veld.helper 2>/dev/null || true
      sudo launchctl bootstrap system "$HELPER_PLIST" 2>/dev/null || true
    fi
  else
    # User mode: helper runs as a user LaunchAgent.
    HELPER_PLIST="$HOME/Library/LaunchAgents/dev.veld.helper.plist"
    if [ -f "$HELPER_PLIST" ]; then
      echo "Restarting veld-helper service..."
      launchctl bootout "gui/$(id -u)/dev.veld.helper" 2>/dev/null || true
      launchctl bootstrap "gui/$(id -u)" "$HELPER_PLIST" 2>/dev/null || true
    fi
  fi

  DAEMON_PLIST="$HOME/Library/LaunchAgents/dev.veld.daemon.plist"
  if [ -f "$DAEMON_PLIST" ]; then
    echo "Restarting veld-daemon service..."
    launchctl bootout "gui/$(id -u)/dev.veld.daemon" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$DAEMON_PLIST" 2>/dev/null || true
  fi
else
  # Linux: restart systemd services if they exist.
  if [ -n "$PRIVILEGED_MODE" ]; then
    if systemctl is-active --quiet veld-helper 2>/dev/null; then
      echo "Restarting veld-helper service (privileged)..."
      sudo systemctl restart veld-helper 2>/dev/null || true
    fi
  else
    if systemctl --user is-active --quiet veld-helper 2>/dev/null; then
      echo "Restarting veld-helper service..."
      systemctl --user restart veld-helper 2>/dev/null || true
    fi
  fi
  if systemctl --user is-active --quiet veld-daemon 2>/dev/null; then
    echo "Restarting veld-daemon service..."
    systemctl --user restart veld-daemon 2>/dev/null || true
  fi
fi

# --- macOS: clear extended attributes and re-sign binaries ---
#
# Downloaded binaries carry com.apple.quarantine and com.apple.provenance
# attributes. On macOS Sequoia (15+), provenance alone can cause Gatekeeper
# to SIGKILL unsigned/adhoc-signed binaries. Clearing all xattrs and
# re-signing locally makes macOS treat them as trusted.

if [ "$OS" = "macos" ]; then
  echo "Clearing macOS extended attributes and re-signing binaries..."
  $NEED_SUDO xattr -cr "${INSTALL_DIR}/veld" 2>/dev/null || true
  $NEED_SUDO codesign --force --sign - "${INSTALL_DIR}/veld" 2>/dev/null || true
  for bin in veld-helper veld-daemon; do
    if [ -f "${LIB_DIR}/${bin}" ]; then
      $NEED_SUDO xattr -cr "${LIB_DIR}/${bin}" 2>/dev/null || true
      $NEED_SUDO codesign --force --sign - "${LIB_DIR}/${bin}" 2>/dev/null || true
    fi
  done
fi

# --- Clean up stale binaries from alternate install locations ---
#
# Previous installs may have placed binaries in a different location.
# Remove stale copies so `veld version` doesn't pick them up.

# Stale user-level binaries when installing to system paths.
if [ "$LIB_DIR" != "$HOME/.local/lib/veld" ] && [ -d "$HOME/.local/lib/veld" ]; then
  echo "Removing stale binaries from $HOME/.local/lib/veld/..."
  for bin in veld-helper veld-daemon caddy; do
    rm -f "$HOME/.local/lib/veld/$bin" 2>/dev/null || true
  done
  rmdir "$HOME/.local/lib/veld" 2>/dev/null || true
fi
if [ "$INSTALL_DIR" != "$HOME/.local/bin" ] && [ -f "$HOME/.local/bin/veld" ]; then
  echo "Removing stale veld binary from $HOME/.local/bin/..."
  rm -f "$HOME/.local/bin/veld" 2>/dev/null || true
fi

# Stale system-level binaries when installing to user paths.
if [ "$LIB_DIR" != "/usr/local/lib/veld" ] && [ -d "/usr/local/lib/veld" ]; then
  echo "Removing stale binaries from /usr/local/lib/veld/..."
  for bin in veld-helper veld-daemon caddy; do
    if [ -n "$NEED_SUDO" ]; then
      $NEED_SUDO rm -f "/usr/local/lib/veld/$bin" 2>/dev/null || true
    elif [ -w "/usr/local/lib/veld" ] 2>/dev/null; then
      rm -f "/usr/local/lib/veld/$bin" 2>/dev/null || true
    fi
  done
  if [ -n "$NEED_SUDO" ]; then
    $NEED_SUDO rmdir "/usr/local/lib/veld" 2>/dev/null || true
  elif [ -w "/usr/local/lib/veld" ] 2>/dev/null; then
    rmdir "/usr/local/lib/veld" 2>/dev/null || true
  fi
fi
if [ "$INSTALL_DIR" != "/usr/local/bin" ] && [ -f "/usr/local/bin/veld" ]; then
  echo "Removing stale veld binary from /usr/local/bin/..."
  if [ -n "$NEED_SUDO" ]; then
    $NEED_SUDO rm -f "/usr/local/bin/veld" 2>/dev/null || true
  elif [ -w "/usr/local/bin" ] 2>/dev/null; then
    rm -f "/usr/local/bin/veld" 2>/dev/null || true
  fi
fi

# --- Next steps (no auto-run of veld setup) ---

echo ""
echo "Run 'veld start' in any project to get going."
echo "Run 'veld setup' for more options."

# --- PATH handling ---

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
  echo ""
  echo "Note: ${INSTALL_DIR} is not on your PATH."

  if [ -t 1 ] && [ -z "${VELD_NON_INTERACTIVE:-}" ]; then
    # Interactive: offer to add to shell rc
    SHELL_NAME="$(basename "$SHELL")"
    case "$SHELL_NAME" in
      zsh)  RC_FILE="$HOME/.zshrc" ;;
      bash) RC_FILE="$HOME/.bashrc" ;;
      fish) RC_FILE="$HOME/.config/fish/config.fish" ;;
      *)    RC_FILE="" ;;
    esac

    if [ -n "$RC_FILE" ]; then
      printf "Add it automatically to ${RC_FILE}? [Y/n] "
      read -r answer < /dev/tty 2>/dev/null || answer="y"
      answer="${answer:-y}"
      if [ "$answer" = "y" ] || [ "$answer" = "Y" ]; then
        if [ "$SHELL_NAME" = "fish" ]; then
          echo "fish_add_path $INSTALL_DIR" >> "$RC_FILE"
        else
          echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "$RC_FILE"
        fi
        echo "Added to ${RC_FILE}. Restart your shell or run: source ${RC_FILE}"
      fi
    else
      echo "Add this to your shell configuration:"
      echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    fi
  else
    echo "Add ${INSTALL_DIR} to your PATH:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
  fi
fi

# --- Print success ---

echo ""
echo "veld ${VERSION} installed successfully!"
echo ""
echo "  veld binary:   ${INSTALL_DIR}/veld"
echo "  veld-helper:   ${LIB_DIR}/veld-helper"
echo "  veld-daemon:   ${LIB_DIR}/veld-daemon"
echo "  caddy:         ${LIB_DIR}/caddy"
