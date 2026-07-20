export PATH := env("HOME") + "/.cargo/bin:" + env("PATH")

# Dedicated DEV INSTANCE for source-built binaries: tier-1 `just dev` (and
# the veld-dev wrapper / dev-daemon) run with their own database, daemon
# port, and daemon socket — never the installed veld's. Dev builds carry
# newer schema migrations, and letting one loose on the real DB would migrate
# it forward and blind the installed daemon (NewerSchema) until `veld update`.
# The dev daemon runs ALONGSIDE the installed one and serves its own
# dashboard at https://veld-dev.localhost.
# NOTE: the helper/Caddy/DNS layer is NOT instanced — it is a singleton
# owning 443/18443 and system DNS; both instances share it (distinct
# hostnames and route ids keep them apart).
dev_db := justfile_directory() + "/.veld-dev/veld.db"
# The dev instance's daemon port — installed daemon keeps 19899; both run
# side by side. Dev CLI and dev daemon must agree on this.
dev_daemon_port := "19898"

# ============================================================================
# Veld Development Workflow
#
# Three tiers — use the lightest one that covers your change:
#
#   just dev <args>           CLI only, no install, own dev instance (most changes)
#   just dev-daemon           Daemon from source, alongside the installed one
#                             (own port/DB/socket, dashboard: veld-dev.localhost)
#   just dev-db-reset         Wipe the dev DB (fresh state)
#   just dev-db-from-real     Snapshot the REAL DB into the dev DB (migration rehearsal)
#   just dev-install-daemon   Install daemon (overlay/feedback changes)
#   just dev-install-helper   Install helper + restart Caddy (proxy changes, sudo)
#   just dev-install          CLI + daemon (no sudo)
#   just dev-install-all      Everything including helper (sudo)
#   just dev-restore          Go back to the released version
#
# Tier 1 uses a dedicated SQLite file (.veld-dev/veld.db, gitignored); the
# install tiers replace the system binaries and operate on the real DB.
# ============================================================================

# --- Tier 1: Run CLI from source (no install, own state) ---

# Build and run veld from source against the dev DB. Does NOT touch the
# system install or its database.
# Usage: just dev start --name foo website:local
dev *ARGS:
    cargo build
    mkdir -p "{{justfile_directory()}}/.veld-dev"
    VELD_DB_PATH="{{dev_db}}" \
    VELD_DAEMON_PORT="{{dev_daemon_port}}" \
    VELD_DAEMON_SOCK="{{justfile_directory()}}/.veld-dev/daemon.sock" \
        ./target/debug/veld {{ARGS}}

# Run the daemon from source, foreground, ALONGSIDE the installed one — own
# DB, own port (19898), own socket, own dashboard at https://veld-dev.localhost
# (self-registered Caddy route; removed again on Ctrl-C). Gives dev-DB runs
# their monitoring/GC without touching the installed daemon.
dev-daemon:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build -p veld-daemon
    mkdir -p .veld-dev
    echo "Dev daemon: port {{dev_daemon_port}}, DB {{dev_db}}, dashboard https://veld-dev.localhost"
    VELD_DB_PATH="{{dev_db}}" \
    VELD_DAEMON_PORT="{{dev_daemon_port}}" \
    VELD_DAEMON_SOCK="{{justfile_directory()}}/.veld-dev/daemon.sock" \
    VELD_MANAGEMENT_HOST="veld-dev.localhost" \
        ./target/debug/veld-daemon

# Run the source-built CLI against the REAL installed DB — for inspecting
# runs the installed veld started (e.g. feedback loops on a shared run).
# ⚠ ONLY safe when your branch adds no schema migration: a schema-ahead dev
# binary MIGRATES the real DB forward on open, and the installed veld/daemon
# then fail with NewerSchema until `veld update`. If in doubt, don't — use
# `just dev` + `just dev-daemon`, or `just dev-install`.
dev-real *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build
    # Highest schema version this branch's binary migrates to = number of
    # MIGRATIONS entries (their `version:` fields are consecutive from 1,
    # enforced by the migrations_are_consecutive test).
    branch_v=$(grep -cE '^        version: [0-9]+,' crates/veld-core/src/db/mod.rs || true)
    real="$HOME/Library/Application Support/veld/veld.db"
    if [ -f "$real" ]; then
        real_v=$(sqlite3 "$real" 'PRAGMA user_version;' 2>/dev/null || echo '?')
        if [ "$real_v" != "?" ] && [ "$branch_v" -gt "$real_v" ]; then
            echo "⚠ This branch has schema v$branch_v; your real DB is v$real_v."
            echo "  Running would migrate the REAL DB and break the installed veld."
            echo "  Aborting. Use 'just dev' (isolated) or 'just dev-install'."
            exit 1
        fi
    fi
    VELD_LIB_DIR="{{justfile_directory()}}/target/debug" \
        ./target/debug/veld {{ARGS}}

# Wipe the dev DB (including WAL/SHM sidecars) for a fresh-state run.
dev-db-reset:
    rm -f "{{dev_db}}" "{{dev_db}}-wal" "{{dev_db}}-shm"
    @echo "Dev DB reset ({{dev_db}})"

# Snapshot the REAL installed DB into the dev DB — migration rehearsal:
# the next `just dev <cmd>` migrates the COPY forward while the real file
# stays untouched (and the installed daemon stays healthy). Uses sqlite3
# .backup for a consistent online copy (a plain cp can tear a WAL DB).
dev-db-from-real:
    #!/usr/bin/env bash
    set -euo pipefail
    real="$HOME/Library/Application Support/veld/veld.db"
    [ -f "$real" ] || { echo "No installed DB at $real"; exit 1; }
    mkdir -p "{{justfile_directory()}}/.veld-dev"
    rm -f "{{dev_db}}" "{{dev_db}}-wal" "{{dev_db}}-shm"
    sqlite3 "$real" ".backup '{{dev_db}}'"
    chmod 600 "{{dev_db}}"
    echo "Snapshotted real DB → {{dev_db}} (schema v$(sqlite3 "{{dev_db}}" 'PRAGMA user_version;'))"
    echo "Next 'just dev <cmd>' will migrate this copy; the real DB is untouched."

# Create a `veld-dev` wrapper in ~/.local/bin for cross-project use.
# Carries the dev DB too — veld-dev state never mixes with the installed veld's.
dev-link:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build
    mkdir -p "$HOME/.local/bin" .veld-dev
    wrapper="$HOME/.local/bin/veld-dev"
    printf '#!/usr/bin/env bash\nexport VELD_DB_PATH="{{dev_db}}"\nexport VELD_DAEMON_PORT="{{dev_daemon_port}}"\nexport VELD_DAEMON_SOCK="{{justfile_directory()}}/.veld-dev/daemon.sock"\nexec "{{justfile_directory()}}/target/debug/veld" "$@"\n' > "$wrapper"
    chmod +x "$wrapper"
    echo "Created $wrapper — use 'veld-dev' from any directory."
    echo "Remove with: rm $wrapper"

# --- Tier 2: Install daemon (user-level, no sudo) ---

# Install dev daemon and restart the service.
# Use when: you changed the feedback overlay, client-log, or daemon code.
dev-install-daemon:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cd crates/veld-daemon/frontend && npm run build && cd ../../..
    touch crates/veld-daemon/src/main.rs
    cargo build -p veld-daemon

    # Find where the launchd plist actually points
    plist_bin=""
    for plist in "$HOME/Library/LaunchAgents/dev.veld.daemon.plist"; do
        if [ -f "$plist" ]; then
            plist_bin=$(grep "veld-daemon" "$plist" | head -1 | sed 's/.*<string>//;s/<\/string>.*//' | tr -d '[:space:]')
            [ -n "$plist_bin" ] && break
        fi
    done

    # Always install to lib dir
    lib_dst="$HOME/.local/lib/veld/veld-daemon"
    cp ./target/debug/veld-daemon "$lib_dst"
    codesign -s - -f "$lib_dst" 2>/dev/null || true

    # Also copy to wherever the plist points (if different)
    if [ -n "${plist_bin:-}" ] && [ "$plist_bin" != "$lib_dst" ]; then
        echo "Plist points to $plist_bin — copying there too"
        cp ./target/debug/veld-daemon "$plist_bin"
        codesign -s - -f "$plist_bin" 2>/dev/null || true
    fi

    echo "Installed: $("$lib_dst" --version)"

    # Restart daemon service
    rm -f ~/.veld/daemon.sock
    if launchctl list dev.veld.daemon &>/dev/null; then
        launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon" 2>/dev/null || true
    fi

    sleep 2
    if curl -sf http://127.0.0.1:19899/api/environments >/dev/null 2>&1; then
        echo "✓ Daemon running"
    else
        echo "✗ Daemon not responding — run 'veld doctor'"
    fi

# --- Tier 2b: Sandbox daemon (dev schema, copied database) ---

# Run the dev daemon in the FOREGROUND against a COPY of the real database.
# Use when: the dev build carries schema migrations the released binaries
# don't know yet — installing it via dev-install-daemon would migrate the
# real veld.db and every released binary would then refuse to open it
# ("created by a newer veld version"). This recipe never touches the real DB:
#   1. snapshots veld.db → target/dev-db/veld.db (sqlite .backup, atomic)
#   2. unloads the released daemon service (reloaded automatically on exit)
#   3. runs the dev daemon in the foreground on the copy; daemon-spawned
#      `veld` commands use the dev CLI + the same copy (VELD_SPAWN_VELD_BIN,
#      VELD_DB_PATH). Ctrl-C to stop; the released daemon comes back.
dev-daemon-sandbox:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build -p veld-daemon
    cargo build

    real_db="$HOME/Library/Application Support/veld/veld.db"
    [ -f "$real_db" ] || real_db="${XDG_DATA_HOME:-$HOME/.local/share}/veld/veld.db"
    sandbox_db="{{justfile_directory()}}/target/dev-db/veld.db"
    mkdir -p "$(dirname "$sandbox_db")"
    rm -f "$sandbox_db" "$sandbox_db-wal" "$sandbox_db-shm"
    sqlite3 "$real_db" ".backup '$sandbox_db'"
    echo "✓ Sandbox DB: $sandbox_db (copy of $real_db)"

    plist="$HOME/Library/LaunchAgents/dev.veld.daemon.plist"
    if [ -f "$plist" ] && launchctl list dev.veld.daemon &>/dev/null; then
        echo "Unloading released daemon (restored on exit)…"
        launchctl bootout "gui/$(id -u)/dev.veld.daemon" || true
        trap 'echo "Restoring released daemon…"; launchctl bootstrap "gui/$(id -u)" "'"$plist"'" || true' EXIT
    fi

    echo "Dev daemon on http://127.0.0.1:19899 (v2 UI: /v2) — Ctrl-C to stop."
    VELD_DB_PATH="$sandbox_db" \
    VELD_SPAWN_VELD_BIN="{{justfile_directory()}}/target/debug/veld" \
        ./target/debug/veld-daemon

# --- Tier 3: Install helper (privileged, requires sudo) ---

# Install dev helper and restart Caddy.
# Use when: you changed Caddy config, route building, GODEBUG, TLS, etc.
dev-install-helper:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build -p veld-helper

    # Find where the launchd plist actually points — copy there.
    plist_bin=""
    for plist in /Library/LaunchDaemons/dev.veld.helper.plist "$HOME/Library/LaunchAgents/dev.veld.helper.plist"; do
        if [ -f "$plist" ]; then
            plist_bin=$(grep "veld-helper" "$plist" | head -1 | sed 's/.*<string>//;s/<\/string>.*//' | tr -d '[:space:]')
            [ -n "$plist_bin" ] && break
        fi
    done

    # Always install to lib dir
    lib_dst="$HOME/.local/lib/veld/veld-helper"
    cp ./target/debug/veld-helper "$lib_dst"
    codesign -s - -f "$lib_dst" 2>/dev/null || true

    # Also copy to wherever the plist points (if different)
    if [ -n "${plist_bin:-}" ] && [ "$plist_bin" != "$lib_dst" ]; then
        echo "Plist points to $plist_bin — copying there too"
        cp ./target/debug/veld-helper "$plist_bin"
        codesign -s - -f "$plist_bin" 2>/dev/null || true
    fi

    echo "Installed: $("$lib_dst" --version)"

    # Restart privileged helper (prompts for sudo)
    if sudo launchctl list dev.veld.helper &>/dev/null 2>&1; then
        echo "Restarting privileged helper..."
        sudo launchctl kickstart -k "system/dev.veld.helper"
    elif launchctl list dev.veld.helper &>/dev/null; then
        echo "Restarting unprivileged helper..."
        launchctl kickstart -k "gui/$(id -u)/dev.veld.helper"
    else
        echo "⚠ No helper service found — run 'veld setup'"
        exit 1
    fi

    sleep 1
    echo "✓ Helper restarted."
    echo "  Restart your runs to pick up the new helper: veld restart --name <run>"

# --- Tier 4: Build and install Caddy with local veld_inject module ---

# Build Caddy with the local inject module and install it.
# Use when: you changed caddy/inject/*.go
dev-install-caddy:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    XCADDY_VERSION=$(cat .xcaddy-version)
    echo "Building Caddy with local veld_inject module (xcaddy $XCADDY_VERSION)..."
    xcaddy build \
        --with github.com/prosperity-solutions/veld/caddy/inject=./caddy/inject \
        --output ./target/caddy
    dst="$HOME/.local/lib/veld/caddy"
    cp ./target/caddy "$dst"
    codesign -s - -f "$dst" 2>/dev/null || true
    echo "✓ Caddy installed ($("$dst" version))"
    echo "  Restart your runs to pick up the new Caddy."

# --- Compound targets ---

# Install CLI + daemon (no sudo needed).
dev-install:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build
    mkdir -p "$HOME/.local/lib/veld"
    # CLI
    cp ./target/debug/veld "$HOME/.local/bin/veld"
    codesign -s - -f "$HOME/.local/bin/veld" 2>/dev/null || true
    echo "CLI: $(veld --version)"
    # Daemon
    just dev-install-daemon

# Install everything (CLI + daemon + helper + Caddy). Requires sudo + Go.
dev-install-all:
    just dev-install
    just dev-install-caddy
    just dev-install-helper

# Restore to the released version.
dev-restore:
    veld update

# --- Build / Test / Lint ---

build:
    cd crates/veld-daemon/frontend && npm run build
    cd crates/veld-daemon/ui && npm run build
    cargo build

test:
    cargo test --workspace
    cd crates/veld-daemon/frontend && npm test
    cd crates/veld-daemon/ui && npm test

lint:
    cargo clippy --workspace --all-targets
    cd crates/veld-daemon/frontend && npx tsc --noEmit
    cd crates/veld-daemon/ui && npm run typecheck

build-frontend:
    cd crates/veld-daemon/frontend && npm run build

test-frontend:
    cd crates/veld-daemon/frontend && npm test

lint-frontend:
    cd crates/veld-daemon/frontend && npx tsc --noEmit

setup-frontend:
    cd crates/veld-daemon/frontend && npm install

# --- Management UI v2 (crates/veld-daemon/ui) + desktop shell (desktop/) ---

build-ui:
    cd crates/veld-daemon/ui && npm run build

test-ui:
    cd crates/veld-daemon/ui && npm test

lint-ui:
    cd crates/veld-daemon/ui && npm run typecheck

setup-ui:
    cd crates/veld-daemon/ui && npm install
    cd desktop && npm install

# Vite dev server for the /v2 UI (HMR, proxies /api to the daemon on :19899).
dev-ui:
    cd crates/veld-daemon/ui && npm run dev

# Electron shell pointed at the vite dev server (start `just dev-ui` first).
dev-desktop:
    cd desktop && VELD_DESKTOP_URL=http://localhost:5199 npm start

# Electron shell against the installed daemon's embedded /v2.
desktop:
    cd desktop && npm start

# --- Licenses ---

# Regenerate THIRD-PARTY-LICENSES.md from the current dependency tree.
# Requires cargo-about 0.9.1 (pinned in ci.yml so output matches the CI
# drift check): `cargo install cargo-about@0.9.1 --all-features`.
# CI fails if the committed file drifts from this output, so run it and
# commit the result whenever Cargo.lock changes.
# `tr -d '\r'` normalizes CRLF that some upstream license texts carry, so the
# committed file is pure LF and the CI drift diff is byte-stable across OSes.
licenses:
    cargo about generate about.hbs | tr -d '\r' > THIRD-PARTY-LICENSES.md
