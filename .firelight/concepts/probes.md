---
format: 1
title: Health probes
summary: Readiness probes gate startup; liveness probes detect a failure later and trigger automatic recovery. Types http, port, command, settle.
aliases: [health check, readiness, liveness, probe, recovery, settle, healthcheck]
tags: [config, runs]
related: [nodes-and-variants, debug-a-run]
covers: [crates/veld-core/src/health.rs]
---
- `probes.readiness` is mandatory on a `long_running` node. Dependents wait until it passes.
- `probes.liveness` keeps checking after startup; on failure veld restarts the environment
  (with a failure threshold and a cap on recovery attempts).
- Types: `http` (path, expected status), `port`, `command`, `settle` (still running after N seconds;
  readiness only, for nodes with no port).
- Probe transitions and recovery decisions are in `veld orchestrator logs --source internal`.

## What it is not
- `settle` is not a liveness check; veld rejects it there.
- An unknown probe type is an error, never a silent "healthy".
