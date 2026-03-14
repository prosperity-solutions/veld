dev-setup:
    cargo build
    sudo cp ./target/debug/veld /usr/local/bin/veld
    sudo ./target/debug/veld setup --force

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets
