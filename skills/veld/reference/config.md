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

## Actions

Node-level `actions` are named shell commands exposed via the CLI (`veld action <name>`, `veld actions`) and as buttons on the node's row in the management dashboard. They generalize integrations like "open the database in a GUI client" — define them in `veld.json` instead of relying on built-in commands.

Actions are **node-scoped**: each action belongs to the node it's declared under and can only reference that node's outputs. An action is available only while its node is running and exposes every key in `requires_outputs`; otherwise it doesn't appear in `veld actions`/`veld action`, no dashboard button renders, and it never runs. (There is no project-level / generic action and no cross-node output access.)

```json
"database": {
  "actions": [
    {
      "name": "psql",
      "label": "psql",
      "description": "Open a psql shell to the DB clone",
      "requires_outputs": ["DB_HOST", "DB_PORT", "DB_NAME", "DB_USER", "DB_PASS"],
      "command": "PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER $DB_NAME"
    }
  ],
  "variants": { "dblab": { "type": "start_server", "command": "..." } }
}
```

- `name`: Identifier used as `veld action <name>` (pattern `^[a-zA-Z0-9._-]+$`). Required.
- `command`: Shell command run via `$SHELL -c` in the node's working directory. Required. Inherits the parent env.
- `label`: Dashboard button text (defaults to `name`).
- `description`: One-line summary shown in `veld actions` and as a button tooltip.
- `parameters`: Static `{key: value}` map. Available as `${param.KEY}` and as `$KEY` env vars. Values support `${...}` substitution.
- `requires_outputs`: Output keys that must all be present on the running node for the action to be available. Gates CLI invocation and dashboard button visibility. Omit to always offer the action for a running node.

Substitution available inside `command` and `parameters` values:

- `$KEY` — the node's live outputs, injected as environment variables and expanded by the shell at runtime
- `${output.KEY}` — the same outputs, interpolated by Veld into the command string before it runs
- `${param.KEY}` — this action's parameters
- `${veld.run}`, `${veld.node}`, `${veld.variant}`, `${veld.project}`, `${veld.root}`, `${veld.port}`, `${veld.url}`

**Secrets — prefer `$KEY` over `${output.KEY}`.** A secret referenced as `${output.DB_PASS}` is interpolated into the command string, so it ends up in the process list (`ps`) and any argv-based logging. `$DB_PASS` is passed as an environment variable and expanded by the shell at runtime, so it never appears in argv — as in the `psql` example above. GUI clients launched with a connection URL (`open -a Postico "postgresql://$DB_USER:$DB_PASS@…"`) are the exception: the URL is expanded into the launcher's argv regardless, so to avoid exposure there, omit the password and let the client prompt.

Note: `${VAR}` (braces) is parsed by Veld, so use `$VAR` (no braces) for plain shell/env references inside a command — otherwise Veld tries to resolve it and errors. When an action is defined on multiple nodes, disambiguate with `veld action <name> --node <node>`.

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
| `actions` | node | Named shell commands exposed via `veld action`/dashboard buttons. See [Actions](#actions). |
| `sharing` | project | `{relays?: "public" \| [url \| {url, token?},...], gateway?: url, dangerouslyEmbedRelayTokensInTicket?: bool}`. Relay policy (compliance) + public gateway URL. Relay `token` gates a self-hosted relay. Config wins over `VELD_SHARE_RELAY`. See [Sharing](#sharing). |
| `share` | variant | `{expose: ["peer" \| "web", ...]}`. Per-service opt-in — absent/empty = not shareable. See [Sharing](#sharing). |

## Sharing

A service is shareable only if its variant declares `share.expose` — `veld share` refuses anything that hasn't opted in.

```json
{
  "sharing": { "relays": "public" },
  "nodes": {
    "frontend": {
      "variants": {
        "local": { "type": "start_server", "command": "npm run dev", "share": { "expose": ["peer"] } }
      }
    }
  }
}
```

- `sharing.relays` — **must be opted into explicitly (no default):** `"public"` (n0's relays) or an array of self-hosted relay entries (confines share traffic for compliance). `veld share` is refused if unset (and no `VELD_SHARE_RELAY` env). Config wins over the env var. The daemon binds one iroh endpoint per relay policy, so shares on different relays run concurrently (no restart).
  - A relay entry is a bare URL string, or `{ "url": ..., "token": ... }` to send an `Authorization: Bearer` token to a relay that requires one. `token` = a literal string (inline; lands in config), or `{ "env": "VAR" }` / `{ "file": "/path" }` / `{ "command": "op read ..." }` to resolve it on the daemon at share time without storing the secret. A token that fails to resolve fails the share (never connects unauthenticated). `VELD_SHARE_RELAY_TOKEN` pairs a literal token with the `VELD_SHARE_RELAY` env override.
  - **Join side:** a joiner auto-uses the ticket's relay(s) (a custom-relay share is never joined over public relays). For a token-gated relay it supplies the token via `VELD_SHARE_RELAY` + `VELD_SHARE_RELAY_TOKEN` (attached only to the matching ticket relay).
- `sharing.dangerouslyEmbedRelayTokensInTicket` — **DANGER, default false.** Embeds the resolved relay token(s) in the share ticket so joiners need no token setup. Ships the relay secret in every share link (Slack, email, history) — disposable per-project tokens only, never a shared org secret. camelCase (à la React's `dangerouslySetInnerHTML`) to flag the danger.
- `sharing.gateway` — base URL of the public web gateway (for `web` exposure). **Reserved:** gateway ships later.
- `share.expose` — `peer` (Veld-to-Veld, verbatim URL; available now) and/or `web` (any browser via gateway; reserved). Empty list or absent = not shareable.
