---
format: 1
title: Hand a task off to a new worktree
summary: Create a new git worktree with a coding agent already started on a prompt, optionally pinned to an outfit profile, with veld ide worktree new.
aliases: [worktree new, veld ide worktree new, hand off, handoff, new worktree, --outfit, outfit profile, agent pane, prompt]
tags: [ide, agents]
related: [veld-ide]
covers: [crates/veld/src/commands/worktree.rs, crates/veld-daemon/src/outfit.rs, crates/veld-core/src/ide.rs, crates/veld/src/commands/skills/ide.md]
---
Run this from inside a project Veld knows.

1. `veld ide worktree new --branch <branch> --name "<title>" --prompt-file task.md`.
   The title is at most 18 columns; `--prompt "<text>"` works too.
2. Veld creates the worktree and starts the project's agent pane there with the prompt
   (`--agent <pane>` picks another one; `--group <g>` files it in the rail).
3. If the repo uses [outfit](https://outfit.oss.life.li) profiles, add `--outfit <profile>`.
   `ide.outfit` in `veld.json` (`false`, or `{ "required", "default" }`) tunes this.

Exit codes: 6 = no answer in time (check the rail, don't retry), 7 = an outfit profile is
needed or not offered (nothing created), 8 = created, but outfit could not apply the profile.

## What it does not do
Nothing reports back to the caller, and an existing branch is refused.
