---
format: 1
title: Find out why a run failed
summary: Read status, logs, internal logs and run history, lint the config, and diff two runs to see why an environment failed or a node is unhealthy.
aliases: [debug, logs, veld logs, run failed, crashed, unhealthy, lint, runs diff, config --why, troubleshoot run]
tags: [runs, troubleshooting]
related: [environments-and-runs, probes, veld-json]
covers: [crates/veld/src/commands/logs.rs, crates/veld/src/commands/runs.rs, crates/veld/src/commands/lint.rs, crates/veld/src/commands/config.rs, crates/veld/src/commands/status.rs]
---
1. `veld orchestrator status --name dev`: the run's outcome and each node's state.
2. `veld orchestrator logs --name dev --node <node>` (`-f` to follow, `-s <term>` to search).
3. `veld orchestrator logs --source internal`: probe transitions and recovery decisions.
4. `veld orchestrator runs list`, `runs show <id>`, and `runs diff <old> <new>` for what changed
   in the config between a good and a bad run. `logs --previous` reads the run before.
5. `veld orchestrator lint`: unknown `${veld.*}`, broken presets, secrets in a command line.
6. `veld orchestrator config --why <pointer>`: one effective value and where it was set.

Common causes: a node missing a variable from your shell (use `{ "env": "NAME" }`), a
frontend calling the backend's port instead of `${nodes.backend.url}`, an app CSP that blocks
the proxy origin (`veld agent skills troubleshooting`).

## What it does not do
It does not fix the machine. For certificate or helper problems see
[certificates and URLs](entry:fix-certificates-and-urls).
