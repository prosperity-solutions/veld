dev-setup:
    cargo build
    sudo cp ./target/debug/veld /usr/local/bin/veld
    sudo cp ./target/debug/veld-helper /usr/local/lib/veld/veld-helper
    sudo cp ./target/debug/veld-daemon /usr/local/lib/veld/veld-daemon
    sudo ./target/debug/veld setup --force

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets
