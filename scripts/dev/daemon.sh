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
#
# A DIGEST of `<root>|<run>`, and both halves earn their place.
#
# A digest rather than the name, because this path is the numerator of the same
# 104-byte budget the header is about: a run name slugs to at most 48 characters,
# and `<home>/.veld/pty-dv-<48>/<16 hex>.sock` is 103 bytes for a 19-character
# home — one byte inside the limit, and over it for any longer home. The digest
# caps the contribution at 10 characters and is stable across restarts, which is
# the property the directory needs.
#
# Keyed on the project root as well as the run, because a run name is unique
# within a project and nowhere else — `generate_run_name` yields the worktree
# folder for a linked worktree and the branch for a main checkout, and `--name`
# is free-form — so two checkouts can hold the same live run name. Sharing one
# holder directory would have two dev daemons adopting each other's sessions,
# whose `worktree_id`s come from different databases, and sharing the `shims/`
# subdirectory whose executables point at one specific daemon's session
# registry. `run_route_id` carries a project discriminator for the same reason.
run_digest="$(printf '%s|%s' "$root" "$run" | cksum | awk '{print $1}')"
export VELD_PTY_DIR="$HOME/.veld/pty-dv-$run_digest"

# The digest is not guessable from the run name, and `veld doctor` reports the
# directory rather than the mapping — so say it once, here, where someone
# chasing a stranded holder will find it in the node's log.
echo "dev daemon: port $port, socket $VELD_DAEMON_SOCK" >&2
echo "            terminals in $VELD_PTY_DIR (run '$run')" >&2

exec "$root/target/debug/veld-daemon"
