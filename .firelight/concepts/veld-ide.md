---
format: 1
title: Veld IDE, dashboard and Veld Desktop
summary: The browser dashboard (https://veld.localhost), the /ide worktree view with terminal, agent and browser panes, and Veld Desktop, its macOS/Linux app.
aliases: [veld desktop, desktop app, dashboard, management ui, ide, worktree, panes, agent pane, veld.localhost, feedback, veld feedback]
tags: [ide]
links: [releases]
related: [hand-off-to-a-worktree, update-veld]
covers: [desktop/**, crates/veld-daemon/ui/**, crates/veld-core/src/ide.rs, crates/veld/src/commands/desktop.rs, crates/veld/src/commands/ide.rs]
---
- **Dashboard** at `https://veld.localhost` (`:18443` unprivileged; `veld ide dashboard` opens it):
  runs, health, logs, stop/restart, sharing.
- **IDE** at `/ide`: one entry per git worktree, with terminal panes, coding-agent panes
  (`ide.panes`), browser panes and a read-only file pane. Projects add badges and buttons with
  `ide.extensions`. Agents use `veld ide open`, `veld ide link set` and `veld ide worktree new`.
- **Veld Desktop**: the same `/ide` in a native window. It needs the veld CLI.
  `veld self desktop install|uninstall|status`; also a `.dmg`/`.AppImage`/`.deb` on
  [the releases page](link:releases). `veld self update` keeps both in step.

## What it is not
- Not a second implementation: the panes live in the daemon, so a browser tab and the app show the same ones.
- No in-browser feedback threads anymore: the `feedback` command and its overlay were removed in #424,
  with no replacement.
