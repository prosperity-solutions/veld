---
format: 1
title: Update or uninstall veld
summary: Update the whole release (CLI, daemon, helper, Veld Desktop) with veld self update; install or remove the desktop app; uninstall veld.
aliases: [update, upgrade, veld self update, latest version, uninstall, remove veld, veld self desktop install, install desktop app, gc]
tags: [setup]
links: [releases]
related: [install-veld, veld-ide]
covers: [crates/veld/src/commands/update.rs, crates/veld/src/commands/desktop.rs, crates/veld/src/commands/uninstall.rs, crates/veld/src/commands/gc.rs]
---
- `veld self update`: CLI, daemon, helper and (if you want it) Veld Desktop, from one release.
  Running environments keep running. `--status` shows an update in progress.
- `veld self desktop install`: add the macOS app and keep it updated.
  `veld self desktop uninstall`: remove it and stop fetching it. `veld self desktop status`.
- `veld self gc`: clean up stale state and logs.
- `veld self uninstall`: remove veld.

## What it does not do
Only one update runs at a time; others exit 75 until it is done.
