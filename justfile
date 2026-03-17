# Build and install veld for local development (no sudo).
dev-setup:
    cargo build
    mkdir -p ~/.local/bin ~/.local/lib/veld
    cp ./target/debug/veld ~/.local/bin/veld
    cp ./target/debug/veld-helper ~/.local/lib/veld/veld-helper
    cp ./target/debug/veld-daemon ~/.local/lib/veld/veld-daemon
    ./target/debug/veld setup unprivileged

# Build and install with privileged ports (80/443, requires sudo once).
dev-setup-privileged:
    cargo build
    mkdir -p ~/.local/bin ~/.local/lib/veld
    cp ./target/debug/veld ~/.local/bin/veld
    cp ./target/debug/veld-helper ~/.local/lib/veld/veld-helper
    cp ./target/debug/veld-daemon ~/.local/lib/veld/veld-daemon
    sudo ./target/debug/veld setup privileged

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets
