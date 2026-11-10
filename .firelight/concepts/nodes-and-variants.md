---
format: 1
title: Nodes and variants
summary: A node is one service; a variant is one way to run it (local, docker, remote). You start node:variant, and depends_on decides the order.
aliases: [node, variant, service, depends_on, long_running, start_server, command node, node:variant, dependency graph, step type]
tags: [config]
related: [veld-json, presets, probes]
covers: [crates/veld-core/src/graph.rs, crates/veld-engine/**]
---
- A **node** is a service in `nodes`. Each has `variants` and a `default_variant`.
- You select with `node:variant`, e.g. `veld orchestrator start frontend:local --name dev`.
- A variant's `type` is its lifecycle:
  - `long_running`: supervised for the life of the run; gets a port and URL by default; a
    readiness probe is mandatory. `start_server` is a permanent alias.
  - `command`: runs to completion (a build, a migration); writes outputs as `key=value` lines to
    `$VELD_OUTPUT_FILE`.
- `depends_on` builds the graph: dependencies start first, in parallel where possible, and stop
  in reverse order. `veld orchestrator graph` prints it; `veld orchestrator nodes` lists all.

## What it is not
- Not a container. A node runs a local process; that process may start a container.
- Nodes do not inherit your terminal's environment: name a value with `{ "env": "NAME" }`.
