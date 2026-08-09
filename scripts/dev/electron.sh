#!/usr/bin/env bash
# Supervise the dev Electron shell — and let you close it without losing the
# rest of the dev stack.
#
# WHY THIS IS NOT JUST `electron .`
#
# veld's health monitor treats *any* node process dying as a crash of the whole
# run: it marks the run `crashed` and SIGTERMs every surviving sibling
# (`veld-daemon/src/monitor.rs`). That is right for a service, and wrong for a
# desktop app, which the user is supposed to be able to quit. Run bare, one
# Cmd+Q took the dev daemon and the vite server down with it — measured.
#
# So the NODE is this script, not Electron. It stays alive across a quit; the
# stack stays up; and `veld action open --node dev-electron` brings the window
# back.
#
# WHY EVERYTHING ARRIVES AS AN ARGUMENT
#
# This script runs from two places with different capabilities, and the
# intersection is small enough to be worth stating (all three measured):
#
#                    argv interpolated?   node `env` map?
#   node process     yes                  yes
#   action           yes                  NO
#   probe command    NO                   NO
#
# A node's `env` reaches the node's own process and nothing else, so an earlier
# version that read VELD_DEV_ROOT here failed in both other places — the action
# with a visible error, the readiness probe *silently*, because a probe's
# stdout and stderr are sent to /dev/null. Hence: the run directory is passed
# positionally, and the repo root is derived from this script's own location.
#
# Usage:
#   electron.sh <run-dir> <grace-seconds>   supervise (the node)
#   electron.sh --open <run-dir>            the action: bring a window back
set -uo pipefail

# `<root>/scripts/dev/electron.sh` — no env var, correct from anywhere.
root="$(cd "$(dirname "$0")/../.." && pwd)"

# --- --open: the action ----------------------------------------------------
if [ "${1:-}" = "--open" ]; then
    dir="${2:?usage: electron.sh --open <run-dir>}"
    pidfile="$dir/electron.pid"
    pid="$([ -f "$pidfile" ] && cat "$pidfile" 2>/dev/null)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        # Electron is running, so this is the closed-WINDOW case, not the quit
        # case — on macOS `window-all-closed` deliberately does not quit (the
        # tray stays), so the process is alive with nothing on screen. Electron
        # opens a window again on its `activate` event (see `app.on("activate")`
        # in desktop/src/main.js), and activating the process is how that fires.
        #
        # Addressed by unix id rather than by application name: in a dev run the
        # process is called "Electron", which is also every other Electron app's
        # helper name here.
        if [ "$(uname)" = "Darwin" ]; then
            if osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $pid) to true" >/dev/null 2>&1; then
                echo "Electron was already running — brought it to the front." >&2
                exit 0
            fi
            echo "Could not activate pid $pid (macOS may be asking for automation permission)." >&2
            echo "Click the Veld tray icon instead." >&2
            exit 1
        fi
        echo "Electron is already running (pid $pid)." >&2
        exit 0
    fi
    # Electron has exited. Open the gate; the supervisor relaunches within ~1s.
    : >"$dir/electron.open"
    echo "Reopening Electron…" >&2
    exit 0
fi

# --- supervise -------------------------------------------------------------

dir="${1:?usage: electron.sh <run-dir> <grace-seconds>}"
# Keep equal to the `settle` seconds in veld.json — see the launch check below.
grace="${2:?usage: electron.sh <run-dir> <grace-seconds>}"
gate="$dir/electron.open"
pidfile="$dir/electron.pid"
child=""

stop_child() {
    [ -n "$child" ] || return 0
    kill -0 "$child" 2>/dev/null || return 0
    # Escalate. veld signals THIS script; Electron is our child and would
    # otherwise be orphaned by a supervisor that just exited — which is the
    # leak this whole file exists to avoid creating.
    kill -TERM "$child" 2>/dev/null || true
    for _ in $(seq 1 50); do
        kill -0 "$child" 2>/dev/null || return 0
        sleep 0.1
    done
    kill -KILL "$child" 2>/dev/null || true
}

cleanup() {
    stop_child
    rm -f "$pidfile" "$gate"
}
trap 'cleanup; exit 0' TERM INT
trap cleanup EXIT

launch() {
    # `exec` inside the subshell, so $! is Electron itself and not a shell
    # wrapping it — `stop_child` must signal the thing that owns the windows.
    # Directly rather than via `npm start`, for the same reason: npm would sit
    # between us and the process we have to be able to kill.
    (cd "$root/desktop" && exec ./node_modules/.bin/electron .) &
    child=$!
    echo "$child" >"$pidfile"
}

# A gate left over from a previous run would relaunch Electron immediately
# after the first deliberate quit.
rm -f "$gate"
launch

# THE FIRST LAUNCH MUST SURVIVE, and this is what keeps readiness honest.
#
# The probe is `settle`, which only asks whether the node's process is still
# alive after N seconds — and this supervisor always is. So the supervisor
# adopts the probe's question as its own: within the same window, an Electron
# that dies is a failed start and takes the script down with it, which fails
# the node truthfully. Past the window, an exit is the user quitting, and is
# exactly what this script exists to absorb.
for _ in $(seq 1 "$((grace * 10))"); do
    if ! kill -0 "$child" 2>/dev/null; then
        wait "$child"
        status=$?
        echo "Electron exited during startup (status $status) — failing the node." >&2
        exit "${status:-1}"
    fi
    sleep 0.1
done

echo "Electron supervised as pid $child. Quit it freely — the stack stays up." >&2
echo "Bring it back with: veld action open --node dev-electron" >&2

while true; do
    if kill -0 "$child" 2>/dev/null; then
        sleep 1
        continue
    fi
    # Electron exited. Stay alive so the run does too, and wait to be asked.
    rm -f "$pidfile"
    if [ -f "$gate" ]; then
        rm -f "$gate"
        launch
        echo "Electron reopened as pid $child." >&2
    fi
    sleep 1
done
