#!/usr/bin/env bash
# Write (or remove) `~/.local/bin/veld-dev-<run>` — the CLI that talks to THIS
# run's dev instance, from any directory.
#
# This is the binary you point at a real project to test veld against it, so it
# has to be per-worktree: the old single `veld-dev` hardcoded one worktree path,
# and whichever worktree wrote it last owned the name.
#
# Rewritten at every start and removed at every stop, because what it carries is
# a live allocation — the run's actual daemon port.
#
# THE REMOVAL IS BEST-EFFORT, so the wrapper also checks itself. `on_stop` has
# exactly one caller, inside `Orchestrator::stop`; the crash path
# (`monitor.rs` → `finalize_crashed`) and the stale-run sweep both kill PIDs
# without running any hook. For a stack whose whole purpose is breaking the
# daemon, crashing is the *likely* ending, not the exceptional one — so relying
# on removal alone would leave the stale wrapper this file exists to prevent.
# The generated script therefore refuses when its port is not listening.
#
# It is also keyed on a run name, which is NOT unique across checkouts: two
# clones can hold the same live run name (`generate_run_name` yields a folder
# for a worktree and a branch for a main checkout, and `--name` is free-form).
# Hence the ownership check on removal below — a stop must never delete another
# checkout's wrapper.
set -euo pipefail

root="${VELD_DEV_ROOT:?VELD_DEV_ROOT must be set by the veld node}"
run="${VELD_DEV_RUN:?VELD_DEV_RUN must be set by the veld node}"
wrapper="$HOME/.local/bin/veld-dev-$run"

if [ "${1:-}" = "--remove" ]; then
    # Only ours. A same-named run in another checkout owns a wrapper naming a
    # different root, and deleting that would reintroduce exactly the
    # last-writer-wins failure this script replaced.
    #
    # `-F` and `-x`, not a pattern: `$root` is an absolute path interpolated
    # into the expression, and a checkout under a directory containing `[`
    # makes grep abort on an unbalanced bracket — whereupon `! grep` is true and
    # the script cheerfully reports that its OWN wrapper belongs to someone
    # else. (`.` in a path is a wildcard too, wrong in the other direction.)
    #
    # A wrapper with NO marker line predates this check, so it is ours by
    # elimination — refusing it would strand exactly the stale wrapper this
    # file exists to remove, with no way back except a successful start.
    if [ -f "$wrapper" ] &&
        grep -q '^# veld-dev-root: ' "$wrapper" &&
        ! grep -qxF "# veld-dev-root: $root" "$wrapper"; then
        echo "Left $wrapper alone — it belongs to another checkout." >&2
        exit 0
    fi
    rm -f "$wrapper"
    echo "Removed $wrapper" >&2
    exit 0
fi

dir="${VELD_DEV_DIR:?VELD_DEV_DIR must be set by the veld node}"

# The daemon's port arrives as an ARGUMENT, not in the environment, and that is
# load-bearing rather than a style choice. This node's `env` map is resolved
# again for the `on_stop` hook above, `build_env` is all-or-nothing, and
# `${nodes.dev-daemon.port}` cannot resolve once the run is stopping. Putting it
# in `env` therefore emptied the WHOLE map at stop time — so `--remove` lost
# even VELD_DEV_ROOT, failed, and left the stale wrapper behind that this script
# exists to prevent. Keep `env` to `${veld.*}` builtins only.
port="${2:?usage: link.sh --write <daemon-port>}"

mkdir -p "$HOME/.local/bin"

# A quoted heredoc, so nothing here expands NOW. veld has already substituted
# its own `${...}` into the env vars above; `$HOME` and `$@` must survive into
# the wrapper as text, and an unquoted heredoc would resolve both at write time.
cat >"$wrapper" <<'WRAPPER'
#!/usr/bin/env bash
WRAPPER
cat >>"$wrapper" <<WRAPPER
# veld-dev-root: $root
export VELD_DB_PATH="$dir/veld.db"
export VELD_DAEMON_PORT="$port"
export VELD_DAEMON_SOCK="\$HOME/.veld/dev-$port.sock"

# Warn, but do NOT refuse. This wrapper carries a port that was allocated to one
# run; if that run crashed, nothing removed this file, because the \`on_stop\`
# hook runs only on a deliberate \`veld stop\`. Saying so turns "my environments
# vanished" into "the stack is down".
#
# Refusing outright was the first version and it was wrong: most of what you
# reach for after a crash needs no daemon at all. \`stop\` tolerates a dead one
# explicitly, \`logs\`, \`runs\` and \`doctor\` read the database, and nothing
# auto-spawns a daemon — so a hard exit here blocked the cleanup commands in
# exactly the situation the check was written for.
#
# bash's own /dev/tcp rather than \`nc\`, which is not everywhere and comes in
# incompatible flavours. Every mainstream bash build enables it.
if ! (exec 3<>/dev/tcp/127.0.0.1/"\$VELD_DAEMON_PORT") 2>/dev/null; then
    echo "veld-dev-$run: nothing is listening on port \$VELD_DAEMON_PORT — that run is not up." >&2
    echo "  Commands that need the daemon will come back empty. Restart it with:" >&2
    echo "    (cd $root && veld start --preset dev-keep)" >&2
fi

exec "$root/target/debug/veld" "\$@"
WRAPPER

chmod +x "$wrapper"
echo "✓ veld-dev-$run → this worktree, daemon port $port" >&2
echo "  Not on your PATH? add ~/.local/bin to it." >&2
