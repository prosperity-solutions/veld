dev-setup:
    cargo build
    sudo ./target/debug/veld setup --force

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets
