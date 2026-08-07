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
**rename** the root file to `veld.jsonc` — editors pick JSONC mode from the
extension, so nothing else is needed — or map the `.json` names to the `jsonc`
language:

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

Rename — do not copy. Keeping the old `veld.json` around "just in case" leaves the
directory with two different root configs, which is an `ambiguous-root-config`
error and refuses `veld start` until one of them goes.

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
          "type": "long_running",
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
| `presets`           | object | No       | Named shortcuts for node:variant selections (see [Presets](#presets)) |
| `default_preset`    | string | No       | Preset used when `veld start` is given nothing (see [`default_preset`](#default_preset)) |
| `client_log_levels` | array  | No       | Browser log levels to capture (see [Client-Side Log Levels]) |
| `features`          | object | No       | Feature toggles (see [Features](#features))       |
| `proxy`             | object | No       | Reverse-proxy header rules (see [Proxy](#proxy))   |
| `env`               | object | No       | Global environment variables inherited by all nodes |
| `vars`              | object | No       | One definition point per value, referenced as `${vars.<name>}` (see [`vars`](#vars)). A var may declare itself [machine-overridable](#machine-overridable-vars) |
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

Controls which browser console levels Veld captures from `long_running` nodes. Veld injects a small script into proxied HTML responses that hooks `console.log`, `console.warn`, `console.error`, `console.info`, and `console.debug`, plus `window.onerror` and `onunhandledrejection`. Captured logs are sent to the Veld daemon and appear in `veld logs` and the management UI alongside server logs.

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

Controls which Veld capabilities are injected into `long_running` nodes' HTML responses. Each feature defaults to `true` (enabled). Set a feature to `false` to disable it. The same override hierarchy applies: variant > node > project.

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

#### Step output

Everything a step prints — stdout and stderr — is streamed into `veld start`'s
progress output as it arrives and recorded under the run's `setup` stream, so
`veld logs --source setup` (and the management UI's **Setup** filter) shows it
after the fact. Lines are labelled `setup:<step name>` / `teardown:<step name>`,
which is what tells two steps apart in one stream.

Because it is recorded, **a value a step prints is in the log**: veld stores
step output verbatim and does not redact it, the same as it has always done for
a `long_running`'s output. A secret reaches a step through its environment or a
`files:` entry (see [`secret`](#secret)) — don't `echo` it, and remember that
`set -x` echoes the command line for you.

A step's stdin is `/dev/null`, so a command that prompts fails on EOF rather
than blocking a startup nobody can answer. Steps that need a credential should
read it from the environment.

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
| `type`              | string           | Yes      | All            | `"command"` or `"long_running"` (`"start_server"` is a permanent alias) |
| `argv` / `shell`    | array / string   | Yes (exactly one) | All   | What to run — `argv` is spawned directly, `shell` runs via `sh -c` |
| `script`            | string           | Varies   | `command` only    | Path to script file, relative to `veld.json`          |
| `health_check`      | object           | No       | `long_running` | Legacy readiness probe. Deprecated: use `probes.readiness` |
| `probes`            | object           | No       | All            | Readiness and liveness probe configuration            |
| `depends_on`        | object           | No       | All            | Dependencies on other nodes                           |
| `env`               | object           | No       | All            | Extra environment variables                           |
| `ports`             | object or `null` | No       | `long_running` | Named ports (see [`ports`](#ports)); `null` means this node serves nothing |
| `outputs`           | array or object  | No       | All            | Output declarations (format varies by type)           |
| `sensitive_outputs`  | array of strings | No       | All            | Output keys to mask and encrypt                       |
| `url_template`      | string           | No       | `long_running` | URL template override for this variant                |
| `on_stop`           | object           | No       | All            | Teardown command (`{ "argv": … }` / `{ "shell": … }`) run when the environment is stopped |
| `skip_if`           | object           | No       | `command` only    | Idempotency check — skip if exits 0 (alias: `verify`)|
| `client_log_levels` | array of strings | No       | `long_running` | Browser log levels override for this variant          |
| `features`          | object           | No       | `long_running` | Feature toggles override for this variant             |
| `proxy`             | object           | No       | `long_running` | Reverse-proxy header rules override for this variant (see [Proxy](#proxy)) |
| `share`             | object           | No       | any (inert on `command`) | Sharing opt-in for this variant (see [Sharing](#sharing)) |

### `type`

There are exactly two primitives, and they describe **lifecycle only**: a node
either runs to completion or stays running. Whether a node *serves* anything is a
property of its [`ports`](#ports), not of its type.

#### `command`

Runs a shell command or script to completion. Used for setup tasks such as database cloning, seeding, data migration, or exporting remote service URLs.

- The working directory defaults to `${veld.root}` (the directory containing `veld.json`)
- Must specify exactly one of `argv`, `shell`, or `script`
- Can declare outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE` (preferred) or via `VELD_OUTPUT key=value` on stdout (legacy, discouraged — exposes values in terminal/logs)
- Built-in output: `exit_code`
- Supports the `skip_if` field for idempotency
- Everything it prints (stdout and stderr, minus the `VELD_OUTPUT` control
  lines) goes to that node's log stream, exactly like a `long_running`'s: read
  it live in `veld start`'s progress output, afterwards with
  `veld logs --node <name>`, or in the management UI. A `skip_if` probe's output
  is a predicate, not the node's, and is not logged

```json
{
  "type": "command",
  "shell": "echo 'DATABASE_URL=postgresql://localhost:5432/mydb' >> \"$VELD_OUTPUT_FILE\"",
  "outputs": ["DATABASE_URL"]
}
```

#### `long_running`

Starts and supervises a process for the life of the run. By default Veld allocates one port, injects it as `${veld.port}`, configures DNS and Caddy routing, and monitors health.

- The working directory defaults to `${veld.root}`
- Must specify exactly one of `argv` or `shell` (required)
- With a port declared, the process **must** bind to `${veld.port}` -- if it does not, the readiness probe fails with a clear error
- Built-in outputs: `url` (the full HTTPS URL) and `port` (the allocated port number); one `urls.<name>` per additional `http` port
- Built-in variables: `${veld.port}` and `${veld.url}` are available in this node's `argv`/`shell`, `env`, and `outputs` templates
- Ports and URLs are **pre-computed** before any node executes, so `${nodes.X.url}` and `${nodes.X.port}` for any `long_running` node are available everywhere -- no dependency edge required
- **Requires a readiness probe** (`long-running-needs-readiness`): use `probes.readiness` (preferred) or the legacy `health_check` field
- Users never see or deal with port numbers -- only clean HTTPS URLs

```json
{
  "type": "long_running",
  "argv": ["pnpm", "--filter", "backend", "dev", "--port", "${veld.port}"],
  "probes": { "readiness": { "type": "http", "path": "/health" } }
}
```

##### A long-running node that serves nothing

`"ports": null` declares a supervised process with no ports at all: no
allocation, no `${veld.port}`, no URL, no DNS host, no Caddy route. This is how an
Electron shell, a file watcher, or a background compiler is written — a process
veld starts, keeps, reports on, and stops with the run, and nothing more.

```jsonc
{
  "type": "long_running",
  "shell": "electron .",
  "ports": null,
  "env": { "APP_URL": "${nodes.web.url}" },
  "probes": { "readiness": { "type": "settle", "seconds": 5 } }
}
```

Readiness stays **mandatory** here. A portless node cannot use a `port` or `http`
probe — there is nothing to connect to, and answering "healthy" because there is
nothing to check is exactly the failure `probe-needs-port` exists to stop. Use
[`command`](#readiness-probe) when the process publishes something observable, or
[`settle`](#strategy-settle) when it publishes nothing.

##### `start_server` is a permanent alias

`start_server` was the historical spelling and it still loads, forever, exactly as
`bash` still loads as an alias for `command`. It was renamed because it named the
common case rather than the contract: the type has only ever decided *lifecycle*,
and once a portless long-running node became legal, "server" described the
minority of them.

Nothing rewrites your file, and `veld lint` does not flag the old spelling.
`long_running` is the **canonical** one — it is the name Veld's own messages use
and what gets persisted into run history and graph snapshots — but a config that
says `start_server` is not wrong and is not deprecated.

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

Defines how Veld verifies that a `long_running` process is healthy. Veld runs a two-phase health check:

1. **Phase 1 -- the process got off the ground.** With a port, that means the TCP listener opened. Without one (`"ports": null`), it means the process was still alive after the `settle` window.
2. **Phase 2 -- the strategy's own check:** an HTTP request, a command, or nothing at all for `port` and `settle`, which Phase 1 already covered.

Both branches of Phase 1 are **raced against the process's own exit**, and that
race is the only crash-fast in the start path: a command that dies on startup
fails the run there, instead of letting its dependents start behind a corpse. It
is why readiness is mandatory on a `long_running` node.

If Phase 1 fails, the error is a process issue. If Phase 1 passes but Phase 2 fails, the process is up but not answering. This distinction produces precise error messages.

#### Health Check Fields

| Field              | Type    | Required | Description                                          |
|--------------------|---------|----------|------------------------------------------------------|
| `type`             | string  | Yes      | Strategy: `"http"`, `"port"`, `"command"`, or `"settle"` |
| `path`             | string  | No       | HTTP path to poll (`http` type only)                 |
| `expect_status`    | integer | No       | Expected HTTP status code (`http` type only, default: 200) |
| `command`          | string  | No       | Shell command to run (`command` type only)              |
| `port`             | string  | No       | Which [named port](#ports) to probe (`http` / `port` types); default: the primary |
| `seconds`          | integer | No       | How long the process must stay alive (`settle` type only, default: 3) |
| `timeout_seconds`  | integer | No       | Max seconds to wait (default: 60)                    |
| `interval_ms`      | integer | No       | Milliseconds between checks (default: 1000, min: 100)|

A `type` Veld does not implement is an **error** (`unknown-probe-type`), not a
probe that quietly passes. `{"type": "htpp"}` used to mean "always healthy" on
both the readiness and the liveness path, so a typo was the silent way to turn a
check off.

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

`"port": "<name>"` probes one of the node's [named ports](#ports) instead of the
primary. On a node that has no such port — including any node declaring
`"ports": null` — a `port` or `http` probe is a `probe-needs-port` lint error and
fails the run. It used to report healthy, which is the failure the rule exists to
stop.

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

<a id="strategy-settle"></a>

#### Strategy: `settle`

Waits `seconds` (default 3) and passes if the process is still running. The
readiness probe for a `long_running` node that binds no port.

```json
"probes": { "readiness": { "type": "settle", "seconds": 5 } }
```

Its claim is deliberately weak, and it says so: *the process was still running N
seconds after it was spawned*. Nothing more. That is still worth having, because
the wait is raced against process exit exactly as the port check is — an Electron
shell that dies on a missing binary, a watcher that exits on a bad flag, a
compiler that cannot find its config all fail the run here rather than reporting
healthy and releasing their dependents.

**Prefer `command` whenever the process publishes something observable** — a
socket, a built file, a pid file, a line in a log:

```jsonc
// better than a timer: this waits for the thing you actually depend on
"probes": { "readiness": { "type": "command", "shell": "test -f ./generated/index.ts" } }
```

`settle` is the honest fallback, not the recommendation, and it is **readiness
only**. As a liveness probe it would report healthy forever — it describes
startup, and the monitor has no notion of it — so `veld lint` rejects it there
(`unknown-probe-type`).

### `probes`

Configures readiness and liveness probes for a variant. Available for both `command` and `long_running` types. `probes.readiness` supersedes the legacy `health_check` field.

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

Gates the dependency graph during startup. Same fields as `health_check`. For `long_running` nodes, runs after the process starts. For `command` nodes, runs after the command exits 0.

**A `long_running` node must have one** (`long-running-needs-readiness`). Without
it the node is reported healthy the moment its port opens — or, for a portless
one, the moment it is spawned — so the graph proceeds before the process can
serve, and nothing catches it exiting on startup.

Four types: `http`, `port`, `command`, and [`settle`](#strategy-settle).

#### Liveness Probe

Runs continuously after the node becomes healthy. Detects failures like dropped SSH tunnels, crashed background processes, or unreachable databases. Supports three check types:

- **`http`**: Polls an HTTP endpoint. Passes when the expected status code is returned.
- **`port`**: Checks if a TCP port is accepting connections.
- **`command`**: Runs an arbitrary shell command (via `sh -c`). Exit code `0` means healthy, non-zero means unhealthy. Pipes, redirects, and `&&` chains all work. The node's outputs are injected as environment variables, so you can reference them directly (e.g., `pg_isready -h $DB_HOST -p $DB_PORT`).

`settle` is **not** one of them: it describes startup, so a liveness `settle`
would pass forever. A node with no port has exactly one usable liveness type,
`command` — `http` and `port` now report *unhealthy* there rather than shrugging
and returning healthy. Absent is never zero.

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

**Which shell environment gets inherited depends on who started the run.** `veld start` in a terminal passes your terminal's environment straight through, including anything exported from `.zprofile`/`.profile` and `.zshrc`. A run started from the management UI or Veld Desktop is spawned by the Veld daemon, which inherits only `PATH` — resolved from your login shell so `npx`, `pg_isready`, `op` and version-manager shims are found — and not the rest of your shell environment. So a node that depends on a variable you export from a shell rc file works from the terminal and not from the UI. Declare it in `env` and it works from both; that is the only form that doesn't depend on how the run was launched.

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

#### For `long_running` variants: Object (key-value map)

Defines synthetic outputs whose values are string templates interpolated after the port and URL are resolved. Templates support all `${veld.*}` and `${nodes.*}` variables. This is especially useful for Docker infrastructure nodes where the process cannot write to `$VELD_OUTPUT_FILE`.

```json
{
  "type": "long_running",
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

A `long_running` variant with a routed port also automatically provides the built-in outputs `url` (the full HTTPS URL of the primary port) and `port` (its allocated number), plus `ports.<name>` per declared port and `urls.<name>` per `http` one. A variant declaring `"ports": null` provides none of them — it has nothing to serve.

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

A teardown command that runs when `veld stop` is called. Executed in reverse dependency order, after the process is killed (for `long_running` nodes) but before state is cleaned up.

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
- Every `${veld.*}` built-in the node itself had — for a `long_running` node that includes `${veld.port}`, `${veld.url}`, `${veld.url.hostname}` and the rest of the URL family, and `${veld.ports.<name>}`. See [Availability](#availability).
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

`on_stop` works with both `command` and `long_running` variants:

```json
{
  "type": "long_running",
  "argv": ["docker", "run", "--rm", "--name", "veld-redis-${veld.run}", "-p", "${veld.port}:6379", "redis:7"],
  "on_stop": { "argv": ["docker", "stop", "veld-redis-${veld.run}"] },
  "health_check": { "type": "port" }
}
```

---

## Sharing

Sharing has two config surfaces: an environment-wide `sharing` block (relays and the public gateway) and a `share` opt-in on each **port** you want exposed. A port is shareable **only** if it declares `share` (directly, or via the node-level shorthand below) — `veld share` refuses everything else. This makes what leaves your machine explicit and auditable.

> **Behavior change:** earlier versions shared every URL-bearing service in a run. Now nothing is shared until a port declares `share.expose`; `veld share` errors (listing the candidate `node:variant#port`s) until you opt one in.

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
          "type": "long_running",
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

All forms trim trailing whitespace (secret stores commonly append a newline). Prefer the `env` / `file` / `command` forms over a literal so the secret stays out of the config file. The token is resolved on the daemon at share time; a token that fails to resolve (missing env var, unreadable file, command exits non-zero or times out, or an empty result) is a hard error — Veld never binds a relay unauthenticated when a token was declared. `command` runs an arbitrary shell command from your config, exactly like `long_running`/`command` steps already do, so the same trust applies: only run configs you trust.

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

The audiences a **port** may be shared to. A list; an empty list (or an absent `share` block) means the port is never shareable.

| Value  | Audience          | Requires | URL fidelity                       | Command |
|--------|-------------------|----------|------------------------------------|---------|
| `peer` | Other Veld users  | `http` or `tcp` | Verbatim — exact origin URL reproduced (`http`); a bare local TCP port (`tcp`, see below) | `veld share` |
| `web`  | Any browser (no Veld needed) | **`http` only** | Best-effort — real public URL minted by the gateway, origin `Host` preserved toward the app, redirects/cookies adapted (see the [operator guide](gateway.md)) | `veld share --web` |

```json
"share": { "expose": ["peer", "web"] }
```

The two audiences are independent shares with independent capabilities: `veld share` serves the `peer`-opted ports, `veld share --web` mints a separate share of the `web`-opted ones and registers it with the gateway — revoking one never touches the other. Sharing only ever exposes something a port listens on, so `share` is meaningful on `long_running` variants; on a `command` variant it is accepted but inert (nothing to share).

#### Where `share` is written

On the port entry, which is where exposure happens. A node-level (or variant-level) `share` is **shorthand for the primary port's policy** and never spreads to any other port — see [Consent is per port](#consent-is-per-port) for the full resolution rules:

```jsonc
"api": { "variants": { "dev": {
  "type": "long_running",
  "shell": "api --port ${veld.port} --admin ${veld.ports.admin}",
  "ports": {
    "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer", "web"] } },
    "admin":    { "port": "auto", "protocol": "http" },                     // ops console: never shared
    "postgres": { "port": 5432,   "protocol": "tcp",  "share": { "expose": ["peer"] } }
  },
  "probes": { "readiness": { "type": "port" } }
}}}
```

`veld share` names every port it excluded and why, so a partial share is never silent: one line for the ports with no opt-in for this audience (`api:dev#admin`), another for the ports that opted into the *other* audience only, and a third for `tcp` ports dropped from a `--web` share. The `--node` filter narrows *within* the opted-in set and can never widen it.

#### `web` requires `protocol: "http"`

The gateway speaks HTTP/1.1 over the tunnel, and a browser cannot speak a raw protocol through it whatever the gateway does. `"expose": ["web"]` on a `tcp` port is therefore **`web-share-needs-http`, a lint error** — not a limitation waiting to be lifted, but a statement of what "web" means. Three gates enforce it: `veld lint` (and `veld start`) rejects the config, the daemon refuses the port at share time and says why, and the gateway drops any non-routed entry it is somehow handed.

A database you want a colleague to reach goes to the `peer` audience instead.

#### Raw `tcp` sharing (peer only)

A `tcp` port opted into `peer` is carried over the same encrypted iroh tunnel as an HTTP one and reproduced on the joining machine as a **bare local TCP port** — a listener spliced to the origin, with no Caddy route in front of it (a raw connection carries no hostname to match a route on).

**The port number on the consumer is their local listener's, not yours.** Nothing is in front of the socket to preserve the original number, so the joiner must use the address `veld join` prints and not the one from your `veld.json`:

```
✓ Joined — 3 endpoint(s) now reachable on this machine:

    https://web.demo.acme.localhost
    https://api-admin.demo.acme.localhost
    api-postgres.demo.acme.localhost:49317  (tcp)
```

Raw endpoints print as `host:port  (tcp)`, listed apart from URLs for exactly that reason — shown as a URL, `5432` would send someone to a port nothing is listening on. In `--json` they arrive in `addresses`, a separate field from `urls`.

Two consequences of "no route" worth knowing:

- **No TLS, no header rules.** `proxy` rules are an HTTP concept and are not applied (they already were not on the peer path). The tunnel itself is end-to-end encrypted; what the local listener hands your client is whatever the origin service speaks.
- **A joiner running an older Veld refuses the whole join.** A manifest carrying a `tcp` endpoint has no `url` for it, and an older consumer parses `url` as required — so it fails to deserialize the manifest and joins nothing. That is deliberate and fail-closed: a peer that cannot represent an endpoint must not silently reproduce it as an HTTP route. Both sides upgrade, or neither shares.

There is no `udp` audience because there is no `udp` protocol — see [`protocol`](#protocol).

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

`vars`, `presets`, `env`, `setup`, `teardown`, `hooks`, and `ide` may appear in any
file. Other project-level settings (`url_template`, `default_preset`, `features`,
`proxy`, `sharing`, `client_log_levels`) are read from the root file. So a preset
may be declared in any file, but which one is the default is decided in one
place.

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
  "type": "long_running",
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
| `env`, `ports`, `depends_on`, `files` (maps) | **Additive per key**; the variant wins on collision | `"KEY": null`, or `"ports": null` for every port at once |
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
- **`"ports": null` is the one `null` that is a declaration rather than an
  erasure.** Every other `null` here removes an inherited value and lets the
  default back in; this one says "this node has no ports" and suppresses the
  synthesized default too. See [the three authorings](#the-three-authorings).

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

**A var literal is interpolated, so every `${…}` in it is Veld's to resolve.** A
reference in no Veld namespace — `"cache": "${HOME}/.cache"` — is a
`var-unresolvable-reference` error. Write `$HOME` without braces to leave it for
the shell, or make the var a value source: `{ "env": "HOME" }`.

```sh
veld config --why nodes.api.variants.dev.env.DATABASE_URL
```

prints the effective value, where it was defined, and what it overrides. A
`secret` value is described, never printed, and there is no flag that will.

### Machine-overridable vars

Some values in a checked-in config are not facts about the *project* — they are
facts about the **machine**. Which of two locally installed container runtimes
should run the containers. A memory ceiling that a 16 GB laptop and a 64 GB
workstation genuinely disagree about. The path to a tool installed somewhere
unusual. `veld.json` is committed, so every value in it is identical for everyone
who clones the repo, and none of those are.

A var declares itself overridable with a `machine` block. The declaration stays
in the committed file; the *answer* lives in Veld's own database.

```jsonc
"vars": {
  "container_runtime": {
    "machine": {
      "default": "docker",
      "choices": ["docker", "podman"],
      "description": "Which local container runtime runs this project's containers"
    }
  },
  "vendor_token": {
    "machine": { "prompt": "Vendor API token (per developer)" },
    "secret": true
  }
}
```

It is consumed like any other var — `${vars.container_runtime}` in `argv`, `env`,
`cwd` — so nothing downstream changes.

| Key | |
|---|---|
| `default` | The checked-in fallback for a machine with no answer. Omit it to require every machine to answer. May itself be a [value source](#value-sources) (`{ "env": "…" }`), but never another machine var — that is a type error, not a lint |
| `choices` | The legal answers. Enforced when setting **and** when resolving, because the config can change under an answer that was valid when it was stored. `[]` is a lint error: nothing could satisfy it |
| `description` | What the value means. Shown by `veld config vars` |
| `prompt` | The question asked when there is no default and no answer. Falls back to `description` |
| `secret` | Sits beside `machine`, not inside it — it describes the var, not one of its layers |

An unknown key under `machine` is a `machine-var-unknown-key` **error finding**, not a load failure. That split is deliberate: a typo (`defualt`) silently costs the var its default, so it must block `veld start` and `veld lint` — but a config written for a *newer* veld has to stay loadable by an older one, or `veld stop` cannot tear down a run that is already going and its containers leak.

```sh
veld config vars                              # name, effective value, and which scope it came from
veld config set container_runtime podman      # this machine, every worktree of the project
veld config set container_runtime podman --worktree   # this checkout only
veld config unset container_runtime           # back to the next scope, then the default
```

**Resolution is most-specific-first:** `veld start --var NAME=VALUE` (this run
only, never stored) → the worktree-scoped answer → the project-scoped answer →
the config's `default` → an error naming the var and printing the command that
sets it.

#### What an override is keyed by

**The project, across every worktree of it.** An override describes the laptop,
not the checkout, so answering it once per worktree would be the bug rather than
the feature. The key is where this config lives in the repo's *main* checkout, so
six worktrees of one repo share one answer, and two configs in one monorepo stay
separate. Outside a git repo it falls back to the config's own directory.

`veld config vars` always prints which scope a value came from. That is not
decoration: a value arriving silently from a scope you had forgotten is worse
than no feature at all.

Moving or renaming a checkout orphans its answers — the key is a path, like every
other project-scoped thing Veld stores. Re-set it, or `veld config vars` will
show the var back on its default.

#### Secrets

A `secret` machine var **is** overridable, because an override is a
[value source](#value-sources) and not necessarily a literal:

```sh
veld config set vendor_token --env VENDOR_TOKEN     # a pointer
veld config set vendor_token --shell 'op read op://dev/vendor/token'
veld config set vendor_token sk_live_…              # the value itself
```

The first two keep Veld carrying *a pointer plus a sensitivity flag*, which is
the rule everywhere else in this file. The third stores the value in Veld's
database (owner-readable, not encrypted) and says so when you run it. Either way
the value is redacted in `veld config vars`, in `--json`, and in the management
UI — there is no flag or endpoint that prints it.

#### Asking, and refusing to guess

A machine var with no `default` and no answer stops the run **before anything
spawns** — presence is checked across the whole execution plan, including
transitive dependencies, so it can never surface after half the graph is up.

At a terminal, Veld asks, and saves the answer only if you say yes. With no
terminal — CI, a scripted start, a run launched by the daemon for the management
UI — it refuses with the exact `veld config set` command instead. It never
resolves a default nobody chose, and it never persists a guess: a background
process quietly taking a default is indistinguishable from a human choosing it,
and the wrong value then sticks around.

From the management UI and Veld Desktop there *is* a reachable human, so a start
that would need a value opens a form instead of failing.

#### Run history

A run's snapshot records which machine vars were in play and which scope each
answer came from — **names and provenance only, never values**. `config_hash`
hashes the `veld.json` bytes, so without this two runs of the same commit that
behaved differently would be reported as identical. A hash of the value was
considered and rejected: the values people override are low-entropy (`true`,
`5432`, a handful of hostnames), so a digest over that domain is a
brute-forceable oracle in the most-copied artifact Veld produces.

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

That is a rule about what *Veld* does with the value. It cannot un-print what a
process does with it: a node or step that echoes its own environment puts the
value in that run's log, which `veld logs` and the management UI will show. Veld
records process output verbatim and never redacts it.

**Two different rules, because there are two different risks.**

`${vars.db_pass}`, `${output.DB_PASS}`, `${nodes.db.PASSWORD}` — Veld substitutes
these into the command string itself, so the value is in the process table for
certain. That is `secret-in-command`, an **error**.

`$DB_PASS` is expanded by the *shell*, not by Veld, and whether it leaks depends
entirely on what the shell does with the expansion:

```jsonc
"env": { "DB_PASS": { "shell": "pass show db", "secret": true } },

// safe — a builtin, no exec, the value stays inside the shell
"shell": "echo $DB_PASS | some-consumer",
// safe — an environment assignment; psql reads it from its environment
"shell": "PGPASSWORD=$DB_PASS psql -h localhost -U u db",
// LEAKS — the shell execs psql with the expanded value as an argument,
// so `ps` shows the password on the psql process, not on the shell
"shell": "psql \"postgres://u:$DB_PASS@localhost/db\"",
// safe — the container is handed the name, not the value
"argv": ["docker", "run", "-e", "DB_PASS", "img"]
```

The shell's own `ps` entry shows the literal `$DB_PASS` in every case; the program
it *runs* is where the value shows up. Veld cannot tell which case it is looking
at, so a `$NAME` matching a secret is `secret-shell-expansion`, a **warning** —
loud enough to check, not so loud it blocks a run that is fine. Prefer handing the
program the variable *name*: `PGPASSWORD=`, `--password-file`, `-e NAME`.

Not flagged, and not a leak: `${DB_PASS}` (no such Veld namespace, so
interpolation fails rather than substituting) and a bare `$DB_PASS` in an `argv`
element no shell ever sees (inert text — a mistake, but not an exposure).

### Lint rules

| Rule id | What it catches | Severity |
|---|---|---|
| `secret-in-command` | A value marked `secret` that Veld would *substitute* into `argv` or `shell` — `${vars.x}`, `${output.x}`, `${nodes.a.x}` | **error** |
| `secret-shell-expansion` | A bare `$NAME` matching a secret. The shell expands it, so it leaks only if the expansion becomes another program's argument — Veld cannot tell, so it says so instead of guessing | warn |
| `var-unresolvable-reference` | A `${…}` in a `vars` literal that names no Veld namespace (`${HOME}`). Var literals are interpolated, so it fails the run | **error** |
| `ambiguous-root-config` | A directory holding `veld.json` and `veld.jsonc` as two *different* files (a symlink between them is fine). Veld reads `veld.json`, so the file you edit may not be the one it runs | **error** (a finding, not a load failure — `veld stop` still works) |
| `credential-shaped-literal` | A credential-shaped literal (`sk-`, `ghp_`, a JWT, `scheme://user:pass@host`), marked or not | warn |
| `credential-shaped-proxy-header` | The same shape in a `proxy` header value, which travels to Caddy and to every joiner verbatim | warn |
| `long-running-needs-readiness` | A `long_running` node with no readiness probe. Reached by either spelling of the type; the remedy it prints differs for a portless node, which cannot use a `port` probe | **error** |
| `unknown-probe-type` | A probe `type` Veld does not implement — including `settle` written as a *liveness* probe. A typo used to mean "always healthy" on both paths | **error** |
| `probe-needs-port` | A `port` or `http` probe on a node with no such port: it names a port that is not declared, or needs the primary on a node that has none | **error** |
| `ambiguous-primary-port` | Two or more ports, none named `http` and none marked `"protocol": "http"`, so `${veld.port}` has no unambiguous meaning | **error** |
| `web-share-needs-http` | A `"protocol": "tcp"` port opting into the `web` audience. The gateway serves HTTP and a browser cannot speak a raw protocol through it, so the share would silently drop the port the author asked to publish | **error** |
| `unknown-var` | `${vars.x}` naming a var that is not declared, listing the declared names | **error** |
| `vars-cannot-nest` | A var referencing another var | **error** |
| `machine-var-empty-choices` | `"choices": []` on a machine var — no value could satisfy it, including the declared default | **error** |
| `machine-var-default-not-a-choice` | A machine var whose `default` is not one of its own `choices`, so it fails on every machine that has not overridden it | **error** |
| `machine-var-duplicate-choice` | The same string listed twice in `choices` | warn |
| `machine-var-unexplained` | A machine var with no `default`, so every machine must answer it, and no `prompt` or `description` saying what to answer | warn |
| `machine-var-unknown-key` | A key under `machine` (or beside it) that this veld does not know. Reported rather than refused, so a config written for a newer veld can still be torn down by an older one | **error** (a finding, not a load failure) |
| `machine-var-secret-placement` | `secret: true` written inside `machine.default` rather than beside `machine`. The var is treated as secret either way; the placement misleads a reader | notice |
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
- A probe may name one: `"probes": { "readiness": { "type": "port", "port": "http" } }`.
  A multi-port node's readiness is rarely "any port is open" — a debugger port
  opens long before the app is listening.

A fixed number (`"debug": 9229`) is accepted but discouraged: a literal port
silently breaks parallel worktrees, which is the reason named auto-ports exist. If
a fixed port is taken, Veld errors rather than substituting another — a debugger
pointed at 9229 must reach the process that asked for it.

### The three authorings

`ports` is one key with three meanings, and the middle one is the whole reason
the type is called `long_running` rather than `start_server`:

| `ports` | Meaning |
|---|---|
| **absent** | One auto-allocated `http` port. The historical default, unchanged. |
| **`null`** | No ports at all: no allocation, no `${veld.port}`, no URL, no DNS host, no Caddy route. |
| **`{ … }`** | That map. Merged node → variant **per key**; `"name": null` erases one entry. |

```jsonc
"electron": {
  "variants": { "dev": {
    "type": "long_running",
    "shell": "electron .",
    "ports": null,                     // supervised, never served
    "probes": { "readiness": { "type": "settle", "seconds": 5 } }
  }}
}
```

Erasing every entry by name lands on "no ports" too. A variant writing
`"ports": { "http": null }` over a node that declared only `http` gets zero ports
— it used to collapse back to "nothing declared" and silently allocate a **fresh**
one, which is the opposite of what the author wrote.

### `protocol`

An entry is either the scalar shorthand — unchanged — or the long form:

```jsonc
"ports": {
  "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer"] } },
  "admin":    { "port": "auto", "protocol": "http" },   // no `share` → never shared
  "postgres": { "port": 5432,   "protocol": "tcp" },
  "debug":    "auto"                                    // shorthand: the port, nothing else
}
```

| Field | Meaning |
|---|---|
| `port` | `"auto"` or a fixed number. Exactly what the shorthand says. |
| `protocol` | `"http"` or `"tcp"`. Decides whether Veld *routes* the port. Both get a hostname. |
| `host` | A `url_template` for **this port only**, replacing the effective one wholesale. |
| `share` | Who this **port** may be exposed to — see [Consent is per port](#consent-is-per-port). Absent means never shared. |

**Every port gets a hostname and a DNS entry. Only `http` gets a Caddy route.**
Naming and routing are separate concerns, and Veld's helper has always kept
`add_host` and `add_route` apart.

An **`http`** port gets the hostname, a Caddy route in front of it, and a
`${veld.urls.<name>}` family — so it is reachable as a URL.

A **`tcp`** port gets the hostname and nothing else. It is allocated, reserved
and exported — `${veld.hosts.<name>}`, `${veld.ports.<name>}`,
`VELD_HOST_<NAME>`, `VELD_PORT_<NAME>`, `${nodes.<node>.ports.<name>}` — and
deliberately **never routed**. Not an omission: a raw TCP connection carries no
hostname for a proxy to demultiplex on, so there is nothing for Caddy to match a
route against, and a route would add nothing but a certificate no `psql` will
ever present a hostname to. The address is the name plus the port:

```jsonc
"env": { "DATABASE_URL": "postgres://app@${veld.hosts.pg}:${veld.ports.pg}/app" }
```

The DNS half is what makes that name resolve, and whether it does anything
depends on your domain:

- On **`.localhost`** it is a no-op Veld skips outright — the OS wildcards every
  `*.localhost` name (RFC 6761), so `pg.anything.veld.localhost:5432` already
  worked and always did.
- On a **custom apex domain** (`{service}.myapp.test`, privileged mode) hostnames
  exist *only* because Veld writes an `/etc/hosts` or dnsmasq entry. Without one,
  a `tcp` port has no name at all and is reachable only as `127.0.0.1:<port>`.
  This is the case the DNS entry exists for.

`udp` is not a value, on purpose. It would change no behaviour anywhere in Veld,
and a schema field that does nothing is worse than no field.

#### The default is asymmetric

**`http` for the primary port, `tcp` for every other one.**

That asymmetry is deliberate back-compat, not an aesthetic choice. It is what
stops an existing `{"http": "auto", "debug": "auto"}` node from suddenly minting
an HTTPS route in front of its Node inspector port the first time it runs on a
newer Veld. A secondary port that *should* be reachable as a URL says so:

```jsonc
"admin": { "port": "auto", "protocol": "http" }
```

### Which port is primary

`${veld.port}`, `VELD_PORT`, `${veld.url}` and the node's `url` state field all
mean the **primary** port. Veld picks it in this order:

1. The port named `http`.
2. Otherwise, the sole port marked `"protocol": "http"`.
3. Otherwise, the sole entry — provided it states no protocol. A lone port
   explicitly marked `tcp` is a tcp-only node, and must not acquire a hostname by
   being alone.

Anything else has no primary, and a node with no primary has no `${veld.port}`,
no `VELD_PORT` and no `${veld.url}` — its ports are reachable only by name.
Several ports with none named `http` and none marked `"protocol": "http"` is
`ambiguous-primary-port`, a lint error rather than a guess about what
`${veld.port}` means; marking exactly one port `"protocol": "http"` answers it.
Marking *two* is a different statement — two front doors, neither of them the
one — so name one of them `http` if the node needs a `${veld.port}` at all.

A node declaring `"ports": null` has no primary either, by construction. That is
not an error; it is the declaration.

### Every `http` port gets its own hostname

Not just the primary. `{service}` in the [URL template](#url-templates) is the
node name for the primary port and `<node>-<port>` for a secondary one, so node
`web`'s `admin` port is served at:

```
web-admin.dev.veld.localhost      ← and not admin.web.dev.veld.localhost
```

The fusion is the point. Every hostname a node owns stays a **sibling at the same
depth**, so a wildcard TLS certificate or a dnsmasq suffix rule that already
covers the node covers its extra ports for free; a deeper label
(`admin.web.…`) falls outside both and would need new infrastructure per port.

The cost is that the port-hostname space and the node-hostname space stop being
provably disjoint: node `web`'s `admin` port and a node actually named
`web-admin` claim the same name. That is caught loudly at `veld start`, before
anything is spawned, naming both owners:

```
web:dev#admin and web-admin:dev#http both resolve to hostname
web-admin.dev.veld.localhost, so one would silently shadow the other.
```

The way out is the per-port `host` override, which **replaces the whole
`url_template`** for that port rather than layering onto it — the collision is
usually with the very suffix the project template supplies:

```jsonc
"docs": { "variants": { "dev": {
  "type": "long_running",
  "shell": "docs-server --port ${veld.port}",
  "ports": {
    "http": { "port": "auto", "protocol": "http", "host": "handbook.demo.localhost" }
  },
  "probes": { "readiness": { "type": "http", "path": "/index.html" } }
}}}
```

### Referring to a secondary URL

`${veld.url}` is permanently the primary's. Every other `http` port has the same
family under its own name — see
[Built-in Variables](#built-in-variables-veld):

```jsonc
"env": {
  "ADMIN_ORIGIN": "${veld.urls.admin.origin}",     // this node
  "API_ADMIN":    "${nodes.api.urls.admin}"         // another node
}
// and in the process's environment: VELD_URL_ADMIN
```

Asking for the URL of a `tcp` port is a lint error that says *why* — that the port
exists but has no hostname because of its protocol — rather than reporting an
unknown built-in.

### Consent is per port

`share` is a field on a **port entry**. That is where exposure happens, and it is
the only place a config can grant it:

```jsonc
"ports": {
  "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer", "web"] } },
  "admin":    { "port": "auto", "protocol": "http" },                      // never shared
  "postgres": { "port": 5432,   "protocol": "tcp",  "share": { "expose": ["peer"] } }
}
```

A node used to have exactly one exposed port, which made "share this node" and
"share this port" the same sentence. It is not the same sentence any more: the
node above declares an app port, an ops console and a database, and a node-wide
grant would hand a colleague all three. **An absent `share` is never shared** —
consent is opt-in, and nothing anywhere widens a port that declared none.

#### The node-level shorthand

`share` on a node or a variant still works, and is **defined as shorthand for the
primary port's policy**:

```jsonc
"web": { "variants": { "dev": {
  "type": "long_running",
  "shell": "vite --port ${veld.port}",
  "share": { "expose": ["peer"] }        // === "ports": { "http": { …, "share": … } }
}}}
```

Every config written before per-port consent therefore means exactly what it
meant — such a node had one exposed port, and that port is the primary. What the
shorthand can never do is spread: it lands on the primary and stops there, so
adding a `postgres` port to the node above shares nothing new.

Two resolution rules, and neither of them can widen a port:

- **A port's own `share` replaces the shorthand for that port.** It does not
  merge — the more specific declaration is the one the author looked at last.
- **The shorthand reaches the primary only, and only if the primary states
  nothing.** A node with no primary (every port `tcp`, or `"ports": null`) has
  nowhere to fold it into, so a node-level `share` there grants nothing.

For everything the two audiences mean, see [Sharing](#sharing).

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

## `ide`: quicklinks, permissions, external origins and panes

`ide` is where a project configures Veld's own IDE surfaces — Veld Desktop and
the `/ide` view in a browser. Three keys under it are interpreted; the rest of
`ide` stays reserved and opaque (see
[below](#reserved-hooks-and-the-rest-of-ide)), so a JSON-defined IDE extension is
free to use whatever shape it likes.

The key was spelled `ui` while it was wholly reserved and was renamed here, in
the release that first gave it a meaning — a top-level key rename is breaking, so
there was no later chance at it. A config still using `ui` gets an
`unknown-top-level-key` error naming the rename; nothing else changes.

Nothing here is required, and nothing here affects a run. A project without an
`ide` section behaves exactly as before.

### `ide.quicklinks`

The links that are *not* veld's. A browser pane with no page yet is the run's
start page and lists the URLs veld made; these are the ones it didn't — staging,
a dashboard, an internal wiki — shown beside them. Shipping a hardcoded set of
those would be an opinion no tool should have, so they come from the project and
are versioned and shared with the repo.

```jsonc
"ide": {
  "quicklinks": [
    { "label": "Staging", "url": "https://staging.example.com" },
    { "label": "Grafana", "url": "https://grafana.internal" }
  ]
}
```

`url` must be `http://` or `https://`. Other schemes are refused: a quicklink is
a repo-controlled string that a click hands to the OS, and `vscode://` or
`file://` would make a config file a launcher for whatever the machine has
registered. Literal only — `${...}` is **not** interpolated here, because the
start page is rendered with no run to resolve against.

### `ide.externalOrigins`

Origins that must open in the user's **system** browser rather than in a Veld
browser pane.

A URL a terminal produces normally becomes a pane beside that terminal — whether
you clicked it in the output or a program running in the shell opened it (Veld
points `$BROWSER` at itself, so `gh`, `git`, Claude Code, vite and next all reach
it). This list is the exception:

```jsonc
"ide": {
  "externalOrigins": [
    "https://accounts.google.com",
    "https://*.okta.com"
  ]
}
```

The reason it exists is cookies. A pane has its own jar, so an SSO or bank flow
started in one begins from scratch — which for a login is not a preview, it is a
dead end. The project is the place that knows which hosts *its app's* sign-in goes
through, so those belong in the repo alongside everything else about running it.

**Unioned with the user's own list, never replacing it.** Each user has a
`browser.externalOrigins` setting (Settings → Links) for the sign-ins *they*
use; this key adds the project's. Neither can remove the other's — an exemption
only ever sends a URL to the real browser, which is where it would have gone
before any of this existed. Turning the whole behaviour off is also the user's
call (`terminal.openUrlsInApp`), not a project's.

The grammar is exactly [`ide.permissions[].origin`](#idepermissions)'s, checked by
the same parser: `scheme://host[:port]`, `http` or `https`, no path, a leading
`*.` for any depth of subdomain (label-wise, so `evilokta.com` does not match
`*.okta.com`), `*` in the port position for any port, and an **omitted port
meaning the scheme's default port exactly**. A malformed entry is dropped and
reported by `veld lint` as a warning, like every other `ide` problem.

Two limits worth knowing, both of which end with the URL in a *pane* rather than
somewhere unexpected: an origin is matched, not a path — you cannot exempt
`https://example.com/login` and keep the rest of that host in panes — and an
internationalised host must be written in its `xn--…` form, because that is what a
browser compares.

### `ide.permissions`

Browser permissions the project pre-answers for web content shown in a Veld
Desktop browser pane. A dev server that needs geolocation, a camera, or clipboard
read then works for everyone who clones the repo instead of prompting each of
them — and, because Electron's permission *check* is synchronous and cannot
prompt, it is also the only way `navigator.permissions.query()` reports anything
but `denied` before a feature has been used once.

```jsonc
"ide": {
  "permissions": [
    // Veld's own URLs — almost certainly the ones you mean. They are
    // {service}.{run}.{project}.localhost, so BOTH wildcards are needed: the run
    // name sits in the hostname, and the port is 18443 on a no-sudo install.
    { "origin": "https://*.myproject.localhost:*", "allow": ["geolocation", "clipboard-read"] },
    // A server veld does not route — its port moves, its host does not.
    { "origin": "http://localhost:*", "allow": ["clipboard-read"] },
    { "origin": "https://staging.example.com", "deny": ["display-capture"] }
  ]
}
```

> **The first rule is the one to copy.** A pane showing one of your run's
> services is at `{service}.{run}.{project}.localhost` — *not* at `localhost`. A
> rule written `http://localhost:*` is perfectly valid, lints clean, and matches
> a veld-served pane never; it is right only for something you started outside
> veld's routing.

**Origins.** A bare `scheme://host[:port]`, http or https, with no path.

A leading `*.` on the host matches **any subdomain, at any depth**. That form is
not a convenience — it is the only way to name veld's own URLs, which are
`{service}.{run}.{project}.localhost` by default and therefore carry the **run
name in the hostname**. Run names come from the worktree folder, the branch, or
`--name`, so a pinned hostname is a rule that works for exactly one run:

```jsonc
{ "origin": "https://*.veld.localhost:*", "allow": ["notifications"] }
// matches website.my-feature.veld.localhost, api.other-run.veld.localhost, …
```

**Write `:*` for veld's own URLs**, as above. An omitted port means the scheme's
*default* port (see below), and only a **privileged** install serves on 443 —
`veld setup unprivileged`, the no-sudo default, uses **18443**. A rule written
`https://*.veld.localhost` therefore matches nothing on that install, and nothing
tells you: the grant is simply never applied.

The rules around it, each of which exists to stop a specific mistake:

- The `*` is legal **only as the leading label**. `web*.example.com`, `a.*.b.com`
  and a wildcard on an IP literal are all refused rather than interpreted.
- Matching is **label-wise**: `*.veld.localhost` does not match
  `evilveld.localhost`. It also does not match `veld.localhost` itself — write the
  apex as its own rule if you want it.
- A wildcard over a **single label** is refused, because `*.com` or `*.dev` is a
  whole top-level domain and never what anyone meant. `*.localhost` is the
  deliberate exception: RFC 6761 pins it to the local machine. This is not a
  public-suffix list and does not pretend to be one — `*.co.uk` passes. It catches
  the mistake people actually make.
- The **port** may be `*` for "any port". An **omitted** port means the scheme's
  default port, so `http://example.com` is port 80 exactly and not "any port on
  that host".

**Precedence**, highest first:

1. **The user's own answer**, given at a prompt or in the pane's per-site panel.
   It wins over this file in *both* directions — someone who blocks the camera at
   a config-granted origin stays blocked, or the panel that offered them the
   switch was decorative.
2. **This file.** `deny` beats `allow`, across separate matching rules as well as
   within one: two rules can match the same origin, and the safe reading of a
   config that says both things is the restrictive one.
3. **Veld's defaults** (below).

**Defaults**, which is what "no rule matches" means:

| Permission | Default | Why |
|---|---|---|
| `fullscreen`, `pointer-lock` | **allow** | Every browser grants these on a user gesture without asking, and both are reversible with Escape. Prompting would make a pane worse than the browser it embeds. **`keyboard-lock` is deliberately not here** — capturing Escape is what it does, so the justification does not extend to it |
| `display-capture` | **allow at an origin veld serves**, otherwise ask | A pane only ever captures *its own frame*, which is what `preferCurrentTab` asks for — and it is what makes `veld feedback` screenshots work inside a pane. At any other origin it prompts |
| everything else | **ask** | |

Nothing else is granted behind your back. Two permissions — sanitized clipboard
*write* and encrypted-media playback — used to be allowed before the policy ran,
on the reasoning that no browser shows them as a per-site switch. That was wrong
for this surface: an allow with no row in the panel is one nobody can see and
nobody can revoke. They are ordinary permissions now (`clipboard-write`,
`protected-media`) and they ask like everything else.

A `deny` withdraws any of these, including the screen-capture default.

**Understand what a grant is.** An entry for a *remote* origin hands that
server's JavaScript a standing capability on the machine of anyone who opens it
in a pane. That is a real step beyond what the rest of a veld config does: a
config command runs once, locally, as you. Loopback entries are a different
matter — a config that can already run `argv` on your machine is not meaningfully
constrained by withholding a camera from its own dev server. Every grant from
this file is shown in the pane's per-site panel labelled *set by veld.json*, and
can be revoked there.

**Permission ids** are veld's own, not Electron's. The difference is deliberate
in one place: Electron reports camera and microphone as a single `media`
permission, while a per-site panel has to show them as the two switches every
browser shows.

`camera`, `clipboard-read`, `clipboard-write`, `display-capture`, `file-system`, `fullscreen`,
`geolocation`, `hid`, `idle-detection`, `keyboard-lock`, `microphone`, `midi`,
`notifications`, `open-external`, `pointer-lock`, `protected-media`, `serial`,
`speaker-selection`, `storage-access`, `usb`, `window-management`.

Anything malformed here — an unparseable origin, an unknown id, a wrong-typed
field — is **dropped and reported by `veld lint` as a warning**, never a load
error. The dropped entry grants nothing: a permission rule that cannot be
understood must not be half-applied.

**Browser panes are Veld Desktop only.** A plain browser tab has no panes, so
nothing under `ide.permissions` applies there — an `<iframe>`'s permissions are
the embedding document's business, not veld's.

---

### Splitting `ide` across files

`ide` may appear in any file an `include` glob picks up — `veld.d/ide.jsonc` is
the obvious home for it — and the interpreted lists **concatenate in file order**
rather than the later file replacing the earlier one:

```jsonc
// veld.json          → "include": ["veld.d/*.jsonc"]
// veld.d/ide.jsonc   → { "ide": { "permissions": [ … ] } }
```

That matters for `permissions` specifically: `deny` beats `allow` across *all*
matching rules, so a rule arriving from another file can tighten the result but
never loosen one already written. Every other key under `ide` stays last-wins,
because veld does not interpret it and has no idea how to combine two of them.

---

### `ide.panes`: the project's own panes

A pane is a tab in Veld Desktop's dock. Veld ships four kinds (terminal,
browser, run logs, node health); `ide.panes` lets a project add its own to the
`+` menu, the pane chooser and the ⌘K palette. Today a declared pane is always a
**terminal** that runs the project's command instead of a login shell.

```jsonc
{
  "ide": {
    "panes": [
      {
        "id": "claude",
        "type": "terminal",
        "label": "Claude",
        "description": "Claude Code in this worktree",
        "icon": "sparkles",
        "requires_bin": ["claude"],
        "argv": ["claude", "--session-id", "${veld.pane.token}"],
        "resume": { "argv": ["claude", "--resume", "${veld.pane.token}"] },
        "auto_resume": true
      }
    ]
  }
}
```

| Field | Meaning |
|---|---|
| `id` | **Required.** Stable, unique in the project; `[A-Za-z0-9_-]`, ≤64 characters. Names the pane on the wire and in `${veld.pane.id}` — the user sees `label`. |
| `type` | **Required.** `terminal` is the only kind this version renders. The discriminator is required so a future kind is additive; a type veld does not know is skipped with a lint problem and the rest of `ide.panes` still applies. |
| `label` | Menu and tab text. Defaults to `id`. |
| `description` | One line, shown in the menu and as the tab's tooltip. |
| `icon` | An icon name or an emoji — see [below](#pane-icons). |
| `requires_bin` | Executable **names** (never paths) that must be on your `PATH` for the pane to be offered. Resolved with a lookup, never by running anything. |
| `argv` / `shell` | **Required**, exactly one. What a fresh pane runs. |
| `resume` | An object with `argv` or `shell`: what to run instead when the pane is restored and its shell is gone. |
| `auto_resume` | Whether veld may run `resume` without being asked. Defaults to `false`. |
| `close_on_exit` | Whether a **clean** exit closes the pane. Defaults to `true`. A non-zero exit never closes it. |

Nothing about this is specific to a vendor. Veld knows how to run a command in a
terminal and how to hand it a stable token; which tool that is, and what its
resume flag is called, lives in your repo.

#### Faking a session that survived a reboot

Nothing can carry a running process across a reboot — Veld's terminals already
survive a daemon restart and a `veld update` (each shell lives in its own holder
process), but a reboot ends every one of them.

What *can* survive is the conversation. A coding-agent CLI keeps its own
transcript keyed by a session id it is told, so if veld launches the tool under
an id it chose, it can later re-launch with that same id and the conversation
comes back. That is what `${veld.pane.token}` is:

- Veld mints a UUID the first time a pane launches and remembers it against that
  pane, in its database. **The token never reaches the browser or the app** — it
  is interpolated into the command inside the daemon.
- A fresh launch always mints a **new** token. `--session-id` is a *create*, so
  reusing one is at best refused, and "start fresh" has to mean a new
  conversation.
- Two panes of the same type in one worktree therefore get different tokens.
  That is the case the token exists for: with only `claude --continue`, the
  second pane would reopen the first pane's conversation.

#### The other shape: a tool that won't take your id

Plenty of tools mint their own session id and offer no way to be told one.
`codex` is the worked example: there is no launch-time `--session-id`, but
`codex resume` will continue the most recent session, and its resume is filtered
by working directory unless `--all` is passed — so "most recent" means most
recent *in this worktree*, which is exactly a pane's scope.

```jsonc
{
  "id": "codex",
  "type": "terminal",
  "label": "Codex",
  "icon": "robot",
  "requires_bin": ["codex"],
  "argv": ["codex"],
  "resume": { "argv": ["codex", "resume", "--last"] }
}
```

No `${veld.pane.token}` anywhere, and none needed — the pane still restores, the
Resume button still appears, and `auto_resume` still works. The token is an
optimisation for tools that cooperate, not a requirement.

What you give up is the thing the token buys: **two Codex panes in one worktree
would both resume the same session**, because "the most recent one" has a single
answer. With an id per pane, they don't. If a tool ever grows a way to accept an
externally-chosen id, moving it to the first shape is a one-line change.

#### When a pane starts by itself

`auto_resume` is narrower than it sounds, on purpose. These commands launch
coding agents: an unattended one spends money and runs tools with nobody
watching.

**It only ever fires at the moment a pane comes into being** — Veld Desktop
starting up and restoring your layout — and only when that pane's shell is
already gone. Concretely:

| What happened | What the pane does |
|---|---|
| You picked the pane from a menu | Runs the launch command. The click is the consent. |
| App restarted / rebooted, shell gone | `auto_resume: true` resumes; otherwise the pane waits with a **Resume** button. |
| The daemon restarted, or you ran `veld update` | Nothing — the shell survived, and the pane simply reattaches. |
| The tool exited while you were looking at it | Buttons, always. `auto_resume` is not consulted; an exit you saw is one you get to answer. |
| The session was reaped after the detach grace | Buttons, next time you look at it. |
| You dragged the pane to another window | Nothing — the shell is alive and moves with the pane. |

#### When a pane closes itself

`close_on_exit` defaults to **true**: a pane whose command exits with status `0`
closes, which is what a terminal emulator does and what quitting the tool
usually means.

Two bounds on it, both deliberate:

- **A non-zero exit never closes the pane**, whatever the setting says. The
  reason a tool died is printed on the screen it dies on, and a pane that
  disappears with it takes the error along — the oldest complaint about terminal
  emulators.
- **It cannot compete with `auto_resume`**, because it only fires on an exit
  somebody was there to see. A reboot, a quit app, a crashed daemon and a reaped
  session all leave the pane to be *restored from the layout* instead, and that
  is the path `auto_resume` governs.

The trade to know about: with the default on, a deliberate `/exit` closes the
pane, so the **Resume** button never appears for that exit. Set
`"close_on_exit": false` on a pane where you would rather stop and choose.

A `resume` that fails is **never** retried as a fresh launch. Silently starting a
new conversation would spend money and read to you as the old one having been
lost, so the pane says so and offers **Start fresh** as a separate button.

`auto_resume: true` without a `resume` command is a lint problem and is treated
as `false`.

**What `auto_resume` actually consents to.** The rest of veld runs a config
command because you asked — `veld start`, an action button, a pane you clicked.
This is the one place a repo-declared command can run when you merely *open the
app*, so it is worth being precise about what the consent covers.

veld remembers that this pane launched once. It does **not** pin the command
that ran. The `resume` command is re-read from the worktree's `veld.json` every
time, so a `git pull` that rewrites it — or flips `auto_resume` from `false` to
`true` on a pane you once started by hand — changes what runs unattended on your
next Desktop start. That is in keeping with a config file you already trust to
run `veld start`, but it means `auto_resume` is trust in the *repository*, not
in the specific command you clicked. On a repo whose config you do not control,
leave it off.

#### Quote your interpolations

The same rule as every other `shell` position — you own the quoting — with one
extra reason to care here. `${veld.branch}` is the one pane variable somebody
else can choose: check out a colleague's (or a stranger's) pull-request branch
and the branch name is theirs, not yours. Unquoted in a `shell` command, a
branch named `` `curl …` `` runs at pane launch.

```jsonc
"shell": "git -C \"${veld.root}\" log --oneline"       // quoted
"argv": ["git", "-C", "${veld.root}", "log"]            // better: no shell at all
```

`argv` interpolates per element after the array is fixed, so a value containing
spaces, quotes or backticks can never change the argument count. Prefer it.

#### Variables in a pane command

A pane has no run, no node and no ports, so its scope is much smaller than a
node's — referencing anything else is a lint problem rather than a pane that
dies on launch:

`${veld.pane.id}` · `${veld.pane.label}` · `${veld.pane.token}` ·
`${veld.worktree}` · `${veld.root}` · `${veld.branch}` · `${veld.project}` ·
`${veld.username}`

They mean exactly what they mean everywhere else — in particular
`${veld.worktree}` is the **slugified directory name**, not a path, and
`${veld.branch}` is slugified too. **The path you almost always want is
`${veld.root}`**, which for a pane is the worktree's own checkout.

`VELD_PANE_ID` and `VELD_PANE_TOKEN` are also set in the command's environment,
so a `shell` pane can use `$VELD_PANE_TOKEN` directly.

The command's `PATH` is your login shell's, not the daemon's — the same rule
every other daemon-spawned command follows.

#### Pane icons

`icon` takes either an emoji (any non-ASCII string, e.g. `"🤖"`) or one of these
names, from [Tabler](https://tabler.io/icons), which is the set every built-in
pane tab uses:

`atom` · `bolt` · `book` · `brain` · `bug` · `bulb` · `chart-line` · `cloud` ·
`code` · `compass` · `cpu` · `database` · `flask` · `git-branch` · `key` ·
`map` · `message-chatbot` · `notebook` · `package` · `player-play` · `plug` ·
`puzzle` · `refresh` · `robot` · `rocket` · `search` · `server` · `shield` ·
`sparkles` · `terminal-2` · `tool` · `wand`

It is an allowlist rather than "any Tabler name" because the app has to bundle
every icon that can be rendered. An ASCII string means "this is a name", so a
misspelled one is reported by `veld lint` instead of rendering as the literal
text.

#### There are no pane variants

Two modes of the same tool are two entries, not one entry with variants:

```jsonc
{ "id": "claude",      "type": "terminal", "label": "Claude",        "argv": ["claude"] },
{ "id": "claude-yolo", "type": "terminal", "label": "Claude (yolo)",
  "argv": ["claude", "--dangerously-skip-permissions"] }
```

Node `variants` exist because a node is a vertex in the dependency graph that a
preset selects across. A pane is neither, so variants would import the machinery
without the reason — and each pane owning exactly one token is what keeps "which
conversation am I resuming" from having an answer nobody can predict.

---

## Reserved: `hooks` and the rest of `ide`

Both are **reserved**: they parse, are stored, and are **not executed by this
version**. `veld lint` says so, so a hook that does nothing is distinguishable
from a config mistake. For `ide` the notice now names the specific keys that are
inert, since `quicklinks`, `permissions`, `externalOrigins` and `panes` are not.

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

A preset is either an array of `"node:variant"` strings, as above, or an object
that adds a picker key and a description -- see [Describing a
preset](#describing-a-preset). Use presets with:

```sh
veld start --preset fullstack --name my-feature
```

In interactive mode (TTY with presets defined), `veld start` with no arguments presents a preset selector. Presets are purely additive -- they select end nodes that Veld then resolves through the dependency graph, starting all required upstream nodes automatically.

### Preset keys: the number people actually type

Most people run `veld start` and type a number. That number is a preset's **key**,
and a key is an identity, not a position in the list:

```jsonc
"presets": {
  "dev-local":   { "key": 1, "selections": ["web:dev", "api:local"] },
  "dev-staging": { "key": 2, "selections": ["web:dev", "api:staging"] },
  "docker":      { "key": 5, "selections": ["web:docker"] }
}
```

A pinned `key` never moves -- not when presets are added, removed, renamed, or
regrouped. That is what makes it safe to memorise, to put in a runbook, and to
say to a colleague.

> **Upgrading?** These numbers used to be positions in an alphabetically sorted
> list, and they are now assigned in declaration order -- so for a config whose
> presets are not written alphabetically, they change once. See
> [the migration note](./migrating-to-v3.md#presets-gained-keys-and-metadata), and
> run `veld presets --pin` to freeze them.

Presets without a `key` take the **lowest number not already claimed**, in the
order they are declared. A preset *named* like a number (`"7"`) claims that number,
so its name and its key agree. Two consequences worth knowing:

- **Appending a preset leaves every existing key alone** -- the normal workflow.
  So does pinning a preset at the number it is already showing, which is what
  makes `--pin` below safe to apply piecemeal.
- An **unpinned** key still moves when a preset is added or removed *ahead of it*
  in declaration order. In a split config that includes the file another team
  owns: include globs load in sorted order, so someone adding
  `veld.d/api.jsonc` renumbers the unpinned presets declared in
  `veld.d/web.jsonc`. Pin the keys people actually type and this stops being your
  problem.

`veld presets` marks which keys are auto-assigned, and `veld presets --pin` prints
the current numbering as a block to paste:

```sh
veld presets --pin
```

Veld never rewrites your config, so applying the block is your call -- run
`veld lint` afterwards to check it. Two things to know when you do:

- **A preset is defined in exactly one file.** The block is one merged
  `presets` object, so in a split config add each entry's `key` to the file that
  already declares that preset -- pasting the whole block into the root file is a
  `duplicate-definition` error. `veld config --files` shows which file is which.
  Pinning a few at a time is safe, precisely because pinning a preset at the
  number it already shows moves nothing else.
- `--pin` refuses to run if the config did not load completely (say an included
  file has a syntax error), because the keys it would freeze are not the ones
  you will get once it does.

`--preset` accepts a key as well as a name, so `veld start --preset 2` and
`veld start --preset dev-staging` are the same thing.

In a **script**, prefer the name, or a key you have pinned. An unpinned key is a
convenience for whoever is looking at the list right now; passing one from a script
means passing a number that can move.

The two differ only for a config that has a preset *named* like a number. A
preset named `7` takes key 7, so normally there is nothing to disambiguate; if
some other preset pins key 7 as well, then `--preset 7` means the preset **named**
`7` -- names are what scripts and runbooks were written against -- while typing
`7` at the picker selects whatever the list showed beside `[7]`. `veld lint` warns
about that config either way.

### Describing a preset

A preset may be an object instead of a bare array. Both forms are fully
supported: the array form is right when the name says everything, and the object
form is for a list that has grown past what anyone can identify at a glance.

```jsonc
"presets": {
  // Array form — nothing more to say about it.
  "dev-local": ["web:dev", "api:local"],

  // Object form.
  "designer-preview": {
    "key": 1,
    "label": "Site preview (staging content)",
    "when_to_use": "Reviewing visual changes against real CMS content. Slow to start (~90s); not for API work.",
    "group": "For non-developers",
    "selections": ["web:prod", "api:staging"]
  }
}
```

| Field | Purpose |
|---|---|
| `selections` | Required. `node:variant` entries and `@preset` references -- the array form is exactly this field |
| `key` | The picker number, pinned permanently (see above) |
| `label` | Human-readable name, shown instead of the config key in the picker and the desktop UI |
| `when_to_use` | When to pick this one. Read by anyone who did not write the config -- and by coding agents, which get it from `veld presets --json` and from `veld presets` output. Say what it gives you and what it costs (start time, network, credentials) |
| `group` | Optional heading to chunk the list under. Purely visual |

Groups are ordered by their lowest member key, and presets within a group by key
-- so a group can move on screen without changing a single number. Note that the
list is ascending *within* each group, not read straight down: with keys 1 and 10
in one group and 2 in another, the first group prints in full first, so the
sequence is 1, 10, 2. A config that never mentions `group` prints one flat list,
and presets with no `group` in a config that uses them are collected under
`Other`.

### `default_preset`

```jsonc
"presets": { "dev-local": ["web:dev", "api:local"] },
"default_preset": "dev-local"
```

The preset `veld start` uses when given nothing to start. At the interactive
picker, pressing enter takes it. Without a TTY -- a script, CI, or a coding agent
running `veld start` in a non-interactive shell -- it is used directly, where a
bare `veld start` would otherwise fail with "No selections provided".

It must name a preset that exists; `veld lint` reports it if not.

Without a TTY and with no `default_preset` declared, a bare `veld start` still
exits 1 with a JSON payload on stdout: `error`, `nodes`, `presets` (the same
records `veld presets --json` emits), and a `hint` naming the three ways to give it
something to start.

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
| `preset-unresolvable` | an `@ref` names a preset that does not exist, the references form a cycle, or expansion exceeds its bounds (below) |
| `preset-unknown-node` | a selection names a node that is not defined. With `include` globs this can also mean no glob matched its file — `veld config --files` prints the glob → file → node chain |
| `preset-unknown-variant` | a selection names a real node but a variant it does not have; the message lists the variants it does have |
| `preset-missing-node-ref` | a node in this preset's plan references `${nodes.X.…}` and `X` is not in that plan — the message names both the preset and the reference |
| `preset-duplicate-key` | two presets pin the same `key`. A key is the number a person types, so it cannot mean two things |
| `preset-invalid-key` | `"key": 0` -- the picker numbers from 1 |
| `preset-empty` | a warning: the preset expands to no selections, so starting it brings up nothing and still reports success |
| `preset-name-shadowed-by-key` | a warning: a preset is *named* like a number that another preset **pins** as its key, so the number and the name select different presets |
| `default-preset-unknown` | `default_preset` names a preset that does not exist |
| `presets-undocumented` | a notice, once, when a config has eight or more presets and none carry a `label` or `when_to_use` -- the size at which the list stops being pickable by anyone who did not write it |

The last one is the "works with preset A, dies with preset B" case. A preset's plan
is fully static — `expand_preset` plus the transitive `depends_on` closure — so
given `"thin": ["web:dev"]` and a `web` whose `env` reads `${nodes.api.url}`, lint
can already tell that `api` is not in `thin`'s plan. In a config with many
overlapping presets that combination is the one thing you cannot check by reading a
single node file. A node pulled in only by `depends_on` counts as present.

**Expansion is bounded**: at most **256** levels of `@preset` nesting and **4096**
expansion steps. A preset referenced from two places is expanded once per reference,
so a tree that doubles at each level costs 2^depth while every individual path
through it stays acyclic — and expansion runs inside the daemon, on the endpoint the
desktop app polls, so an unbounded one is a config that can hang the daemon rather
than just a slow `veld start`. Both numbers are far above any hand-written preset
(real trees are 2-3 levels and a few dozen steps). Exceeding either is reported as
"cannot be expanded" wherever a preset is named; flatten the tree.

---

## Variable Substitution

Veld provides two separate variable systems for different contexts:

1. **`${...}` syntax** -- used in `command`, `script` arguments, and `env` values within variant configurations.
2. **`{...}` syntax** -- used exclusively in the `url_template` field.

### Built-in Variables (`${veld.*}`)

Available to all node variants without any declaration:

| Variable            | Value                                                |
|---------------------|------------------------------------------------------|
| `${veld.port}`          | Allocated **primary** port for this node in this run  |
| `${veld.url}`           | Full HTTPS URL for this node's primary port (`long_running` with an `http` port) |
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
| `${veld.urls.<name>}`   | The URL of a named **`http`** port — the same thing `${veld.url}` is for the primary |
| `${veld.urls.<name>.hostname}` | DNS name only, for that port (`.host`, `.origin`, `.scheme`, `.port` likewise) |

`${veld.url}` means the primary port, permanently. `${veld.urls.<name>}` is the
identical family for every other `http` port, decomposing into the same five
pieces; across nodes it is `${nodes.<node>.urls.<name>.origin}`, and in the
process's environment `VELD_URL_<NAME>` — mirroring `VELD_PORT_<NAME>`, and
uppercased the same way (`-` becomes `_`).

A `tcp` port has a `${veld.ports.<name>}` and no URL, because Veld gives it no
hostname. `veld lint` says exactly that, naming the port and its protocol, rather
than reporting an unknown built-in.

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

| | run, name, project, root, worktree, branch, username | run_id | node, variant | port, url, url.\*, ports.\*, urls.\* |
|---|:-:|:-:|:-:|:-:|
| `vars` value | ✅ | ✅ | — | — |
| project `setup` / `teardown` | ✅ | — | — | — |
| project / node / variant `env` | ✅ | ✅ | ✅ | `long_running` only |
| `command` node: `argv`, `shell`, `skip_if`, `on_stop` | ✅ | ✅ | ✅ | — |
| `long_running` node: `argv`, `shell`, `on_stop` | ✅ | ✅ | ✅ | ✅ |
| `actions[].cmd` | run, name, project, root only | — | ✅ | ✅ (plus `${param.*}`, `${output.*}`) |

An action's context is deliberately the smallest: it also has no `${vars.*}` and
no `${nodes.<other>.…}`, because an action runs against one live node long after
the plan that built it. `${veld.url}` and its pieces, and `${veld.ports.*}`, do
resolve there — they come from the node's recorded state.

Notes:

- **`run_id`** is absent in a project step because a `teardown` also runs from
  `veld stop` after the run row is gone. Use `${veld.run}`, the name you started
  with.
- **`port` / `url` / `ports.*` / `urls.*`** exist only where a port is allocated
  and a route registered, which is a `long_running` step — and only one that has
  the port in question. A node declaring `"ports": null` has none of them, and a
  `tcp` port has a `ports.<name>` but no `urls.<name>`. From anywhere else, reach a
  server's address as [`${nodes.<node>.url}`](#node-output-references-nodes) —
  including from the node's own `env`, which is how a server tells itself its
  public URL (`NEXTAUTH_URL`, `BASE_URL`), and how a portless Electron shell is
  handed the URL of the app it should open.
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

The built-in `url` and `port` outputs for `long_running` nodes are **pre-computed** before any node executes. This means every node in the graph can reference any `long_running` node's URL or port — regardless of dependency order.

This is especially powerful for cross-referencing: the frontend can know the backend's URL and the backend can know the frontend's URL, without creating a dependency cycle. `depends_on` controls execution order only, not variable availability for URLs and ports.

```
${nodes.backend.url}               # long_running built-in: full HTTPS URL (primary port)
${nodes.backend.url.hostname}      # long_running built-in: DNS name only
${nodes.backend.url.host}          # long_running built-in: hostname:port
${nodes.backend.url.origin}        # long_running built-in: scheme + host
${nodes.backend.url.scheme}        # long_running built-in: protocol scheme
${nodes.backend.url.port}          # long_running built-in: HTTPS port
${nodes.backend.port}              # long_running built-in: allocated primary port (rarely needed)
${nodes.backend.ports.debug}       # a named port, whatever its protocol
${nodes.backend.urls.admin}        # a named http port's URL — .hostname/.host/.origin/… too
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
${nodes.backend.url}               # long_running built-in: full HTTPS URL
${nodes.backend.url.hostname}      # long_running built-in: DNS name only
${nodes.backend.url.host}          # long_running built-in: hostname:port
${nodes.backend.port}              # long_running built-in: allocated port (rarely needed)
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
  "type": "long_running",
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

URL templates define how Veld generates HTTPS URLs for a `long_running` node's `http` ports. Templates can be defined at the project, node, or variant level, and a single port can override the lot with [`host`](#protocol).

### Syntax

URL templates use `{variable}` syntax (single braces, not `${}`). This is different from the `${variable}` syntax used in commands and env values.

```json
"url_template": "{service}.{branch ?? run}.my-project.localhost"
```

### Template Variables

All values are slugified automatically (lowercased, non-alphanumeric characters replaced with `-`, consecutive dashes collapsed, leading/trailing dashes stripped, max 48 characters).

| Variable     | Value                                                          |
|--------------|----------------------------------------------------------------|
| `{service}`  | Node name for the node's primary port; `<node>-<port>` for any other `http` port |
| `{variant}`  | Variant name                                                   |
| `{run}`      | Run name (always non-empty)                                    |
| `{project}`  | Project name from `veld.json`                                  |
| `{branch}`   | Current git branch name, slugified (empty string if not in git)|
| `{worktree}` | Slugified worktree directory name                              |
| `{username}` | OS username                                                    |
| `{hostname}` | Machine hostname                                               |

`{branch}` and `{worktree}` are evaluated at run creation time and frozen into the run state. URLs never change if you switch branches mid-run.

**That table is the whole list.** There is no `{port}` placeholder — an unknown
key is an error at `veld start`, not an empty string, so a template that reaches
for one never runs. Ports are the thing URL templates exist to hide: Veld routes
the hostname to the allocated port itself, and `${veld.port}` / `${veld.url.port}`
are how a *command* or `env` value reads a number.

`{service}` carrying the port name for a secondary `http` port is what keeps every
hostname a node owns a sibling at the same depth — see
[Every `http` port gets its own hostname](#every-http-port-gets-its-own-hostname).

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
          "url_template": "{service}-docker.{run}.{project}.localhost",
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
| `{service}.{username}.{project}.localhost`           | `frontend.jane.my-project.localhost`              |
| `{service}.{run}.{project}.localhost`, `admin` port | `frontend-admin.my-feature.my-project.localhost`  |

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
          "type": "long_running",
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
          "type": "long_running",
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
          "type": "long_running",
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
          "type": "long_running",
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
          "type": "long_running",
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
          "type": "long_running",
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
- **The terminal node must be a `command` type** (a `long_running` never exits,
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

All log output (both `long_running` stdout/stderr and internal Veld events) is timestamped when the line is emitted, which is what lets `veld logs` merge nodes chronologically.

**Stored in UTC, shown in your time zone.** Every line is stored as RFC 3339 UTC with microsecond precision — not a display choice but a requirement, since Veld orders and interleaves lines by comparing those strings. What you *read* is converted — in `veld logs`, in `veld start --attach`'s live streaming, and in the `/ide` logs view:

```
$ veld logs
web:local [2026-03-12T09:30:01.123456+01:00] Server listening on port 3000
web:local [2026-03-12T09:30:01.456789+01:00] Connected to database

$ veld logs --utc
web:local [2026-03-12T08:30:01.123456Z] Server listening on port 3000
web:local [2026-03-12T08:30:01.456789Z] Connected to database
```

- `--utc` prints the stored string verbatim; `--local` forces local. Either overrides the `logs.timeZone` setting for one command, and the two cannot be combined with each other or with `--json`. `veld start --attach` has no such flags — it follows the setting, so a `veld logs` beside it shows the same clock.
- **`--json` always emits UTC**, whatever the setting says. It is the machine-readable shape, so `timestamp` has exactly one spelling regardless of who ran the command.
- The setting lives at **Settings → General** in the `/ide` management UI (and Veld Desktop), and defaults to `local`. The `/ide` logs view follows it, and each timestamp's tooltip carries the full date, both zones, and the exact stored value.
- **The first-generation dashboard at `https://veld.localhost/` always shows local time** and does not read this setting — it fetches no settings at all. So with `logs.timeZone` set to `utc`, that one page still shows local. It is a frozen surface; `/ide` is where the setting applies.
- **"Local" means each reader's own clock, not one shared clock.** The setting fixes the policy; the zone is resolved where the timestamp is rendered — from the process environment for the CLI, from the browser for `/ide`. So a `veld logs` run with an empty `TZ` prints `+00:00` (an empty `TZ` resolves to UTC) while the same daemon's `/ide` shows your machine's zone, and a browser on another machine shows that machine's. Use `--utc` when two readers must agree exactly.
- **One hour a year, the displayed time is not monotonic.** At the DST fall-back the local clock repeats an hour, so correctly-ordered lines can render `02:59:00` then `02:01:00`. The lines are not misordered and nothing is wrong with the data — the stored UTC values are strictly increasing, which is what Veld sorts by. `veld logs` shows the offset inline (`+02:00` then `+01:00`), and in `/ide` the tooltip's offset is the tiebreak. `--utc` avoids the ambiguity entirely.

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
- `type` values are one of `"command"` or `"long_running"` (`"start_server"` also validates — it is a permanent alias)
- Health check types are one of `"http"`, `"port"`, `"command"`, or `"settle"`
- `long_running` variants require exactly one of `argv` or `shell`
- `command` variants require exactly one of `argv`, `shell`, or `script`
- Preset entries match the `node:variant` pattern
- Numeric constraints (timeouts, intervals, status codes)
