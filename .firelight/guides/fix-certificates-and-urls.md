---
format: 1
title: Fix certificate and URL errors
summary: When the browser rejects a veld URL (ERR_CERT_DATE_INVALID, not trusted) or a URL does not resolve, check veld self doctor and re-run setup.
aliases: [certificate expired, ERR_CERT_DATE_INVALID, cert error, not trusted, https error, veld self doctor, doctor, url not working, caddy log, 18443]
tags: [troubleshooting, setup]
related: [urls-and-https, architecture, install-veld]
covers: [crates/veld/src/commands/doctor.rs, crates/veld-helper/**, crates/veld/src/commands/skills/troubleshooting.md]
---
1. Run `veld self doctor`. Read the `Certificate:` and `CA:` rows and the Installation block.
2. `Certificate: EXPIRED`: the helper restarts Caddy by itself; run doctor again. If it stays
   expired, read `~/.local/lib/veld/caddy-data/caddy.log` (doctor prints the path).
3. `CA: not trusted`: re-run `veld self setup privileged` or `veld self setup unprivileged`.
4. `NOT VALID for another …`: your clock is behind. Fix the clock.
5. URLs carry `:18443`: you are in unprivileged mode. `veld self setup privileged` removes it.
6. Daemon problems: `~/.veld/veld-daemon.log` (macOS), `journalctl --user -u veld-daemon` (Linux).

Exit code 75 on any command means an update is running (`veld self update --status`).

## What it does not do
It does not debug your app's own errors; see [why a run failed](entry:debug-a-run).
