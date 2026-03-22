# Build and install veld for local development (no sudo).
dev-setup:
    cargo build
    mkdir -p ~/.local/bin ~/.local/lib/veld
    cp ./target/debug/veld ~/.local/bin/veld
    cp ./target/debug/veld-helper ~/.local/lib/veld/veld-helper
    cp ./target/debug/veld-daemon ~/.local/lib/veld/veld-daemon
    ./target/debug/veld setup unprivileged
    @just _update-hammerspoon
    @just _restart-services

# Build and install with privileged ports (80/443, requires sudo once).
dev-setup-privileged:
    cargo build
    mkdir -p ~/.local/bin ~/.local/lib/veld
    cp ./target/debug/veld ~/.local/bin/veld
    cp ./target/debug/veld-helper ~/.local/lib/veld/veld-helper
    cp ./target/debug/veld-daemon ~/.local/lib/veld/veld-daemon
    sudo ./target/debug/veld setup privileged
    @just _update-hammerspoon
    @just _restart-services

# Update the Hammerspoon Spoon from source (if installed).
_update-hammerspoon:
    #!/usr/bin/env bash
    spoon_dir="$HOME/.hammerspoon/Spoons/Veld.spoon"
    if [ -d "$spoon_dir" ]; then
        cp integrations/hammerspoon/Veld.spoon/init.lua "$spoon_dir/init.lua"
        echo "Updated Hammerspoon Spoon."
    fi

# Restart daemon and helper so they pick up new binaries, then verify.
_restart-services:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "Restarting veld services..."

    # Remove stale daemon socket so the new process can bind cleanly.
    rm -f ~/.veld/daemon.sock

    # Restart daemon (user-level LaunchAgent).
    if launchctl list dev.veld.daemon &>/dev/null; then
        echo "  Restarting daemon..."
        launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon" || {
            echo "  ⚠ Daemon kickstart failed, trying bootout + bootstrap..."
            launchctl bootout "gui/$(id -u)/dev.veld.daemon" 2>/dev/null || true
            launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/dev.veld.daemon.plist 2>/dev/null || true
        }
    else
        echo "  Daemon service not loaded, skipping."
    fi

    # Restart helper (system-level LaunchDaemon in privileged mode).
    if sudo -n true 2>/dev/null; then
        if sudo launchctl list dev.veld.helper &>/dev/null 2>&1; then
            echo "  Restarting helper..."
            sudo launchctl kickstart -k "system/dev.veld.helper" || {
                echo "  ⚠ Helper kickstart failed, trying bootout + bootstrap..."
                sudo launchctl bootout "system/dev.veld.helper" 2>/dev/null || true
                sudo launchctl bootstrap system /Library/LaunchDaemons/dev.veld.helper.plist 2>/dev/null || true
            }
        fi
    else
        # Check user-level helper (unprivileged mode).
        if launchctl list dev.veld.helper &>/dev/null; then
            echo "  Restarting helper..."
            launchctl kickstart -k "gui/$(id -u)/dev.veld.helper" || true
        fi
    fi

    # Wait for services to come up and verify.
    echo "  Waiting for services..."
    sleep 2

    ok=true
    if ! curl -sf http://localhost:2019/id/veld-sentinel >/dev/null 2>&1; then
        echo "  ✗ Caddy sentinel not responding"
        ok=false
    else
        echo "  ✓ Caddy running"
    fi

    # Daemon may take a moment longer to bind its socket.
    for i in 1 2 3 4 5; do
        if curl -sf http://127.0.0.1:19899/api/environments >/dev/null 2>&1; then
            echo "  ✓ Daemon running (management UI available)"
            break
        fi
        if [ "$i" -eq 5 ]; then
            echo "  ✗ Daemon not responding on port 19899"
            ok=false
        fi
        sleep 1
    done

    if [ "$ok" = true ]; then
        echo "Done — all services running."
    else
        echo ""
        echo "Some services failed to start. Run 'veld doctor' for details."
    fi

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets

# Build frontend TypeScript assets (requires Node.js + npm).
build-frontend:
    cd crates/veld-daemon/frontend && npm run build

# Run frontend tests.
test-frontend:
    cd crates/veld-daemon/frontend && npm test

# Type-check frontend without emitting.
lint-frontend:
    cd crates/veld-daemon/frontend && npm run typecheck

# Install frontend dependencies (run once after clone).
setup-frontend:
    cd crates/veld-daemon/frontend && npm install
