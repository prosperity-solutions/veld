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
  `${nodes.<node>.*}`. The set is closed but not uniform: `port`/`url`/`url.*`/
  `ports.*` exist only on a `start_server` node, `node`/`variant` only on a node,
  `run_id` not in a project step, and a `vars` value gets the run-scoped names
  only. `veld lint` reports a name written out of scope as
  `builtin-not-in-scope`. A node's `on_stop` has exactly what the node had.
- **Any new field that runs something is called `argv`/`shell`** — never
  `command`, `cmd`, `exec`, or `run`.

## Schema

```jsonc
{
  // Every config file accepts // and /* */ comments and trailing commas, at any
  // schemaVersion. The root file may be veld.json OR veld.jsonc; with both in
  // one directory veld.json wins and lint errors `ambiguous-root-config`. With
  // .json, editors need `"files.associations": {"veld.json": "jsonc"}`.
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
  // "default_preset": "<preset-name>",  // root file only
  "nodes": { },
  "hooks": { },   // reserved: parsed, stored, NOT executed by this version
  "ide": { }      // quicklinks + permissions are rendered; every other key is reserved
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

A var literal is **interpolated**, so every `${…}` in it is veld's to resolve: a
reference in no veld namespace (`"${HOME}/.cache"`) is a
`var-unresolvable-reference` error — write `$HOME` unbraced for the shell, or use
`{ "env": "HOME" }`. It is interpolated against the **run-scoped** built-ins — `${veld.run}`,
`run_id`, `name`, `project`, `root`, `worktree`, `branch`, `username`. The per-node
ones (`port`, `url`, `url.*`, `ports.*`, `node`, `variant`) are a
`builtin-not-in-scope` error in a var: a var is one value for the whole run, so
compose those at the use site. A var whose value is a *fetched* source is used
verbatim (no interpolation inside secret-store content).

A var backed by a source is resolved **only when the resolved plan reaches it** —
the same laziness a node-level `env` source has — so a credential-helper var does
not run on a `veld start` whose nodes need no secret. `${vars.*}` works in `setup`
steps too (it silently did not before).

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
`argv`, `shell`. Resolved at most once per run, at start — never at parse, and
only if the resolved plan reaches the value. A missing `env` source is an error
naming the node and the variable. A source command has a 30s timeout (an
interactive credential helper has no terminal under the daemon, so it hangs — use
a non-interactive source).

**A `secret` value must not be substituted by veld into `argv` or `shell`** —
that is a lint error (`secret-in-command`), because a command line lands in the
process table. Deliver it via the environment or `files`.

Refused — the forms veld resolves, so the value really does land in argv:
`${vars.x}`, `${output.x}`, `${nodes.a.x}` naming a secret, anywhere in a command.

Allowed, and the recommended form: a bare `$NAME`, in **any** position. veld's
interpolation consumes `${…}` and nothing else, so `$NAME` reaches argv untouched
— a shell expands it later in the child, where the value never appears in any
process's arguments, and where no shell is involved it is inert text. There is no
shell-detection heuristic. Also allowed: handing a container the *name* only,
`["docker", "run", "-e", "NAME", "img"]`.

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

## Presets

```jsonc
"presets": {
  // Array form: selections only.
  "core": ["api:dev", "web:dev"],
  "ci":   ["@core", "e2e:dev"],    // @name references another preset

  // Object form: adds a stable picker key and the metadata needed to choose it.
  "designer-preview": {
    "key": 1,
    "label": "Site preview (staging content)",
    "when_to_use": "Reviewing visual changes against real CMS content. Slow to start; not for API work.",
    "group": "For non-developers",
    "selections": ["web:prod", "api:staging"]
  }
},
"default_preset": "core"
```

- An entry starting with `@` names **another preset** instead of a node, so
  overlapping sets do not repeat every selection and then drift.
- Selections are **de-duplicated**: a node reached through two presets starts once.
- A **cycle is an error** naming the path (`@a → @b → @a`), not a hang.
- Presets are additive — they select end nodes, and veld resolves the dependency
  graph from there, so upstream nodes start automatically.
- **Both forms are fully supported.** The array form is right when the name says
  everything; the object form is for a list too long to identify at a glance.

### Keys

`key` is the number typed at the `veld start` picker, and it is an **identity, not
a list position** — a pinned key does not move when presets are added, removed,
renamed, or regrouped. Presets without one take the **lowest unclaimed number**, in
declaration order. So appending a preset changes no existing key, and neither does
pinning one at the number it already shows (which is what makes `--pin` safe to
apply to a subset). An unpinned key *does* move when a preset is added or removed
ahead of it — including from another `include` file that sorts earlier, so in a
monorepo another team can renumber presets it does not own. `veld presets` marks
the auto-assigned keys and `veld presets --pin` prints a paste-ready block that
freezes them (veld never rewrites a config itself).

**Upgrade note:** these numbers used to be positions in an alphabetically sorted
list and are now assigned in declaration order, so they changed once. Tell the user
to re-check any runbook that names a number, and to run `veld presets --pin`.

`--preset` takes either: `veld start --preset 2` == `veld start --preset dev-staging`.
In a script, pass the **name** or a **pinned** key — an unpinned key can move.
A preset *named* like a number takes that number as its key, so there is normally
nothing to disambiguate. If another preset also pins that key, `--preset 7` resolves
the **name** first (scripts predate keys) while the picker resolves the **key** first
(the number is what the list showed) — `veld lint` warns on such a config.

`--pin` refuses to run on a config that did not load completely, and its block must
be applied per declaring file (a preset is defined in exactly one file, so pasting
the merged block into the root is a `duplicate-definition` error).

Display order is derived from keys — groups by their lowest member key, presets
within a group by key — so a group can move on screen without changing a number.
Ascending *within* a group, not globally: keys 1 and 10 in one group and 2 in
another print as 1, 10, 2. Ungrouped presets are collected under `Other`.

### `default_preset`

Root file only. The preset used when `veld start` is given nothing: enter at the
picker, and **without a TTY it is used directly** rather than failing with "No
selections provided". This is the field that makes "start the app" a defined
request. Must name a preset that exists.

### Lint rules

`veld lint` checks presets statically, so a broken preset fails at lint time
rather than at `veld start`:

| Rule | Severity | Fires when |
|---|---|---|
| `preset-unresolvable` | error | dangling `@ref`, or a cycle |
| `preset-unknown-node` | error | a selection names a node that does not exist |
| `preset-unknown-variant` | error | a real node, a variant it does not have |
| `preset-duplicate-key` | error | two presets pin the same `key` |
| `preset-invalid-key` | error | `"key": 0` — the picker numbers from 1 |
| `preset-empty` | warning | expands to no selections: starts nothing, still reports success |
| `preset-name-shadowed-by-key` | warning | a preset named like a number another preset **pins** as its key |
| `default-preset-unknown` | error | `default_preset` names nothing |
| `presets-undocumented` | notice | 8+ presets, none with a `label` or `when_to_use` |
| `unknown-node-ref` | error | `${nodes.X.…}` where `X` is not a node anywhere |
| `preset-missing-node-ref` | error | `${nodes.X.…}` where `X` is real but not in *this* preset's plan |

The last two resolve `${nodes.X.…}` against each preset's plan — the
"works with preset A, dies with preset B" class, which is the one thing a reader
cannot check by opening a single node file. A node pulled in transitively by
`depends_on` counts as present, and the message names both the preset and the
reference.

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

Variables: `${veld.name}`, `${veld.project}`, `${veld.root}`, `${veld.run}`,
`${veld.worktree}`, `${veld.branch}`, `${veld.username}` and `${vars.*}`, plus
shell env vars. No node-scoped vars (`${veld.port}`, `${veld.url}`,
`${veld.node}`, `${nodes.*}`) and no `${veld.run_id}` — a teardown step also runs
from `veld stop` after the run row is gone. `veld lint` reports these as
`builtin-not-in-scope`.

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

Its stdout and stderr go to that node's log stream, like a server's — live in
`veld start`'s progress output, then `veld logs --node <name>`. A `skip_if`
probe's output is not logged (it is a predicate, not the node's output).

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

**Secrets — `$KEY` beats `${output.KEY}`, but is not automatically safe.** `${output.DB_PASS}` is interpolated by veld into the command string, so it is in `ps` for certain — a `secret-in-command` **error**. `$DB_PASS` is expanded by the *shell*, and where the expansion lands decides the outcome: `echo $DB_PASS` (builtin) and `PGPASSWORD=$DB_PASS psql -U u db` (environment assignment) leak nothing, while `psql "postgres://u:$DB_PASS@host/db"` makes the shell `execve` `psql` with the expanded value in *that* program's argv. The shell's own `ps` entry shows the literal `$DB_PASS`; the program it runs shows the value. veld cannot distinguish them, so this is a `secret-shell-expansion` **warning**. Prefer giving the program the variable *name* (`PGPASSWORD=`, `--password-file`, `-e NAME`). GUI clients launched with a connection URL (`open -a Postico "postgresql://$DB_USER:$DB_PASS@…"`) always expand into the launcher's argv — omit the password and let the client prompt.

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
| `ide` | project (any file) | Veld's own IDE surfaces (Veld Desktop, `/ide`). `ide.quicklinks` and `ide.permissions` are rendered; **every other key under `ide` is reserved** — parsed, stored, NOT rendered. See the section below. |

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

## `ide` — quicklinks and permissions

Per-project settings for Veld's own IDE surfaces (Veld Desktop, and `/ide` in a
browser). Absent from most configs, and never affects a run. Every key under `ide`
other than the two below is **reserved**: parsed, stored, not rendered, and
`veld lint` emits a notice naming it.

> Spelled `ui` before it was interpreted. A config still using `ui` fails
> `veld lint` with `unknown-top-level-key`, and the message names the rename —
> renaming the key is the whole migration.

```jsonc
"ide": {
  "quicklinks": [
    { "label": "Staging", "url": "https://staging.example.com" }
  ],
  "permissions": [
    { "origin": "https://*.veld.localhost:*", "allow": ["notifications"] },
    { "origin": "http://localhost:*", "allow": ["geolocation"] },
    { "origin": "https://staging.example.com", "deny": ["display-capture"] }
  ]
}
```

### `ide.quicklinks`

Project links that are **not** veld's — staging, a dashboard, a wiki — listed
beside the run's own URLs on a browser pane's start page. `label` and `url` are
both required; `url` must be `http://` or `https://` (other schemes are refused,
because a click hands the string to the OS). **Literal only** — `${...}` is not
interpolated here, since the start page renders with no run to resolve against.

### `ide.permissions`

Browser permissions the project pre-answers for pages shown in a Veld Desktop
browser pane, so a dev server needing geolocation or a camera works for everyone
who clones the repo. A browser tab has no panes, so none of this applies there.

**Writing the `origin` is the part to get right:**

| Form | Matches |
|---|---|
| `https://*.veld.localhost:*` | any subdomain at **any depth**, on any port — the form to use for veld's own URLs |
| `https://*.veld.localhost` | the same hosts, but **port 443 only** — matches nothing on an unprivileged install, which serves on 18443 |
| `http://localhost:*` | that exact host, any port |
| `https://staging.example.com` | that host, port 443 exactly |
| `http://example.com` | port **80** exactly — an omitted port is the scheme's default, *not* "any port" |

- **Use `*.<project>.localhost:*` for veld's own URLs** — both wildcards. The
  host wildcard because URLs are `{service}.{run}.{project}.localhost`, so the
  **run name is in the hostname** and changes with the worktree, the branch or
  `--name`. The port wildcard because `veld setup unprivileged` (the no-sudo
  default) serves on **18443**, and an omitted port means 443 — so the portless
  form silently matches nothing there.
- The wildcard is only legal as the **leading label**, and `*.x` does **not** match
  `x` itself — write the apex out as its own rule if you want it.
- Matching is label-wise, so `evilveld.localhost` does not match
  `*.veld.localhost`.
- `*.com`, `*.dev` and friends are **refused**: a wildcard over one label is a
  whole TLD. `*.localhost` is allowed — RFC 6761 pins it to loopback.
- No `${...}` interpolation, no `*` inside a label (`web*.example.com`), no
  wildcards on an IP literal.

**Permission ids** (veld's own spelling — Electron reports camera and microphone
as one `media` permission, and a per-site panel needs two switches):
`camera`, `clipboard-read`, `clipboard-write`, `display-capture`, `file-system`, `fullscreen`,
`geolocation`, `hid`, `idle-detection`, `keyboard-lock`, `microphone`, `midi`,
`notifications`, `open-external`, `pointer-lock`, `protected-media`, `serial`,
`speaker-selection`, `storage-access`, `usb`, `window-management`.

**Precedence**, highest first — worth knowing before writing a rule, because two
of the three layers are not in this file:

1. The **user's own answer** (a prompt, or the pane's per-site panel). Beats the
   config in both directions; a config grant they blocked stays blocked.
2. **This file.** `deny` beats `allow`, across separate matching rules as well as
   within one.
3. **Veld's defaults:** `fullscreen` and `pointer-lock` are allowed (every
   browser grants them on a gesture, and Escape undoes both — which is why
   `keyboard-lock`, whose whole job is capturing Escape, is *not* in that set and
   asks like anything else); `display-capture` is allowed
   **at an origin veld serves** — which is what makes `veld feedback` screenshots
   work inside a pane — and asks anywhere else; everything else asks. A `deny`
   withdraws any of these.

**Anything malformed is dropped and reported by `veld lint` as a warning, never a
load error** — and the dropped rule grants nothing. Do not assume a rule works
because the config loaded; run `veld lint`.

**Understand what a grant is.** A rule for a *remote* origin hands that server's
JavaScript a standing capability on the machine of anyone who opens it in a pane.
Loopback and veld's own URLs are a different matter — a config that can already
run `argv` on the machine is not meaningfully constrained by withholding a camera
from its own dev server. Every config grant is shown in the pane's per-site panel
labelled *set by veld.json*, where it can be revoked.
