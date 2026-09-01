#!/usr/bin/env bash
# Veld installer — detects OS/arch and installs the latest release.
#
# Usage:
#   curl -fsSL https://veld.oss.life.li/get | bash
#
# Options (via env vars):
#   VELD_VERSION=1.0.0    Install a specific version (default: latest)
#   VELD_INSTALL_DIR=$HOME/.local/bin   Where to put the veld binary
#   VELD_DESKTOP=0        Skip Veld Desktop, the macOS app, for THIS run. The app
#                         is optional and the answer is remembered in
#                         ~/.veld/desktop.json (see `desktop_preference`), so this
#                         variable is the per-run override for a CI box or a
#                         server that wants no Dock icon — it deliberately does
#                         NOT record a preference, because an environment variable
#                         is a statement about one invocation and a preference is
#                         one the user can be asked about and change.
#   VELD_DESKTOP_ONLY=1   Install ONLY the app: no CLI tarball, no binaries, no
#                         service restarts, no sudo, no PATH edits. macOS only.
#                         This is what `veld desktop install|update` runs, and
#                         what Veld Desktop's own updater ends up in — an app
#                         update has no business bouncing the daemon or asking
#                         for a password.
#   VELD_DESKTOP_DIR=/Applications   Where the app goes. When set it is the ONLY
#                         location consulted, for installs and for finding an
#                         existing one.
#   VELD_DESKTOP_WAIT_PID=<pid>   Wait for that process to exit before replacing
#                         the app — how Veld Desktop updates itself (it hands off
#                         to this script and quits).
#   VELD_BINARY_ICONS=0   Do not give the CLI/daemon/helper the app's icon. They get
#                         it so an authorization prompt raised on their behalf (1Password,
#                         sudo) shows the Veld mark instead of a generic "exec" tile.
#   VELD_EMBEDDED=1       This script is running *inside* another veld command
#                         (`veld update` and friends run it with stdout
#                         inherited). Two consequences: progress chatter, the
#                         next-steps footer and the success banner are suppressed
#                         so the caller owns the output (warnings and errors are
#                         not), and the privileged helper restart is left to the
#                         caller, which can do it without sudo and then verify it
#                         worked. Always set on a CLI-driven run.
#   VELD_VERBOSE=1        Print the chatter anyway. `veld update --verbose` sets
#                         it to get the raw installer stream back for debugging.
#                         Deliberately does NOT hand the privileged helper
#                         restart back to this script — that is VELD_EMBEDDED's
#                         job, and a debug flag must not bounce a root service.
#   VELD_DESKTOP_RELAUNCH=1   Reopen the app afterwards — on EVERY exit path, not
#                         only a successful one. The app quit itself to hand over,
#                         so an unhandled failure would otherwise leave the user
#                         with no window and no message.
#
# Related (read by `veld setup`, not this script):
#   VELD_ALLOW_UNMANAGED_HELPER=1   Let setup direct-spawn the helper when
#                                   service registration fails (containers/CI
#                                   without launchd/systemd). Unmanaged helpers
#                                   do not survive reboots or binary updates.

set -euo pipefail

REPO="prosperity-solutions/veld"

# --- Output mode ---
#
# `veld update` runs this script with VELD_EMBEDDED=1 and inherits its stdout, so
# every line printed here lands in the middle of the CLI's own output. Left
# unchecked that produces one command with three "installed successfully!"
# banners, a first-install footer emitted halfway through an update, and two raw
# curl meters.
#
# In embedded mode the CLI narrates — it is the half that knows the step count,
# both versions, and whether the app is also moving — and this script speaks only
# when something goes wrong. So `say` is for progress chatter and disappears when
# embedded; warnings, errors and anything the user must act on keep using plain
# `echo` and are never suppressed. When in doubt, use `echo`: a swallowed warning
# is a worse bug than a duplicated line.
# Two variables, because they answer two different questions and one of them is
# not about output at all. `VELD_EMBEDDED` says *a veld command owns this run* —
# which decides both that the caller prints the summary and that the caller, not
# this script, restarts the privileged helper (it can do that without sudo and
# then verify it worked). `VELD_VERBOSE` only asks for the chatter back.
#
# Keeping them apart is the point: tying the privileged-restart hand-off to the
# output mode meant `veld update --verbose` silently re-enabled this script's own
# `sudo launchctl kill`, so a debug flag bounced a root service and raced the CLI
# for it.
EMBEDDED=""
case "${VELD_EMBEDDED:-}" in
  1|true|yes) EMBEDDED="1" ;;
esac

VERBOSE=""
case "${VELD_VERBOSE:-}" in
  1|true|yes) VERBOSE="1" ;;
esac

# An `if`, not `[ … ] && [ … ] && QUIET=1`: under `set -e` that compound is the
# statement's exit status, so a standalone install would abort here.
QUIET=""
if [ -n "$EMBEDDED" ] && [ -z "$VERBOSE" ]; then
  QUIET="1"
fi

say() {
  [ -n "$QUIET" ] || echo "$@"
}

# curl's default meter is a 13-column table (`% Xferd`, `Dload`, `Spent`) of which
# one column matters, printed with an all-zeros first row. `--progress-bar` is the
# same information as one self-overwriting line.
#
# Deliberately NOT silenced when embedded, unlike the text above: the app archive
# is ~113 MB and takes about ten seconds, and a step line followed by ten seconds
# of nothing reads as a hang. Progress is the one thing the script can say better
# than the caller, because it is the half holding the socket.
CURL_PROGRESS="--progress-bar"

# Every download here is a GitHub URL, and GitHub has incidents: `429 Too Many
# Requests` on raw.githubusercontent, a 503 on a release asset. Both are gone by
# the next request, and without this a blip in the middle of an install aborts a
# run that had already closed the app and stopped nothing else.
#
# `--retry` alone, not `--retry-all-errors`: curl's own transient set (a timeout, a
# DNS resolution failure, and HTTP 408/429/500/502/503/504) is the right one here,
# whereas retrying *any* error would spend six seconds re-asking for a 404 that will
# stay a 404 — an old release with no app archive is a supported case, not a blip.
# Note what that set excludes, because it decides what the cap below can cost:
# connection-refused (exit 7) is not retried, and neither is a transfer that dies
# part-way (exit 18/56) — measured, one connection, no second attempt.
#
# `--retry-max-time` is not belt-and-braces on `--retry-delay`, it is the only thing
# bounding the wait: **curl obeys a server-sent `Retry-After` in preference to
# `--retry-delay`, and `--max-time` does not cap it.** Measured against a local
# listener on curl 8.7.1 — a `429` carrying `Retry-After: 25` took 75s, not the 6s
# the delay implies, and `Retry-After: 600` was still sleeping past two minutes.
# That header is exactly what a rate-limiting CDN sends, so without a cap an
# incident turns a failed install into a half-hour stall with the app already quit.
#
# 30s, and what that cap does is one sentence: the timer starts before the *first*
# attempt and includes transfer time, so a retry that would begin more than 30s in
# does not happen. Measured — a 503 that takes 5s per attempt gets three retries at
# 30s and none at all at 3s.
#
# For everything this script downloads, that is free. A failure curl retries by
# *status* is a status line, and one arrives as fast for the 113 MB app archive as
# for a 2 KB checksums file, so the retries all begin in the first second. The only
# retryable failure slow enough for the cap to refuse one is a timeout — and how long
# a dead network takes to become a timeout belongs to the kernel's SYN retries rather
# than to curl, so it is not worth a number here. An installer that stops asking a
# network that is not answering is the outcome we want anyway.
#
# Not the exception it looks like: a transfer that dies part-way is exit 18/56, which
# is outside curl's retry set with or without a cap (measured: one connection, no
# second attempt).
CURL_RETRY="--retry 3 --retry-delay 2 --retry-max-time 30"

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

say "Detected platform: ${SUFFIX}"

# --- What this run is allowed to install ---
#
# Resolved once, up front, because the two answers select entirely different
# halves of this script rather than toggling a step inside one of them.

# Whether the app half is *allowed* on this run — not whether it is wanted. That
# second question is `desktop_preference`'s, asked and remembered further down,
# and the two are kept apart deliberately: `VELD_DESKTOP=0` is a per-invocation
# override for a CI box or a server, and an override must not silently become a
# recorded answer the user is never asked about again.
WANT_DESKTOP="1"
case "${VELD_DESKTOP:-}" in
  0|false|no) WANT_DESKTOP="" ;;
esac
[ "$OS" = "macos" ] || WANT_DESKTOP=""

# App-only. Everything between here and the desktop section — the tarball, the
# sudo negotiation, the service restarts, the stale-binary sweep, the PATH
# advice — is skipped rather than made conditional, which is the point: an app
# update that cannot reach any of that code cannot restart a daemon, prompt for
# a password, or install a second CLI somewhere the caller never asked for.
DESKTOP_ONLY=""
case "${VELD_DESKTOP_ONLY:-}" in
  1|true|yes) DESKTOP_ONLY="1" ;;
esac

if [ -n "$DESKTOP_ONLY" ]; then
  if [ "$OS" != "macos" ]; then
    echo "Error: VELD_DESKTOP_ONLY is macOS-only (Veld Desktop ships as an AppImage/.deb elsewhere)."
    exit 1
  fi
  if [ -z "$WANT_DESKTOP" ]; then
    # Refusing beats picking a winner: one of the two variables is a mistake,
    # and installing either the app or nothing would be wrong half the time.
    echo "Error: VELD_DESKTOP_ONLY=1 and VELD_DESKTOP=0 contradict each other."
    exit 1
  fi
fi

# --- Resolve version ---

if [ -n "${VELD_VERSION:-}" ]; then
  VERSION="$VELD_VERSION"
  TAG="v${VERSION}"
else
  say "Fetching latest release..."
  # `if ! TAG=…` rather than a bare assignment, and the construct is the whole
  # point: under `set -euo pipefail` a failing command substitution aborts the
  # script *inside* the assignment, so the `[ -z "$VERSION" ]` check below was
  # unreachable on every path that actually fails — a 429, a 503, a DNS error.
  # The user got curl's one-line stderr and a silent exit. `if !` suspends
  # `errexit` for this command, which is what lets the error message run.
  #
  # `-f` plus `pipefail` also means a 200 whose body has no `tag_name` lands here
  # (grep exits 1). That case is why the message below opens cause-neutrally: GitHub
  # *was* reached, so a first line asserting it was not would be a false statement
  # about the one case curl said nothing about.
  if ! TAG="$(curl -fsSL $CURL_RETRY -H "Accept: application/json" "https://api.github.com/repos/${REPO}/releases/latest" | grep -o '"tag_name": *"[^"]*"' | cut -d'"' -f4)"; then
    TAG=""
  fi
  VERSION="${TAG#v}"
fi

if [ -z "$VERSION" ]; then
  # A neutral first line and the causes underneath it as possibilities, because
  # three unrelated failures land here and the script cannot tell which: GitHub
  # answered an error, nothing answered at all, or something answered `200` with a
  # body that has no `tag_name`. curl printed its own reason above for the first
  # two and *nothing* for the third, so the lines below have to stand on their own.
  #
  # The offline case is listed because curl retries a DNS failure — measured, four
  # attempts and 6.1s with the flags above — so a laptop with no network reaches
  # this after a pause long enough to look like GitHub being slow, and telling it
  # to wait for GitHub to recover would send it in the wrong direction.
  echo "Error: could not determine the latest version from the GitHub API."
  echo "  If GitHub is having an incident (https://www.githubstatus.com/), try again later."
  echo "  If this machine is offline or behind a proxy, that is the more likely cause."
  echo "  To skip this lookup entirely, pin a version: VELD_VERSION=x.y.z"
  exit 1
fi

if [ -n "$DESKTOP_ONLY" ]; then
  # No mention of VELD_DESKTOP_ONLY: nobody typed that variable — `veld desktop
  # install`, `veld update` and the app's own updater all set it — so naming it
  # reads as a debug print rather than an explanation.
  say "Installing Veld Desktop ${VERSION} (the CLI is left alone)..."
else
  say "Installing veld ${VERSION}..."
fi

# --- Working directory, and the state the exit path has to undo ---

TARBALL="veld-${VERSION}-${SUFFIX}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${TARBALL}"
CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${TAG}/checksums.txt"
TMP_DIR="$(mktemp -d)"

# Set by the desktop section as it goes, and read only by `cleanup`. Declared
# here because `cleanup` is installed as the EXIT trap before any of them
# exists, and an unset variable under `set -u` in a trap is a confusing way to
# die during someone else's failure.
DESKTOP_APP=""           # the installed bundle, once there is one
DESKTOP_LOCK_DIR=""      # held lock, removed on the way out
DESKTOP_SWAP_DEST=""     # bundle being replaced
DESKTOP_SWAP_BACKUP=""   # its `.old` copy, while the swap is in flight
DESKTOP_RELAUNCH_PATH="" # bundle to reopen when VELD_DESKTOP_RELAUNCH is set

# Runs on success, on failure, and on Ctrl-C — the three ways an app update can
# end with the app not on screen.
cleanup() {
  rm -rf "$TMP_DIR"

  # An interrupted swap leaves the bundle moved aside; auto-mode keys off "a
  # directory exists there", so nothing would ever put it back. Restore it
  # before anything else, because the relaunch below depends on it.
  #
  # Note what is NOT guarded on here: whether the destination is missing. It
  # very often is not. Ctrl-C during a multi-hundred-megabyte `ditto` kills the
  # copy partway, so what sits at the destination is *half a bundle* — and an
  # earlier version of this guard (`[ ! -e "$DESKTOP_SWAP_DEST" ]`) therefore
  # refused to restore in exactly the interruption it was written for, leaving
  # the user a broken app, reopening it, and letting the next run's
  # `rm -rf "${dest}.old"` destroy the only intact copy. These two variables are
  # non-empty *only* while a swap is in flight — the success path clears them
  # before removing the backup — so anything at the destination here is rubble.
  if [ -n "$DESKTOP_SWAP_BACKUP" ] && [ -d "$DESKTOP_SWAP_BACKUP" ]; then
    echo "Restoring the previous Veld Desktop from ${DESKTOP_SWAP_BACKUP}..."
    rm -rf "$DESKTOP_SWAP_DEST"
    mv "$DESKTOP_SWAP_BACKUP" "$DESKTOP_SWAP_DEST" 2>/dev/null || true
  fi

  if [ -n "$DESKTOP_LOCK_DIR" ]; then
    rm -rf "$DESKTOP_LOCK_DIR" 2>/dev/null || true
  fi

  # The app handed its own update to this script and quit. Whatever happened
  # since, it has to come back — `open` on an app that is already running just
  # focuses it, so this is safe on the success path too.
  if [ -n "${VELD_DESKTOP_RELAUNCH:-}" ] && [ -n "$DESKTOP_RELAUNCH_PATH" ] && [ -d "$DESKTOP_RELAUNCH_PATH" ]; then
    open "$DESKTOP_RELAUNCH_PATH" 2>/dev/null || true
  fi
}
trap cleanup EXIT
# `exit` runs the EXIT trap; a bare signal would not, and Ctrl-C during the swap
# is exactly when the restore above matters most.
trap 'exit 130' INT
trap 'exit 143' TERM

say "Downloading checksums..."
HAVE_CHECKSUMS=""
if curl -fSL $CURL_RETRY -o "${TMP_DIR}/checksums.txt" "$CHECKSUMS_URL" 2>/dev/null; then
  HAVE_CHECKSUMS="1"
else
  echo "Warning: checksums.txt not available, skipping verification"
fi

# Verify a downloaded release asset against checksums.txt.
# $1 = path to the downloaded file, $2 = its name as published on the release,
# $3 = "required" to treat an absent hash as failure (default: warn and pass).
#
# The default is fail-open on purpose, and only for the tarball: an old release
# that predates a given asset must still be installable, and that is how this
# script has always treated it. The app archive passes "required" instead —
# every release that publishes one publishes its hash, and this is the path that
# writes an executable bundle into /Applications. A checksum that is present and
# *wrong* is fatal either way.
#
# Returns non-zero rather than exiting, so the desktop section can fail without
# taking the rest of the installer with it.
verify_checksum() {
  local file="$1" name="$2" required="${3:-}" expected actual
  if [ -z "$HAVE_CHECKSUMS" ]; then
    if [ "$required" = "required" ]; then
      echo "Error: checksums.txt is not available, refusing to install ${name} unverified"
      return 1
    fi
    return 0
  fi

  # Exact field match, not `grep -F " ${name}"`: every asset with a sibling whose
  # name merely *starts* the same — `…zip` and `…zip.blockmap`, both published for
  # the desktop app — would otherwise match twice and compare a two-line "hash"
  # against one, failing every verification. checksums.txt is `sha256sum` output,
  # so field 1 is the hash and field 2 the name.
  expected="$(awk -v n="$name" '$2 == n { print $1; exit }' "${TMP_DIR}/checksums.txt")"
  if [ -z "$expected" ]; then
    if [ "$required" = "required" ]; then
      echo "Error: no checksum for ${name} in checksums.txt, refusing to install it unverified"
      return 1
    fi
    echo "Warning: checksum for ${name} not found in checksums.txt, skipping verification"
    return 0
  fi

  say "Verifying checksum..."
  if [ "$OS" = "macos" ]; then
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  else
    actual="$(sha256sum "$file" | awk '{print $1}')"
  fi

  if [ "$expected" != "$actual" ]; then
    echo "Error: checksum verification failed for ${name}"
    echo "  Expected: ${expected}"
    echo "  Actual:   ${actual}"
    return 1
  fi
  say "Checksum verified."
}

# --- Veld Desktop (macOS) ---
#
# Why the CLI installs the app at all: a build downloaded in a browser carries
# `com.apple.quarantine`, which is what makes Gatekeeper refuse the first launch
# of a build that is not notarized. curl does not set that attribute, so an app
# delivered through this script simply opens — no dialog, no Developer ID needed.
# The trust boundary is unchanged: you are already running this script from the
# same origin, and the archive is checksum-verified like the tarball.
#
# Note what that means for the archive: the no-dialog property comes from curl
# never *setting* the flag, so nothing here strips one. `xattr -dr
# com.apple.quarantine` would only ever fire on a bundle that arrived some other
# way — the one case where the flag is doing its job — and it also removes
# XProtect assessment and Apple's revocation check permanently, including for the
# signed build this is a bridge to.
#
# Deliberately NOT re-signed after unpacking, unlike the binaries: the bundle
# arrives with a valid signature of its own, and `codesign --force --sign -`
# would replace a real Developer ID signature with an ad-hoc one the day releases
# are signed.

# Where an existing Veld.app is, if there is one. Sets DESKTOP_APP (empty when
# there is none).
#
# VELD_DESKTOP_DIR, when set, is the *only* place looked at — it names where this
# machine keeps apps, so falling back to /Applications after it would install
# somewhere the caller explicitly did not ask for. Veld Desktop passes its own
# bundle's directory here when it hands over an update, so the copy that gets
# replaced is the one the user launched. Otherwise /Applications wins over
# ~/Applications, and whichever exists is what gets updated.
#
# Separate from `install_desktop_app` because two other things need the answer
# even when the app half does not run: the closing "Run 'veld desktop install'"
# hint, and the icon step below.
find_desktop_app() {
  DESKTOP_APP=""
  if [ -n "${VELD_DESKTOP_DIR:-}" ]; then
    if [ -d "${VELD_DESKTOP_DIR}/Veld.app" ]; then
      DESKTOP_APP="${VELD_DESKTOP_DIR}/Veld.app"
    fi
    return 0
  fi
  for candidate in "/Applications/Veld.app" "$HOME/Applications/Veld.app"; do
    if [ -d "$candidate" ]; then
      DESKTOP_APP="$candidate"
      return 0
    fi
  done
}

# --- Does this machine want the app at all? ---
#
# `~/.veld/desktop.json` is the recorded answer, shared with the CLI
# (`veld_core::desktop_pref` — read that module for why it is a file and not the
# database). Three states, and the third one is the reason this exists: `yes`,
# `no`, and *nothing recorded*, i.e. nobody has ever been asked. Every user who
# installed veld before this release is in that third state with the app already
# on their disk, having never chosen it.
#
# Echoes `yes`, `no`, or nothing. Never fails the script: an unreadable or
# half-written file means "not answered", which costs one prompt rather than
# deciding a ~113 MB download from a corrupt byte.
#
# The whitespace strip is what lets Rust pretty-print the file some day without
# breaking this: `{ "wanted": true }` collapses to the literal matched below.
# What it does *not* survive is a renamed or nested key — see
# `crates/veld-core/tests/install_script_contract.rs`, which runs this very
# function against a file the Rust half wrote.
#
# **Anchored at `{`, not floating.** An unanchored `*'"wanted":true'*` matched the
# literal *anywhere*, including inside another key's string value — so a
# hand-written `{"wanted":false,"note":"\"wanted\":true"}` read as **yes** here
# and as **unwanted** in `desktop_pref::read_in`, a two-parser disagreement on the
# file that gates a 113 MB download and, on one path, a deletion. Anchoring costs
# one thing and it is the right thing to pay: a file whose *first* key is not
# `wanted` reads as unanswered here while Rust still reads the answer. Nothing
# writes that shape, and the divergence only ever asks the question again — which
# is the one direction a parser mismatch is allowed to fail in.
desktop_preference() {
  local file="$HOME/.veld/desktop.json" body
  [ -f "$file" ] || return 0
  # `2>/dev/null` **before** the input redirection: bash applies redirections
  # left to right, so with `< "$file" 2>/dev/null` the failure to open the file is
  # reported through the still-unredirected stderr — an unreadable `desktop.json`
  # (root-owned by an old `sudo curl | bash`, the exact case the CLI's own error
  # message anticipates) printed a raw "Permission denied" into the middle of an
  # install that then carried on silently. Measured both orders.
  body="$(tr -d ' \t\r\n' 2>/dev/null < "$file")" || return 0
  case "$body" in
    '{"wanted":true'*)  echo "yes" ;;
    '{"wanted":false'*) echo "no" ;;
  esac
}

# Persist the answer. Canonical single-line form, matching what
# `veld_core::desktop_pref::write_in` produces.
record_desktop_preference() {
  local value
  case "$1" in
    yes) value="true" ;;
    no)  value="false" ;;
    *)   return 1 ;;
  esac
  mkdir -p "$HOME/.veld" || return 1
  printf '{"wanted":%s}\n' "$value" > "$HOME/.veld/desktop.json"
}

# Can this run put a question to a human?
#
# `curl … | bash` leaves stdin bound to the *script*, so every prompt in this
# file reads `/dev/tty` instead — and the test has to match: `[ -t 0 ]` is false
# on the standard install path, which is precisely the path that must be able to
# ask. `VELD_NON_INTERACTIVE=1` is the caller's way of saying "nobody is
# watching" and is set by every veld-driven run, so a `veld update` never reaches
# a prompt in here (the CLI owns that question — see `plan_desktop` in
# `crates/veld/src/commands/update.rs`).
#
# **Both output descriptors, and the conjunction is the point.**
# `ask_desktop_preference` writes the prompt to stderr — it has to, since its
# stdout is captured by the caller's `$( )` — while every *consequence* of the
# answer ("Removed …", the failure warning, the recovery instruction) is an
# ordinary `echo` to stdout. So the two descriptors answer two halves of one
# question, and testing either alone breaks the other half:
#
# - `[ -t 1 ]` alone (the original) put an **invisible** question up under
#   `curl … | bash 2>err.log` and then took the default from a user who saw
#   nothing.
# - `[ -t 2 ]` alone (the first fix) let `curl … | bash >install.log` ask on the
#   terminal, delete a bundle, and file the outcome in the log — and it bought
#   that case nothing, since the whole script's output was going to the log
#   anyway.
#
# One token restores the coupling the first version had by accident. The rule is
# that a question may only be asked when the human can see it *and* see what it
# did.
desktop_can_ask() {
  [ -z "${VELD_NON_INTERACTIVE:-}" ] || return 1
  [ -r /dev/tty ] || return 1
  [ -t 1 ] || return 1
  [ -t 2 ] || return 1
}

# Ask, once, and record the answer. Echoes `yes`/`no`; empty when nobody
# answered, which stays "not recorded" rather than becoming a decision.
#
# Three wordings, because these are three different questions. Someone who has
# been running veld for months already *has* the app — earlier releases brought it
# along with the CLI without asking — so their prompt has to say so, and say what
# "no" will do, before it can be a fair question. An existing veld user *without*
# the app is being offered a download they have so far managed without. And a
# genuinely fresh install is choosing what veld is for them in the first place, so
# it gets the fullest description. (The Rust sibling `ask_desktop_choice` has two:
# it is never the thing a first-time user meets.)
#
# The default (bare Enter) is whatever the machine already looks like: keep an app
# that is there, do not fetch one that is not — except on a first install, where
# there is no status quo and the answer that matches every release before this one
# is yes.
ask_desktop_preference() {
  local answer default prompt
  echo "" >&2
  if [ -n "$DESKTOP_APP" ]; then
    default="y"
    prompt="Keep Veld Desktop? [Y/n] "
    echo "Veld Desktop is already installed at ${DESKTOP_APP}." >&2
    echo "  It is the Mac app for Veld's IDE — worktree tabs, terminal panes and browser" >&2
    echo "  panes in one window. Earlier releases installed it alongside the CLI without" >&2
    echo "  asking; from now on it is your choice, so this is the one time we ask." >&2
    echo "" >&2
    echo "  Yes — veld keeps it up to date on every update, exactly as it does today." >&2
    echo "  No  — veld removes it and never downloads it again." >&2
  elif [ -n "${EXISTING_VELD:-}" ]; then
    default="n"
    prompt="Install Veld Desktop? [y/N] "
    echo "Veld Desktop — the Mac app for Veld's IDE — is not installed on this machine." >&2
    echo "  Worktree tabs, terminal panes and browser panes in one window, over the same" >&2
    echo "  daemon the CLI drives. It is a ~113 MB download, kept in step by veld update." >&2
  else
    default="y"
    prompt="Install Veld Desktop? [Y/n] "
    echo "Veld Desktop is the Mac app for Veld's IDE — worktree tabs, terminal panes and" >&2
    echo "  browser panes in one window, over the same daemon the CLI drives." >&2
    echo "  The CLI does everything without it; the app is a window onto it." >&2
    echo "  It is a ~113 MB download, kept in step by veld update." >&2
  fi
  echo "" >&2
  printf '%s' "$prompt" >&2
  # A failed `read` is EOF — a closed or redirected tty — and is *not* the
  # default. An answer nobody gave must not be recorded as a preference, which is
  # why this is an `if` and not `read … || answer="$default"`: the latter would
  # have a broken pipe opt a machine into a GUI app, or out of one.
  if ! read -r answer < /dev/tty 2>/dev/null; then
    echo "" >&2
    return 0
  fi
  # A bare Enter *is* the default: `read` succeeded, the user chose the offer.
  answer="${answer:-$default}"
  # **The answer is echoed whether or not it could be persisted.** It used to be
  # `record_desktop_preference yes && echo "yes"`, which coupled acting on the
  # answer to storing it and was wrong twice over on an unwritable `~/.veld` (a
  # root-owned `desktop.json` from an old `sudo curl | bash`, a full disk): the
  # caller saw an empty answer, so a user who typed *n* got the app installed
  # anyway and a user who typed *y* was told "nobody to ask" — and, because a
  # failing `&&` chain is this function's exit status, `set -e` aborted the whole
  # installer at the assignment, after the binaries were in place and before the
  # PATH advice and the summary. The Rust half already states the rule
  # (`remember` in `crates/veld/src/commands/desktop.rs`): a caller that cannot
  # persist the choice still has to act on the answer it was just given, and the
  # cost of the failed write is being asked once more.
  case "$(printf '%s' "$answer" | tr '[:upper:]' '[:lower:]')" in
    y|yes) record_desktop_preference yes || warn_unrecorded; echo "yes" ;;
    n|no)  record_desktop_preference no  || warn_unrecorded; echo "no" ;;
    # Anything else is not an answer. Left unrecorded so the next run asks
    # again, rather than reading a typo as a decision about a GUI app.
    *) ;;
  esac
  # Explicit, so this function's status can never be a `case` arm's. Nothing
  # above may fail the installer.
  return 0
}

# Said out loud rather than swallowed: "I answered this last week" is otherwise a
# mystery when the next run asks again.
warn_unrecorded() {
  echo "Warning: could not record your answer in ${HOME}/.veld/desktop.json — you may be asked again." >&2
}

# Bring the machine in line with a "no" the user gave **on this run**, while the
# app was installed.
#
# Delegated to the CLI rather than done with an `rm -rf` here, and that is the
# whole reason this function is short: deleting an app bundle is the one
# destructive act in this script, `veld desktop uninstall` already has to do it
# (it is the command a user runs by hand), and one implementation that quits the
# running app before unlinking its bundle is worth more than two that race it.
#
# Two ways the subcommand can be missing from the binary this run just installed,
# and neither is exotic. `VELD_VERSION` can pin a release older than the feature —
# and, more likely in practice, **this script is served from `main`** (see the
# header), so between merging and the next release the newest release itself has
# no `veld desktop uninstall`. Both surface as clap's exit 2, which is why every
# failure here is a warning with a manual instruction rather than an abort, and
# why the caller does not promise anything about future updates until this has
# actually succeeded.
remove_desktop_app_via_cli() {
  local cli="${INSTALL_DIR}/veld" rc=0 err=""
  [ -x "$cli" ] || return 1
  # **stderr is captured; stdout is discarded unconditionally.** Both halves are
  # deliberate and the second is easy to misread, so it is stated: `>/dev/null`
  # applies on every exit path, so the child's own `println!` lines — its
  # "Removed <path>" success line and its dim recovery tip — never appear. That is
  # fine only because this caller re-states both in its own words; anything new
  # added to `uninstall()`'s *stdout* will not surface here.
  #
  # The captured stderr is re-printed for the failures where it is the only
  # explanation: "Veld Desktop did not quit … it may be showing a dialog" and
  # "could not remove <path>: Permission denied". Discarding all of it — the first
  # version of this — left the caller's "run 'veld desktop uninstall' to finish"
  # pointing at a command that would fail the same way with no reason given.
  #
  # Two exit codes are filtered instead, because for both this function has a
  # better explanation of its own: **2** is clap's usage dump from a binary too
  # old to have the subcommand, and **75** is EX_TEMPFAIL from the update gate,
  # answered below. Reprinting either puts veld's two lines in front of ours.
  err="$("$cli" desktop uninstall --yes 2>&1 >/dev/null)" || rc=$?
  if [ "$rc" -ne 0 ] && [ "$rc" -ne 2 ] && [ "$rc" -ne 75 ] && [ -n "$err" ]; then
    printf '%s\n' "$err" >&2
  fi
  # An `if`, not `[ … ] && return 0`: under `set -e` that compound *is* the
  # statement's exit status, so the non-zero case would abort the installer if
  # this function were ever called outside an `if`. Same hazard this file's
  # QUIET/`say` block already documents.
  if [ "$rc" -eq 0 ]; then
    return 0
  fi
  # 75 is EX_TEMPFAIL: `veld desktop uninstall` is on the list of commands an
  # in-flight `veld update` refuses (`command_survives_an_update` in
  # crates/veld/src/main.rs), because it would delete the bundle that update is
  # swapping. Named separately because it is the one failure here that is neither
  # the user's problem nor permanent.
  if [ "$rc" -eq 75 ]; then
    echo "  Another veld update is running, so the app was left in place."
    echo "  Run 'veld desktop uninstall' once it has finished."
  fi
  return 1
}

# Give the CLI, daemon and helper the app's icon.
#
# Why this is not cosmetic: an authorization prompt raised on behalf of a bare
# Mach-O executable — 1Password's "Allow veld-daemon to get CLI access", and any
# other consent sheet that shows the requesting process — renders the generic
# "exec" tile, which tells the user nothing about who is asking. A user is being
# asked to approve access to their secrets by something they cannot identify.
# A custom icon is what turns that into the Veld mark.
#
# The source is the *installed app's* own .icns, not a new release asset: it
# cannot drift from the app's icon, it needs no change to the release pipeline,
# and it is absent exactly where it does not matter — a CI box or a server that
# set VELD_DESKTOP=0 has no GUI to show a prompt in.
#
# The cost, measured rather than assumed: a custom icon is stored in
# `com.apple.ResourceFork` + `com.apple.FinderInfo`, and `codesign --verify
# --strict` then rejects the binary ("resource fork, Finder information, or
# similar detritus not allowed", exit 1). Plain `codesign --verify` still passes,
# the ad-hoc signature is intact, and the binaries execute and are spawned by
# launchd normally. Nothing in veld runs a strict verify on its own binaries, and
# Gatekeeper does not assess a locally ad-hoc-signed executable at all — but the
# day these binaries carry a Developer ID, this step is the reason a strict
# verification of them would fail, so it is here in writing. `VELD_BINARY_ICONS=0`
# opts out.
#
# Runs AFTER `install_bin` has signed each binary: `xattr -cr` there would strip
# the icon straight back off. `osascript -l JavaScript` rather than the
# Rez/SetFile dance every recipe for this uses — those are Xcode command-line
# tools, absent on a plain macOS, which is precisely the machine a `curl | bash`
# installer lands on.
apply_binary_icons() {
  local icns="$1" target rc=0
  [ -f "$icns" ] || return 0

  for target in "${INSTALL_DIR}/veld" "${LIB_DIR}/veld-helper" "${LIB_DIR}/veld-daemon"; do
    [ -f "$target" ] || continue
    $NEED_SUDO osascript -l JavaScript \
      -e 'function run(a){ObjC.import("AppKit");var i=$.NSImage.alloc.initWithContentsOfFile(a[0]);if(!i||i.isNil())throw new Error("cannot read "+a[0]);if(!$.NSWorkspace.sharedWorkspace.setIconForFileOptions(i,a[1],0))throw new Error("setIcon refused "+a[1])}' \
      "$icns" "$target" >/dev/null 2>&1 || rc=1
  done
  return "$rc"
}

# The app is installed by two processes that can overlap — `veld update` and the
# app's own updater — and the loser of that race would delete the winner's only
# backup.
#
# In `~/.veld` rather than `${TMPDIR:-/tmp}`. `/tmp` is `drwxrwxrwt`, so any
# other local user can pre-create the lock directory with a pid file naming a
# process of their own that stays alive; the steal branch below only fires on a
# *dead* pid, so every Veld Desktop update on the machine would block for 60s and
# then fail. `TMPDIR` is per-user on macOS and would have been fine — but it is
# not always set, and it is specifically *unset* on the path that matters, since
# the app spawns the CLI with a deliberately small environment. A lock whose
# safety depends on a variable being present is a lock that is unsafe exactly
# when nobody is looking.
#
# Per-user, which is the right grain for `~/Applications` and one grain too fine
# for a shared `/Applications`: two different humans updating the same machine's
# app in the same second is a race this does not close.
desktop_lock() {
  local lock="$HOME/.veld/desktop-install.lock" owner waited=0 stole=""
  # The parent has to exist before a failed `mkdir` can be read as "someone else
  # holds this" rather than "this path is unusable" — an unset-but-nonexistent
  # TMPDIR otherwise sends the steal-and-retry below into a busy loop with no
  # sleep and no timeout, which is exactly what it did the first time it ran.
  mkdir -p "$(dirname "$lock")" || return 1
  while ! mkdir "$lock" 2>/dev/null; do
    if [ ! -d "$lock" ]; then
      echo "Error: cannot create the Veld Desktop install lock at ${lock}"
      return 1
    fi
    owner="$(cat "${lock}/pid" 2>/dev/null || true)"
    # A crashed run leaves the directory behind and its pid does not answer.
    # Stealing is one-shot: if the steal did not win the next `mkdir`, something
    # live is racing for the lock and waiting is the right answer.
    #
    # An *empty* pid file is not the same as a dead one, and treating it as one
    # was a race: `mkdir` succeeds a few instructions before `echo $$ >` runs, so
    # a racer arriving in that window would read no owner, delete a lock that had
    # just been legitimately taken, and run the bundle swap concurrently with its
    # holder — the exact thing this lock exists to prevent, and the first process
    # would then `rm -rf` a lock the second one owned. So an empty pid is only
    # stealable after waiting long enough that "the holder is mid-acquire" is no
    # longer a plausible explanation.
    if [ -z "$stole" ] && { { [ -z "$owner" ] && [ "$waited" -gt 25 ]; } || { [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; }; }; then
      stole="1"
      rm -rf "$lock" 2>/dev/null || true
      continue
    fi
    if [ "$waited" -eq 0 ]; then
      echo "Waiting for another Veld Desktop install (pid ${owner:-unknown}) to finish..."
    fi
    sleep 0.2
    waited=$((waited + 1))
    if [ "$waited" -gt 300 ]; then   # 60s
      echo "Error: another Veld Desktop install is still running (${lock})"
      return 1
    fi
  done
  echo "$$" > "${lock}/pid"
  DESKTOP_LOCK_DIR="$lock"
}

# Install or replace Veld.app. Sets DESKTOP_APP on success.
#
# Returns non-zero instead of exiting, for two reasons: a full install has
# already written the binaries by the time this runs, so aborting here would
# skip the PATH advice and the summary a first-time install needs; and every
# failure has to reach `cleanup`, which restores the previous bundle and reopens
# the app.
install_desktop_app() {
  local desktop_arch zip url dest new_app new_id waited pattern

  case "$ARCH" in
    arm64) desktop_arch="arm64" ;;
    amd64) desktop_arch="x64" ;;   # electron-builder spells it x64, not amd64
    *)
      echo "Warning: no Veld Desktop build for ${ARCH}, skipping"
      return 1
      ;;
  esac

  find_desktop_app

  zip="veld-desktop-${VERSION}-mac-${desktop_arch}.zip"
  url="https://github.com/${REPO}/releases/download/${TAG}/${zip}"
  dest="${DESKTOP_APP:-${VELD_DESKTOP_DIR:-/Applications}/Veld.app}"

  # Fall back to a per-user location rather than asking for sudo: the app is
  # not a system component and nothing else needs to read it.
  if [ -z "$DESKTOP_APP" ] && [ -z "${VELD_DESKTOP_DIR:-}" ] && [ ! -w "/Applications" ]; then
    dest="$HOME/Applications/Veld.app"
  fi

  # `[ -d ]` follows symlinks, so a dev's `ln -s …/dist/mac/Veld.app` reads as an
  # install and the swap would silently replace the link with a real bundle —
  # taking their build out of the loop with no way to notice.
  if [ -L "$dest" ]; then
    echo "Error: ${dest} is a symlink, not an installed bundle — refusing to replace it."
    echo "  Remove the link first, or set VELD_DESKTOP_DIR to install elsewhere."
    return 1
  fi

  # From here on the app must come back on screen whatever happens.
  DESKTOP_RELAUNCH_PATH="$dest"

  desktop_lock || return 1
  mkdir -p "$(dirname "$dest")" || return 1

  say ""
  say "Installing Veld Desktop ${VERSION}..."

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
        return 1
      fi
    done
  fi

  # `pgrep -f` matches a REGEX against a process's WHOLE command line, and both
  # halves of that sentence have bitten this guard:
  #
  #  - Regex, not literal: a destination containing `+`, `.` or `(` — a versioned
  #    directory, a user named `a.b` — matches a different set of processes than
  #    the one asked about, in either direction. Hence the escaping.
  #  - Whole command line, so **anchoring is load-bearing**. Unanchored, this
  #    matched the veld CLI that spawned this script, because the app passes
  #    `--app-path <dest>/Contents/MacOS/Veld` and that argument contains the
  #    pattern verbatim. The guard fired against its own caller and the app's
  #    self-update could never succeed — it reported "Veld Desktop is running"
  #    every single time. A running app's argv[0] *is* its executable path, so
  #    `^` matches the app and nothing that merely mentions it.
  #
  # Worth knowing if you go to test this: `pgrep -f` only sees a bounded prefix
  # of a long command line, so a bundle under a deep path hides the bug that a
  # bundle in /Applications shows. Test it with a short path.
  #
  # This still races anything launched in the microsecond after the check; the
  # pid handoff above is the path that is actually airtight, and this is the
  # courtesy guard for a human running the installer with the app open.
  pattern="$(printf '%s' "${dest}/Contents/MacOS/" | sed 's/[][(){}.*+?^$|\\]/\\&/g')"
  if pgrep -f -- "^${pattern}" >/dev/null 2>&1; then
    echo "Veld Desktop is running — skipping the app update."
    echo "  Quit it and re-run, or use the app's own 'Check for Updates…'."
    return 1
  fi

  say "Downloading ${url}..."
  if ! curl -fSL $CURL_PROGRESS $CURL_RETRY -o "${TMP_DIR}/${zip}" "$url"; then
    echo "Warning: could not download ${zip}, skipping the app"
    return 1
  fi

  verify_checksum "${TMP_DIR}/${zip}" "$zip" required || return 1

  # `ditto -x -k`, not `unzip`: it is the tool that preserves the symlinks,
  # permissions and metadata an .app bundle's code signature is sealed over.
  rm -rf "${TMP_DIR}/desktop"
  ditto -x -k "${TMP_DIR}/${zip}" "${TMP_DIR}/desktop" || return 1

  new_app="${TMP_DIR}/desktop/Veld.app"
  new_id="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "${new_app}/Contents/Info.plist" 2>/dev/null || true)"
  if [ "$new_id" != "dev.veld.desktop" ]; then
    # Same reasoning as the tarball's contents check: refuse to install
    # something that is not the thing this script claims to install.
    echo "Error: ${zip} does not contain Veld.app (bundle id: ${new_id:-none})"
    return 1
  fi

  # Move the old one aside instead of deleting it, so a failed copy leaves a
  # working app rather than a gap. Recorded in DESKTOP_SWAP_* first: between the
  # `mv` and the `ditto` there is no app at that path, and `cleanup` is what puts
  # it back if the script dies in that window.
  # Checked again, here, because the check above is a hundred megabytes of
  # download and an unpack away from this line — and the gap is not theoretical:
  # the app quits itself to hand this script the update, so a user clicking the
  # Dock icon while the zip downloads relaunches the very bundle about to be
  # moved aside. The pid wait is airtight only up to the instant it returns.
  # This check is free (no network) and closes all but the final microseconds.
  if pgrep -f -- "^${pattern}" >/dev/null 2>&1; then
    echo "Veld Desktop started while the update was downloading — not replacing it."
    echo "  Quit it and re-run, or use the app's own 'Check for Updates…'."
    return 1
  fi

  rm -rf "${dest}.old" || return 1
  DESKTOP_SWAP_DEST="$dest"
  DESKTOP_SWAP_BACKUP="${dest}.old"
  if [ -d "$dest" ]; then
    mv "$dest" "${dest}.old" || return 1
  fi
  if ditto "$new_app" "$dest"; then
    # Signals are ignored across the next three lines, and the reason is narrow
    # but real: between `ditto` returning 0 and `DESKTOP_SWAP_*` being cleared,
    # the destination holds a *complete, new* bundle while `cleanup` still
    # believes a swap is in flight. Bash dispatches traps between simple
    # commands, so a signal landing in that gap would delete the install that
    # just succeeded and put the old version back. (The guard removed above,
    # `[ ! -e "$DESKTOP_SWAP_DEST" ]`, made this window safe by accident — and
    # made the interrupted-copy case, which is the one that actually happens,
    # unrecoverable. This closes the window without reopening that.)
    trap '' INT TERM
    DESKTOP_SWAP_BACKUP=""
    DESKTOP_SWAP_DEST=""
    rm -rf "${dest}.old"
    trap 'exit 130' INT
    trap 'exit 143' TERM

    DESKTOP_APP="$dest"
    say "Veld Desktop installed to ${dest}"
    return 0
  fi

  echo "Error: could not install Veld Desktop to ${dest}"
  # A failed `ditto` does not necessarily leave nothing behind: it fails
  # *mid-copy* on ENOSPC or an I/O error, and what is at `$dest` then is half a
  # bundle. `cleanup` restores the backup only when the destination is missing,
  # so without this the half-copy would survive, get reopened by the relaunch,
  # and have its only intact backup deleted by the next run's `rm -rf .old`.
  # Remove the partial copy and leave DESKTOP_SWAP_* set, so the restore fires.
  rm -rf "$dest"
  return 1
}

# **This path deliberately does not consult `desktop_preference`**, and that is
# worth stating because the gate further down does. Every caller has already
# **decided** — which is a weaker and more accurate claim than "already answered",
# the one an earlier version of this comment made and two review angles refuted:
#
# - `veld desktop install|update` records "wanted" before spawning this script.
# - `veld update` reaches its app half only through `plan_desktop`, which either
#   read a recorded "yes" *or* took the "an app is already installed and nobody
#   could be asked, so keep it in step" default (`desktop_gate`'s `None if
#   installed` arm). Nothing is recorded on that second route, and that is
#   deliberate — see `crates/veld/src/commands/update.rs`.
#
# Re-checking here would not be a second line of defence, it would be a
# contradiction: the preference write is deliberately best-effort (see `remember`
# in crates/veld/src/commands/desktop.rs), so on a machine where it fails a
# `veld desktop install` typed by hand would be refused by a stale "no" it could
# not overwrite.
#
# Two consequences this script cannot fix, both stated rather than papered over.
# A veld binary older than the preference reads no `desktop.json` at all, so its
# `veld update` still moves the app half through here — self-healing on the next
# update, and the caller's version is not something a child process gets to audit.
# And `VELD_DESKTOP_ONLY=1` is a documented variable (see the header), so a human
# can set it by hand and install the app while recording nothing. What happens
# next depends on what was already on disk, and it is worth being exact because an
# earlier version of this paragraph named a message that cannot fire:
#
# - Nothing recorded: the next `veld update` either asks (interactively) or keeps
#   the now-installed app in step without asking. Neither is wrong.
# - A recorded "no": that answer still stands, so every later `veld update` says
#   the app "was left alone" and the bundle goes stale where it is. Recovered with
#   one `veld desktop install`, which is what that message names.
if [ -n "$DESKTOP_ONLY" ]; then
  if ! install_desktop_app; then
    echo ""
    echo "Veld Desktop ${VERSION} was not installed."
    exit 1
  fi
  # The failure above stays on `echo` — an app that did not install is news
  # whoever called this needs. The success banner is the caller's to print.
  say ""
  say "Veld Desktop ${VERSION} installed successfully!"
  say ""
  say "  Veld Desktop:  ${DESKTOP_APP}"
  exit 0
fi

# --- Download and extract ---

say "Downloading ${URL}..."
curl -fSL $CURL_PROGRESS $CURL_RETRY -o "${TMP_DIR}/${TARBALL}" "$URL"

verify_checksum "${TMP_DIR}/${TARBALL}" "$TARBALL" || exit 1

say "Extracting..."
# Verify tarball only contains expected files before extracting.
TAR_CONTENTS="$(tar -tzf "${TMP_DIR}/${TARBALL}")"
for entry in $TAR_CONTENTS; do
  entry="${entry#./}"
  case "$entry" in
    # Binaries, plus the attribution files release tarballs carry since v10.6.x
    # (release.yml copies LICENSE + THIRD-PARTY-LICENSES.md into dist/). New
    # non-binary entries must be added here or `veld update` refuses the tarball.
    veld|veld-helper|veld-helper.sig|veld-daemon|caddy|LICENSE|THIRD-PARTY-LICENSES.md|"") ;;
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
      say "Existing veld found at ${EXISTING_VELD}, updating in place."
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

say "Installing binaries..."
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

# The org's detached signature must travel with the helper (issue #261): the
# running root helper verifies the on-disk binary against it before it will
# relaunch onto a changed file, so without this every future update is refused
# fail-closed. The binary above is byte-identical to the CI-signed artifact
# (install.sh's re-sign is a no-op on the already-signed macOS binary), so the
# shipped .sig matches.
if [ -f "${TMP_DIR}/veld-helper.sig" ]; then
  $NEED_SUDO cp "${TMP_DIR}/veld-helper.sig" "${LIB_DIR}/veld-helper.sig"
  $NEED_SUDO chmod 644 "${LIB_DIR}/veld-helper.sig"
fi

# In privileged mode the helper is served from a ROOT-OWNED directory (issue
# #262) — the point being that this script, running as the user, cannot write
# it. So instead of copying, hand the download to the already-root helper over
# its socket: it verifies the org signature and refuses anything older than
# itself before installing. That is what keeps updates sudo-free (issue #338's
# rule 1) on an install whose whole purpose is to be unwritable.
#
# Deliberately unconditional, and deliberately quiet. The command is a no-op on
# every install this does not apply to — unprivileged, or privileged but not yet
# migrated — because on those the copy above IS the update and there is nothing
# for a user to act on. It also cleans up that now-inert copy once the real
# install has succeeded, so a migrated machine stops carrying a root-daemon-shaped
# binary in a user-writable directory.
#
# Both halves of the guard are required. The `.sig` copy above already treats the
# signature as optional (a `VELD_VERSION` pinned to a pre-signing release has
# none), and handing the helper a binary with no signature beside it would
# surface "no readable 64-byte signature" mid-install for a case that should be
# the silent no-op path.
#
# A refusal is recorded rather than fatal, and the recording is the point: the
# CLI, daemon and Caddy halves are still worth completing, but the restart
# section below must not then promise that the helper will pick up a binary it
# has just rejected.
HELPER_INSTALL_OK="1"
if [ -f "${TMP_DIR}/veld-helper" ] && [ -f "${TMP_DIR}/veld-helper.sig" ]; then
  if ! "${INSTALL_DIR}/veld" _helper-install --binary "${TMP_DIR}/veld-helper"; then
    # Recorded rather than fatal: the CLI, daemon and Caddy halves of this
    # install are still worth completing, and the command has already printed
    # why it refused. What must NOT happen is the restart section below going on
    # to promise that the helper will pick up a binary it just rejected.
    HELPER_INSTALL_OK=""
  fi
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
        # The root-owned helper directory goes with the daemon that used it
        # (issue #262). Removed here, under the sudo this branch already holds,
        # because after this point `setup.json` no longer says "privileged" and
        # `veld uninstall` will not escalate — the directory would otherwise
        # survive as root-owned files with no veld able to delete them.
        sudo rm -rf /var/db/veld-helper 2>/dev/null || true
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
        # See the macOS branch above.
        sudo rm -rf /var/lib/veld-helper 2>/dev/null || true
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
# A job that is already registered does not need re-registering: an update
# changes the binary and not the definition launchd holds. The consequence is
# worth stating rather than discovering: because this no longer re-bootstraps, an
# update is no longer a point where a plist file that is newer than launchd's
# registration converges.
#
# Two writers of the privileged helper's plist now exist, not one. `veld setup`
# is still the deliberate convergence point, and the helper's own one-time
# migration onto its root-owned directory (issue #262) is the other — that one
# re-registers itself, from a detached child, and `veld doctor` reports the
# drift if it does not. So a stale registration is still repaired by `veld setup` — which is where such a mismatch comes from in the first place (a bootstrap
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
    say "Restarting ${name} service..."
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
    say "Loading ${name} service..."
    launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null || true
  fi
}

if [ "$OS" = "macos" ]; then
  if [ -n "$PRIVILEGED_MODE" ] && [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    # Privileged mode (staying in place): helper runs as a system LaunchDaemon
    # (root), so restarting it in the system domain requires root.
    #
    # Embedded runs skip this entirely. `veld update` restarts the privileged
    # helper itself — over the helper's own socket, which needs no root — and it
    # is the half that can then wait for the version to flip and report the
    # result. Racing it from here would bounce the helper twice and, worse, print
    # a promise this script cannot keep: the old code said "will restart itself
    # (no sudo needed)" seconds before the CLI asked for a password.
    #
    # A standalone `curl … | bash` has no such caller, so it keeps the
    # passwordless attempt (never a prompt — `sudo -n`) and otherwise leaves the
    # helper's own binary watcher to pick the new version up.
    #
    # Use a graceful SIGTERM (`launchctl kill TERM`), NOT `kickstart -k`: the
    # helper handles SIGTERM by exiting while leaving Caddy running, and the
    # plist's unconditional KeepAlive relaunches it onto the new binary — so
    # every live URL stays up across the swap. A hard `kickstart -k` (SIGKILL,
    # possibly escalating to launchd job teardown) is riskier for the child
    # Caddy; the helper also spawns Caddy in its own process group as a second
    # safeguard (see veld-helper caddy.rs).
    HELPER_PLIST="/Library/LaunchDaemons/dev.veld.helper.plist"
    if [ -z "$EMBEDDED" ] && [ -f "$HELPER_PLIST" ]; then
      if sudo -n true 2>/dev/null; then
        echo "Restarting veld-helper service (privileged)..."
        sudo launchctl kill TERM system/dev.veld.helper 2>/dev/null || true
      elif [ -n "$HELPER_INSTALL_OK" ]; then
        echo "Privileged veld-helper will restart itself to pick up the new binary (no sudo needed)."
      else
        # The store still holds the OLD binary, so there is nothing for the
        # helper to pick up. Printing the promise anyway is the same false
        # reassurance the comment above says was removed from the old code.
        echo "The privileged veld-helper was NOT updated — see the error above. It keeps running the previous version."
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
  # Embedded: `veld update` owns the privileged restart (see the macOS branch).
  if [ -n "$PRIVILEGED_MODE" ] && [ -z "$SWITCHING_TO_USER_PATHS" ] && [ -z "$EMBEDDED" ]; then
    if systemctl is-active --quiet veld-helper 2>/dev/null; then
      echo "Restarting veld-helper service (privileged)..."
      $NEED_SUDO systemctl restart veld-helper 2>/dev/null || true
    fi
  elif [ -z "$PRIVILEGED_MODE" ] && [ -z "$SWITCHING_TO_USER_PATHS" ]; then
    if systemctl --user is-active --quiet veld-helper 2>/dev/null; then
      say "Restarting veld-helper service..."
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
# The function, the reasoning and the failure handling all live next to
# `verify_checksum` above, because `VELD_DESKTOP_ONLY=1` calls it before any of
# the CLI install runs. A failure here is a warning, not an abort: the binaries
# are already in place, and the PATH advice and summary below are what a
# first-time install came for.
# The app is **optional and remembered**. `~/.veld/desktop.json` holds the answer
# and `desktop_preference` explains the three states; what follows is what this
# script does with each of them.
#
# The unanswered state is the interesting one, and it splits on whether the app is
# already here — because "nobody could be asked" must not silently change either
# machine. A user mid-update who is running the app keeps it in step; a machine
# that has never had it does not get a ~113 MB download it never asked for. That
# second half is the actual complaint this change answers: an orchestrator-only
# user was paying for the app on every single update.
DESKTOP_FAILED=""
DESKTOP_DECLINED=""
if [ "$OS" = "macos" ]; then
  # First, unconditionally: not installing one does not mean there isn't one, and
  # the closing hint, the icon step, the summary and the *wording of the question*
  # all need to know which it is.
  find_desktop_app
fi
if [ -n "$WANT_DESKTOP" ]; then
  # **Where the answer came from is part of the answer**, and conflating the two
  # was a critical bug that three independent review angles found: a *stored* "no"
  # must never delete anything, while a "no" typed on this run must. Without
  # `DESKTOP_ASKED`, someone who opted out and later re-installed the app by hand
  # (the `.dmg` route the README documents) had it deleted, with no prompt, by the
  # next `curl … | bash`. `plan_desktop` in crates/veld/src/commands/update.rs
  # holds the same invariant and the two must not drift — the truth table below is
  # `desktop_gate`'s, cell for cell.
  DESKTOP_WANTED="$(desktop_preference)"
  DESKTOP_ASKED=""
  if [ -z "$DESKTOP_WANTED" ] && desktop_can_ask; then
    DESKTOP_ASKED="1"
    DESKTOP_WANTED="$(ask_desktop_preference)"
  fi
  case "$DESKTOP_WANTED" in
    yes)
      install_desktop_app || DESKTOP_FAILED="1"
      ;;
    no)
      DESKTOP_DECLINED="1"
      if [ -z "$DESKTOP_APP" ]; then
        : # Nothing here and nothing wanted. The common opted-out run: silent.
      elif [ -n "$DESKTOP_ASKED" ]; then
        # Authorised by the answer just given. The success line comes *after* the
        # removal, not before it: on the failure branch the old wording promised
        # "veld will not install it again", which an installer whose CLI is too
        # old to have the subcommand cannot deliver.
        #
        # **Re-read rather than assume the answer is on disk.**
        # `ask_desktop_preference` deliberately returns the answer even when it
        # could not be written — that is what stops a failed write inverting it —
        # so "veld will not install it again" is a promise only the *file* can
        # keep. Without this check a single run could print `warn_unrecorded`'s
        # "you may be asked again" and then that promise, in that order.
        DESKTOP_RECORDED=""
        if [ "$(desktop_preference)" = no ]; then
          DESKTOP_RECORDED="1"
        fi
        if remove_desktop_app_via_cli; then
          if [ -n "$DESKTOP_RECORDED" ]; then
            echo "Removed ${DESKTOP_APP} — veld will not install it again."
          else
            echo "Removed ${DESKTOP_APP}. Your answer was not saved, so you may be asked again."
          fi
          DESKTOP_APP=""
        else
          echo "Warning: could not remove ${DESKTOP_APP}."
          if [ -n "$DESKTOP_RECORDED" ]; then
            echo "  Your answer is recorded. Run 'veld desktop uninstall' to finish, or drag it to the Trash."
          else
            echo "  Run 'veld desktop uninstall' to finish, or drag it to the Trash."
          fi
        fi
      else
        # A stored "no" and a bundle that is here anyway — dragged from a .dmg,
        # or left behind by a removal that failed. Left alone and named, because
        # a recent manual install is a better signal than an old answer.
        echo "Veld Desktop is at ${DESKTOP_APP} but you opted out, so it was left alone."
        echo "  'veld desktop install' opts back in; 'veld desktop uninstall' removes it."
      fi
      ;;
    *)
      if [ -n "$DESKTOP_APP" ]; then
        # Said once, on the one path that moves the app with no answer on record.
        # A run piped through `tee`, or one where the question was not answered,
        # keeps the app in step and would otherwise never mention that there was a
        # choice — which for the user this whole change is about is the difference
        # between paying the download forever and knowing they need not.
        #
        # Only on success, and that is not pedantry: on failure the caller prints
        # "Run 'veld desktop install' to retry the app on its own", and "was kept
        # up to date" immediately above it is a straight contradiction.
        if install_desktop_app; then
          say "Veld Desktop was kept up to date. 'veld desktop uninstall' removes it and stops that; 'veld desktop install' records that you want it."
        else
          DESKTOP_FAILED="1"
        fi
      elif [ -n "$DESKTOP_ASKED" ]; then
        # Asked, and the answer was not yes or no. Saying "nobody to ask" to
        # somebody who was just asked is the wrong sentence.
        say "No answer, so Veld Desktop was not installed. Run 'veld desktop install' when you want it."
      else
        say "Skipping Veld Desktop — nobody to ask. Run 'veld desktop install' to add it."
      fi
      ;;
  esac
fi
if [ -n "$DESKTOP_FAILED" ]; then
  echo "  Run 'veld desktop install' to retry the app on its own."
fi

# --- Binary icons (macOS) ---
#
# See `apply_binary_icons`. Deliberately last of the install steps: it must run
# after `install_bin` has signed each binary, or `xattr -cr` there strips the
# icon straight back off.
WANT_BINARY_ICONS="1"
case "${VELD_BINARY_ICONS:-}" in
  0|false|no) WANT_BINARY_ICONS="" ;;
esac
if [ "$OS" = "macos" ] && [ -n "$WANT_BINARY_ICONS" ] && [ -n "$DESKTOP_APP" ]; then
  say "Applying the Veld icon to the binaries..."
  if ! apply_binary_icons "${DESKTOP_APP}/Contents/Resources/icon.icns"; then
    # Never fatal: an authorization prompt with the wrong icon is a worse prompt,
    # not a broken install.
    echo "Warning: could not set the Veld icon on one or more binaries."
    echo "  Harmless — authorization prompts will show the generic executable icon."
  fi
fi

# --- Next steps (no auto-run of veld setup) ---
#
# Suppressed when embedded: this is a *first install*'s footer, and `veld update`
# printed it halfway through an update — telling someone who has been running
# veld for months to run `veld start` to get going.

say ""
say "Run 'veld start' in any project to get going."
say "Run 'veld setup' for more options."
# Not offered to somebody who just declined it, or who declined it once before:
# an installer that answers "no" with "here is how to say yes" is asking again.
if [ "$OS" = "macos" ] && [ -z "$DESKTOP_APP" ] && [ -z "$DESKTOP_DECLINED" ]; then
  say "Run 'veld desktop install' for the Mac app."
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
#
# Suppressed when embedded: `veld update` prints one summary of its own, at the
# true end of the run. Left in, this banner claimed the update was finished while
# the service restarts and the app half were still to come — and it was one of
# three "installed successfully!" lines in a single command.

say ""
say "veld ${VERSION} installed successfully!"
say ""
say "  veld binary:   ${INSTALL_DIR}/veld"
say "  veld-helper:   ${LIB_DIR}/veld-helper"
say "  veld-daemon:   ${LIB_DIR}/veld-daemon"
say "  caddy:         ${LIB_DIR}/caddy"
# An `[ … ] && say` here would be the script's last command, so a machine with no
# app installed would exit non-zero — and `veld update` reads that exit code.
# (`say` is a function, so it exits 0 even when it prints nothing; the hazard is
# the `[ … ] &&` test itself, which is why this stays an `if`.)
if [ -n "$DESKTOP_APP" ]; then
  say "  Veld Desktop:  ${DESKTOP_APP}"
fi
