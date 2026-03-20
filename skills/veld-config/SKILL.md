---
name: veld-config
description: Write and edit veld.json configuration files. Use when the user asks to configure Veld, add nodes/services, set up dependencies, create presets, configure health checks, define URL templates, or troubleshoot veld.json issues.
allowed-tools: Read, Edit, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "2.0.0"
---

# Veld Configuration

Write correct `veld.json` files for Veld, the local development environment orchestrator.

## Current Configuration

!`cat veld.json 2>&1`

## Schema

Always include `$schema` for editor autocompletion:

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "myproject",
  "url_template": "{service}.{run}.{project}.localhost",
  "presets": { },
  "nodes": { }
}
```

## Node Types

### `start_server` — Long-running processes

For dev servers, databases, anything that stays alive. Must bind to `${veld.port}`. Requires `health_check`.

```json
{
  "type": "start_server",
  "command": "npm run dev -- --port ${veld.port}",
  "health_check": { "type": "http", "path": "/health", "timeout_seconds": 30 },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" },
  "outputs": { "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app" },
  "sensitive_outputs": ["DATABASE_URL"],
  "on_stop": "docker rm -f container-name"
}
```

### `command` — Run-to-completion tasks

For setup steps, migrations, seeding. Emits outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE`.

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "outputs": ["DATABASE_URL", "DB_NAME"],
  "verify": "./scripts/verify-db.sh"
}
```

## Health Checks

Every `start_server` variant **requires** one. Three types:

```json
{ "type": "http", "path": "/health", "expect_status": 200, "timeout_seconds": 30 }
{ "type": "port", "timeout_seconds": 15 }
{ "type": "command", "command": "./scripts/check-ready.sh", "timeout_seconds": 45 }
```

Defaults: `timeout_seconds`: 60, `interval_ms`: 1000 (min: 100).

## Variable Interpolation

### In commands, scripts, env values: `${...}`

| Variable | Description |
|----------|-------------|
| `${veld.port}` | Allocated port (`start_server` only) |
| `${veld.url}` | Full HTTPS URL (`start_server` only) |
| `${veld.url.hostname}` | DNS name only |
| `${veld.url.host}` | hostname:port |
| `${veld.url.origin}` | scheme + host |
| `${veld.url.scheme}` | Protocol (`https`) |
| `${veld.url.port}` | HTTPS port |
| `${veld.run}` | Run name |
| `${veld.root}` | Absolute path to veld.json directory |
| `${veld.project}` | Project name |
| `${veld.branch}` | Current git branch (slugified) |
| `${veld.worktree}` | Worktree directory name (slugified) |
| `${veld.username}` | OS username |
| `${nodes.<node>.<output>}` | Output from another node |
| `${nodes.<node>.url}` | HTTPS URL of a start_server node |
| `${nodes.<node>.port}` | Allocated port of a start_server node |

Qualified references when two variants run: `${nodes.backend:local.url}`.

### In URL templates: `{...}`

`{service}`, `{run}`, `{project}`, `{branch}`, `{worktree}`, `{username}`, `{hostname}`

Fallback: `{branch ?? run}` — uses first non-empty value. Cascades: variant > node > project.

## Dependencies

Explicit `node → variant` mapping. Default variants are **never** silently assumed.

```json
"depends_on": { "database": "docker", "backend": "local" }
```

## Presets

Named shortcuts:

```json
"presets": {
  "fullstack": ["frontend:local", "backend:local", "database:docker"],
  "ui-only": ["frontend:local", "backend:staging"]
}
```

## Other Fields

| Field | Level | Description |
|-------|-------|-------------|
| `cwd` | node, variant | Working directory. Relative paths resolve from project root. Variant overrides node. |
| `hidden` | node | Hide from `veld nodes` output |
| `client_log_levels` | project, node, variant | Browser log levels: `["log", "warn", "error", "info", "debug"]` |
| `features` | project, node, variant | `{"feedback_overlay": bool, "client_logs": bool}`. All default `true`. |

## Common Mistakes

- Missing `health_check` on `start_server` variants (required)
- Using `${veld.port}` in `command` variants (only for `start_server`)
- Using `{...}` syntax in commands (that's URL templates — use `${...}`)
- Not specifying variant in `depends_on` (`"backend"` instead of `"backend": "local"`)
- `outputs` as object on `command` (must be array) or array on `start_server` (must be object)
