---
name: veld
description: >
  Veld local development environment orchestrator. Use when the user asks to
  start/stop/restart environments, view logs or status, configure veld.json
  (add nodes, services, dependencies, presets, health checks, URL templates),
  get human feedback on UI changes, debug environment issues, or run any veld command.
allowed-tools: Read, Edit, Bash(veld *), Bash(cat veld.json)
metadata:
  author: prosperity-solutions
  version: "3.0.0"
---

# Veld

Veld orchestrates local dev environments. It starts services from `veld.json`, wires dependencies, and gives each service an HTTPS URL like `https://frontend.my-feature.myproject.localhost`.

## Live State

### Configuration
!`cat veld.json 2>&1`

### Nodes & presets
!`veld nodes 2>&1`
!`veld presets 2>&1`

### Active runs
!`veld runs 2>&1`

## CLI

!`veld --help 2>&1`

Run `veld <subcommand> --help` for flags and options.

## Editing veld.json

For the full config schema, variables, and node types, see [reference/config.md](reference/config.md).

Quick reference for the two node types:

**`start_server`** — long-running process. Must bind to `${veld.port}`. Requires `health_check`.
```json
{
  "type": "start_server",
  "command": "npm run dev -- --port ${veld.port}",
  "health_check": { "type": "http", "path": "/health" },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" }
}
```

**`command`** — run-to-completion. Emits outputs via `$VELD_OUTPUT_FILE`.
```json
{
  "type": "command",
  "script": "./scripts/setup.sh",
  "outputs": ["DATABASE_URL"],
  "verify": "./scripts/check.sh"
}
```

## Feedback Loop

For the full feedback workflow, events, and thread fields, see [reference/feedback.md](reference/feedback.md).

Core pattern: listen → fix → answer → listen again with `--after <seq>` → repeat until `session_ended`.

## Gotchas

- **`health_check` is required** on every `start_server` variant — veld will reject config without it
- **`depends_on` needs the variant** — write `"backend": "local"`, not just `"backend"`
- **`${...}` vs `{...}`** — `${veld.port}` in commands/env, `{service}` in URL templates. Mixing them up silently produces wrong values.
- **`outputs` shape differs by type** — object (`{"KEY": "template"}`) for `start_server`, array (`["KEY"]`) for `command`
- **`${veld.port}` is only for `start_server`** — `command` variants don't get an allocated port
- **Ports are dynamic** (19000–29999) — never hardcode a port in veld.json or dependent config
- **Commands run from veld.json directory**, not your CWD — use `cwd` field if a node needs a different working directory
- **Name resolution** — if `--name` omitted: one run → auto-selects, multiple → prompts, none → errors
- **`--json`** — most commands accept it for machine-readable output, prefer it when parsing results
