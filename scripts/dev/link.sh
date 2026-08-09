#!/usr/bin/env bash
# Write (or remove) `~/.local/bin/veld-dev-<run>` — the CLI that talks to THIS
# run's dev instance, from any directory.
#
# This is the binary you point at a real project to test veld against it, so it
# has to be per-worktree: the old single `veld-dev` hardcoded one worktree path,
# and whichever worktree wrote it last owned the name.
#
# Rewritten at every start and removed at every stop, because what it carries is
# a live allocation — the run's actual daemon port. A wrapper left behind after
# the run stops points at a port nothing serves, which is a worse failure than
# no wrapper at all: the CLI hangs or reports an empty instance rather than
# telling you the dev stack is down.
set -euo pipefail

root="${VELD_DEV_ROOT:?VELD_DEV_ROOT must be set by the veld node}"
run="${VELD_DEV_RUN:?VELD_DEV_RUN must be set by the veld node}"
wrapper="$HOME/.local/bin/veld-dev-$run"

if [ "${1:-}" = "--remove" ]; then
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
export VELD_DB_PATH="$dir/veld.db"
export VELD_DAEMON_PORT="$port"
export VELD_DAEMON_SOCK="\$HOME/.veld/dev-$port.sock"
exec "$root/target/debug/veld" "\$@"
WRAPPER

chmod +x "$wrapper"
echo "✓ veld-dev-$run → this worktree, daemon port $port" >&2
echo "  Not on your PATH? add ~/.local/bin to it." >&2
