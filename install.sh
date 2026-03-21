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
SWITCHING_TO_USER_PATHS=""  # set to "1" when downgrading from system install
if [ -n "$EXISTING_VELD" ] && [ -z "${VELD_INSTALL_DIR:-}" ]; then
  EXISTING_DIR="$(dirname "$EXISTING_VELD")"
  case "$EXISTING_DIR" in
    /usr/local/*)
      if [ -n "${VELD_NON_INTERACTIVE:-}" ]; then
        # Non-interactive mode (e.g. called from `veld update`).
        # Try passwordless sudo; if unavailable, FAIL rather than silently
        # moving binaries (which would break a privileged LaunchDaemon that
        # still references /usr/local paths).
        echo "Existing veld found at ${EXISTING_VELD} (system path)."
        if sudo -n true 2>/dev/null; then
          echo "Sudo available — updating in place."
          NEED_SUDO="sudo"
          INSTALL_DIR="$EXISTING_DIR"
        else
          echo ""
          echo "============================================================"
          echo "  SUDO REQUIRED"
          echo "============================================================"
          echo ""
          echo "  Your veld binary is installed in a system path:"
          echo "    ${EXISTING_VELD}"
          echo ""
          echo "  Updating requires administrator (sudo) access, but sudo"
          echo "  is not available in non-interactive mode."
          echo ""
          echo "  To update, run the installer directly:"
          echo "    curl -fsSL https://veld.oss.life.li/get | bash"
          echo ""
          echo "============================================================"
          exit 1
        fi
      else
        # Interactive mode — show the full choice.
        echo ""
        echo "============================================================"
        echo "  EXISTING SYSTEM-LEVEL INSTALLATION DETECTED"
        echo "============================================================"
        echo ""
        echo "  Your current veld binary is installed at:"
        echo "    ${EXISTING_VELD}"
        echo ""
        echo "  Because this is a system path (/usr/local/...), updating"
        echo "  the binaries in place requires administrator (sudo) access."
        echo ""
        echo "  You have two options:"
        echo ""
        echo "    [1] Update in place (requires sudo)"
        echo "        Keeps binaries in ${EXISTING_DIR}"
        echo ""
        echo "    [2] Move to user-level install (no sudo needed)"
        echo "        Installs to ~/.local/bin instead. If you are in"
        echo "        privileged mode, you will need to run"
        echo "        'veld setup unprivileged' afterwards."
        echo ""
        echo "============================================================"
        echo ""
        printf "Choose [1] or [2] (default: 1): "
        read -r answer < /dev/tty 2>/dev/null || answer="1"
        answer="${answer:-1}"
        if [ "$answer" = "2" ]; then
          echo "Switching to user-level install (no sudo required)."
          INSTALL_DIR="${VELD_INSTALL_DIR:-$HOME/.local/bin}"
          SWITCHING_TO_USER_PATHS="1"
        else
          echo "Updating in place — sudo is needed to write to ${EXISTING_DIR}."
          if sudo true </dev/tty; then
            NEED_SUDO="sudo"
            INSTALL_DIR="$EXISTING_DIR"
          else
            echo "Sudo failed. Falling back to user-level install."
            INSTALL_DIR="${VELD_INSTALL_DIR:-$HOME/.local/bin}"
            SWITCHING_TO_USER_PATHS="1"
          fi
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

# Helper, daemon, and Caddy go to LIB_DIR (bundled in the release tarball).
for bin in veld-helper veld-daemon caddy; do
  if [ -f "${TMP_DIR}/${bin}" ]; then
    $NEED_SUDO cp "${TMP_DIR}/${bin}" "${LIB_DIR}/${bin}"
    $NEED_SUDO chmod +x "${LIB_DIR}/${bin}"
  fi
done

# --- macOS: clear extended attributes and re-sign binaries ---
#
# Downloaded binaries carry com.apple.quarantine and com.apple.provenance
# attributes. On macOS Sequoia (15+), provenance alone can cause Gatekeeper
# to SIGKILL unsigned/adhoc-signed binaries. Clearing all xattrs and
# re-signing locally makes macOS treat them as trusted.
#
# This MUST happen before restarting services, otherwise Gatekeeper can
# SIGKILL the freshly installed unsigned binaries on launch.

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

# If the user chose to move from system to user paths while in privileged mode,
# stop the system LaunchDaemon and remove the plist so it doesn't try to launch
# a binary that no longer exists. The user must run `veld setup unprivileged`
# to set up user-level services.
if [ -n "$SWITCHING_TO_USER_PATHS" ] && [ -n "$PRIVILEGED_MODE" ]; then
  echo ""
  echo "Stopping privileged system service before switching to user paths..."
  if [ "$OS" = "macos" ]; then
    HELPER_PLIST="/Library/LaunchDaemons/dev.veld.helper.plist"
    if [ -f "$HELPER_PLIST" ]; then
      # Need sudo to stop a system LaunchDaemon — request it for this one-off.
      if sudo -n true 2>/dev/null || sudo true </dev/tty 2>/dev/null; then
        sudo launchctl bootout system/dev.veld.helper 2>/dev/null || true
        sudo rm -f "$HELPER_PLIST" 2>/dev/null || true
        echo "System LaunchDaemon stopped and removed."
      else
        echo "Warning: could not stop system LaunchDaemon (sudo unavailable)."
        echo "  The old service at $HELPER_PLIST may still be running."
        echo "  Stop it manually: sudo launchctl bootout system/dev.veld.helper"
      fi
    fi
  else
    # Linux: stop the system-level systemd service.
    if systemctl is-active --quiet veld-helper 2>/dev/null; then
      if sudo -n true 2>/dev/null || sudo true </dev/tty 2>/dev/null; then
        sudo systemctl stop veld-helper 2>/dev/null || true
        sudo systemctl disable veld-helper 2>/dev/null || true
        echo "System service stopped and disabled."
      else
        echo "Warning: could not stop system veld-helper service (sudo unavailable)."
        echo "  Stop it manually: sudo systemctl stop veld-helper"
      fi
    fi
  fi

  # Clear privileged mode from setup.json so veld doesn't think it's still
  # running in privileged mode.
  if [ -f "$SETUP_JSON" ]; then
    echo "Clearing privileged mode from setup.json..."
    # Simple: overwrite with empty mode. `veld setup unprivileged` will set it properly.
    echo '{}' > "$SETUP_JSON"
  fi

  echo ""
  echo "============================================================"
  echo "  IMPORTANT: Run 'veld setup unprivileged' to set up"
  echo "  user-level services after this install completes."
  echo "============================================================"
  echo ""
fi

if [ "$OS" = "macos" ]; then
  if [ -n "$PRIVILEGED_MODE" ] && [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    # Privileged mode (staying in place): helper runs as a system LaunchDaemon.
    HELPER_PLIST="/Library/LaunchDaemons/dev.veld.helper.plist"
    if [ -f "$HELPER_PLIST" ]; then
      echo "Restarting veld-helper service (privileged)..."
      $NEED_SUDO launchctl bootout system/dev.veld.helper 2>/dev/null || true
      $NEED_SUDO launchctl bootstrap system "$HELPER_PLIST" 2>/dev/null || true
    fi
  elif [ -z "$SWITCHING_TO_USER_PATHS" ]; then
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
  # Linux: restart systemd services if they exist (skip if switching to user paths).
  if [ -n "$PRIVILEGED_MODE" ] && [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    if systemctl is-active --quiet veld-helper 2>/dev/null; then
      echo "Restarting veld-helper service (privileged)..."
      $NEED_SUDO systemctl restart veld-helper 2>/dev/null || true
    fi
  elif [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    if systemctl --user is-active --quiet veld-helper 2>/dev/null; then
      echo "Restarting veld-helper service..."
      systemctl --user restart veld-helper 2>/dev/null || true
    fi
  fi
  if [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    if systemctl --user is-active --quiet veld-daemon 2>/dev/null; then
      echo "Restarting veld-daemon service..."
      systemctl --user restart veld-daemon 2>/dev/null || true
    fi
  fi
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
# When switching from a system install, these are root-owned and need sudo.
if [ "$LIB_DIR" != "/usr/local/lib/veld" ] && [ -d "/usr/local/lib/veld" ]; then
  echo "Removing stale binaries from /usr/local/lib/veld/..."
  if [ -n "$NEED_SUDO" ]; then
    for bin in veld-helper veld-daemon caddy; do
      $NEED_SUDO rm -f "/usr/local/lib/veld/$bin" 2>/dev/null || true
    done
    $NEED_SUDO rmdir "/usr/local/lib/veld" 2>/dev/null || true
  elif [ -w "/usr/local/lib/veld" ]; then
    for bin in veld-helper veld-daemon caddy; do
      rm -f "/usr/local/lib/veld/$bin" 2>/dev/null || true
    done
    rmdir "/usr/local/lib/veld" 2>/dev/null || true
  elif sudo -n true 2>/dev/null || { [ -n "$SWITCHING_TO_USER_PATHS" ] && sudo true </dev/tty 2>/dev/null; }; then
    for bin in veld-helper veld-daemon caddy; do
      sudo rm -f "/usr/local/lib/veld/$bin" 2>/dev/null || true
    done
    sudo rmdir "/usr/local/lib/veld" 2>/dev/null || true
  else
    echo "Warning: cannot remove stale binaries in /usr/local/lib/veld/ (sudo required)."
    echo "  Remove manually: sudo rm -rf /usr/local/lib/veld"
  fi
fi
if [ "$INSTALL_DIR" != "/usr/local/bin" ] && [ -f "/usr/local/bin/veld" ]; then
  echo "Removing stale veld binary from /usr/local/bin/..."
  if [ -n "$NEED_SUDO" ]; then
    $NEED_SUDO rm -f "/usr/local/bin/veld" 2>/dev/null || true
  elif [ -w "/usr/local/bin" ]; then
    rm -f "/usr/local/bin/veld" 2>/dev/null || true
  elif sudo -n true 2>/dev/null || { [ -n "$SWITCHING_TO_USER_PATHS" ] && sudo true </dev/tty 2>/dev/null; }; then
    sudo rm -f "/usr/local/bin/veld" 2>/dev/null || true
  else
    echo "Warning: cannot remove stale /usr/local/bin/veld (sudo required)."
    echo "  Remove manually: sudo rm -f /usr/local/bin/veld"
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
