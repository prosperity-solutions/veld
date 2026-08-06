#!/usr/bin/env bash
# Veld installer — detects OS/arch and installs the latest release.
#
# Usage:
#   curl -fsSL https://veld.oss.life.li/get | bash
#
# Options (via env vars):
#   VELD_VERSION=1.0.0    Install a specific version (default: latest)
#   VELD_INSTALL_DIR=$HOME/.local/bin   Where to put the veld binary
#   VELD_DESKTOP=0        Skip Veld Desktop, the macOS app. It is installed by
#                         default — the app and the CLI are two halves of one
#                         release — so this is the opt-out for a CI box or a
#                         server that wants no Dock icon.
#   VELD_DESKTOP_DIR=/Applications   Where the app goes. When set it is the ONLY
#                         location consulted, for installs and for finding an
#                         existing one.
#   VELD_DESKTOP_WAIT_PID=<pid>   Wait for that process to exit before replacing
#                         the app — how Veld Desktop updates itself (it hands off
#                         to this script and quits).
#   VELD_DESKTOP_RELAUNCH=1   Reopen the app afterwards.
#
# Related (read by `veld setup`, not this script):
#   VELD_ALLOW_UNMANAGED_HELPER=1   Let setup direct-spawn the helper when
#                                   service registration fails (containers/CI
#                                   without launchd/systemd). Unmanaged helpers
#                                   do not survive reboots or binary updates.

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
HAVE_CHECKSUMS=""
if curl -fSL -o "${TMP_DIR}/checksums.txt" "$CHECKSUMS_URL" 2>/dev/null; then
  HAVE_CHECKSUMS="1"
else
  echo "Warning: checksums.txt not available, skipping verification"
fi

# Verify a downloaded release asset against checksums.txt.
# $1 = path to the downloaded file, $2 = its name as published on the release.
#
# A missing checksums.txt or a missing entry is a warning rather than an error,
# matching how this script has always treated the tarball: an old release that
# predates a given asset must still be installable. A checksum that is present
# and *wrong* is always fatal.
verify_checksum() {
  local file="$1" name="$2" expected actual
  [ -n "$HAVE_CHECKSUMS" ] || return 0

  # Exact field match, not `grep -F " ${name}"`: every asset with a sibling whose
  # name merely *starts* the same — `…zip` and `…zip.blockmap`, both published for
  # the desktop app — would otherwise match twice and compare a two-line "hash"
  # against one, failing every verification. checksums.txt is `sha256sum` output,
  # so field 1 is the hash and field 2 the name.
  expected="$(awk -v n="$name" '$2 == n { print $1; exit }' "${TMP_DIR}/checksums.txt")"
  if [ -z "$expected" ]; then
    echo "Warning: checksum for ${name} not found in checksums.txt, skipping verification"
    return 0
  fi

  echo "Verifying checksum..."
  if [ "$OS" = "macos" ]; then
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  else
    actual="$(sha256sum "$file" | awk '{print $1}')"
  fi

  if [ "$expected" != "$actual" ]; then
    echo "Error: checksum verification failed for ${name}"
    echo "  Expected: ${expected}"
    echo "  Actual:   ${actual}"
    exit 1
  fi
  echo "Checksum verified."
}

verify_checksum "${TMP_DIR}/${TARBALL}" "$TARBALL"

# --- Extract ---

echo "Extracting..."
# Verify tarball only contains expected files before extracting.
EXPECTED_BINS="veld veld-helper veld-daemon"
TAR_CONTENTS="$(tar -tzf "${TMP_DIR}/${TARBALL}")"
for entry in $TAR_CONTENTS; do
  entry="${entry#./}"
  case "$entry" in
    # Binaries, plus the attribution files release tarballs carry since v10.6.x
    # (release.yml copies LICENSE + THIRD-PARTY-LICENSES.md into dist/). New
    # non-binary entries must be added here or `veld update` refuses the tarball.
    veld|veld-helper|veld-daemon|caddy|LICENSE|THIRD-PARTY-LICENSES.md|"") ;;
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
#
# Running environments are intentionally NOT stopped for an update. State lives
# in a single SQLite DB with a forward-only migration system, so swapping the
# binaries no longer risks the stale/incompatible state files that the old
# JSON-per-run storage did. Service processes are independent of the CLI,
# daemon, and helper (the helper leaves Caddy running across its own restart, so
# URLs stay up), and they pick up the new orchestrator on the next
# `veld start`/`veld restart`.

echo "Installing binaries..."
$NEED_SUDO mkdir -p "$INSTALL_DIR"
$NEED_SUDO mkdir -p "$LIB_DIR"

# Install a binary and, on macOS, immediately clear its extended attributes and
# re-sign it BEFORE moving on to the next file.
#
# Downloaded binaries carry com.apple.quarantine / com.apple.provenance; on
# macOS Sequoia (15+) an unsigned/adhoc binary can be SIGKILLed by Gatekeeper on
# launch. Signing inline (rather than in a separate pass after all copies) keeps
# the unsigned window to milliseconds — critical because veld-helper is now
# relaunched automatically when its binary changes (launchd WatchPaths + the
# helper's own binary-change watcher), and either could otherwise relaunch a
# freshly-copied-but-not-yet-signed binary into a Gatekeeper kill/throttle loop.
# $1 = source, $2 = destination, $3 = "sign" (macOS re-sign) | "nosign".
install_bin() {
  $NEED_SUDO cp "$1" "$2"
  $NEED_SUDO chmod +x "$2"
  if [ "$OS" = "macos" ] && [ "$3" = "sign" ]; then
    $NEED_SUDO xattr -cr "$2" 2>/dev/null || true
    $NEED_SUDO codesign --force --sign - "$2" 2>/dev/null || true
  fi
}

# veld CLI goes to INSTALL_DIR (on PATH).
install_bin "${TMP_DIR}/veld" "${INSTALL_DIR}/veld" sign

# Helper and daemon go to LIB_DIR (bundled in the release tarball) and are
# re-signed. Caddy is a Go binary shipped signed upstream, so it is not re-signed.
for bin in veld-helper veld-daemon; do
  if [ -f "${TMP_DIR}/${bin}" ]; then
    install_bin "${TMP_DIR}/${bin}" "${LIB_DIR}/${bin}" sign
  fi
done
if [ -f "${TMP_DIR}/caddy" ]; then
  install_bin "${TMP_DIR}/caddy" "${LIB_DIR}/caddy" nosign
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

# Restart a user LaunchAgent so it runs the binary just installed.
#
# Deliberately NOT `bootout` followed by `bootstrap`. `bootout` returns before
# launchd has finished tearing the job down, and a `bootstrap` into that window
# fails with exit 5 — which this script swallows, leaving NO service registered
# and, for the daemon, nothing running at all. `veld setup` hit the same race and
# fixed it by waiting for the teardown to drain (`wait_for_launchd_job_removal`
# in crates/veld-core/src/setup.rs); this path never got the fix, which is why an
# update usually ended with a dead daemon and a manual `veld setup` to revive it.
#
# A job that is already registered does not need re-registering: only `veld setup`
# writes these plists, so an update changes the binary and not the definition
# launchd holds. The consequence is worth stating rather than discovering: because
# this no longer re-bootstraps, an update is no longer a point where a plist file
# that is newer than launchd's registration converges. `veld setup` is now the only
# one — which is where such a mismatch comes from in the first place (a bootstrap
# that lost the race and fell back to kickstarting the stale registration, the
# `BootstrapOutcome::KickstartedStale` warning), and that warning already tells the
# user to re-run setup.
#
# Signalling it to exit is enough, because `KeepAlive` is
# unconditionally true in both — measured, not assumed: a `KeepAlive=true` agent
# with `RunAtLoad=false` is started by launchd anyway, so a registered veld job is
# always running or on its way back.
#
# SIGTERM rather than `kickstart -k`'s SIGKILL so each service runs its own
# shutdown path: the helper exits while leaving Caddy up (every live URL survives
# the swap), and the daemon deregisters its route, records the terminal sessions it
# is leaving behind and removes its socket. It does *not* drain in-flight requests —
# `main.rs` aborts its tasks — so this is about veld's own cleanup, not about
# request draining.
# $1 = launchd label, $2 = plist path, $3 = display name
restart_launch_agent() {
  local label="$1" plist="$2" name="$3" target
  target="gui/$(id -u)/${label}"
  if launchctl print "$target" >/dev/null 2>&1; then
    echo "Restarting ${name} service..."
    launchctl kill TERM "$target" 2>/dev/null || true
    # Belt, not redundancy: it makes this restart correct *without* depending on a
    # `KeepAlive` that lives in a different file. A job launchd is not currently
    # running has nothing to signal (`kill` exits 3, "No process to signal"), and a
    # later plist edit — a `SuccessfulExit` condition, say — would silently turn
    # this whole function back into the dead-daemon-after-update it exists to fix.
    # Verified no-op when the job is healthy: `kickstart` without `-k` on a running
    # job exits 0 and leaves the same pid.
    launchctl kickstart "$target" 2>/dev/null || true
  else
    # Nothing registered to signal, and a bootstrap here has no teardown to race.
    echo "Loading ${name} service..."
    launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null || true
  fi
}

if [ "$OS" = "macos" ]; then
  if [ -n "$PRIVILEGED_MODE" ] && [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    # Privileged mode (staying in place): helper runs as a system LaunchDaemon
    # (root). Restarting it in the system domain requires root — the old code
    # ran `$NEED_SUDO launchctl ... system/...` with NEED_SUDO empty for the
    # default user-path install, so it silently failed and left a stale helper.
    # If passwordless sudo is available, restart it now for an immediate swap;
    # otherwise the helper restarts itself when it detects its binary changed
    # (in-process watcher + the plist's WatchPaths) — no password prompt.
    #
    # Use a graceful SIGTERM (`launchctl kill TERM`), NOT `kickstart -k`: the
    # helper handles SIGTERM by exiting while leaving Caddy running, and the
    # plist's unconditional KeepAlive relaunches it onto the new binary — so
    # every live URL stays up across the swap. A hard `kickstart -k` (SIGKILL,
    # possibly escalating to launchd job teardown) is riskier for the child
    # Caddy; the helper also spawns Caddy in its own process group as a second
    # safeguard (see veld-helper caddy.rs). `veld update`'s own post-install
    # restart uses this same graceful path.
    HELPER_PLIST="/Library/LaunchDaemons/dev.veld.helper.plist"
    if [ -f "$HELPER_PLIST" ]; then
      if sudo -n true 2>/dev/null; then
        echo "Restarting veld-helper service (privileged)..."
        sudo launchctl kill TERM system/dev.veld.helper 2>/dev/null || true
      else
        echo "Privileged veld-helper will restart itself to pick up the new binary (no sudo needed)."
      fi
    fi
  elif [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    # User mode: helper runs as a user LaunchAgent.
    HELPER_PLIST="$HOME/Library/LaunchAgents/dev.veld.helper.plist"
    if [ -f "$HELPER_PLIST" ]; then
      restart_launch_agent dev.veld.helper "$HELPER_PLIST" veld-helper
    fi
  fi

  DAEMON_PLIST="$HOME/Library/LaunchAgents/dev.veld.daemon.plist"
  if [ -f "$DAEMON_PLIST" ]; then
    restart_launch_agent dev.veld.daemon "$DAEMON_PLIST" veld-daemon
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
    # Ensure KillMode=process before the restart below, on units written by an
    # older veld. Terminal sessions now live in holder processes that are children
    # of the daemon (`veld-daemon --pty-holder`), and systemd's default
    # KillMode=control-group SIGKILLs every one of them on `systemctl restart` —
    # i.e. an update would end the shells the holders exist to keep alive. Only
    # `veld setup` writes the unit, and an update deliberately does not run setup,
    # so without this an existing install never gets the setting.
    DAEMON_UNIT="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/veld-daemon.service"
    if [ -f "$DAEMON_UNIT" ] && ! grep -q '^KillMode=' "$DAEMON_UNIT"; then
      echo "Updating veld-daemon service (KillMode=process)..."
      # Inserted under [Service]: appending would land in [Install], where
      # systemd ignores it.
      sed -i.veld-bak 's/^\[Service\]$/[Service]\nKillMode=process/' "$DAEMON_UNIT" 2>/dev/null || true
      rm -f "$DAEMON_UNIT.veld-bak"
      # sed exits 0 when it substitutes nothing, so verify rather than trust: a
      # [Service] line with trailing whitespace, a CRLF unit or a hand-edited
      # header would otherwise leave the file untouched while this script had
      # already announced success and was about to restart the daemon — SIGKILLing
      # every holder it was trying to protect.
      if grep -q '^KillMode=process$' "$DAEMON_UNIT"; then
        systemctl --user daemon-reload 2>/dev/null || true
      else
        echo "  Warning: could not add KillMode=process to $DAEMON_UNIT."
        echo "           Open terminals in Veld Desktop will not survive this restart."
        echo "           Run 'veld setup' to rewrite the service file."
      fi
    fi
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

# --- Veld Desktop (macOS) ---
#
# Why the CLI installs the app at all: a build downloaded in a browser carries
# `com.apple.quarantine`, which is what makes Gatekeeper refuse the first launch
# of a build that is not notarized. curl does not set that attribute, so an app
# delivered through this script simply opens — no dialog, no Developer ID needed.
# The trust boundary is unchanged: you are already running this script from the
# same origin, and the archive is checksum-verified like the tarball above.
#
# Deliberately NOT re-signed after unpacking, unlike the binaries: the bundle
# arrives with a valid signature of its own, and `codesign --force --sign -`
# would replace a real Developer ID signature with an ad-hoc one the day releases
# are signed. Clearing quarantine is all that is needed.
DESKTOP_APP=""
if [ "$OS" = "macos" ]; then
  # Where an existing install would be. VELD_DESKTOP_DIR, when set, is the *only*
  # place looked at — it names where this machine keeps apps, so falling back to
  # /Applications after it would install somewhere the caller explicitly did not
  # ask for. Otherwise /Applications wins over ~/Applications, and whichever
  # exists is what gets updated.
  if [ -n "${VELD_DESKTOP_DIR:-}" ]; then
    if [ -d "${VELD_DESKTOP_DIR}/Veld.app" ]; then
      DESKTOP_APP="${VELD_DESKTOP_DIR}/Veld.app"
    fi
  else
    for candidate in "/Applications/Veld.app" "$HOME/Applications/Veld.app"; do
      if [ -d "$candidate" ]; then
        DESKTOP_APP="$candidate"
        break
      fi
    done
  fi

  # Default ON: the app is half of veld, not an add-on, and the two ship from one
  # tag with one version — so an install brings both and an update moves both.
  # `VELD_DESKTOP=0` is the opt-out, for a CI box or a server that wants the CLI
  # and nothing with a Dock icon.
  WANT_DESKTOP="1"
  case "${VELD_DESKTOP:-}" in
    0|false|no) WANT_DESKTOP="" ;;
  esac
fi

if [ -n "${WANT_DESKTOP:-}" ]; then
  case "$ARCH" in
    arm64) DESKTOP_ARCH="arm64" ;;
    amd64) DESKTOP_ARCH="x64" ;;   # electron-builder spells it x64, not amd64
    *)     DESKTOP_ARCH="" ;;
  esac

  if [ -z "$DESKTOP_ARCH" ]; then
    echo "Warning: no Veld Desktop build for ${ARCH}, skipping"
  else
    DESKTOP_ZIP="veld-desktop-${VERSION}-mac-${DESKTOP_ARCH}.zip"
    DESKTOP_URL="https://github.com/${REPO}/releases/download/${TAG}/${DESKTOP_ZIP}"
    DESKTOP_DEST="${DESKTOP_APP:-${VELD_DESKTOP_DIR:-/Applications}/Veld.app}"

    # Fall back to a per-user location rather than asking for sudo: the app is
    # not a system component and nothing else needs to read it.
    if [ -z "$DESKTOP_APP" ] && [ -z "${VELD_DESKTOP_DIR:-}" ] && [ ! -w "/Applications" ]; then
      DESKTOP_DEST="$HOME/Applications/Veld.app"
    fi
    mkdir -p "$(dirname "$DESKTOP_DEST")"

    echo ""
    echo "Installing Veld Desktop ${VERSION}..."

    # The app that is being replaced must not be running: an Electron app reads
    # from its own bundle while it runs (asar, framework dylibs), so swapping the
    # directory under a live process is how you get a half-broken window rather
    # than an updated one. Veld Desktop's own updater passes its pid here and
    # quits, which is what makes that handoff safe.
    if [ -n "${VELD_DESKTOP_WAIT_PID:-}" ]; then
      echo "Waiting for Veld Desktop (pid ${VELD_DESKTOP_WAIT_PID}) to quit..."
      waited=0
      while kill -0 "${VELD_DESKTOP_WAIT_PID}" 2>/dev/null; do
        sleep 0.2
        waited=$((waited + 1))
        if [ "$waited" -gt 150 ]; then   # 30s
          echo "Error: Veld Desktop did not quit within 30s, leaving it alone"
          exit 1
        fi
      done
    fi

    if pgrep -f "${DESKTOP_DEST}/Contents/MacOS/" >/dev/null 2>&1; then
      echo "Veld Desktop is running — skipping the app update."
      echo "  Quit it and re-run, or use the app's own 'Check for Updates…'."
    else
      echo "Downloading ${DESKTOP_URL}..."
      if ! curl -fSL -o "${TMP_DIR}/${DESKTOP_ZIP}" "$DESKTOP_URL"; then
        echo "Warning: could not download ${DESKTOP_ZIP}, skipping the app"
      else
        verify_checksum "${TMP_DIR}/${DESKTOP_ZIP}" "$DESKTOP_ZIP"

        # `ditto -x -k`, not `unzip`: it is the tool that preserves the symlinks,
        # permissions and metadata an .app bundle's code signature is sealed over.
        rm -rf "${TMP_DIR}/desktop"
        ditto -x -k "${TMP_DIR}/${DESKTOP_ZIP}" "${TMP_DIR}/desktop"

        NEW_APP="${TMP_DIR}/desktop/Veld.app"
        NEW_ID="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "${NEW_APP}/Contents/Info.plist" 2>/dev/null || true)"
        if [ "$NEW_ID" != "dev.veld.desktop" ]; then
          # Same reasoning as the tarball's contents check above: refuse to install
          # something that is not the thing this script claims to install.
          echo "Error: ${DESKTOP_ZIP} does not contain Veld.app (bundle id: ${NEW_ID:-none})"
          exit 1
        fi

        # Quarantine is set by whatever downloads a file; curl does not set it,
        # but a bundle extracted from an archive that was ever handled by a browser
        # can carry it, so clear it rather than assume.
        xattr -dr com.apple.quarantine "$NEW_APP" 2>/dev/null || true

        # Move the old one aside instead of deleting it, so a failed copy leaves a
        # working app rather than a gap.
        rm -rf "${DESKTOP_DEST}.old"
        if [ -d "$DESKTOP_DEST" ]; then
          mv "$DESKTOP_DEST" "${DESKTOP_DEST}.old"
        fi
        if ditto "$NEW_APP" "$DESKTOP_DEST"; then
          rm -rf "${DESKTOP_DEST}.old"
          DESKTOP_APP="$DESKTOP_DEST"
          echo "Veld Desktop installed to ${DESKTOP_DEST}"
          if [ -n "${VELD_DESKTOP_RELAUNCH:-}" ]; then
            open "$DESKTOP_DEST" 2>/dev/null || true
          fi
        else
          echo "Error: could not install Veld Desktop to ${DESKTOP_DEST}"
          if [ -d "${DESKTOP_DEST}.old" ]; then
            mv "${DESKTOP_DEST}.old" "$DESKTOP_DEST"
          fi
          exit 1
        fi
      fi
    fi
  fi
fi

# --- Next steps (no auto-run of veld setup) ---

echo ""
echo "Run 'veld start' in any project to get going."
echo "Run 'veld setup' for more options."
if [ "$OS" = "macos" ] && [ -z "$DESKTOP_APP" ]; then
  echo "Run 'veld desktop install' for the Mac app."
fi

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
# An `[ … ] && echo` here would be the script's last command, so a machine with no
# app installed would exit non-zero — and `veld update` reads that exit code.
if [ -n "$DESKTOP_APP" ]; then
  echo "  Veld Desktop:  ${DESKTOP_APP}"
fi
