#!/usr/bin/env bash
# Run this worktree's dev daemon, as a veld node.
#
# A script rather than the node's `argv`, for one reason: two of the paths below
# must be under $HOME, and only a shell can expand it.
#
# Why under $HOME and not in the checkout — this is a length bound, not
# tidiness. A unix socket path is capped by `sockaddr_un::sun_path` (104 bytes
# on macOS, 108 on Linux), and a worktree at `~/git/_worktrees/<branch>/` blows
# through it. The failure is `bind` reporting "path must be shorter than
# SUN_LEN", which reaches you as a daemon that will not start and names nothing
# you can act on. `veld_core::instance::pty_dir` documents the same bound.
set -euo pipefail

root="${VELD_DEV_ROOT:?VELD_DEV_ROOT must be set by the veld node}"
run="${VELD_DEV_RUN:?VELD_DEV_RUN must be set by the veld node}"
# veld allocated this and passes it in as the daemon's own HTTP port.
port="${VELD_DAEMON_PORT:?VELD_DAEMON_PORT must be set by the veld node}"

mkdir -p "$HOME/.veld"

# Keyed by the allocated port, so two worktrees' dev daemons never share one.
export VELD_DAEMON_SOCK="$HOME/.veld/dev-$port.sock"

# Keyed by the RUN, not by the port — and that difference is the whole reason
# this line exists. The default holder directory is `~/.veld/pty-<daemon port>`,
# which was stable back when the dev daemon's port was a constant in the
# justfile. Under veld the port is allocated afresh on every start, so the
# default would strand every previous start's holder processes in a directory
# nothing ever looks at again, and no terminal would survive a restart. The
# `pty-` prefix is load-bearing: `veld uninstall` finds every instance's holders
# by it.
export VELD_PTY_DIR="$HOME/.veld/pty-dv-$run"

exec "$root/target/debug/veld-daemon"
