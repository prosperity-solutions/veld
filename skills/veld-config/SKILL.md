---
name: veld-config
description: Write and edit veld.json configuration files. Use when the user asks to configure Veld, add nodes/services, set up dependencies, create presets, configure health checks, define URL templates, or troubleshoot veld.json issues.
metadata:
  author: prosperity-solutions
  version: "1.0.0"
---

# Veld Configuration

Write correct `veld.json` files for Veld, the local development environment orchestrator.

## Schema Reference

Always include the `$schema` field for editor autocompletion:

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "myproject",
  "nodes": { }
}
```

## Top-Level Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `$schema` | string | No | Schema URL for editor autocompletion |
| `schemaVersion` | string | Yes | Must be `"1"` |
| `name` | string | Yes | Project name (alphanumeric, dots, hyphens, underscores) |
| `url_template` | string | No | Default: `{service}.{run}.{project}.localhost` |
| `presets` | object | No | Named shortcuts for node:variant selections |
| `client_log_levels` | array | No | Browser log levels to capture: `["log", "warn", "error"]` (default). Valid: `"log"`, `"warn"`, `"error"`, `"info"`, `"debug"`. Exceptions always captured. |
| `features` | object | No | Feature toggles: `{"feedback_overlay": bool, "client_logs": bool}`. All default `true`. Cascades: variant > node > project. |
| `nodes` | object | Yes | At least one node required |

## Node-Level Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `default_variant` | string | No | The variant to use when none is specified |
| `url_template` | string | No | URL template override for all variants of this node |
| `hidden` | boolean | No | Hide from `veld nodes` output (default: `false`) |
| `client_log_levels` | array | No | Client-side log levels override |
| `features` | object | No | Feature toggles override |
| `cwd` | string | No | Working directory for all variants. Relative paths resolve from the project root. Overridable at variant level. Supports `${...}` variable substitution. |
| `variants` | object | Yes | At least one variant required |

## Node Types

### `start_server` — Long-Running Processes

For dev servers, databases, any process that stays alive. Veld allocates a port and gives it to the process via `${veld.port}`. The process **must** bind to this port.

```json
{
  "type": "start_server",
  "command": "npm run dev -- --port ${veld.port}",
  "health_check": { "type": "http", "path": "/health", "timeout_seconds": 30 },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" }
}
```

### `command` — Run-to-Completion Tasks

For setup steps, migrations, data seeding. Can emit outputs via stdout.

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "outputs": ["DATABASE_URL", "DB_NAME"],
  "verify": "./scripts/verify-db.sh"
}
```

The script emits outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE` (preferred). The legacy method of printing `VELD_OUTPUT key=value` to stdout is still supported but discouraged as it exposes values in the terminal.

## Health Checks

Every `start_server` variant **requires** a health check. Three types:

```json
{ "type": "http", "path": "/health", "expect_status": 200, "timeout_seconds": 30 }
{ "type": "port", "timeout_seconds": 15 }
{ "type": "command", "command": "./scripts/check-ready.sh", "timeout_seconds": 45 }
```

- `http`: Two-phase — TCP port check first, then HTTP endpoint. Default status: 200. Default path: `/`.
- `port`: Simplest — just checks TCP connection.
- `command`: Exit 0 = healthy.
- Default `timeout_seconds`: 60. Default `interval_ms`: 1000 (min: 100).

## Variable Interpolation

### In commands, scripts, and env values: `${...}`

| Variable | Description |
|----------|-------------|
| `${veld.port}` | Allocated port (only for `start_server`) |
| `${veld.url}` | Full HTTPS URL (only for `start_server`) |
| `${veld.url.hostname}` | DNS name only (e.g. `app.my-run.proj.localhost`) |
| `${veld.url.host}` | hostname:port (omits port if 443) |
| `${veld.url.origin}` | scheme + host (same as `${veld.url}`) |
| `${veld.url.scheme}` | Protocol scheme (`https`) |
| `${veld.url.port}` | HTTPS port (note: `${veld.port}` is the backend bind port) |
| `${veld.run}` | Run name |
| `${veld.root}` | Absolute path to directory containing veld.json |
| `${veld.project}` | Project name |
| `${veld.branch}` | Current git branch (slugified) |
| `${veld.worktree}` | Worktree directory name (slugified) |
| `${veld.username}` | OS username |
| `${nodes.<node>.<output>}` | Output from another node |
| `${nodes.<node>.url}` | Built-in: HTTPS URL of a start_server node |
| `${nodes.<node>.url.hostname}` | Built-in: DNS name of a start_server node |
| `${nodes.<node>.url.host}` | Built-in: hostname:port of a start_server node |
| `${nodes.<node>.url.origin}` | Built-in: scheme + host of a start_server node |
| `${nodes.<node>.url.scheme}` | Built-in: protocol scheme of a start_server node |
| `${nodes.<node>.url.port}` | Built-in: HTTPS port of a start_server node |
| `${nodes.<node>.port}` | Built-in: allocated port of a start_server node |

When two variants of the same node run simultaneously, use qualified references: `${nodes.backend:local.url}`.

### In URL templates: `{...}`

| Variable | Description |
|----------|-------------|
| `{service}` | Node name |
| `{run}` | Run name |
| `{project}` | Project name |
| `{branch}` | Git branch |
| `{worktree}` | Worktree dir name |
| `{username}` | OS username |
| `{hostname}` | Machine hostname |

Fallback operator: `{branch ?? run}` uses the first non-empty value. Since `{run}` is always non-empty, it makes a good final fallback.

URL templates cascade: variant-level overrides node-level overrides project-level.

## Dependencies

Explicit `node → variant` mapping. Default variants are never silently assumed.

```json
"depends_on": {
  "database": "docker",
  "backend": "local"
}
```

Dependencies start before dependents. Independent branches run in parallel. Teardown is reverse order.

## Outputs

### `start_server` outputs (object — templates interpolated with `${veld.port}` etc.)
```json
"outputs": {
  "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app"
}
```
Built-in outputs: `url`, `port`.

### `command` outputs (array — emitted via stdout)
```json
"outputs": ["DATABASE_URL", "DB_NAME"]
```
Built-in output: `exit_code`.

### Sensitive outputs (masked in logs, encrypted at rest)
```json
"sensitive_outputs": ["DATABASE_URL"]
```

## Presets

Named shortcuts for common selections:

```json
"presets": {
  "fullstack": ["frontend:local", "backend:local", "database:docker"],
  "ui-only": ["frontend:local", "backend:staging"]
}
```

Used with `veld start --preset fullstack --name my-feature`.

## Teardown

Optional cleanup command that runs on `veld stop`:

```json
"on_stop": "docker rm -f veld-db-${veld.run}"
```

## Complete Example

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "my-saas",
  "url_template": "{service}.{branch ?? run}.{project}.localhost",
  "presets": {
    "fullstack": ["frontend:local", "admin:local"],
    "backend-only": ["backend:local"]
  },
  "nodes": {
    "database": {
      "hidden": true,
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm -p ${veld.port}:5432 -e POSTGRES_PASSWORD=veld postgres:16",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app"
          },
          "sensitive_outputs": ["DATABASE_URL"],
          "on_stop": "docker rm -f $(docker ps -q --filter publish=${veld.port})"
        }
      }
    },
    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "database": "docker" },
          "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" }
        }
      }
    },
    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "npm run dev -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": { "NEXT_PUBLIC_API_URL": "${nodes.backend.url}" }
        }
      }
    },
    "admin": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "npm run dev -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": { "NEXT_PUBLIC_API_URL": "${nodes.backend.url}" }
        }
      }
    }
  }
}
```

## Common Patterns

### Monorepo with workspaces
Set `cwd` at the node level so all commands run from the right subdirectory. This replaces verbose `cd <dir> && ...` prefixes:

```json
"api": {
  "cwd": "packages/api",
  "variants": {
    "local": {
      "type": "start_server",
      "command": "pnpm run dev --port ${veld.port}",
      "on_stop": "./scripts/cleanup.sh",
      "health_check": { "type": "http", "path": "/health" }
    }
  }
}
```

`cwd` cascades: variant-level overrides node-level. Supports variable substitution (e.g., `"cwd": "${veld.project}/api"`). Relative paths resolve from the veld.json directory.

### Branch-based URLs
Use `{branch ?? run}` in `url_template` so each git branch gets its own URL namespace, falling back to run name if not in a git repo.

### Docker services
Use `start_server` with a `docker run` command, `port` health check, and `on_stop` for cleanup. Map `${veld.port}` to the container's exposed port.

### Setup steps with idempotency
Use `command` type with `verify` — the verify script runs first, and if it exits 0, the main command is skipped.

### Wiring environment variables between nodes
Use `${nodes.<node>.<output>}` in the `env` block of dependent nodes. Veld resolves these after dependencies complete.

### Client-side log levels

Captures browser console output from `start_server` nodes. Cascades: variant > node > project.

```json
"client_log_levels": ["log", "warn", "error", "info", "debug"]
```

Set at project, node, or variant level. Unhandled exceptions are always captured.

### Feature toggles

Control which Veld capabilities are injected into `start_server` HTML responses. Cascades: variant > node > project.

```json
"features": { "feedback_overlay": false, "client_logs": true }
```

Available: `feedback_overlay` (toolbar/comments), `client_logs` (browser log collector). All default `true`.

## Common Mistakes

- Forgetting `health_check` on `start_server` variants (required)
- Using `${veld.port}` in a `command` variant (only available for `start_server`)
- Using `{...}` syntax in commands (that's for URL templates — use `${...}` in commands)
- Not specifying the variant in `depends_on` (e.g., writing `"backend"` instead of `"backend": "local"`)
- Setting `outputs` as an object on `command` variants (must be an array of strings)
- Setting `outputs` as an array on `start_server` variants (must be an object of key:template pairs)
