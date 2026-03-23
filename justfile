export PATH := env("HOME") + "/.cargo/bin:" + env("PATH")

# ============================================================================
# Veld Development Workflow
#
# Three tiers — use the lightest one that covers your change:
#
#   just dev <args>           CLI only, no install (most changes)
#   just dev-install-daemon   Install daemon (overlay/feedback changes)
#   just dev-install-helper   Install helper + restart Caddy (proxy changes, sudo)
#   just dev-install          CLI + daemon (no sudo)
#   just dev-install-all      Everything including helper (sudo)
#   just dev-restore          Go back to the released version
# ============================================================================

# --- Tier 1: Run CLI from source (no install, no side effects) ---

# Build and run veld from source. Does NOT touch the system install.
# Usage: just dev start --name foo website:local
dev *ARGS:
    cargo build
    VELD_LIB_DIR="{{justfile_directory()}}/target/debug" \
        ./target/debug/veld {{ARGS}}

# Create a `veld-dev` wrapper in ~/.local/bin for cross-project use.
dev-link:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cargo build
    mkdir -p "$HOME/.local/bin"
    wrapper="$HOME/.local/bin/veld-dev"
    printf '#!/usr/bin/env bash\nexport VELD_LIB_DIR="{{justfile_directory()}}/target/debug"\nexec "{{justfile_directory()}}/target/debug/veld" "$@"\n' > "$wrapper"
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
