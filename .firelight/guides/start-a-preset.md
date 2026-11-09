---
format: 1
title: Start a preset
summary: List the project's presets with their keys and start one by name or number with --preset.
aliases: [preset, --preset, presets, preset key, default preset]
tags: [runs]
related: [presets, first-environment]
covers: [crates/veld/src/commands/presets.rs, crates/veld-core/src/presets.rs]
---
1. `veld orchestrator presets` lists each preset with its key, label and `when_to_use`.
2. `veld orchestrator start --preset <name-or-key> --name dev`.
3. A bare `veld orchestrator start` prompts (Enter takes `default_preset`), or without a
   terminal runs `default_preset` directly.

To freeze the numbering: `veld orchestrator presets --pin` and paste the block into `veld.json`.

## What it does not do
A preset name is never a positional argument; `start fullstack` means a node called `fullstack`.
