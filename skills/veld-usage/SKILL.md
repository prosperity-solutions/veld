---
name: veld-usage
description: Use the Veld CLI to manage local development environments. Use when the user asks to start, stop, restart, or check the status of environments, view logs, list URLs, debug environment issues, or run any veld command.
allowed-tools: Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "2.0.0"
---

# Veld CLI Usage

Veld orchestrates local dev environments. It starts services from `veld.json`, wires dependencies, and gives each service an HTTPS URL like `https://frontend.my-feature.myproject.localhost`.

## Current Project State

### Available nodes and variants
!`veld nodes 2>&1`

### Available presets
!`veld presets 2>&1`

### Active runs
!`veld runs 2>&1`

## Core Commands

```bash
# Start — resolves deps, allocates ports, starts processes, configures HTTPS
veld start frontend:local --name my-feature
veld start --preset fullstack --name dev
veld start                          # interactive mode (TTY only)
veld start frontend:local --name dev --attach  # foreground, stream logs

# Stop / restart
veld stop --name dev
veld stop --all
veld restart --name dev

# Status and URLs
veld status --name dev
veld status --name dev --outputs    # include env vars, ports
veld urls --name dev

# Logs
veld logs --name dev                          # last 50 lines, all nodes
veld logs --name dev --node backend           # single node
veld logs --name dev --lines 200              # more lines
veld logs --name dev --since 5m               # last 5 minutes
veld logs --name dev --follow                 # stream (like tail -f)
veld logs --name dev --search "error"         # filter by term
veld logs --name dev --search "timeout" --context 3

# Explore
veld nodes                          # list nodes/variants
veld presets                        # list presets
veld graph frontend:local           # print dependency graph
veld runs                           # list all runs
veld list                           # list all veld projects on machine

# Feedback (see veld-feedback skill for full workflow)
veld feedback listen --name dev --json
veld feedback answer --name dev --thread <id> "Fixed it"
veld feedback ask --name dev "Which shade of blue?"

# Setup / maintenance
veld init                           # create veld.json interactively
veld ui                             # open dashboard (https://veld.localhost)
veld doctor                         # diagnose installation health
veld gc                             # clean up stale state
veld update                         # update veld
```

## Key Details

- **Ports**: Allocated from 19000–29999. Never hardcode.
- **Working directory**: Commands run from the veld.json directory, not your CWD.
- **TLS**: Caddy handles HTTPS automatically after `veld setup`.
- **Name resolution**: If `--name` omitted — one run → auto-selects; multiple → prompts; none → error.
- **Multiple runs**: Each is isolated with its own ports and URLs.
- **State**: Stored in `~/.local/share/veld/`.
- **JSON output**: Most commands accept `--json` for machine-readable output.

## Debugging a Service

```bash
veld status --name dev              # check node states and health
veld logs --name dev --node <node> --lines 100  # check recent logs
veld status --name dev --outputs    # check ports, env vars
veld doctor                         # check system-level issues
```
