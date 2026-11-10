---
format: 1
title: veld, veld-daemon and veld-helper
summary: Three binaries. The veld CLI is a thin client; veld-daemon runs the engine and the IDE; veld-helper owns DNS and Caddy routes.
aliases: [daemon, helper, veld-daemon, veld-helper, engine, caddy, three binaries, architecture]
tags: [internals]
related: [urls-and-https, fix-certificates-and-urls]
covers: [crates/veld-daemon/src/**, crates/veld-helper/**, crates/veld-engine/**]
---
- **`veld`**: the CLI. Sends start/stop/status/logs requests and draws the progress. Starts the
  daemon if it is not running.
- **`veld-daemon`**: user-space daemon. Hosts the engine; each start/stop runs in its own worker,
  so runs survive a daemon restart and `veld self update`. Also health monitoring, garbage
  collection, the dashboard and IDE. Log: `~/.veld/veld-daemon.log` (macOS),
  `journalctl --user -u veld-daemon` (Linux).
- **`veld-helper`**: DNS entries and Caddy routes, over a Unix socket. A root service in privileged
  mode (80/443), a user process in unprivileged mode (18443).

State lives in one SQLite database (`veld self backup list|now|restore`).

## What it is not
- No cloud service, no accounts. Everything runs on your machine.
