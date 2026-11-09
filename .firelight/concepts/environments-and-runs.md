---
format: 1
title: Environments and runs
summary: An environment is a named slot (--name dev); a run is one execution of it, kept as history with its logs and outcome.
aliases: [environment, run, run history, --name, runs list, oneshot, named environment]
tags: [runs]
related: [debug-a-run, first-environment]
covers: [crates/veld/src/commands/runs.rs, crates/veld/src/commands/start.rs, crates/veld/src/commands/skills/runs.md]
---
- **Environment**: the durable name you pass with `--name dev`. Several can run side by side.
  Starting the same name again is idempotent.
- **Run**: one execution of an environment, with an id, start/end time and outcome
  (stopped, failed, crashed). The last 10 per environment are kept for 7 days, with logs.
- `veld orchestrator runs list|show|diff` reads the history; `logs --run <id>` or
  `logs --previous` reads a past run's logs.
- `start` returns once the environment is up: the daemon owns the run, not your terminal.
  `--attach` follows the logs; `--oneshot` runs a command node to completion and exits with its code.

## What it is not
- An environment is not a git branch or worktree, though `url_template` can use `{branch}`.
