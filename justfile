export PATH := env("HOME") + "/.cargo/bin:" + env("PATH")

# Dedicated state for source-built binaries: tier-1 `just dev` (and the
# veld-dev wrapper / dev-daemon) never touch the installed veld's database at
# ~/Library/Application Support/veld/veld.db — dev builds carry newer schema
# migrations, and letting one loose on the real DB would migrate it forward
# and blind the installed daemon (NewerSchema) until `veld update`.
# NOTE: this isolates STATE only. The helper/Caddy/DNS are still the real,
# shared system services, and the installed daemon only watches the real DB —
# dev-DB runs get no crash detection/GC unless `just dev-daemon` is running.
dev_db := justfile_directory() + "/.veld-dev/veld.db"

# ============================================================================
# Veld Development Workflow
#
# Three tiers — use the lightest one that covers your change:
#
#   just dev <args>           CLI only, no install, own dev DB (most changes)
#   just dev-daemon           Run daemon from source against the dev DB
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
    VELD_LIB_DIR="{{justfile_directory()}}/target/debug" \
    VELD_DB_PATH="{{dev_db}}" \
        ./target/debug/veld {{ARGS}}

# Run the daemon from source, foreground, against the dev DB — gives dev-DB
# runs their monitoring/GC. Stops the installed daemon service first (port
# 19899 is single-occupancy) and restarts it when you Ctrl-C out.
dev-daemon:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build -p veld-daemon
    mkdir -p .veld-dev
    restart_installed() {
        if launchctl print "gui/$(id -u)/dev.veld.daemon" &>/dev/null; then
            echo "Restarting installed daemon service..."
            launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon" 2>/dev/null || true
        fi
    }
    if launchctl print "gui/$(id -u)/dev.veld.daemon" &>/dev/null; then
        echo "Stopping installed daemon (restored on exit)..."
        launchctl kill SIGTERM "gui/$(id -u)/dev.veld.daemon" 2>/dev/null || true
        sleep 1
    fi
    trap restart_installed EXIT
    VELD_DB_PATH="{{dev_db}}" ./target/debug/veld-daemon

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
    printf '#!/usr/bin/env bash\nexport VELD_LIB_DIR="{{justfile_directory()}}/target/debug"\nexport VELD_DB_PATH="{{dev_db}}"\nexec "{{justfile_directory()}}/target/debug/veld" "$@"\n' > "$wrapper"
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
    cargo build

test:
    cargo test --workspace
    cd crates/veld-daemon/frontend && npm test

lint:
    cargo clippy --workspace --all-targets
    cd crates/veld-daemon/frontend && npx tsc --noEmit

build-frontend:
    cd crates/veld-daemon/frontend && npm run build

test-frontend:
    cd crates/veld-daemon/frontend && npm test

lint-frontend:
    cd crates/veld-daemon/frontend && npx tsc --noEmit

setup-frontend:
    cd crates/veld-daemon/frontend && npm install

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
