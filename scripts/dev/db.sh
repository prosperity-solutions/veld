#!/usr/bin/env bash
# Prepare this run's dev database.
#
# One database per RUN, not per worktree: two runs of the dev stack in one
# checkout (say, one on the branch and one bisecting) would otherwise share a
# file that a live dev daemon holds open, and the second would migrate it under
# the first. `.veld-dev/<run>/veld.db`, gitignored with the rest of `.veld-dev`.
#
# Never the real database. A dev build carries migrations the installed veld
# does not, and a binary refuses a `user_version` newer than it supports — so
# one careless open takes the developer's working veld offline until the schema
# is hand-rolled back. See AGENTS.md.
set -euo pipefail

mode="${1:?usage: db.sh ensure|fresh|from-real}"
dir="${VELD_DEV_DIR:?VELD_DEV_DIR must be set by the veld node}"
db="$dir/veld.db"

mkdir -p "$dir"

case "$mode" in
ensure)
    # The variant `dev-daemon` depends on, so it runs on EVERY start — including
    # the starts where a preset also named `fresh` or `from-real`.
    #
    # Therefore: creates the directory, reports, and writes nothing else. Those
    # two run concurrently with this one, in the same stage (veld's plan is
    # keyed on (node, variant), so naming one adds a node rather than replacing
    # this one). Anything written here would race the variant the user actually
    # asked for. Reading a file that is being deleted underneath is fine, which
    # is why the version query below tolerates failure.
    if [ -f "$db" ]; then
        echo "Dev DB: $db (schema v$(sqlite3 "$db" 'PRAGMA user_version;' 2>/dev/null || echo '?'))" >&2
    else
        echo "Dev DB: $db (new — the daemon will create it)" >&2
    fi
    ;;
fresh)
    # WAL and SHM too: a stranded sidecar is how a database comes back at a
    # `user_version` whose migration was later rewritten.
    rm -f "$db" "$db-wal" "$db-shm"
    echo "Dev DB reset: $db" >&2
    ;;
from-real)
    # Migration rehearsal: the next dev-daemon start migrates this COPY forward
    # while the real file stays untouched and the installed daemon stays
    # healthy. `.backup` rather than `cp` — a plain copy can tear a WAL DB.
    real="$HOME/Library/Application Support/veld/veld.db"
    if [ ! -f "$real" ]; then
        echo "No installed DB at $real" >&2
        echo "(from-real is macOS-only today — the path above is hardcoded.)" >&2
        exit 1
    fi
    rm -f "$db" "$db-wal" "$db-shm"
    sqlite3 "$real" ".backup '$db'"
    chmod 600 "$db"
    echo "Snapshotted real DB → $db (schema v$(sqlite3 "$db" 'PRAGMA user_version;'))" >&2
    echo "The real DB is untouched; this copy is what gets migrated." >&2
    ;;
*)
    echo "unknown mode '$mode' (expected ensure, fresh, or from-real)" >&2
    exit 1
    ;;
esac
