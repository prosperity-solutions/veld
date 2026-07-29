# Veld Configuration Reference

## Overview

Veld is configured through a single root config file placed in the root of your project. This file is committed to version control and defines your entire local development environment: the services to run, how they depend on each other, health checks, environment wiring, and URL routing.

The root file may be named **`veld.json` or `veld.jsonc`** — both are read identically, and `veld init` writes `veld.json`. If a directory holds both, `veld.json` wins and `veld lint` reports `ambiguous-root-config` as an error, so `veld start` refuses until you delete or rename one. It is a *finding* rather than a load failure on purpose: discovery runs on every subcommand including `stop`, and a root config that refuses to load is one whose `on_stop` hooks never run, leaking the containers they exist to remove.

Veld discovers it by walking up the directory tree from your current working directory, exactly like Git discovers `.git`. If no config file is found, Veld exits with a clear error suggesting `veld init`.

All relative paths in the configuration resolve relative to the directory containing the root config -- never the current working directory.

### Comments and trailing commas

Every Veld config file accepts `//` line comments, `/* */` block comments, and
trailing commas — at every `schemaVersion`, whatever the file is called:

```jsonc
{
  // The API service. Owned by @some-team.
  "schemaVersion": "3",
  "name": "example",
  "nodes": { /* … */ },
}
```

A duplicate key is an error rather than silently last-wins, so a copy-pasted node
or a doubled `variants` block cannot quietly drop half of itself. (It is reported
by `veld lint` and `veld start`, not by the loader, so `veld stop` still works
against a config you are in the middle of editing.)

**Your editor does not know this.** A `veld.json` carrying a `$schema` is validated
by the editor's strict JSON parser, which flags comments as syntax errors. Either
name the root file `veld.jsonc` — editors pick JSONC mode from the extension, so
nothing else is needed — or map the `.json` names to the `jsonc` language:

```jsonc
// .vscode/settings.json
{
  "files.associations": {
    "veld.json": "jsonc",
    "veld.local.json": "jsonc",
    "*.node.json": "jsonc"
  }
}
```

Included files have always been matched by glob, so `include: ["nodes/*.jsonc"]`
worked before the root name did.

The same applies to anything else that reads the file as strict JSON — `veld config
| jq` will break on a commented config.

### Minimal Example

```json
{
  "$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json",
  "schemaVersion": "3",
  "name": "my-app",
  "nodes": {
    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "argv": ["npm", "run", "dev", "--", "--port", "${veld.port}"],
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
| `schemaVersion`  | string | Yes      | `"1"` or `"2"`. Use `"2"` for new projects.       |
| `name`           | string | Yes      | Human-readable project name                       |
| `url_template`      | string | No       | URL template for services (see [URL Templates])   |
| `presets`           | object | No       | Named shortcuts for node:variant selections       |
| `client_log_levels` | array  | No       | Browser log levels to capture (see [Client-Side Log Levels]) |
| `features`          | object | No       | Feature toggles (see [Features](#features))       |
| `proxy`             | object | No       | Reverse-proxy header rules (see [Proxy](#proxy))   |
| `env`               | object | No       | Global environment variables inherited by all nodes |
| `sharing`           | object | No       | Sharing policy: relays and public gateway (see [Sharing](#sharing)) |
| `setup`             | array  | No       | Lifecycle steps that run before the graph (see [Setup & Teardown]) |
| `teardown`          | array  | No       | Lifecycle steps that run after all nodes stop (see [Setup & Teardown]) |
| `nodes`             | object | Yes      | The dependency graph nodes                        |

[Client-Side Log Levels]: #client-side-log-levels

[URL Templates]: #url-templates

[Setup & Teardown]: #setup--teardown

---

## Project Settings

### `name`

A human-readable project name used in URLs and registry entries. Must match the pattern `^[a-zA-Z0-9][a-zA-Z0-9._-]*$` -- start with an alphanumeric character, followed by alphanumerics, dots, underscores, or hyphens.

```json
"name": "my-project"
```

The name is available as the `{project}` variable in URL templates and as `${veld.project}` in commands and environment variables.

### `schemaVersion`

Must be `"3"`. Required in the root config file; every other key is optional in
every file.

```json
"schemaVersion": "3"
```

**`"1"` and `"2"` are not supported.** A config declaring either fails to load, with
an error that states every change needed. There are three mechanical rewrites and
one semantic change — [docs/migrating-to-v3.md](migrating-to-v3.md) is the complete
rule set, written so you can hand it to a coding agent and check the result with
`veld lint`.

Supporting two readings was tried and abandoned: every rule then needed a severity
that depended on the document's version, and every new field was silently live in an
old document that had never opted into it. One reading is the feature.

A document containing `command` fails to load, with a message naming every offending
position.

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
| `inject`            | boolean | `true`  | Auto-inject bootstrap scripts into HTML responses. When `false`, `/__veld__/*` routes are still available for manual `<script>` tags. |

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

### `proxy`

Reverse-proxy header rules applied when forwarding requests to a service and responses back to the browser. Rules apply to **both** the local Caddy reverse proxy (local dev) and the public web gateway (`veld share --web`). They do **not** apply to direct iroh peer sharing (`veld share` without `--web`) — that path is a transport-level byte splice with no HTTP layer, so header rules cannot be applied there. The same override hierarchy applies: variant > node > project.

By default (`proxy` absent) Veld does **no** header manipulation — headers pass through with only the proxies' intrinsic, correctness-required rewrites.

| Field      | Type   | Description                                              |
|------------|--------|---------------------------------------------------------|
| `request`  | object | Header rules applied to the request forwarded upstream  |
| `response` | object | Header rules applied to the response returned to the browser |

Each of `request` and `response` is a set of header rules:

| Field    | Type             | Description                                                    |
|----------|------------------|---------------------------------------------------------------|
| `remove` | array of strings | Header names to strip                                         |
| `set`    | object (map)     | Header name → value pairs to set, replacing any existing value |

Header names are matched case-insensitively.

```json
{
  "proxy": {
    "request":  { "remove": ["Origin"], "set": { "X-Env": "dev" } },
    "response": { "remove": ["Server"], "set": { "X-Frame-Options": "DENY" } }
  }
}
```

**Resolution / merge.** When `proxy` is set at more than one level, Veld merges across project → node → variant: `remove` lists are **unioned** (case-insensitive dedup) and `set` maps are **merged** per key, with the more specific level winning. This mirrors how [`env`](#env) cascades.

```json
{
  "proxy": {
    "response": { "set": { "X-Frame-Options": "DENY" } }
  },
  "nodes": {
    "frontend": {
      "variants": {
        "local": {
          "proxy": { "request": { "remove": ["Origin"] } }
        }
      }
    }
  }
}
```

In this example, `frontend:local` inherits the `X-Frame-Options: DENY` response header from the project and additionally strips the `Origin` request header at the variant level.

#### Default behavior change: `Origin`

Earlier versions stripped the `Origin` header by default to make dev-server WebSocket HMR (Next.js, etc.) work: the local Caddy proxy deleted `Origin` on all requests, and the web gateway dropped `Origin` on WebSocket upgrades. Veld now does **no** header manipulation by default — `Origin` passes through the local proxy, and the gateway rewrites `Origin` *coherently* to the origin host on **all** requests (including WebSocket upgrades) rather than dropping it.

A Next.js dev server that gates WS HMR on `Origin` may reject the passed-through value. The recommended fix is to allow the origin host in your framework's config — for Next.js, set [`allowedDevOrigins`](https://nextjs.org/docs/app/api-reference/config/next-config-js/allowedDevOrigins) in `next.config.js`. For frameworks with no such allow-list, use the escape hatch of stripping `Origin` at the proxy:

```json
"proxy": { "request": { "remove": ["Origin"] } }
```

The local proxy route is (re)built when a run starts, so a `proxy` change — or the default change itself after an update — takes effect on the **next `veld start`** of that service, not for a service already running.

#### Notes and limitations

- **`remove` only unions — it cannot un-remove.** A header stripped at the project level cannot be re-enabled for one variant; the resolved `remove` is the union across levels. Set the rule at the narrowest level you need it, rather than broadly then trying to exempt a variant.
- **Header names are case-insensitive.** Both `remove` (dedup) and `set` (override) treat names case-insensitively, so `X-Foo` and `x-foo` are the same header. A header named in both `remove` and `set` is set (the value wins).
- **Don't `set` the proxy's own routing headers.** `set`/`remove` on `Host`, `Origin`, `Referer`, or `X-Forwarded-*` override the gateway's coherent-origin rewrite and forwarding metadata — you can break origin routing or spoof the client IP the origin sees. These are an escape hatch for people who know they need them.
- **WebSocket (101) responses on the gateway.** `response` rules are not applied to a WebSocket upgrade's `101` response — it carries only handshake-critical headers. `request` rules (including `remove: ["Origin"]`) *do* apply to upgrades.
- **On the gateway, response rules run last, but two security headers behave differently.** `Cache-Control: no-store` is only injected on password-mode content when the app itself is silent, so a `response.set` of `Cache-Control` overrides it — exactly as the app setting that header would. `Referrer-Policy: no-referrer`, however, is **non-negotiable**: the gateway forces it and re-asserts it *after* your rules (the slug is a bearer credential on link-access nodes), so a `response.set` of `Referrer-Policy` on the gateway has no effect. Both are gateway-only; the local proxy applies your rules as written.

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

### Setup & Teardown

Setup and teardown are project-level lifecycle steps that run outside the dependency graph. They are not nodes — they have no variants, no health checks, no outputs, and do not participate in the dependency graph.

**Setup** steps run sequentially (top to bottom) before any node starts. If any step exits non-zero, startup is aborted immediately. Use setup for prerequisite checks (is Docker running? is Node >= 20?) and environment preparation (create Docker networks, generate `.env` files, seed directories).

**Teardown** steps run sequentially after all nodes stop (after all per-node `on_stop` hooks complete). Teardown is best-effort — failures are logged but never block the stop operation. Commands should be written idempotently (e.g. `|| true` suffixes).

```json
{
  "setup": [
    { "name": "docker", "argv": ["docker", "info"], "failureMessage": "Docker must be running" },
    { "name": "node-version", "shell": "node -v | grep -q 'v2[0-9]'", "failureMessage": "Need Node >= 20" },
    { "name": "veld-network", "shell": "docker network create ${veld.name}-net 2>/dev/null || true" }
  ],
  "teardown": [
    { "name": "veld-network", "shell": "docker network rm ${veld.name}-net 2>/dev/null || true" }
  ]
}
```

#### Step fields

| Field            | Type   | Required | Description |
|------------------|--------|----------|-------------|
| `name`           | string | Yes      | Human-readable name for progress reporting and error messages |
| `argv` / `shell` | array / string | Yes (exactly one) | What to run. See [`argv` and `shell`](#argv-and-shell) |
| `failureMessage` | string | No       | Message shown when the command fails (non-zero exit) |

#### Variable availability

Setup and teardown steps run outside the node graph, so node-scoped variables —
`${veld.port}`, `${veld.url}`, `${veld.node}`, `${veld.variant}`, and
`${nodes.*}` — are not available. Everything project-level is:

| Variable | Description |
|----------|-------------|
| `${veld.name}` | Project name from `veld.json` |
| `${veld.project}` | Same as `${veld.name}` |
| `${veld.root}` | Absolute path to the directory containing `veld.json` |
| `${veld.run}` | Run name |
| `${veld.worktree}` | Slugified worktree directory name |
| `${veld.branch}` | Current git branch, slugified |
| `${veld.username}` | OS username |
| `${vars.*}` | Any project [`vars`](#vars) entry |
| Shell env vars | `$HOME`, `$PATH`, `$CI`, etc. — expanded by the shell (in a `shell` step; an `argv` step has no shell to expand them) |

`${veld.run_id}` is also unavailable: a `teardown` step runs from `veld stop` after
the run row is gone, so there is no id to report. Use `${veld.run}`.

> **Fixed in this release.** `${vars.*}` was documented here but silently empty —
> a setup step referencing a var failed with *"no var named …"* while the var was
> declared three lines up. Vars a setup step names are now resolved just before it
> runs (and, being cached, are not resolved a second time for the graph).

#### Execution lifecycle

The full execution lifecycle with setup and teardown is:

1. **Setup** — runs sequentially, gates startup
2. **Node graph** — resolved, ports pre-computed, stages executed
3. _(environment running)_
4. **Per-node `on_stop`** — runs in reverse dependency order
5. **Teardown** — runs sequentially, best-effort

If setup fails, Veld runs teardown steps to clean up anything that earlier setup steps may have created.

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
      "argv": ["./scripts/generate-certs.sh"]
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

### `proxy` (node-level)

Overrides the project-level `proxy` for all variants of this node. See [`proxy`](#proxy) for the field reference and merge semantics.

```json
"api": {
  "proxy": { "response": { "set": { "X-Frame-Options": "DENY" } } },
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
| `argv` / `shell`    | array / string   | Yes (exactly one) | All   | What to run — `argv` is spawned directly, `shell` runs via `sh -c` |
| `script`            | string           | Varies   | `command` only    | Path to script file, relative to `veld.json`          |
| `health_check`      | object           | No       | `start_server` | Legacy readiness probe. Deprecated: use `probes.readiness` |
| `probes`            | object           | No       | All            | Readiness and liveness probe configuration            |
| `depends_on`        | object           | No       | All            | Dependencies on other nodes                           |
| `env`               | object           | No       | All            | Extra environment variables                           |
| `outputs`           | array or object  | No       | All            | Output declarations (format varies by type)           |
| `sensitive_outputs`  | array of strings | No       | All            | Output keys to mask and encrypt                       |
| `url_template`      | string           | No       | `start_server` | URL template override for this variant                |
| `on_stop`           | object           | No       | All            | Teardown command (`{ "argv": … }` / `{ "shell": … }`) run when the environment is stopped |
| `skip_if`           | object           | No       | `command` only    | Idempotency check — skip if exits 0 (alias: `verify`)|
| `client_log_levels` | array of strings | No       | `start_server` | Browser log levels override for this variant          |
| `features`          | object           | No       | `start_server` | Feature toggles override for this variant             |
| `proxy`             | object           | No       | `start_server` | Reverse-proxy header rules override for this variant (see [Proxy](#proxy)) |
| `share`             | object           | No       | any (inert on `command`) | Sharing opt-in for this variant (see [Sharing](#sharing)) |

### `type`

#### `command`

Runs a shell command or script to completion. Used for setup tasks such as database cloning, seeding, data migration, or exporting remote service URLs.

- The working directory defaults to `${veld.root}` (the directory containing `veld.json`)
- Must specify exactly one of `argv`, `shell`, or `script`
- Can declare outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE` (preferred) or via `VELD_OUTPUT key=value` on stdout (legacy, discouraged — exposes values in terminal/logs)
- Built-in output: `exit_code`
- Supports the `skip_if` field for idempotency

```json
{
  "type": "command",
  "shell": "echo 'DATABASE_URL=postgresql://localhost:5432/mydb' >> \"$VELD_OUTPUT_FILE\"",
  "outputs": ["DATABASE_URL"]
}
```

#### `start_server`

Starts and manages a long-lived process. Veld allocates a port, injects it as `${veld.port}`, configures DNS and Caddy routing, and monitors health.

- The working directory defaults to `${veld.root}`
- Must specify exactly one of `argv` or `shell` (required)
- The process **must** bind to `${veld.port}` -- if it does not, the health check fails with a clear error
- Built-in outputs: `url` (the full HTTPS URL) and `port` (the allocated port number)
- Built-in variables: `${veld.port}` and `${veld.url}` are available in this node's `argv`/`shell`, `env`, and `outputs` templates
- Ports and URLs are **pre-computed** before any node executes, so `${nodes.X.url}` and `${nodes.X.port}` for any `start_server` node are available everywhere -- no dependency edge required
- Requires a readiness probe: use `probes.readiness` (preferred) or the legacy `health_check` field
- Users never see or deal with port numbers -- only clean HTTPS URLs

```json
{
  "type": "start_server",
  "argv": ["pnpm", "--filter", "backend", "dev", "--port", "${veld.port}"],
  "health_check": { "type": "http", "path": "/health" }
}
```

### `argv` and `shell`

**One vocabulary for every place Veld runs something.** Two keys, exactly one of
them:

| Key | Meaning |
|---|---|
| `"argv": ["pnpm", "dev"]` | An array, spawned directly. No shell, so no word splitting, no globbing, no `$VAR` expansion. |
| `"shell": "pnpm dev \| tee out.log"` | A string, run via `sh -c`. You own the quoting. |

```jsonc
"argv": ["docker", "run", "--rm", "--name", "veld-db-${veld.run}",
         "-p", "${veld.port}:5432", "postgres:16"]

"shell": "pnpm build && pnpm start --port ${veld.port}"
```

#### The `argv` guarantee

Interpolation runs **per element, after the array is fixed**, so a value
containing spaces, globs, quotes, or newlines can never change the argument
count:

```jsonc
"argv": ["psql", "${vars.db_url}"]   // always exactly two arguments
"shell": "psql ${vars.db_url}"       // a URL containing a space becomes two words
```

This holds on the detached path too, where the command runs inside a shell
pipeline: Veld passes the argv as positional parameters and expands `"$@"`, which
produces exactly one word per element whatever it contains.

#### `shell` is not deprecated

It is permanently supported, and that is what makes `argv` a safe default: any
node that misbehaves under `argv` can be reverted to a string by its author, with
no Veld change and no config version change. Use `shell` when you actually want a
shell — pipes, redirection, `&&`, globbing, `$VAR` expansion.

#### Where they apply

Everywhere Veld runs something: a variant, `on_stop`, `skip_if`, a
`command`-type probe, `actions[]`, `setup` / `teardown` steps, and
[value sources](#value-sources). In a nested position the pair is wrapped in an
object:

```jsonc
"on_stop": { "argv": ["docker", "rm", "-f", "veld-db-${veld.run}"] }
"skip_if": { "shell": "test -f .migrated" }
```

#### `command` (removed)

`command` was the single shell-string form in `schemaVersion` 1 and 2. Neither
version loads any more, and a `schemaVersion: "3"` document containing `command`
fails to load with a message naming every position. Replace each one with `argv`
or `shell` — see [docs/migrating-to-v3.md](migrating-to-v3.md).

### `script`

A path to a script file, relative to the project root. An alternative to
`argv`/`shell`.

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
  "argv": ["./scripts/check-db-ready.sh"],
  "timeout_seconds": 45,
  "interval_ms": 2000
}
```

### `probes`

Configures readiness and liveness probes for a variant. Available for both `command` and `start_server` types. `probes.readiness` supersedes the legacy `health_check` field.

```json
"probes": {
  "readiness": {
    "type": "http",
    "path": "/health",
    "timeout_seconds": 30
  },
  "liveness": {
    "type": "command",
    "argv": ["pg_isready", "-h", "localhost", "-p", "5432"],
    "interval_ms": 5000,
    "failure_threshold": 3,
    "max_recoveries": 3
  }
}
```

#### Readiness Probe

Gates the dependency graph during startup. Same fields as `health_check`. For `start_server` nodes, runs after the process starts. For `command` nodes, runs after the command exits 0.

#### Liveness Probe

Runs continuously after the node becomes healthy. Detects failures like dropped SSH tunnels, crashed background processes, or unreachable databases. Supports the same three check types as readiness probes:

- **`http`**: Polls an HTTP endpoint. Passes when the expected status code is returned.
- **`port`**: Checks if a TCP port is accepting connections.
- **`command`**: Runs an arbitrary shell command (via `sh -c`). Exit code `0` means healthy, non-zero means unhealthy. Pipes, redirects, and `&&` chains all work. The node's outputs are injected as environment variables, so you can reference them directly (e.g., `pg_isready -h $DB_HOST -p $DB_PORT`).

| Field               | Type    | Required | Description                                                  |
|---------------------|---------|----------|--------------------------------------------------------------|
| `type`              | string  | Yes      | Strategy: `"http"`, `"port"`, or `"command"`                 |
| `path`              | string  | No       | HTTP path to poll (`http` type only)                         |
| `expect_status`     | integer | No       | Expected HTTP status code (`http` type only, default: 200)   |
| `command`           | string  | No       | Shell command to run (`command` type only)                    |
| `interval_ms`       | integer | No       | Milliseconds between checks (default: 5000, min: 1000)      |
| `failure_threshold` | integer | No       | Consecutive failures before triggering recovery (default: 3) |
| `max_recoveries`    | integer | No       | Max recovery attempts before permanent failure (default: 3)  |

When `failure_threshold` consecutive liveness checks fail, Veld automatically restarts the entire environment (equivalent to `veld restart`). If the restart succeeds and the probe starts passing, the node returns to healthy. If `max_recoveries` restart attempts are exhausted, the node is marked as permanently failed and no further restarts are attempted. You can see recovery status via `veld status` and `veld logs --source internal`.

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
  "argv": ["docker", "run", "--rm", "-p", "${veld.port}:5432", "postgres:16"],
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

### `skip_if`

An idempotency check command (previously named `verify`, which is still accepted as an alias). Only applies to `command` type variants. Before running the main command/script, Veld executes the `skip_if` command:

- **Exit code 0:** The step is considered already complete and is skipped.
- **Non-zero exit code:** The step runs normally.
- If `skip_if` itself errors unexpectedly, the step re-runs (safe default).

The `skip_if` command receives the previous run's output variables as environment variables, so it can check whether the previous result is still valid.

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "skip_if": { "argv": ["./scripts/verify-db.sh"] },
  "outputs": ["DATABASE_URL"]
}
```

### `on_stop`

A teardown command that runs when `veld stop` is called. Executed in reverse dependency order, after the process is killed (for `start_server` nodes) but before state is cleaned up.

This is especially useful for `command` nodes that provision external resources during start — databases, Docker containers, temporary credentials — that need explicit cleanup.

```json
{
  "type": "command",
  "argv": ["docker", "run", "-d", "--name", "veld-db-${veld.run}", "-p", "${veld.port}:5432", "postgres:16"],
  "on_stop": { "argv": ["docker", "rm", "-f", "veld-db-${veld.run}"] },
  "outputs": ["DATABASE_URL"]
}
```

Declared at node level, `on_stop` applies to every variant. A variant replaces it by
declaring its own, and **disables it with `"on_stop": null`**:

```jsonc
"db": {
  "on_stop": { "argv": ["docker", "rm", "-f", "veld-db-${veld.run}"] },
  "variants": {
    "docker":   { /* inherits the hook */ },
    "external": { "on_stop": null }   // nothing to tear down — and nothing runs
  }
}
```

The `on_stop` command receives the same variable context that was available during start:
- Every `${veld.*}` built-in the node itself had — for a `start_server` node that includes `${veld.port}`, `${veld.url}`, `${veld.url.hostname}` and the rest of the URL family, and `${veld.ports.<name>}`. See [Availability](#availability).
- This node's outputs as `${output.KEY}`, and its own as `${nodes.<self>.KEY}` (including the automatic `exit_code` of a `command` node)
- Environment variables from the variant's `env` block

Naming a resource after the same built-ins in `argv` and in `on_stop` is the point:
a container called `${veld.project}-${veld.node}-${veld.run}` in both places cannot
drift, whereas the same name copied by hand into two places is exactly how a
container survives a `veld stop`.

> **Changed in this release.** `on_stop` used to receive `${veld.port}` but not
> `${veld.url}`, `${veld.url.*}`, or `${veld.ports.*}` — an asymmetry with no
> reason behind it, and the URL half is the one a teardown hook is more likely to
> want. All of them are now populated.

> **Changed:** node outputs used to *also* be reachable as `${veld.KEY}` here — and only
> here. That let an output named like a built-in (`run`, `branch`, `port`) shadow it during
> teardown but nowhere else, so the same string resolved to two different values. `veld.*`
> is now a closed set. Rewrite `${veld.exit_code}` as `${output.exit_code}` and
> `${veld.MY_OUTPUT}` as `${output.MY_OUTPUT}`. `veld lint` and `veld start` reject any
> `${veld.*}` name that is not a built-in, so this is caught before a run starts rather
> than at teardown.

If the `on_stop` command fails (non-zero exit code or execution error), Veld logs a warning but continues tearing down the remaining nodes. A failing teardown hook never blocks the stop operation.

If `on_stop` references a variable that cannot be resolved, the hook is **skipped** — Veld
prints a prominent warning naming the command and what it was meant to clean up, because
whatever it would have removed has been left behind.

`on_stop` works with both `command` and `start_server` variants:

```json
{
  "type": "start_server",
  "argv": ["docker", "run", "--rm", "--name", "veld-redis-${veld.run}", "-p", "${veld.port}:6379", "redis:7"],
  "on_stop": { "argv": ["docker", "stop", "veld-redis-${veld.run}"] },
  "health_check": { "type": "port" }
}
```

---

## Sharing

Sharing has two config surfaces: an environment-wide `sharing` block (relays and the public gateway) and a per-variant `share` opt-in. A service is shareable **only** if its variant declares `share` — `veld share` refuses any service that hasn't opted in. This makes what leaves your machine explicit and auditable.

> **Behavior change:** earlier versions shared every URL-bearing service in a run. Now nothing is shared until its variant declares `share.expose`; `veld share` errors (listing the candidate `node:variant`s) until you opt one in.

```json
{
  "sharing": {
    "relays": ["https://relay.acme.internal"],
    "gateway": "https://share.acme.internal"
  },
  "nodes": {
    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "argv": ["npm", "run", "dev"],
          "share": { "expose": ["peer"] }
        }
      }
    }
  }
}
```

### `sharing.relays`

Which iroh relays share traffic routes through — a compliance control. Relays forward only sealed, end-to-end-encrypted bytes; they never see URLs or content. **Relays must be opted into explicitly — including public.** There is no implicit default, so nothing is routed over public relays by accident; `veld share` refuses a run whose config sets no relay (and no `VELD_SHARE_RELAY` env override is present).

- `"public"` — n0's public relays (an explicit opt-in, not a default). **Development and testing only.** These are a free community service shared across all iroh users worldwide: rate-limited, best-effort, no uptime or throughput guarantees. Per [iroh's guidance](https://docs.iroh.computer/concepts/relays), don't route production or high-volume sharing over them — self-host instead. (A fair-use recommendation from n0, not a licensing restriction — iroh is MIT/Apache-2.0.)
- An array of relay entries — confine traffic to relays you self-host (a single Docker container). Must be non-empty; use `"public"` for public relays. Each entry is either a bare URL string or an object `{ "url": ..., "token": ... }` (see [Relay auth tokens](#relay-auth-tokens)). **The recommended choice for production sharing.**

When set here, config wins over the legacy `VELD_SHARE_RELAY` env var (read from the daemon's process environment, not your shell, and not an enforceable floor — `"relays": "public"` overrides it). The custom-relay guarantee covers **both legs**: the join side automatically confines to the relay(s) advertised in the ticket, so a share minted on a self-hosted relay is never joined over n0's public relays. A joiner only needs env config to supply a *token* for a token-gated relay (see [Relay auth tokens](#relay-auth-tokens)). The daemon binds **one iroh endpoint per relay policy** on demand, each with its own node identity (the public endpoint reuses the daemon's persistent identity; custom-relay endpoints get a fresh per-run one), so shares on different relays run concurrently without conflict.

#### Relay auth tokens

A self-hosted relay can require an **authorization token** so it isn't an open relay anyone can route through. Veld sends the token to the relay as an `Authorization: Bearer <token>` header when it connects (iroh's native relay auth). This is a lightweight gate — "you need the shared secret to use our relay" — not a per-user identity system.

Give a relay entry a `token` to send one:

```json
{
  "sharing": {
    "relays": [
      "https://open.acme.internal",
      { "url": "https://lit.acme.internal",  "token": "the-shared-secret" },
      { "url": "https://env.acme.internal",  "token": { "env": "VELD_RELAY_TOKEN" } },
      { "url": "https://file.acme.internal", "token": { "file": "/run/secrets/relay-token" } },
      { "url": "https://op.acme.internal",   "token": { "argv": ["op", "read", "op://vault/relay/token"] } }
    ]
  }
}
```

`token` is one of:

| Form | Meaning | Use for |
|------|---------|---------|
| `"a-string"` | Literal token, inline in config | Quick local setup — but it lands the secret in `veld.json` (and version control) |
| `{ "env": "VAR" }` | Read environment variable `VAR` **from the daemon's process environment, not your shell** (so `export VAR=… && veld share` won't work — a running daemon doesn't inherit it) | 12-factor / CI secret injection |
| `{ "file": "/path" }` | Read the file's contents (trailing whitespace trimmed). Use an absolute path — a relative one resolves against the daemon's working directory, not your project | Docker / Kubernetes secret mounts (`/run/secrets/…`) |
| `{ "argv": ["…"] }` | Run the string through `sh -c` and use its stdout (trailing whitespace trimmed). Runs with your login-shell `PATH`, so user-installed CLIs (`op`, `vault`, brew tools) are found even though the command executes on the daemon — but *only* `PATH` is inherited, not other shell-exported variables or aliases | 1Password, Vault, or any secret-manager CLI |

All forms trim trailing whitespace (secret stores commonly append a newline). Prefer the `env` / `file` / `command` forms over a literal so the secret stays out of the config file. The token is resolved on the daemon at share time; a token that fails to resolve (missing env var, unreadable file, command exits non-zero or times out, or an empty result) is a hard error — Veld never binds a relay unauthenticated when a token was declared. `command` runs an arbitrary shell command from your config, exactly like `start_server`/`command` steps already do, so the same trust applies: only run configs you trust.

**Running a token-gated relay.** The `token` here is only the *client* half — Veld sends it as an `Authorization: Bearer <token>` header. Your relay must be configured to *require* it, or it stays an open relay regardless. Veld's relays are [iroh relays](https://iroh.computer); a self-hosted `iroh-relay` enforces a shared token via its `access.shared_token` config (a list of accepted tokens). Deploy it with its own TLS on a reachable host — see iroh-relay's docs for the relay-side config; Veld only speaks the client side.

**Joining a token-gated relay.** A per-relay `token` in `veld.json` applies only to **hosting** (`veld share`). The join side has no project config: a joiner learns *which* relay to use from the ticket automatically (so a custom-relay share is always joined over that relay, never public), but to authenticate to a **token-gated** relay it needs the token. In precedence order (highest first), the joiner's token comes from:

1. **A token entered at the prompt** this attempt (see below).
2. **The ticket itself** — only if the host opted into `dangerouslyEmbedRelayTokensInTicket` (see below).
3. **The joiner's local cache** — a token entered at a previous prompt, cached per relay URL in the central veld database (`<data_dir>/veld/veld.db`, `0600`).
4. **The joiner's env** — `VELD_SHARE_RELAY` + `VELD_SHARE_RELAY_TOKEN` on **their** daemon. Veld attaches that token only when `VELD_SHARE_RELAY` matches the relay URL in the ticket, so the secret is never sent to a relay the joiner did not name.

There is no `veld.json` path for a joiner's relay token.

**The prompt.** If none of the above produces a working token, the join detects the relay's auth denial (iroh reports the relay connection as *not authorized*, distinct from an unreachable host) and **asks for the token**: the browser join overlay shows a token field (with a "remember for this relay" checkbox, on by default), and `veld join` prompts on the terminal. A supplied token is verified against the relay; on success it's cached so future joins to that relay don't re-prompt — clear the "remember" checkbox (browser) or pass `veld join --no-remember` to skip caching — and a wrong token re-prompts. `veld join --json` does not prompt — it returns `{ "needs_relay_token": "<relay-url>" }` so a caller can supply `relay_tokens` on a retry.

#### `sharing.dangerouslyEmbedRelayTokensInTicket`

**DANGER — off by default.** When `true`, the host resolves its relay auth token(s) and **embeds them in the share ticket**, so a joiner needs no out-of-band token setup — the ticket alone authenticates. Named à la React's `dangerouslySetInnerHTML` (hence the camelCase key standing out from veld's snake_case config) to force a deliberate choice.

```json
{
  "sharing": {
    "relays": [{ "url": "https://relay.acme.internal", "token": { "argv": ["op", "read", "op://vault/relay/token"] } }],
    "dangerouslyEmbedRelayTokensInTicket": true
  }
}
```

This ships the relay secret **inside every share link** — Slack, email, browser history, anywhere a `veld.localhost/join#…` URL travels. That defeats the token's purpose (keeping the relay from being an open one) for any **shared or long-lived** relay secret. Enable it **only** for a **disposable, per-project token you rotate freely** — never a shared org relay secret. When off (the default), a joiner supplies the token via the env vars above.

Because a token declaration is part of a relay's endpoint identity, changing the *declaration* (e.g. switching the `env` var name) is picked up on the next share, but rotating the *underlying* secret behind an unchanged declaration takes effect only when the daemon next binds that relay — in practice, on daemon restart, since a bound endpoint is cached for the daemon's life.

### `sharing.gateway`

The public web gateway this environment registers `web` shares with (used by `veld share --web`). A bare URL string, or an object carrying the gateway's registration auth token:

```json
{
  "sharing": {
    "gateway": { "url": "https://share.acme.internal", "token": { "env": "VELD_GW_TOKEN" } }
  }
}
```

`token` is a secret source exactly like [relay auth tokens](#relay-auth-tokens) — a literal string, `{ "env": … }`, `{ "file": … }`, `{ "argv": [ … ] }`, or `{ "shell": … }` — resolved on the daemon when the share starts, Debug-redacted, and required (the gateway never accepts unauthenticated registrations). Without config, the `VELD_SHARE_GATEWAY` + `VELD_SHARE_GATEWAY_TOKEN` env vars (on the **daemon's** environment) work as an ad-hoc override; config wins when both are present.

The gateway itself is one self-hosted container — see the [gateway operator guide](gateway.md) for deployment (DNS, TLS, env vars).

### `share.expose`

The audiences a variant may be shared to. A list; an empty list (or an absent `share` block) means the service is never shareable.

| Value  | Audience          | URL fidelity                       | Command |
|--------|-------------------|------------------------------------|---------|
| `peer` | Other Veld users  | Verbatim — exact origin URL reproduced | `veld share` |
| `web`  | Any browser (no Veld needed) | Best-effort — real public URL minted by the gateway, origin `Host` preserved toward the app, redirects/cookies adapted (see the [operator guide](gateway.md)) | `veld share --web` |

```json
"share": { "expose": ["peer", "web"] }
```

The two audiences are independent shares with independent capabilities: `veld share` serves the `peer`-opted services, `veld share --web` mints a separate share of the `web`-opted ones and registers it with the gateway — revoking one never touches the other. Sharing only ever exposes services with a URL, so `share` is meaningful on `start_server` variants; on a `command` variant it is accepted but inert (nothing to share).

### `share.web.access`

How a browser viewer is admitted to this service's public URL (web shares only):

| Value | Meaning |
|-------|---------|
| `password` | **Default (also when absent).** The gateway shows a password page before serving; `veld share --web` generates and prints the share password (`--password` to choose your own). One password per web share, entered once per service — a session cookie (12 h, never outliving the share) keeps the viewer in. |
| `link` | Anyone with the URL is served. The unguessable 128-bit slug is the only gate — treat the link as a secret. |

```json
"share": { "expose": ["web"], "web": { "access": "link" } }
```

An **explicit** value here always wins over the `veld share --web --access …` flag — the flag only sets the mode for services whose config is silent. Config is the reviewable compliance surface; the CLI can never weaken it.

Two things `share.web` deliberately does **not** hold:

- **The password value.** It is generated per share (or set with `--password`, min 8 chars) — never stored in `veld.json`, where it would land in version control. A `"password"` key under `share.web` is rejected at config load: unlike most config blocks, `share.web` rejects unknown keys outright, so a typo'd or unsupported key fails loudly instead of being silently ignored. (One consequence: a `veld.json` using a `share.web` key added by a *newer* Veld won't load on an older daemon — keep the toolchain aligned when you adopt a new `share.web` field.)
- **Live-share edits.** The policy is captured when the share starts; editing `share.web.access` (or wanting a new password) while a web share is live has no effect on it. Re-run `veld share --web` — this replaces the share and **rotates the public URLs and the password** (old links die; the CLI warns when a replacement happened).

Practical note for multi-service shares: the viewer's session cookie is scoped per public host, so a password-protected API called cross-origin by a shared frontend gets 401s. Give API nodes `"web": { "access": "link" }` — their slug is unguessable and only the app's code ever uses it.

---

## Splitting the config across files

Set `schemaVersion: "3"` and add `include` globs to the root file. Every config
file uses **the same schema**, with all top-level keys optional except
`schemaVersion` and `name` in the root — so `$schema` autocompletion works in every
file and there is one schema to learn.

```jsonc
// veld.json — the root file
{
  "$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json",
  "schemaVersion": "3",
  "name": "example-monorepo",
  "include": [
    "veld.d/*.jsonc",
    "apps/*/veld.node.json",
    "services/*/veld.node.json"
  ],
  "presets": { "core": ["api:dev", "web:dev"] }
}
```

```jsonc
// services/api/veld.node.json — CODEOWNERS: /services/api/ @some-team
{
  "nodes": {
    "api":       { /* … */ },
    "worker":    { /* … */ },
    "api-build": { /* … */ }
  }
}
```

**A node is defined in exactly one file. A file may define any number of nodes.**

| Rule | Why |
|---|---|
| The same node name in two files is an error naming both | Removes precedence rules for node bodies entirely — there is never a question of which file won |
| A file that fails to parse is a named, fatal error | Skipping it would turn a typo into "unknown node", answered three hours later |
| Duplicate `vars` or `preset` names across files are errors | No shadowing, no file-local scope, no ordering dependency |
| Relative paths resolve from the **project root**, not the declaring file | A file-relative reading would silently change every existing `cwd`, `script`, and output path |
| Only the root file may `include` | Nested includes would make load order — and so error messages — depend on a graph nobody can see |

Glob syntax: `*` matches within one path segment, `?` one character, `**` across
segments. Dotfiles are not matched by a bare `*`. Matches load in sorted order, so
errors are deterministic.

`vars`, `presets`, `env`, `setup`, `teardown`, `hooks`, and `ui` may appear in any
file. Other project-level settings (`url_template`, `features`, `proxy`,
`sharing`, `client_log_levels`) are read from the root file.

### Finding out why a node seems missing

```sh
veld config --files
```

prints each glob, the files it matched, and the nodes each defines. Under globs,
"unknown node" has four different causes — never defined, defined but not matched
by a glob, its file renamed out of a glob, or its file present but unparseable —
and this is what tells them apart. `veld nodes` also shows `file:line` per node.

---

## Node-level defaults

A node may declare any variant field **once**, at node level; **any variant may
override it**.

```jsonc
"api": {
  "type": "start_server",
  "probes": { "readiness": { "type": "http", "path": "/healthz" } },
  "env": { "API_URL": "${vars.remote_api}", "LOG_LEVEL": "info" },
  "ports": { "http": "auto" },
  "variants": {
    "dev": { "argv": ["node", "server.js"] },
    "debug": {
      "argv": ["node", "--inspect", "server.js"],
      "ports": { "debug": "auto" },
      "env": { "LOG_LEVEL": "debug", "API_URL": null }
    }
  }
}
```

This deduplicates **values, never structure**. Which keys a node has is still
written in that node, and `rg API_URL` still finds the line that sets it. There is
**no inheritance, no mixins, no templates** — a variant body is never assembled
from somewhere else.

Hoistable: `type`, `argv`/`shell`, `cwd`, `env`, `ports`, `files`, `depends_on`,
`probes`, `outputs`, `on_stop`, `share`, `features`, `proxy`,
`client_log_levels`, `url_template`.

### The merge table

There are **three** distinct strategies, plus `proxy`'s pre-existing one. They are
deliberately not unified.

| Field | Node → variant | How a variant removes it |
|---|---|---|
| `env`, `ports`, `depends_on`, `files` (maps) | **Additive per key**; the variant wins on collision | `"KEY": null` |
| `features` | Per field; the variant wins field by field | — |
| `probes.readiness`, `probes.liveness` | **Replace the whole probe object** | `"liveness": null` |
| `share` | Replace wholesale | `"share": null` |
| `outputs` | Replace wholesale | `"outputs": null` |
| `proxy` | `remove` lists **union** case-insensitively, `set` maps override per key; a header in both is resolved in favour of `set` | — |
| `type`, `cwd`, `argv`, `shell`, `url_template` | Replace | — |
| `on_stop` | Replace | `"on_stop": null` |
| `skip_if` | *not a node-level field* — declare it per variant | — |

Two of these are worth the words:

- **`probes` replaces per probe** — a deliberate exception to the per-field
  pattern `features` uses. A probe is a tagged union, so field-by-field merging
  would let a variant switch `type: "http"` to `type: "command"` and silently
  inherit a stale `path`, producing a probe that checks the wrong thing forever.
- **`share` replaces wholesale** — sharing is a consent decision, and a
  half-inherited `expose` list is exactly the surprise it must not have.

`null` is only valid on a key that is optional. `strict_outputs` and the
project-level `url_template` are not, and setting either to `null` is an error
naming the field.

---

## `vars`

One definition point per value, referenced by name at every use site.

```jsonc
// veld.d/vars.jsonc
{
  "vars": {
    "remote_api":  "https://api.example.com",
    "health_path": "/healthz",
    "db_password": { "value": "devpassword", "secret": true },
    "db_url":      { "argv": ["secret-tool", "read", "path/to/secret"], "secret": true }
  }
}
```

```jsonc
"env": {
  "DATABASE_URL": "${vars.db_url}",
  "API_URL":      "${vars.remote_api}"
}
```

**The key never leaves the node that uses it.** `${vars.db_url}` is written where
`DATABASE_URL` is set, so a reader of that node still sees which keys it has and
`rg DATABASE_URL` still finds the line.

The rules, all enforced:

1. **A var is a scalar or a single [value source](#value-sources)** — never an
   object, never a partial config body. If a probe block or an `env` map could
   live in a var, this would be a template system.
2. **A var may not reference another var.** One hop, always, so provenance is a
   single lookup.
3. **A duplicate var name is an error**, including across files. No shadowing, no
   file-local scope, no ordering dependency.
4. **An unknown `${vars.x}` is an error** listing the declared names, because the
   cause is nearly always a typo.

A var is resolved **at most once per run**, so a var backed by a command runs that
command exactly once — two references to a rotating credential can never disagree.

**Only if something reaches for it.** A var whose value is a
[value source](#value-sources) is resolved only when the resolved plan can reach
it — a node in the plan, or a project-level surface like `env`, `setup`, or
`teardown`, which every node inherits. This is the same laziness a node-level
`env` source always had. It matters when the source is a credential helper:
putting the command in a var is the natural move, since it is one value used by
several nodes, and it used to mean *every* `veld start` reached for the credential
store — including runs whose nodes need no secret at all.

### `${veld.*}` inside a var

A var literal is interpolated against the **run-scoped** built-ins:

```jsonc
"vars": {
  "run_url": "https://web.${veld.run}.example.test",
  "cache":   "${veld.project}-${veld.branch}"
}
```

| In a var value | |
|---|---|
| `run`, `run_id`, `name`, `project`, `root`, `worktree`, `branch`, `username` | resolved |
| `port`, `url`, `url.*`, `ports.*`, `node`, `variant` | **error** (`builtin-not-in-scope`) |

The second group is per-node, and a var is one value for the whole run, so
`${veld.port}` in one could only mean some arbitrary node's port. Compose those at
the use site instead — a node's `env` can mix `${veld.port}` and `${vars.…}` in
the same string.

A var whose value is a *fetched* source (`file`, `env`, `argv`, `shell`) is used
verbatim, exactly as a node's `env` treats one: substituting inside content that
came out of a secret store would make the store an interpolation vector.

```sh
veld config --why nodes.api.variants.dev.env.DATABASE_URL
```

prints the effective value, where it was defined, and what it overrides. A
`secret` value is described, never printed, and there is no flag that will.

---

## Value sources

A value is **a string**, or **an object with exactly one source key** plus an
optional `secret` flag.

```jsonc
"env": {
  "REGION":       "eu-central-1",
  "PG_PASSWORD":  { "value": "devpassword", "secret": true },
  "GITHUB_TOKEN": { "env": "GITHUB_TOKEN", "secret": true },
  "SIGNING_KEY":  { "file": ".secrets/signing.key", "secret": true },
  "DATABASE_URL": { "argv": ["secret-tool", "read", "path/to/secret"], "secret": true }
}
```

| Source | Meaning |
|---|---|
| `value` | An inline literal. This object form exists so a literal can carry `secret: true` |
| `env` | Read from the environment Veld was launched with. **Missing at start is an error naming the node and the variable** |
| `file` | The (trimmed) contents of a file, relative to the project root |
| `argv` / `shell` | Run a command, take its trimmed stdout |

**Nothing in Veld knows any vendor tool.** `argv` runs a command and reads stdout;
which command is your business. There is no provider table and no vendor name in
the schema.

Allowed in: `env.*`, `vars.*`, `sharing.relays[].token`,
`sharing.gateway.token`, and [`files`](#files).

**Not yet supported** in `proxy.*.set.*` or `actions[].parameters.*` — those take
plain strings. Both are on the list because a proxy `Authorization` header is one
of the likeliest real secrets in a config, but the resolved proxy travels to Caddy
and to the public web gateway inside a route, so marking one secret means teaching
those paths to carry and scrub it. Until then, **do not put a credential in a proxy
header value**: it is sent to every joiner of a share and to the gateway verbatim.

Refused in `argv`/`shell` elements, `cwd`, `depends_on`, `url_template`, `type`,
and `include` — those must be statically known for graph building and linting.

### Timing

Sources are resolved **at most once per run, at start, after the graph is built**
— never during parse. That ordering is what keeps `veld stop`, `veld status`, and
the daemon monitor working against a config whose secret source has since broken:
they parse the config, they never resolve it.

Only what the plan reaches is resolved at all — a node's `env` source when that
node is in the plan, a [`vars`](#vars) source when something in the plan
references it. `setup` steps run before the graph does, so the vars they name are
resolved just before them and the rest just before the first node spawns; either
way each is resolved once.

Only an **inline literal** is interpolated. A value fetched from a file, the
environment, or a command is used verbatim — substituting `${…}` into fetched
content would make any secret store an interpolation vector.

A source command has a **30-second timeout**, and the message says why it matters:
an interactive credential helper (a biometric prompt, an MFA push) has no terminal
when the run is started by the daemon, so it hangs rather than failing. The fix is
a non-interactive source, not a longer timeout. Source commands inherit your
login-shell `PATH`, so `op`, `vault`, and version-manager shims are found.

### `secret`

`secret: true` is a declaration, not a type. It is what lets Veld *refuse* the
unsafe uses.

Where a secret may go: a child process's environment, or a [file](#files). **Not**
interpolated by Veld into an `argv` element or a `shell` string (both appear in
the process table, readable by every other user on the machine), not into logs,
not into `--json` output, not into the share payload.

**`$KEY` is fine; `${vars.db_pass}` is not.** The rule fires on the forms *Veld
resolves*, and only those — they are the only ones that can put a value in the
process table. A bare `$KEY` is not one: Veld's interpolation consumes `${…}` and
nothing else, so `$KEY` reaches `argv` untouched and is expanded later by a shell
*in the child*, where the value never appears in any process's arguments.

```jsonc
"env": { "DB_PASS": { "shell": "pass show db", "secret": true } },
// fine — the shell expands $DB_PASS after the process is already running
"shell": "psql \"postgres://u:$DB_PASS@localhost/db\"",
"argv": ["bash", "-lc", "psql \"postgres://u:$DB_PASS@localhost/db\""],
// also fine — the container is handed the name, not the value
"argv": ["docker", "run", "-e", "DB_PASS", "img"]
```

Refused: `${vars.db_pass}`, `${output.DB_PASS}`, and `${nodes.db.PASSWORD}`
anywhere in a command — Veld substitutes those into the command string itself.

Not refused, and not a leak either way: `${DB_PASS}` (no such Veld namespace, so
interpolation fails rather than substituting) and a bare `$DB_PASS` in an `argv`
element nothing expands (inert text — a mistake, but not an exposure).

### Lint rules

| Rule id | What it catches | Severity |
|---|---|---|
| `secret-in-command` | A value marked `secret` that Veld would *substitute* into `argv` or `shell` — `${vars.x}`, `${output.x}`, `${nodes.a.x}`. A bare `$NAME` is not flagged: Veld never expands it, so it cannot reach the process table | **error** |
| `ambiguous-root-config` | A directory holding both `veld.json` and `veld.jsonc`. Veld reads `veld.json`, so the file you edit may not be the one it runs | **error** (a finding, not a load failure — `veld stop` still works) |
| `credential-shaped-literal` | A credential-shaped literal (`sk-`, `ghp_`, a JWT, `scheme://user:pass@host`), marked or not | warn |
| `credential-shaped-proxy-header` | The same shape in a `proxy` header value, which travels to Caddy and to every joiner verbatim | warn |
| `start-server-needs-readiness` | A `start_server` with no readiness probe | **error** in a v3 config, warn in v1/v2 |
| `unknown-var` | `${vars.x}` naming a var that is not declared, listing the declared names | **error** |
| `vars-cannot-nest` | A var referencing another var | **error** |
| `unknown-builtin-var` | `${veld.x}` naming something that is not a built-in at all | **error** |
| `builtin-not-in-scope` | A real built-in written where it is not populated — `${veld.url}` in a `command` node, `${veld.port}` in a `vars` value, `${veld.node}` in a `setup` step. See [availability](#availability) | **error**, or warn when only *some* variants inheriting the value lack it |
| `unknown-node-ref` | `${nodes.X.…}` naming a node that is not defined anywhere | **error** |
| `preset-missing-node-ref` | `${nodes.X.…}` where `X` is real but not in a given preset's plan — the "works with preset A, dies with preset B" case, named by preset | **error** |
| `preset-unresolvable` / `preset-unknown-node` / `preset-unknown-variant` | A preset with a dangling `@ref`, a cycle, or a node/variant that does not exist | **error** |
| `secret: true` literal that is not credential-shaped (e.g. `"devpassword"`) | — | silent — the legitimate fixed-local-credential case |

A declared `env` source missing at start is not a lint rule — it cannot be known
statically — but it is an **error** at start, naming the node and the variable.

All of them run in `veld lint`; the errors also run inside `veld start`. None of
them runs in the loader, so `veld stop` is never blocked by one. `veld lint --json`
emits the rule id with each finding.

---

## `ports`

A node may declare multiple named ports; Veld allocates each.

```jsonc
"ports": { "http": "auto", "debug": "auto", "metrics": "auto" }
```

- Referenced as `${veld.ports.http}`, and exported to the process as
  `VELD_PORT_HTTP`.
- Referenced across nodes as `${nodes.api.ports.debug}`.
- `${veld.port}` remains the **primary** — the one named `http`, or the sole entry
  when only one is declared. Several ports with none named `http` is an error,
  rather than a guess about what `${veld.port}` means.
- A probe may name one: `"probes": { "readiness": { "type": "port", "port": "http" } }`.
  A multi-port node's readiness is rarely "any port is open" — a debugger port
  opens long before the app is listening.
- **A `start_server` with no `ports` declaration behaves exactly as before:** one
  allocated port.

A fixed number (`"debug": 9229`) is accepted but discouraged: a literal port
silently breaks parallel worktrees, which is the reason named auto-ports exist. If
a fixed port is taken, Veld errors rather than substituting another — a debugger
pointed at 9229 must reach the process that asked for it.

---

## `files`

Deliver a value to disk, for a program that can only read a credential from a
file.

```jsonc
"files": {
  ".secrets/client.pem": { "env": "CLIENT_CERT", "secret": true, "mode": "0400" },
  "config/app.conf":     { "value": "verbose=1" }
}
```

Same [value sources](#value-sources) as everything else. Paths resolve from the
project root and parent directories are created. The file is created **with** its
mode rather than chmod-ed afterwards, so a credential is never briefly
world-readable, and `mode` defaults to `0600`.

`mode` is a string, in octal, with or without the leading zero — a bare number has
already lost its leading zero, so it is refused rather than guessed at.

---

## Reserved: `hooks` and `ui`

Both are **reserved**: they parse, are stored, and are **not executed by this
version**. `veld lint` says so, so a hook that does nothing is distinguishable
from a config mistake.

```jsonc
// veld.d/hooks.jsonc
{
  "hooks": {
    "worktree.created": [ { "argv": ["./scripts/setup-worktree.sh"] } ],
    "project.created":  [ { "argv": ["pnpm", "install"] } ],
    "run.stopped":      [ { "argv": ["./scripts/collect-artifacts.sh"] } ]
  }
}
```

Rules, stated now so the work that implements them does not distort the node
model:

1. **Hooks are not nodes.** They do not join the dependency graph, get no
   allocated port, and have no probes. If something needs readiness or a port, it
   is a node.
2. They use the same `argv`/`shell` vocabulary and the same value sources. There
   is no second command syntax anywhere.
3. **Repo-declared only.** A hook may never originate from a fetched or remote
   extension — hooks run arbitrary code on a developer machine, and keeping them
   in reviewed repo files is what preserves Veld's no-remote-execution guarantee.

Every **other** unknown top-level key is an **error reported by `veld lint` and
`veld start`** — not a load failure. A typo is still caught, but it cannot strand
`veld stop`, whose job is to tear down an environment that is already running (see
[the parse/validate split](#veld-lint)). If you are upgrading a config that used
the pre-JSONC `"//": "…"` comment idiom, that key now reports a finding telling you
to make it a real comment.

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

### Composing presets

An entry starting with `@` references another preset, so overlapping sets need not
repeat every selection and then drift:

```json
"presets": {
  "core": ["api:dev", "web:dev"],
  "ci":   ["@core", "e2e:dev"]
}
```

Selections are de-duplicated, so two references to the same preset never start
anything twice. A cycle is an error naming the path (`@a → @b → @a`).

`veld lint` checks all of this **statically**, so a broken preset fails at lint time
rather than at `veld start`:

| Rule | Fires when |
|---|---|
| `preset-unresolvable` | an `@ref` names a preset that does not exist, or the references form a cycle |
| `preset-unknown-node` | a selection names a node that is not defined. With `include` globs this can also mean no glob matched its file — `veld config --files` prints the glob → file → node chain |
| `preset-unknown-variant` | a selection names a real node but a variant it does not have; the message lists the variants it does have |
| `preset-missing-node-ref` | a node in this preset's plan references `${nodes.X.…}` and `X` is not in that plan — the message names both the preset and the reference |

The last one is the "works with preset A, dies with preset B" case. A preset's plan
is fully static — `expand_preset` plus the transitive `depends_on` closure — so
given `"thin": ["web:dev"]` and a `web` whose `env` reads `${nodes.api.url}`, lint
can already tell that `api` is not in `thin`'s plan. In a config with many
overlapping presets that combination is the one thing you cannot check by reading a
single node file. A node pulled in only by `depends_on` counts as present.

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
| `${veld.name}`            | Project name (alias for `${veld.project}`)           |
| `${veld.run}`           | Run name                                             |
| `${veld.run_id}`        | Stable run UUID                                      |
| `${veld.root}`          | Absolute path to the directory containing `veld.json`|
| `${veld.project}`       | Project name from `veld.json`                        |
| `${veld.worktree}`      | Slugified worktree directory name                    |
| `${veld.branch}`        | Current git branch, slugified (empty string if not in git) |
| `${veld.username}`      | OS username                                          |
| `${veld.node}`          | This node's name                                     |
| `${veld.variant}`       | This variant's name                                  |
| `${veld.ports.<name>}`  | A named port from this node's `ports` map (see [`ports`](#ports)) |

**`veld.*` is a closed set.** A node's *outputs* are **not** in it — they are
`${output.KEY}` (this node) and `${nodes.<node>.KEY}` (any node). `veld lint` and
`veld start` reject any `${veld.*}` name that is not listed above, so a typo or a
leftover `${veld.MY_OUTPUT}` is caught before a run starts.

> **Changed in this release.** Node outputs used to *also* be reachable as
> `${veld.KEY}` inside `on_stop` — and only there. An output named like a builtin
> (`run`, `branch`) therefore shadowed it during teardown but nowhere else, so the
> same string resolved to two different values. See
> [docs/migrating-to-v3.md](migrating-to-v3.md#the-one-breaking-change-that-affects-v1-and-v2-too).

<a id="availability"></a>

#### Availability

The set is closed but **not uniform** — each context populates what it can know.
`veld lint` and `veld start` check a reference against the context it sits in
(`builtin-not-in-scope`), so `${veld.url}` on a `command` node is refused before
the run rather than failing mid-start with `unknown built-in variable`.

| | run, name, project, root, worktree, branch, username | run_id | node, variant | port, url, url.\*, ports.\* |
|---|:-:|:-:|:-:|:-:|
| `vars` value | ✅ | ✅ | — | — |
| project `setup` / `teardown` | ✅ | — | — | — |
| project / node / variant `env` | ✅ | ✅ | ✅ | `start_server` only |
| `command` node: `argv`, `shell`, `skip_if`, `on_stop` | ✅ | ✅ | ✅ | — |
| `start_server` node: `argv`, `shell`, `on_stop` | ✅ | ✅ | ✅ | ✅ |
| `actions[].cmd` | run, name, project, root only | — | ✅ | ✅ (plus `${param.*}`, `${output.*}`) |

Notes:

- **`run_id`** is absent in a project step because a `teardown` also runs from
  `veld stop` after the run row is gone. Use `${veld.run}`, the name you started
  with.
- **`port` / `url` / `ports.*`** exist only where a port is allocated and a route
  registered, which is a `start_server` step. From anywhere else, reach a server's
  address as [`${nodes.<node>.url}`](#node-output-references-nodes) — including
  from the node's own `env`, which is how a server tells itself its public URL
  (`NEXTAUTH_URL`, `BASE_URL`).
- **`on_stop` has the same set as the node it belongs to**, so a container named
  `${veld.project}-${veld.node}-${veld.url.hostname}` in `argv` can be removed by
  the identical string in `on_stop` — which is the point, since a name copied by
  hand into two places is how containers leak.
- A value inherited by several variants (a project- or node-level `env`) is judged
  against every variant it reaches. Wrong for all of them is an error; wrong for
  only some is a warning naming them.

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
  "argv": ["pnpm", "--filter", "frontend", "dev"],
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
  "$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json",
  "schemaVersion": "3",
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
          "skip_if": { "argv": ["./scripts/verify-db.sh"] },
          "on_stop": { "argv": ["./scripts/drop-db.sh"] },
          "outputs": ["DATABASE_URL"],
          "sensitive_outputs": ["DATABASE_URL"]
        },
        "docker": {
          "type": "start_server",
          "argv": ["docker", "run", "-d", "--name", "veld-db-${veld.run}", "-e", "POSTGRES_PASSWORD=veld", "-p", "${veld.port}:5432", "postgres:16"],
          "on_stop": { "argv": ["docker", "rm", "-f", "veld-db-${veld.run}"] },
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
          "argv": ["./scripts/generate-dev-certs.sh"],
          "skip_if": { "argv": ["test", "-f", "./certs/dev.pem"] }
        }
      }
    },

    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "argv": ["pnpm", "--filter", "backend", "dev", "--port", "${veld.port}"],
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
          "shell": "echo 'BACKEND_URL=https://api.staging.my-project.com' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["BACKEND_URL"]
        }
      }
    },

    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "argv": ["pnpm", "--filter", "frontend", "dev"],
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
          "argv": ["pnpm", "--filter", "frontend", "dev"],
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
          "argv": ["pnpm", "--filter", "admin", "dev"],
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
          "argv": ["pnpm", "--filter", "admin", "dev"],
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

### One-off runs (`--oneshot`)

`veld start <node> --oneshot` turns a `command` node into the run's **terminal
node**: Veld starts the node's dependencies, waits for them to become healthy,
then runs the node to completion — streaming its output live — and, the moment
it exits, tears the whole environment down in reverse dependency order and exits
with the node's exit code. It's the local-dev and CI analog of
`docker compose run --rm` (with `--abort-on-container-exit --exit-code-from`).

This makes end-to-end test setups trivial: one command brings up every backend
and web app the tests need, runs the test runner, prints its results, and cleans
everything up afterwards — with the test runner's pass/fail becoming the process
exit code.

```sh
# Start db + api + web (e2e's dependencies), run the e2e suite, tear down,
# and exit with the suite's exit code.
veld start e2e --oneshot
```

```json
{
  "nodes": {
    "e2e": {
      "variants": {
        "local": {
          "type": "command",
          "argv": ["playwright", "test", "--reporter=line"],
          "env": { "BASE_URL": "${nodes.web.url}" },
          "depends_on": { "web": "local", "api": "local" }
        }
      }
    }
  }
}
```

Ports are allocated dynamically, so the test runner can't hardcode a URL — hand
it the started services' URLs by interpolating `${nodes.<node>.url}` (or
`.url.host`, `.port`, …) into the command or its `env`, as the `BASE_URL` above
does. See [Variables](#variables) for the full set of `${nodes.*}` references.

Details:

- **stdout carries only the terminal node's stdout.** Veld's own chrome (the
  startup summary, "Running…", teardown lines, and the non-TTY NDJSON progress
  stream) all go to **stderr**, so a CI job or coding agent can capture stdout
  and get just the program's output. Dependency logs are recorded (read them
  with `veld logs --node <dep>`), not streamed; pass `--all-logs` to interleave
  them (on stderr) during the run.
- **The terminal node must be a `command` type** (a `start_server` never exits,
  so it can't be the thing whose exit ends the run). Veld errors otherwise —
  and the command itself **must terminate**: a node mistyped as `command` that
  actually runs a long-lived server will hang the run until you Ctrl+C.
- **Exactly one selection** is required — the terminal node (a `--preset` that
  expands to several nodes is rejected). Its dependencies are resolved through
  the graph and started automatically.
- **The exit code is the node's own.** A non-zero exit (failing tests) is the
  node's *result*, not a startup error — Veld propagates it as its own exit
  code so `veld start e2e --oneshot && deploy` works.
- **Teardown always runs** — on normal completion and on Ctrl+C (which aborts
  the run and exits `130`) — and runs to completion once started; a further
  Ctrl+C during teardown is ignored. Per-node `on_stop` hooks and project
  `teardown` steps run in reverse order, exactly as `veld stop` does.
- **Dependencies are not health-monitored while the terminal node runs.** If a
  dependency crashes mid-run it surfaces only as the node's own failure (e.g. a
  connection error), not a distinct diagnostic.
- `--oneshot` cannot be combined with `--attach`.

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
  "$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json",
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
