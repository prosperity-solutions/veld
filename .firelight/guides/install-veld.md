---
format: 1
title: Install veld
summary: Install the veld CLI, daemon and helper with one curl command, without sudo, then optionally run the one-time setup for clean URLs.
aliases: [install, installation, get veld, curl install, setup, veld self setup, VELD_VERSION, build from source]
tags: [setup]
links: [install-script]
related: [urls-and-https, first-environment, update-veld]
covers: [install.sh, crates/veld/src/commands/setup/**, crates/veld/src/commands/skills/install.md]
---
Use this on a new macOS (arm64/x64) or Linux (x64/arm64) machine.

1. Run `curl -fsSL https://veld.oss.life.li/get | bash`. It installs `veld` to `~/.local/bin/`
   and `veld-helper` and `veld-daemon` to `~/.local/lib/veld/`. No sudo.
2. Make sure `~/.local/bin` is on your `PATH`.
3. Optional, once: `veld self setup privileged` (sudo once; URLs without `:18443`, custom apex
   domains) or `veld self setup unprivileged` (no sudo, port 18443). Without it, the first
   command bootstraps unprivileged mode.
4. Check with `veld self doctor`.

A specific version: `VELD_VERSION=1.0.0 curl -fsSL https://veld.oss.life.li/get | bash`.
From source: `cargo build --release`. In containers or CI without launchd/systemd set
`VELD_ALLOW_UNMANAGED_HELPER=1`.

## What it does not do
It does not install Veld Desktop unless you say yes; see [Veld Desktop](entry:veld-ide).
