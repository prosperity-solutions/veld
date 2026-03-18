# Build and install veld for local development (no sudo).
dev-setup:
    cargo build
    mkdir -p ~/.local/bin ~/.local/lib/veld
    cp ./target/debug/veld ~/.local/bin/veld
    cp ./target/debug/veld-helper ~/.local/lib/veld/veld-helper
    cp ./target/debug/veld-daemon ~/.local/lib/veld/veld-daemon
    ./target/debug/veld setup unprivileged
    @just _restart-services

# Build and install with privileged ports (80/443, requires sudo once).
dev-setup-privileged:
    cargo build
    mkdir -p ~/.local/bin ~/.local/lib/veld
    cp ./target/debug/veld ~/.local/bin/veld
    cp ./target/debug/veld-helper ~/.local/lib/veld/veld-helper
    cp ./target/debug/veld-daemon ~/.local/lib/veld/veld-daemon
    sudo ./target/debug/veld setup privileged
    @just _restart-services

# Restart daemon and helper so they pick up new binaries.
_restart-services:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Restarting veld services..."
    if launchctl list dev.veld.daemon &>/dev/null; then
        launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon" 2>/dev/null || true
    fi
    if sudo -n launchctl list dev.veld.helper &>/dev/null 2>&1; then
        sudo -n launchctl kickstart -k "system/dev.veld.helper" 2>/dev/null || true
    fi
    echo "Done."

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets
