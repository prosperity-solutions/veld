# Veld Configuration Reference

## Schema

```json
{
  "$schema": "https://veld.oss.life.li/schema/v2/veld.schema.json",
  "schemaVersion": "2",
  "name": "myproject",
  "url_template": "{service}.{run}.{project}.localhost",
  "setup": [],
  "teardown": [],
  "presets": { },
  "nodes": { }
}
```

## Setup & Teardown

Project-level lifecycle steps. Not nodes — no variants, no health checks, no dependency graph.

**Setup** runs sequentially before any node. Non-zero exit aborts startup.
**Teardown** runs sequentially after all nodes stop. Best-effort (failures logged, not fatal).

```json
"setup": [
  { "name": "docker", "command": "docker info", "failureMessage": "Docker must be running" },
  { "name": "network", "command": "docker network create ${veld.name}-net 2>/dev/null || true" }
],
"teardown": [
  { "name": "network", "command": "docker network rm ${veld.name}-net 2>/dev/null || true" }
]
```

Step fields: `name` (required), `command` (required), `failureMessage` (optional).

Variables: `${veld.name}`, `${veld.project}`, `${veld.root}`, `${veld.run}`, plus shell env vars. No node-scoped vars (`${veld.port}`, `${nodes.*}`).

## Node Types

### `start_server` — Long-running processes

Must bind to `${veld.port}`. Requires a readiness probe (`probes.readiness` or legacy `health_check`).

```json
{
  "type": "start_server",
  "command": "npm run dev -- --port ${veld.port}",
  "probes": {
    "readiness": { "type": "http", "path": "/health", "timeout_seconds": 30 },
    "liveness": { "type": "http", "path": "/health", "interval_ms": 5000 }
  },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" },
  "outputs": { "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app" },
  "sensitive_outputs": ["DATABASE_URL"],
  "on_stop": "docker rm -f container-name"
}
```

### `command` — Run-to-completion tasks

Emits outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE`.

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "outputs": ["DATABASE_URL", "DB_NAME"],
  "skip_if": "./scripts/verify-db.sh",
  "probes": {
    "liveness": { "type": "command", "command": "pg_isready", "interval_ms": 5000 }
  }
}
```

## Probes

### Readiness (startup)

Every `start_server` variant requires a readiness probe. Use `probes.readiness` (preferred) or legacy `health_check`. Three types:

```json
{ "type": "http", "path": "/health", "expect_status": 200, "timeout_seconds": 30 }
{ "type": "port", "timeout_seconds": 15 }
{ "type": "command", "command": "./scripts/check-ready.sh", "timeout_seconds": 45 }
```

- `http`: Two-phase — TCP port check first, then HTTP. Default status: 200, path: `/`.
- `port`: Just checks TCP connection.
- `command`: Exit 0 = healthy.
- Defaults: `timeout_seconds`: 60, `interval_ms`: 1000 (min: 100).

### Liveness (ongoing)

Runs continuously after a node becomes healthy. Available for both `command` and `start_server` types. Same three check types as readiness: `http`, `port`, `command` (arbitrary shell command, exit 0 = healthy).

```json
"probes": {
  "liveness": {
    "type": "command",
    "command": "pg_isready -h localhost -p 5432",
    "interval_ms": 5000,
    "failure_threshold": 3,
    "max_recoveries": 3
  }
}
```

- `type`: `"http"`, `"port"`, or `"command"` — same semantics as readiness probes
- `command`: Shell command run via `sh -c`. Node outputs are available as env vars (e.g., `$DB_HOST`). Pipes, redirects, `&&` chains all work. 30s timeout.
- `interval_ms`: Check interval (default: 5000, min: 1000)
- `failure_threshold`: Consecutive failures before recovery (default: 3)
- `max_recoveries`: Max recovery attempts before permanent failure (default: 3)

Recovery = full environment restart (`veld restart`). After `max_recoveries` exhausted, node is permanently failed.

## Other Fields

| Field | Level | Description |
|-------|-------|-------------|
| `setup` | project | Lifecycle steps before graph execution. Array of `{name, command, failureMessage?}`. |
| `teardown` | project | Lifecycle steps after all nodes stop. Array of `{name, command, failureMessage?}`. Best-effort. |
| `env` | project, node, variant | Environment variables. Cascades: variant > node > project (per-key merge). Supports `${...}` substitution. |
| `cwd` | node, variant | Working directory. Relative paths resolve from project root. Variant overrides node. Supports `${...}` substitution. |
| `hidden` | node | Hide from `veld nodes` output |
| `client_log_levels` | project, node, variant | Browser log levels: `["log", "warn", "error", "info", "debug"]`. Exceptions always captured. |
| `features` | project, node, variant | `{"feedback_overlay": bool, "client_logs": bool, "inject": bool}`. All default `true`. |
| `on_stop` | variant | Per-node teardown command that runs on `veld stop`. |
| `sensitive_outputs` | variant | Output keys to mask in logs and encrypt at rest. |
| `skip_if` | variant (`command` only) | Idempotency check — skip step if exits 0. Alias: `verify`. |
| `probes` | variant | `{readiness?: HealthCheck, liveness?: LivenessProbe}`. Available for both node types. |
