---
format: 1
title: Start your first environment
summary: Write a veld.json, start a node with veld orchestrator start --name dev, read its URLs, and stop it again.
aliases: [quick start, getting started, start services, start my services, veld orchestrator start, veld orchestrator init, run locally, local environment]
tags: [runs]
related: [veld-json, environments-and-runs, start-a-preset]
covers: [crates/veld/src/commands/init.rs, crates/veld/src/commands/start.rs, crates/veld/src/commands/status.rs, crates/veld/src/commands/urls.rs, crates/veld/src/commands/stop.rs]
---
Use this in a project with no `veld.json` yet.

1. `veld orchestrator init` writes a `veld.json` (or write one by hand with
   `"$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json"` and `"schemaVersion": "3"`).
2. Declare your services as [nodes](entry:nodes-and-variants), each with a readiness probe.
3. `veld orchestrator start frontend:local --name dev`. Veld starts dependencies first, waits
   for their probes, and prints the HTTPS URLs.
4. `veld orchestrator status --name dev` and `veld orchestrator urls --name dev`.
5. `veld orchestrator stop --name dev`.

`start` returns once everything is up; add `--attach` to follow the logs (Ctrl+C stops it).

## What it does not do
Nodes don't see your shell's variables unless named with `{ "env": "NAME" }`.
