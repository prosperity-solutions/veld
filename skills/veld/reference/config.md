# Veld Configuration Reference

## Authoring principles

Read these before writing config. They are what the v3 surface is *for*, and every
one of them exists because the obvious alternative makes a monorepo config
unreadable.

- **Deduplicate values, never structure.** Which keys a node has stays written in
  that node. Only *values* get a single definition point (`vars`, node-level
  defaults). A reader — or you, six months later — must be able to open a node file
  and see what that node runs, and `rg <ENV_VAR_NAME>` must still find the line
  that sets it.
- **There is no inheritance, no mixins, no `extends`, no templates.** Do not reach
  for patterns learned from other config systems: a variant body is never
  assembled from somewhere else. If you find yourself wanting one, you want a
  node-level default or a `var`.
- **No loops, no `matrix`, no conditionals, no arithmetic in interpolation.** If
  you need five similar nodes, write five nodes; a node-level default carries the
  shared values.
- **Prefer `argv` over `shell`.** `argv` is spawned directly, so an interpolated
  value can never change the argument count. Use `shell` when you actually want a
  shell (pipes, redirection, `&&`, globbing) — it is permanently supported, not a
  fallback.
- **A secret is a pointer plus a flag, never custody.** Use a value source
  (`env`/`file`/`argv`) and `secret: true`. A secret may reach a process's
  environment or a file, never a command line.
- **`veld.*` is a closed set.** Node outputs are `${output.*}` and
  `${nodes.<node>.*}`.
- **Any new field that runs something is called `argv`/`shell`** — never
  `command`, `cmd`, `exec`, or `run`.

## Schema

```jsonc
{
  // Every config file accepts // and /* */ comments and trailing commas, at any
  // schemaVersion. Editors need `"files.associations": {"veld.json": "jsonc"}`.
  "$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json",
  "schemaVersion": "3",
  "name": "myproject",
  "url_template": "{service}.{run}.{project}.localhost",
  "include": ["veld.d/*.jsonc", "services/*/veld.node.json"],
  "vars": { },
  "env": { },
  "setup": [],
  "teardown": [],
  "presets": { },
  "nodes": { },
  "hooks": { },   // reserved: parsed, stored, NOT executed by this version
  "ui": { }       // reserved: parsed, stored, NOT rendered by this version
}
```

**Only the root file needs `schemaVersion` and `name`** — every other key is
optional in every file, so an included file is just `{ "nodes": { … } }`.

`schemaVersion` must be `"3"` — `"1"` and `"2"` are not supported and fail to load.
There is no converter — apply the rules in `docs/migrating-to-v3.md` yourself and
run `veld lint`. The `command` key is replaced by `argv`/`shell`.

## Running something: `argv` or `shell`

Two keys, exactly one of them, **everywhere** Veld runs something — a variant,
`on_stop`, `skip_if`, a `command`-type probe, `actions[]`, `setup`/`teardown`
steps, and value sources.

```jsonc
"argv":  ["pnpm", "dev"]              // spawned directly: no shell, no word splitting
"shell": "pnpm dev | tee out.log"     // run via sh -c; you own the quoting
```

Interpolation in an `argv` runs **per element after the array is fixed**, so
`["psql", "${vars.db_url}"]` is always exactly two arguments whatever the URL
contains. In a nested position the pair is wrapped:
`"on_stop": { "argv": [...] }`.

`command` — the v1/v2 shell-string form — is gone. A document containing it fails
to load, with every offending position named.

## Splitting across files (`include`, v3)

```jsonc
// veld.json
{ "schemaVersion": "3", "name": "monorepo",
  "include": ["veld.d/*.jsonc", "services/*/veld.node.json"] }

// services/api/veld.node.json
{ "nodes": { "api": { /* … */ } } }
```

- A node is defined in **exactly one file**; the same name in two is an error
  naming both. There is no precedence rule.
- An unparseable included file is a **named, fatal error** — never a silently
  missing node.
- Duplicate `vars`/`preset` names across files are errors.
- **Relative paths (`cwd`, `script`) resolve from the project root**, not from the
  file that declares them.
- Only the root file may `include`.

`veld config --files` prints the glob → file → node chain — use it first when a
node seems missing. `veld nodes` shows `file:line` per node.

## Node-level defaults (v3)

Declare any variant field once on the node; any variant overrides it.

| Field group | Node → variant | Variant removes it with |
|---|---|---|
| `env`, `ports`, `depends_on`, `files` | additive per key, variant wins | `"KEY": null` |
| `features` | per field, variant wins | — |
| `probes.readiness`, `probes.liveness` | **replace the whole probe object** | `"liveness": null` |
| `share`, `outputs` | replace wholesale | `"share": null` |
| `type`, `cwd`, `argv`, `shell`, `url_template` | replace | — |
| `on_stop` | replace | `"on_stop": null` |
| `skip_if` | *not a node-level field* — per variant only | — |
| `proxy` | `remove` unions, `set` overrides per key | — |

`probes` replaces per probe on purpose: field-wise merging would let a variant
switch `type: "http"` to `type: "command"` and inherit a stale `path`.

## `vars` (v3)

```jsonc
"vars": { "remote_api": "https://api.example.com" }
// used as "${vars.remote_api}" at each use site
```

A var is a **scalar or a single value source** — never an object, never a config
fragment. It may not reference another var (one hop). Duplicate names and unknown
references are errors. `veld config --why <pointer>` shows where a value came from.

## Value sources and `secret` (v3)

```jsonc
"env": {
  "REGION":       "eu-central-1",
  "GITHUB_TOKEN": { "env": "GITHUB_TOKEN", "secret": true },
  "SIGNING_KEY":  { "file": ".secrets/key", "secret": true },
  "DATABASE_URL": { "argv": ["secret-tool", "read", "p"], "secret": true }
}
```

Sources: `value` (inline literal, so it can carry the flag), `env`, `file`,
`argv`, `shell`. Resolved once at run start, before the first spawn — never at
parse. A missing `env` source is an error naming the node and the variable. A
source command has a 30s timeout (an interactive credential helper has no terminal
under the daemon, so it hangs — use a non-interactive source).

**A `secret` value must not appear in `argv` or `shell`** — that is a lint error,
because a command line lands in the process table. Deliver it via the environment
or `files`.

## `ports` and `files` (v3)

```jsonc
"ports": { "http": "auto", "debug": "auto" }   // ${veld.ports.debug}, VELD_PORT_DEBUG
"files": { ".secrets/k.pem": { "env": "CERT", "secret": true, "mode": "0400" } }
```

`${veld.port}` stays the primary — the one named `http`, or the sole entry. No
`ports` map means one allocated port, exactly as before. A delivered file is
created with its mode (default `0600`), never chmod-ed afterwards. It is **not**
removed when the run ends — git-ignore the path. veld warns at start if a `secret`
file is not ignored.

## Setup & Teardown

Project-level lifecycle steps. Not nodes — no variants, no health checks, no dependency graph.

**Setup** runs sequentially before any node. Non-zero exit aborts startup.
**Teardown** runs sequentially after all nodes stop. Best-effort (failures logged, not fatal).

```json
"setup": [
  { "name": "docker", "argv": ["docker", "info"], "failureMessage": "Docker must be running" },
  { "name": "network", "shell": "docker network create ${veld.name}-net 2>/dev/null || true" }
],
"teardown": [
  { "name": "network", "shell": "docker network rm ${veld.name}-net 2>/dev/null || true" }
]
```

Step fields: `name` (required), `argv` or `shell` (required, exactly one),
`failureMessage` (optional).

Variables: `${veld.name}`, `${veld.project}`, `${veld.root}`, `${veld.run}`, plus shell env vars. No node-scoped vars (`${veld.port}`, `${nodes.*}`).

## Node Types

### `start_server` — Long-running processes

Must bind to `${veld.port}`. Requires a readiness probe (`probes.readiness` or legacy `health_check`).

```json
{
  "type": "start_server",
  "argv": ["npm", "run", "dev", "--", "--port", "${veld.port}"],
  "probes": {
    "readiness": { "type": "http", "path": "/health", "timeout_seconds": 30 },
    "liveness": { "type": "http", "path": "/health", "interval_ms": 5000 }
  },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" },
  "outputs": { "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app" },
  "sensitive_outputs": ["DATABASE_URL"],
  "on_stop": { "argv": ["docker", "rm", "-f", "container-name"] }
}
```

### `command` — Run-to-completion tasks

Emits outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE`.

A `command` node can also be a run's **terminal node** via
`veld start <node> --oneshot`: veld starts its dependencies, runs it to
completion (streaming its output), then tears everything down and exits with the
node's exit code — the e2e/CI pattern. See the CLI reference / configuration
guide for details.

```json
{
  "type": "command",
  "script": "./scripts/clone-db.sh",
  "outputs": ["DATABASE_URL", "DB_NAME"],
  "skip_if": { "argv": ["./scripts/verify-db.sh"] },
  "probes": {
    "liveness": { "type": "command", "argv": ["pg_isready"], "interval_ms": 5000 }
  }
}
```

## Probes

### Readiness (startup)

Every `start_server` variant requires a readiness probe. Use `probes.readiness` (preferred) or legacy `health_check`. Three types:

```json
{ "type": "http", "path": "/health", "expect_status": 200, "timeout_seconds": 30 }
{ "type": "port", "timeout_seconds": 15 }
{ "type": "command", "argv": ["./scripts/check-ready.sh"], "timeout_seconds": 45 }
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
    "argv": ["pg_isready", "-h", "localhost", "-p", "5432"],
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
      "shell": "PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER $DB_NAME"
    }
  ],
  "variants": { "dblab": { "type": "start_server", "shell": "..." } }
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
| `include` | project (root only) | Globs of further config files. `*` within a segment, `**` across, `?` one char. |
| `vars` | project (any file) | One definition point per value, used as `${vars.name}`. Scalar or single value source only. |
| `setup` | project | Lifecycle steps before graph execution. Array of `{name, argv\|shell, failureMessage?}`. |
| `teardown` | project | Lifecycle steps after all nodes stop. Array of `{name, argv\|shell, failureMessage?}`. Best-effort. |
| `env` | project, node, variant | Environment variables. Cascades: variant > node > project (per-key merge); `"KEY": null` erases an inherited key. Values may be a string or a value source object. |
| `ports` | node, variant | Named ports: `{"http": "auto", "debug": "auto"}`. `${veld.ports.<name>}`, `VELD_PORT_<NAME>`. `${veld.port}` = primary. |
| `files` | node, variant | Values delivered to disk: `{"<path>": {source, secret?, mode?}}`. Mode defaults `0600`. |
| `hooks` | project (any file) | **Reserved.** Parsed and stored, NOT executed by this version. `veld lint` emits a notice. |
| `ui` | project (any file) | **Reserved.** Parsed and stored, NOT rendered by this version. |

Any **other** top-level key is an error reported by `veld lint` and `veld start`
(rule `unknown-top-level-key`) — deliberately not a load failure, so a typo cannot
strand `veld stop`. The pre-JSONC `"//": "…"` comment idiom lands here; make it a
real `//` comment.
| `cwd` | node, variant | Working directory. Relative paths resolve from project root. Variant overrides node. Supports `${...}` substitution. |
| `hidden` | node | Hide from `veld nodes` output |
| `client_log_levels` | project, node, variant | Browser log levels: `["log", "warn", "error", "info", "debug"]`. Exceptions always captured. |
| `features` | project, node, variant | `{"feedback_overlay": bool, "client_logs": bool, "inject": bool}`. All default `true`. |
| `proxy` | project, node, variant | `{request?: {remove?: [str], set?: {k: v}}, response?: {...}}`. Reverse-proxy header rules for the local Caddy proxy + web gateway (NOT peer shares). Cascades: `remove` lists union, `set` maps merge (variant > node > project). Absent = no manipulation. See [Proxy](#proxy). |
| `type` | node, variant | `start_server` or `command`. Declare once on the node if all its variants agree. |
| `argv` / `shell` | node, variant | What to run — exactly one of them. |
| `on_stop` | node, variant | Per-node teardown, run on `veld stop`, in reverse dependency order. `{argv\|shell}`. |
| `depends_on` | node, variant | `{node: variant}`. Both **literal** — no `${...}`; the graph is read before variables exist. `"node": null` erases. |
| `outputs` | node, variant | List of captured names, or a map of computed values (both node types). Replaced wholesale by a variant. |
| `share` | node, variant | Replaced wholesale by a variant, never merged — sharing is a consent decision. |
| `sensitive_outputs` | variant | Output keys to mask in logs and encrypt at rest. |
| `skip_if` | variant (`command` only) | Idempotency check — skip step if exits 0. `{argv\|shell}`. Alias: `verify`. |
| `probes` | node, variant | `{readiness?: Probe\|null, liveness?: Probe\|null}`. Both node types. A variant **replaces** a probe wholesale; `null` erases it. A `command` probe carries `argv`/`shell`; any probe may name a `port`. |
| `actions` | node | Named shell commands exposed via `veld action`/dashboard buttons. See [Actions](#actions). |
| `sharing` | project | `{relays?: "public" \| [url \| {url, token?},...], gateway?: url \| {url, token?}, dangerouslyEmbedRelayTokensInTicket?: bool}`. Relay policy (compliance) + public web gateway. Relay/gateway `token` values are secret sources. Config wins over `VELD_SHARE_RELAY`. See [Sharing](#sharing). |
| `share` | variant | `{expose: ["peer" \| "web", ...], web?: {access?: "password" \| "link"}}`. Per-service opt-in — absent/empty = not shareable. See [Sharing](#sharing). |

## Sharing

A service is shareable only if its variant declares `share.expose` — `veld share` refuses anything that hasn't opted in.

```json
{
  "sharing": { "relays": "public" },
  "nodes": {
    "frontend": {
      "variants": {
        "local": { "type": "start_server", "argv": ["npm", "run", "dev"], "share": { "expose": ["peer"] } }
      }
    }
  }
}
```

- `sharing.relays` — **must be opted into explicitly (no default):** `"public"` (n0's relays) or an array of self-hosted relay entries (confines share traffic for compliance). `veld share` is refused if unset (and no `VELD_SHARE_RELAY` env). Config wins over the env var. **`"public"` is dev/testing only** — n0's public relays are rate-limited, best-effort, no uptime/throughput guarantees; use self-hosted relays for production or high-volume sharing (n0 fair-use guidance, not a license restriction — iroh is MIT/Apache-2.0). The daemon binds one iroh endpoint per relay policy, so shares on different relays run concurrently (no restart).
  - A relay entry is a bare URL string, or `{ "url": ..., "token": ... }` to send an `Authorization: Bearer` token to a relay that requires one. `token` = a literal string (inline; lands in config), or `{ "env": "VAR" }` / `{ "file": "/path" }` / `{ "argv": ["op", "read", "..."] }` to resolve it on the daemon at share time without storing the secret. A source `argv`/`shell` runs with the user's login-shell PATH (like liveness probes), so user-installed CLIs (`op`, `vault`) are found even though the daemon itself has a bare launchd PATH — but only PATH is inherited, not other shell-exported vars or aliases; `env` still reads the daemon's environment, not your shell. A token that fails to resolve fails the share (never connects unauthenticated). `VELD_SHARE_RELAY_TOKEN` pairs a literal token with the `VELD_SHARE_RELAY` env override.
  - **Join side:** a joiner auto-uses the ticket's relay(s) (a custom-relay share is never joined over public relays). For a token-gated relay the token resolves by priority (highest first): prompt-entered > ticket-embedded > local cache (the central veld database, `<data_dir>/veld/veld.db`, 0600) > `VELD_SHARE_RELAY`+`VELD_SHARE_RELAY_TOKEN` (attached only to the matching ticket relay). If none works, the joiner is prompted (browser overlay / `veld join` terminal; `--json` returns `needs_relay_token` instead) and the entered token is cached; a wrong token re-prompts.
- `sharing.dangerouslyEmbedRelayTokensInTicket` — **DANGER, default false.** Embeds the resolved relay token(s) in the share ticket so joiners need no token setup. Ships the relay secret in every share link (Slack, email, history) — disposable per-project tokens only, never a shared org secret. camelCase (à la React's `dangerouslySetInnerHTML`) to flag the danger.
- `sharing.gateway` — the public web gateway `veld share --web` registers with: a bare URL, or `{ "url": ..., "token": ... }` where `token` is a secret source (same forms as relay tokens) for the gateway's required registration auth. Env override: `VELD_SHARE_GATEWAY` + `VELD_SHARE_GATEWAY_TOKEN` on the daemon. The gateway is a self-hosted container (`ghcr.io/prosperity-solutions/veld-gateway`); operator guide: `docs/gateway.md`.
- `share.expose` — `peer` (Veld-to-Veld via `veld share`, verbatim URL) and/or `web` (any browser via `veld share --web` + the gateway; real public URL, best-effort fidelity). Empty list or absent = not shareable. Peer and web are separate shares with separate capabilities — revoking one never touches the other.
- `share.web.access` — viewer access for the public URL: `"password"` (**default, also when absent** — the gateway shows a password page; `veld share --web` generates and prints the share password, `--password` chooses it, and the printed `#veld-key=…` one-link carries it in the URL fragment) or `"link"` (anyone with the URL; the unguessable slug is the only gate — treat the link as a secret). An explicit config value always wins over the `--access` CLI flag; the flag only covers config-silent services. Multi-service caveat: the viewer session cookie is per public host, so a password-protected API called cross-origin from the frontend gets 401s — give API nodes `"web": { "access": "link" }`.

## Proxy

Reverse-proxy header rules applied by the **local Caddy proxy** (local dev) and the **public web gateway** (`veld share --web`) when forwarding to/from a service. **Not** applied to direct iroh peer sharing (`veld share` without `--web`) — that path is a transport-level byte splice with no HTTP layer, so header rules cannot be applied there. Absent = no header manipulation (the default). Resolvable at project/node/variant (most specific wins): `remove` lists union (case-insensitive), `set` maps merge per key.

```json
{
  "proxy": {
    "request":  { "remove": ["Origin"], "set": { "X-Env": "dev" } },
    "response": { "set": { "X-Frame-Options": "DENY" } }
  }
}
```

- `request` → header rules for the request forwarded upstream; `response` → for the response returned to the browser.
- `remove`: header names to strip. `set`: name → value map (replaces any existing value). Header names matched case-insensitively.
- **Default change:** Veld no longer strips `Origin` by default (it used to, so dev-server WS HMR worked). `Origin` now passes through the local proxy; the gateway rewrites it *coherently* to the origin host on all requests (incl. WS upgrades) rather than dropping it. If a Next.js dev server rejects WS HMR on `Origin`, set `allowedDevOrigins` in `next.config.js` (recommended — https://nextjs.org/docs/app/api-reference/config/next-config-js/allowedDevOrigins). Escape hatch for frameworks with no allow-list: `"proxy": { "request": { "remove": ["Origin"] } }`.
