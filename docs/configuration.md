# Veld Configuration Reference

## Overview

Veld is configured through a single `veld.json` file placed in the root of your project. This file is committed to version control and defines your entire local development environment: the services to run, how they depend on each other, health checks, environment wiring, and URL routing.

Veld discovers `veld.json` by walking up the directory tree from your current working directory, exactly like Git discovers `.git`. If no config file is found, Veld exits with a clear error suggesting `veld init`.

All relative paths in the configuration resolve relative to the directory containing `veld.json` -- never the current working directory.

### Minimal Example

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "my-app",
  "nodes": {
    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "npm run dev -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" }
        }
      }
    }
  }
}
```

### Top-Level Structure

| Field            | Type   | Required | Description                                      |
|------------------|--------|----------|--------------------------------------------------|
| `$schema`        | string | No       | JSON Schema URL for editor autocompletion         |
| `schemaVersion`  | string | Yes      | Must be `"1"` for the current version             |
| `name`           | string | Yes      | Human-readable project name                       |
| `url_template`      | string | No       | URL template for services (see [URL Templates])   |
| `presets`           | object | No       | Named shortcuts for node:variant selections       |
| `client_log_levels` | array  | No       | Browser log levels to capture (see [Client-Side Log Levels]) |
| `features`          | object | No       | Feature toggles (see [Features](#features))       |
| `env`               | object | No       | Global environment variables inherited by all nodes |
| `nodes`             | object | Yes      | The dependency graph nodes                        |

[Client-Side Log Levels]: #client-side-log-levels

[URL Templates]: #url-templates

---

## Project Settings

### `name`

A human-readable project name used in URLs and registry entries. Must match the pattern `^[a-zA-Z0-9][a-zA-Z0-9._-]*$` -- start with an alphanumeric character, followed by alphanumerics, dots, underscores, or hyphens.

```json
"name": "my-project"
```

The name is available as the `{project}` variable in URL templates and as `${veld.project}` in commands and environment variables.

### `schemaVersion`

Must be `"1"`. Veld validates this on every command and exits with a clear error if it encounters an unknown version.

```json
"schemaVersion": "1"
```

### `url_template`

Defines how Veld generates HTTPS URLs for your services. See the full [URL Templates](#url-templates) section for details.

```json
"url_template": "{service}.{branch ?? run}.my-project.localhost"
```

**Default:** `{service}.{run}.{project}.localhost`

### `client_log_levels`

Controls which browser console levels Veld captures from `start_server` nodes. Veld injects a small script into proxied HTML responses that hooks `console.log`, `console.warn`, `console.error`, `console.info`, and `console.debug`, plus `window.onerror` and `onunhandledrejection`. Captured logs are sent to the Veld daemon and appear in `veld logs` and the management UI alongside server logs.

```json
"client_log_levels": ["log", "warn", "error"]
```

**Valid values:** `"log"`, `"warn"`, `"error"`, `"info"`, `"debug"`

**Default:** `["log", "warn", "error"]`

Unhandled exceptions and promise rejections are always captured regardless of this setting.

The setting cascades: variant-level overrides node-level overrides project-level. Set it to an empty array to capture only unhandled exceptions.

```json
{
  "client_log_levels": ["warn", "error"],
  "nodes": {
    "frontend": {
      "client_log_levels": ["log", "warn", "error", "info"],
      "variants": {
        "local": {
          "client_log_levels": ["debug", "log", "warn", "error", "info"]
        }
      }
    }
  }
}
```

### `features`

Controls which Veld capabilities are injected into `start_server` nodes' HTML responses. Each feature defaults to `true` (enabled). Set a feature to `false` to disable it. The same override hierarchy applies: variant > node > project.

| Feature             | Type    | Default | Description |
|---------------------|---------|---------|-------------|
| `feedback_overlay`  | boolean | `true`  | Inject the feedback overlay toolbar (FAB, screenshot, comments) |
| `client_logs`       | boolean | `true`  | Inject the client-side log collector |

```json
{
  "features": {
    "feedback_overlay": false
  },
  "nodes": {
    "api": {
      "features": { "client_logs": false },
      "variants": {
        "local": {
          "features": { "feedback_overlay": true }
        }
      }
    }
  }
}
```

In this example, the project disables the feedback overlay by default, and the `api` node also disables client logs. But the `api:local` variant re-enables the feedback overlay.

### `env`

Global environment variables inherited by all node variants. Values support Veld variable substitution. The same override hierarchy applies: variant > node > project. For each key, the most specific layer wins; keys from parent layers that are not overridden are preserved.

```json
{
  "env": {
    "FEATURE_FLAG_X": "1",
    "SHARED_CONFIG": "value"
  },
  "nodes": {
    "api": {
      "env": {
        "SHARED_CONFIG": "api-override"
      },
      "variants": {
        "local": {
          "env": {
            "PORT": "${veld.port}"
          }
        }
      }
    }
  }
}
```

In this example, `api:local` inherits `FEATURE_FLAG_X=1` from the project, gets `SHARED_CONFIG=api-override` from the node (overriding the project value), and adds `PORT` at the variant level.

---

## Nodes

A node represents a unit in your dependency graph -- typically a service, a database, or a setup task. Each node has a name (the object key) and contains one or more variants.

```json
"nodes": {
  "backend": {
    "default_variant": "local",
    "variants": {
      "local": { ... },
      "docker": { ... },
      "staging": { ... }
    }
  }
}
```

### `default_variant`

Specifies which variant to use when none is explicitly selected. Optional -- if omitted and the node has exactly one variant, that variant is used automatically.

```json
"default_variant": "local"
```

If a node has multiple variants and no `default_variant` is set, the user must explicitly specify which variant to use.

### `hidden`

When set to `true`, the node is excluded from `veld nodes` output. Hidden nodes still participate fully in the dependency graph — they are started, stopped, and have their `on_stop` hooks executed like any other node. This is useful for internal setup tasks (certificate generation, database seeding, etc.) that end users don't need to see.

```json
"generate-certs": {
  "hidden": true,
  "variants": {
    "default": {
      "type": "command",
      "command": "./scripts/generate-certs.sh"
    }
  }
}
```

### `client_log_levels` (node-level)

Overrides the project-level `client_log_levels` for all variants of this node. See [Client-Side Log Levels](#client-side-log-levels) for details.

```json
"frontend": {
  "client_log_levels": ["log", "warn", "error", "info"],
  "variants": { ... }
}
```

### `features` (node-level)

Overrides the project-level `features` for all variants of this node. See [Features](#features) for details.

```json
"api": {
  "features": { "feedback_overlay": false },
  "variants": { ... }
}
```

### `url_template` (node-level)

Overrides the project-level `url_template` for all variants of this node. See [URL Template Cascade](#url-template-cascade) for resolution order.

```json
"backend": {
  "url_template": "{service}-api.{branch ?? run}.{project}.localhost",
  "variants": { ... }
}
```

### `variants`

An object mapping variant names to their configuration. Each node must have at least one variant.

---

## Variants

A variant defines how a node behaves in a given context. The same node might be a running server in one variant and a bash script exporting a remote URL in another.

### Complete Variant Fields

| Field               | Type             | Required | Applies To     | Description                                           |
|---------------------|------------------|----------|----------------|-------------------------------------------------------|
| `type`              | string           | Yes      | All            | `"command"` or `"start_server"`                          |
| `command`           | string           | Varies   | All            | Inline shell command to execute                       |
| `script`            | string           | Varies   | `command` only    | Path to script file, relative to `veld.json`          |
| `health_check`      | object           | Required for `start_server` | `start_server` | How to verify the service is healthy |
| `depends_on`        | object           | No       | All            | Dependencies on other nodes                           |
| `env`               | object           | No       | All            | Extra environment variables                           |
| `outputs`           | array or object  | No       | All            | Output declarations (format varies by type)           |
| `sensitive_outputs`  | array of strings | No       | All            | Output keys to mask and encrypt                       |
| `url_template`      | string           | No       | `start_server` | URL template override for this variant                |
| `on_stop`           | string           | No       | All            | Teardown command run when the environment is stopped  |
| `verify`            | string           | No       | `command` only    | Idempotency verification command                      |
| `client_log_levels` | array of strings | No       | `start_server` | Browser log levels override for this variant          |
| `features`          | object           | No       | `start_server` | Feature toggles override for this variant             |

### `type`

#### `command`

Runs a shell command or script to completion. Used for setup tasks such as database cloning, seeding, data migration, or exporting remote service URLs.

- The working directory defaults to `${veld.root}` (the directory containing `veld.json`)
- Must specify either `command` or `script` (mutually exclusive)
- Can declare outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE` (preferred) or via `VELD_OUTPUT key=value` on stdout (legacy, discouraged — exposes values in terminal/logs)
- Built-in output: `exit_code`
- Supports the `verify` field for idempotency

```json
{
  "type": "command",
  "command": "echo 'DATABASE_URL=postgresql://localhost:5432/mydb' >> \"$VELD_OUTPUT_FILE\"",
  "outputs": ["DATABASE_URL"]
}
```

#### `start_server`

Starts and manages a long-lived process. Veld allocates a port, injects it as `${veld.port}`, configures DNS and Caddy routing, and monitors health.

- The working directory defaults to `${veld.root}`
- Must specify `command` (required)
- The process **must** bind to `${veld.port}` -- if it does not, the health check fails with a clear error
- Built-in outputs: `url` (the full HTTPS URL) and `port` (the allocated port number)
- Built-in variables: `${veld.port}` and `${veld.url}` are available in this node's `command`, `env`, and `outputs` templates
- Ports and URLs are **pre-computed** before any node executes, so `${nodes.X.url}` and `${nodes.X.port}` for any `start_server` node are available everywhere -- no dependency edge required
- Supports the `health_check` field (required)
- Users never see or deal with port numbers -- only clean HTTPS URLs

```json
{
  "type": "start_server",
  "command": "pnpm --filter backend dev --port ${veld.port}",
  "health_check": { "type": "http", "path": "/health" }
}
```

### `command`

An inline shell command to execute. Supports full Veld variable substitution.

```json
"command": "docker run --rm --name veld-db-${veld.run} -p ${veld.port}:5432 postgres:16"
```

For `start_server` variants, `command` is required. For `command` variants, you must provide either `command` or `script`.

### `script`

A path to a script file, relative to the directory containing `veld.json`. Mutually exclusive with `command`. Only valid for `command` type variants.

```json
"script": "./scripts/clone-db.sh"
```

### `health_check`

Defines how Veld verifies that a `start_server` process is healthy. Veld runs a two-phase health check:

1. **Phase 1 -- Port Check:** Verifies the process bound to `${veld.port}` via TCP connection.
2. **Phase 2 -- HTTPS URL Check:** Verifies the full stack end-to-end (DNS, Caddy routing, TLS, upstream response).

If Phase 1 fails, the error is a process issue. If Phase 1 passes but Phase 2 fails, it is an infrastructure issue. This distinction produces precise error messages.

#### Health Check Fields

| Field              | Type    | Required | Description                                          |
|--------------------|---------|----------|------------------------------------------------------|
| `type`             | string  | Yes      | Strategy: `"http"`, `"port"`, or `"command"`            |
| `path`             | string  | No       | HTTP path to poll (`http` type only)                 |
| `expect_status`    | integer | No       | Expected HTTP status code (`http` type only, default: 200) |
| `command`          | string  | No       | Shell command to run (`command` type only)              |
| `timeout_seconds`  | integer | No       | Max seconds to wait (default: 60)                    |
| `interval_ms`      | integer | No       | Milliseconds between checks (default: 1000, min: 100)|

#### Strategy: `http`

Polls an HTTP endpoint at the given path. The check passes when the endpoint returns the expected status code.

```json
"health_check": {
  "type": "http",
  "path": "/health",
  "expect_status": 200,
  "timeout_seconds": 30
}
```

If `expect_status` is omitted, it defaults to `200`. If `path` is omitted, Veld checks the root `/`.

#### Strategy: `port`

Checks whether the allocated port is accepting TCP connections. The simplest strategy -- useful for databases, caches, and services without an HTTP health endpoint.

```json
"health_check": {
  "type": "port",
  "timeout_seconds": 15
}
```

#### Strategy: `command`

Runs a shell command and checks the exit code. Exit code `0` means healthy.

```json
"health_check": {
  "type": "command",
  "command": "./scripts/check-db-ready.sh",
  "timeout_seconds": 45,
  "interval_ms": 2000
}
```

### `depends_on`

Declares dependencies as explicit `node:variant` pairs. Dependencies are resolved before this variant starts. The value is an object mapping node names to variant names.

```json
"depends_on": {
  "database": "docker",
  "backend": "local"
}
```

Default variants are never silently assumed -- every dependency must name its variant explicitly. If two selected nodes transitively require the same dependency node with different variants, Veld starts both as independent processes, each with its own port, URL, and state.

Dependencies are started in topological order, with independent branches parallelized. On teardown, the reverse order is used.

### `env`

Extra environment variables injected into the process. Values support Veld variable substitution, including references to outputs from upstream nodes.

```json
"env": {
  "DATABASE_URL": "${nodes.database.DATABASE_URL}",
  "PORT": "${veld.port}",
  "NODE_ENV": "development",
  "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
}
```

**Layering:** Environment variables cascade from project to node to variant. For each key, the most specific layer wins. Keys from parent layers that are not overridden are preserved. See the project-level [`env`](#env) section for a full example.

**Precedence:** The merged `env` block takes strict precedence over the inherited shell environment. Shell variables not overridden by `env` are passed through unchanged.

### `outputs`

Output declarations differ based on the variant type.

#### For `command` variants: Array of strings

Declares the output names that the script will produce. Veld provides a `$VELD_OUTPUT_FILE` environment variable pointing to a temporary file — your script writes `key=value` lines there. This keeps sensitive values (database passwords, API keys) off stdout and out of terminal scrollback and log aggregators.

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "outputs": ["DATABASE_URL", "DB_NAME"]
}
```

Inside the script:
```bash
#!/bin/bash
echo "DATABASE_URL=postgresql://localhost:5432/mydb" >> "$VELD_OUTPUT_FILE"
echo "DB_NAME=mydb" >> "$VELD_OUTPUT_FILE"
```

> **Legacy fallback (discouraged):** For backward compatibility, Veld also parses `VELD_OUTPUT key=value` lines from stdout. This method is **discouraged** because it exposes output values in the terminal, log aggregators, and CI build output. Prefer `$VELD_OUTPUT_FILE` for all new scripts. If both channels emit the same key, the file-based value takes precedence.

Every `command` variant also automatically provides the built-in output `exit_code`.

#### For `start_server` variants: Object (key-value map)

Defines synthetic outputs whose values are string templates interpolated after the port and URL are resolved. Templates support all `${veld.*}` and `${nodes.*}` variables. This is especially useful for Docker infrastructure nodes where the process cannot write to `$VELD_OUTPUT_FILE`.

```json
{
  "type": "start_server",
  "command": "docker run --rm -p ${veld.port}:5432 postgres:16",
  "health_check": { "type": "port" },
  "outputs": {
    "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app",
    "REDIS_URL": "redis://localhost:${veld.port}"
  }
}
```

Since `${veld.url}` is available in output templates, you can build derived URLs:

```json
"outputs": {
  "API_URL": "${veld.url}/api/v1",
  "WEBSOCKET_URL": "${veld.url}/ws"
}
```

Every `start_server` variant also automatically provides the built-in outputs `url` (the full HTTPS URL) and `port` (the allocated port number).

### `sensitive_outputs`

An array of output key names whose values are sensitive. These outputs are:

- Masked as `[REDACTED]` in all terminal output, debug logs, and run logs
- Stored encrypted at rest using a machine-local key
- Never visible in `veld graph` output

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "outputs": ["DATABASE_URL"],
  "sensitive_outputs": ["DATABASE_URL"]
}
```

### `verify`

An idempotency verification command. Only applies to `command` type variants. Before running the main command/script, Veld executes the verify command:

- **Exit code 0:** The step is considered already complete and is skipped.
- **Non-zero exit code:** The step runs normally.
- If `verify` itself errors unexpectedly, the step re-runs (safe default).

The verify command receives the previous run's output variables as environment variables, so it can check whether the previous result is still valid.

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "verify": "./scripts/verify-db.sh",
  "outputs": ["DATABASE_URL"]
}
```

### `on_stop`

A teardown command that runs when `veld stop` is called. Executed in reverse dependency order, after the process is killed (for `start_server` nodes) but before state is cleaned up.

This is especially useful for `command` nodes that provision external resources during start — databases, Docker containers, temporary credentials — that need explicit cleanup.

```json
{
  "type": "command",
  "command": "docker run -d --name veld-db-${veld.run} -p ${veld.port}:5432 postgres:16",
  "on_stop": "docker rm -f veld-db-${veld.run}",
  "outputs": ["DATABASE_URL"]
}
```

The `on_stop` command receives the same variable context that was available during start:
- All `${veld.*}` built-in variables (`${veld.root}`, `${veld.project}`, `${veld.port}`, etc.)
- All outputs produced by this node (e.g. `${veld.exit_code}`, custom outputs)
- Environment variables from the variant's `env` block

If the `on_stop` command fails (non-zero exit code or execution error), Veld logs a warning but continues tearing down the remaining nodes. A failing teardown hook never blocks the stop operation.

`on_stop` works with both `command` and `start_server` variants:

```json
{
  "type": "start_server",
  "command": "docker run --rm --name veld-redis-${veld.run} -p ${veld.port}:6379 redis:7",
  "on_stop": "docker stop veld-redis-${veld.run}",
  "health_check": { "type": "port" }
}
```

---

## Presets

Presets are named shortcuts for node:variant selections. They provide convenience for common configurations without introducing a new core concept.

```json
"presets": {
  "fullstack": ["frontend:local", "admin:local"],
  "ui-only": ["frontend:staging", "admin:staging"],
  "backend-dev": ["backend:local"]
}
```

Each preset maps to an array of `"node:variant"` strings. Use presets with:

```sh
veld start --preset fullstack --name my-feature
```

In interactive mode (TTY with presets defined), `veld start` with no arguments presents a preset selector. Presets are purely additive -- they select end nodes that Veld then resolves through the dependency graph, starting all required upstream nodes automatically.

---

## Variable Substitution

Veld provides two separate variable systems for different contexts:

1. **`${...}` syntax** -- used in `command`, `script` arguments, and `env` values within variant configurations.
2. **`{...}` syntax** -- used exclusively in the `url_template` field.

### Built-in Variables (`${veld.*}`)

Available to all node variants without any declaration:

| Variable            | Value                                                |
|---------------------|------------------------------------------------------|
| `${veld.port}`          | Allocated port for this node in this run             |
| `${veld.url}`           | Full HTTPS URL for this node (`start_server` only)   |
| `${veld.url.hostname}`  | DNS name only (e.g. `app.my-run.proj.localhost`)     |
| `${veld.url.host}`      | hostname:port (omits port when HTTPS port is 443)    |
| `${veld.url.origin}`    | scheme + host (same as `${veld.url}`)                |
| `${veld.url.scheme}`    | Protocol scheme (`https`)                            |
| `${veld.url.port}`      | HTTPS port (note: `${veld.port}` is the backend bind port) |
| `${veld.run}`           | Run name                                             |
| `${veld.run_id}`        | Stable run UUID                                      |
| `${veld.root}`          | Absolute path to the directory containing `veld.json`|
| `${veld.project}`       | Project name from `veld.json`                        |
| `${veld.worktree}`      | Slugified worktree directory name                    |
| `${veld.branch}`        | Current git branch, slugified (empty string if not in git) |
| `${veld.username}`      | OS username                                          |

### Node Output References (`${nodes.*}`)

References to other nodes' outputs. There are two categories with different availability rules:

#### Pre-computed outputs (available to ALL nodes)

The built-in `url` and `port` outputs for `start_server` nodes are **pre-computed** before any node executes. This means every node in the graph can reference any `start_server` node's URL or port — regardless of dependency order.

This is especially powerful for cross-referencing: the frontend can know the backend's URL and the backend can know the frontend's URL, without creating a dependency cycle. `depends_on` controls execution order only, not variable availability for URLs and ports.

```
${nodes.backend.url}               # start_server built-in: full HTTPS URL
${nodes.backend.url.hostname}      # start_server built-in: DNS name only
${nodes.backend.url.host}          # start_server built-in: hostname:port
${nodes.backend.url.origin}        # start_server built-in: scheme + host
${nodes.backend.url.scheme}        # start_server built-in: protocol scheme
${nodes.backend.url.port}          # start_server built-in: HTTPS port
${nodes.backend.port}              # start_server built-in: allocated port (rarely needed)
${nodes.frontend.url}              # works even if frontend runs AFTER this node
```

#### Execution-order outputs (available to downstream nodes only)

Custom outputs — from synthetic output templates (`outputs` object) or `$VELD_OUTPUT_FILE` / `VELD_OUTPUT` lines in command nodes — are only available after the producing node has executed. These require a `depends_on` edge.

```
${nodes.database.DATABASE_URL}     # custom output from bash or outputs declaration
${nodes.clone-db.exit_code}        # bash built-in: exit code
```

#### Short Form

When only one variant of a node is active in the current dependency graph:

```
${nodes.database.DATABASE_URL}     # custom output from bash or outputs declaration
${nodes.backend.url}               # start_server built-in: full HTTPS URL
${nodes.backend.url.hostname}      # start_server built-in: DNS name only
${nodes.backend.url.host}          # start_server built-in: hostname:port
${nodes.backend.port}              # start_server built-in: allocated port (rarely needed)
${nodes.clone-db.exit_code}        # bash built-in: exit code
```

#### Qualified Form

When two variants of the same node are running simultaneously (because different end nodes depend on different variants), you must use the qualified form:

```
${nodes.backend:local.url}         # qualified with variant name
${nodes.backend:staging.BACKEND_URL}
```

Veld validates all variable references for ambiguity at graph resolution time and fails fast with a precise error before starting anything. If a short-form reference is ambiguous (multiple variants of the same node are active), Veld reports exactly which qualified form to use.

### Examples in Context

```json
{
  "type": "start_server",
  "command": "pnpm --filter frontend dev",
  "depends_on": { "backend": "local", "database": "docker" },
  "env": {
    "PORT": "${veld.port}",
    "NEXT_PUBLIC_API_URL": "${nodes.backend.url}",
    "DATABASE_URL": "${nodes.database.DATABASE_URL}"
  }
}
```

---

## URL Templates

URL templates define how Veld generates HTTPS URLs for `start_server` nodes. Templates can be defined at the project, node, or variant level.

### Syntax

URL templates use `{variable}` syntax (single braces, not `${}`). This is different from the `${variable}` syntax used in commands and env values.

```json
"url_template": "{service}.{branch ?? run}.my-project.localhost"
```

### Template Variables

All values are slugified automatically (lowercased, non-alphanumeric characters replaced with `-`, consecutive dashes collapsed, leading/trailing dashes stripped, max 48 characters).

| Variable     | Value                                                          |
|--------------|----------------------------------------------------------------|
| `{service}`  | Node name                                                      |
| `{variant}`  | Variant name                                                   |
| `{run}`      | Run name (always non-empty)                                    |
| `{project}`  | Project name from `veld.json`                                  |
| `{branch}`   | Current git branch name, slugified (empty string if not in git)|
| `{worktree}` | Slugified worktree directory name                              |
| `{username}` | OS username                                                    |
| `{hostname}` | Machine hostname                                               |

`{branch}` and `{worktree}` are evaluated at run creation time and frozen into the run state. URLs never change if you switch branches mid-run.

### The `??` Fallback Operator

The `??` operator provides fallback values. Veld evaluates left to right and uses the first non-empty value.

```json
"url_template": "{service}.{branch ?? run}.my-project.localhost"
```

In this example:
- If the current git branch is `feature/login`, the URL becomes `backend.feature-login.my-project.localhost`
- If not in a git repo (branch is empty), it falls back to the run name: `backend.my-feature.my-project.localhost`

Since `{run}` is always guaranteed to be non-empty, it is the recommended final fallback:

```json
"{service}.{branch ?? worktree ?? run}.{project}.localhost"
```

### Default Template

If `url_template` is not declared, Veld uses:

```
{service}.{run}.{project}.localhost
```

`.localhost` subdomains resolve to `127.0.0.1` automatically on modern macOS and Linux (RFC 6761), so no DNS configuration is needed for the default case.

### Custom Domains

For custom apex domains, Veld manages exact DNS entries via `veld-helper`:

```json
"url_template": "{service}.{branch ?? run}.my-project.life.li"
```

Veld writes exact host entries only -- never wildcard rules. Real domains and unrelated subdomains continue resolving normally via public DNS.

**Important:** Custom (non-`.localhost`) domains require `veld setup privileged`. In unprivileged or auto-bootstrap mode, Veld cannot write to `/etc/hosts` or manage system DNS, so only `.localhost` domains are supported. If you use a custom apex domain in your `url_template` and are not in privileged mode, `veld start` will exit with an error explaining how to fix it.

### URL Template Cascade

URL templates can be overridden at three levels. Veld uses the most specific one:

1. **Variant-level** `url_template` -- highest priority
2. **Node-level** `url_template` -- applies to all variants of the node
3. **Project-level** `url_template` -- the default for all nodes

This lets you use a common template for most services while giving specific nodes or variants a different URL pattern:

```json
{
  "url_template": "{service}.{branch ?? run}.{project}.localhost",
  "nodes": {
    "frontend": {
      "variants": {
        "local": { "..." : "uses project-level template" }
      }
    },
    "backend": {
      "url_template": "{service}-api.{branch ?? run}.{project}.localhost",
      "variants": {
        "local": { "..." : "uses node-level template" },
        "docker": {
          "url_template": "{service}.localhost:{port}",
          "..." : "uses variant-level template"
        }
      }
    }
  }
}
```

### URL Examples

Given a project named `my-project`, a run named `my-feature`, a node named `frontend`, and a branch named `feature/auth`:

| Template                                            | Resulting URL                                     |
|-----------------------------------------------------|---------------------------------------------------|
| `{service}.{run}.{project}.localhost`               | `frontend.my-feature.my-project.localhost`        |
| `{service}.{branch ?? run}.{project}.localhost`     | `frontend.feature-auth.my-project.localhost`      |
| `{service}.localhost:{port}`                        | `frontend.localhost:8432`                         |
| `{service}.{username}.{project}.localhost`           | `frontend.jane.my-project.localhost`              |

---

## Complete Example

Below is a realistic `veld.json` for a monorepo with a database, backend API, frontend app, and admin panel. It demonstrates all major features.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "my-project",
  "url_template": "{service}.{branch ?? run}.my-project.localhost",

  "presets": {
    "fullstack": ["frontend:local", "admin:local"],
    "ui-only": ["frontend:staging", "admin:staging"]
  },

  "nodes": {
    "database": {
      "default_variant": "docker",
      "variants": {
        "local": {
          "type": "command",
          "script": "./scripts/clone-db.sh",
          "verify": "./scripts/verify-db.sh",
          "on_stop": "./scripts/drop-db.sh",
          "outputs": ["DATABASE_URL"],
          "sensitive_outputs": ["DATABASE_URL"]
        },
        "docker": {
          "type": "start_server",
          "command": "docker run -d --name veld-db-${veld.run} -e POSTGRES_PASSWORD=veld -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker rm -f veld-db-${veld.run}",
          "health_check": {
            "type": "port",
            "timeout_seconds": 30
          },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app"
          }
        }
      }
    },

    "generate-certs": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "./scripts/generate-dev-certs.sh",
          "verify": "test -f ./certs/dev.pem"
        }
      }
    },

    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter backend dev --port ${veld.port}",
          "health_check": {
            "type": "http",
            "path": "/health",
            "expect_status": 200,
            "timeout_seconds": 30,
            "interval_ms": 1000
          },
          "depends_on": {
            "database": "docker"
          },
          "env": {
            "DATABASE_URL": "${nodes.database.DATABASE_URL}",
            "NODE_ENV": "development"
          }
        },
        "staging": {
          "type": "command",
          "command": "echo 'BACKEND_URL=https://api.staging.my-project.com' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["BACKEND_URL"]
        }
      }
    },

    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter frontend dev",
          "health_check": {
            "type": "http",
            "path": "/"
          },
          "depends_on": {
            "backend": "local"
          },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:local.url}"
          }
        },
        "staging": {
          "type": "start_server",
          "command": "pnpm --filter frontend dev",
          "health_check": {
            "type": "http",
            "path": "/"
          },
          "depends_on": {
            "backend": "staging"
          },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:staging.BACKEND_URL}"
          }
        }
      }
    },

    "admin": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter admin dev",
          "health_check": {
            "type": "http",
            "path": "/",
            "timeout_seconds": 45
          },
          "depends_on": {
            "backend": "local"
          },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:local.url}"
          }
        },
        "staging": {
          "type": "start_server",
          "command": "pnpm --filter admin dev",
          "health_check": {
            "type": "http",
            "path": "/"
          },
          "depends_on": {
            "backend": "staging"
          },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:staging.BACKEND_URL}"
          }
        }
      }
    }
  }
}
```

### What Happens When You Run This

```sh
veld start --preset fullstack --name my-feature
```

1. Veld resolves the dependency graph: `frontend:local` and `admin:local` both depend on `backend:local`, which depends on `database:docker`.
2. `database:docker` starts first -- Veld allocates a port, runs the Docker command with `${veld.port}` injected, and waits for the port health check to pass. The `DATABASE_URL` synthetic output is interpolated.
3. `backend:local` starts next -- Veld allocates a port, injects `${nodes.database.DATABASE_URL}` into the env, and waits for `/health` to return 200.
4. `frontend:local` and `admin:local` start in parallel -- both depend only on `backend:local`, which is now healthy.
5. Each service gets a stable HTTPS URL like `https://frontend.my-feature.my-project.localhost`.
6. In a terminal (TTY), logs from all services stream in real-time. Press Ctrl+C to stop all services.

### Foreground vs Detached Mode

By default, `veld start` runs in **foreground mode** when invoked from a terminal: after starting all services, it streams logs from all nodes (like `docker compose up`) and stops the environment on Ctrl+C.

Use `--detach` / `-d` to start in the background (like `docker compose up -d`):

```sh
# Foreground (default in TTY) — streams logs, Ctrl+C stops everything
veld start --preset fullstack --name my-feature

# Detached — starts and exits immediately
veld start --preset fullstack --name my-feature -d

# View logs later
veld logs -f
```

When not running in a terminal (e.g. piped or in a script), `veld start` always detaches automatically.

### Log Timestamps

All log output (both `start_server` stdout/stderr and internal Veld events) is timestamped with ISO 8601 timestamps:

```
[2026-03-12T08:30:01.123456+00:00] Server listening on port 3000
[2026-03-12T08:30:01.456789+00:00] Connected to database
```

Timestamps are written at the time each line is emitted, enabling chronological merging across nodes in `veld logs`.

---

## JSON Schema

Veld provides a JSON Schema for editor autocompletion and validation. Add the `$schema` field to your `veld.json`:

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  ...
}
```

### Local Schema Reference

If you have the Veld repository checked out, you can reference the schema locally:

```json
{
  "$schema": "./node_modules/veld/schema/v1/veld.schema.json",
  ...
}
```

Or relative to your project structure:

```json
{
  "$schema": "../../schema/v1/veld.schema.json",
  ...
}
```

### Editor Support

Most modern editors support JSON Schema natively or through extensions:

- **VS Code:** Automatically picks up the `$schema` field. Provides autocompletion, hover documentation, and inline validation.
- **JetBrains IDEs (WebStorm, IntelliJ):** Automatically recognizes the `$schema` field.
- **Neovim (with LSP):** JSON language server respects the `$schema` field.

The schema validates:
- All required fields are present
- Field types are correct
- `type` values are one of `"command"` or `"start_server"`
- Health check types are one of `"http"`, `"port"`, or `"command"`
- `start_server` variants require `command`
- `command` variants require either `command` or `script`
- Preset entries match the `node:variant` pattern
- Numeric constraints (timeouts, intervals, status codes)
