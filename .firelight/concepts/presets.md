---
format: 1
title: Presets
summary: Named selections of nodes (fullstack, ui-only) with a stable number key, started with veld orchestrator start --preset.
aliases: [preset, preset key, default_preset, when_to_use, --preset, presets --pin]
tags: [config]
related: [start-a-preset, nodes-and-variants]
covers: [crates/veld-core/src/presets.rs, crates/veld/src/commands/presets.rs]
---
A preset names a set of `node:variant` selections. It can compose others with `@name`.

- Every preset shows a **key**, the number you can type instead of the name.
  `veld orchestrator presets --pin` prints the numbering to paste, so keys never move.
- `label`, `group` and `when_to_use` explain it to people and agents
  (`veld orchestrator presets --json`).
- `default_preset` is what a bare `veld orchestrator start` runs, also without a terminal.

## What it is not
- Not positional: start one with `--preset <name-or-key>`, never as a bare argument.
