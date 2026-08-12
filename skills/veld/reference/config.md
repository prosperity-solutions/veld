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
  `ports.*`/`hosts.*`/`urls.*` exist only on a `long_running` node that declares the
  port in question (a `"ports": null` node has none of them, and neither has a node
  whose ports are all `tcp` — it has no primary for `port`/`url` to mean),
  `node`/`variant` only on a node,
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
  "ide": { }      // quicklinks + permissions + externalOrigins + panes + news + git (stalenessSensitivity) are rendered; every other key is reserved
}
```

**Only the root file needs `schemaVersion` and `name`** — every other key is
optional in every file, so an included file is just `{ "nodes": { … } }`.

`schemaVersion` must be `"3"` — `"1"` and `"2"` are not supported and fail to load.
There is no converter — apply the rules in `docs/migrating-to-v3.md` yourself and
run `veld lint`. The `command` key is replaced by `argv`/`shell`.

There is no `"4"` either. `long_running`, `"ports": null`, `protocol`/`host` on a
named port, the `settle` probe and per-port `share` were all added *within* `"3"`;
`docs/adopting-long-running-and-ports.md` covers adopting them and the behaviour
changes that come with them.

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

### Machine-overridable vars

A var may declare that its answer belongs to the **machine**, not the repo — a
locally installed container runtime, a memory ceiling, a path to a local tool:

```jsonc
"vars": {
  "container_runtime": {
    "machine": {
      "default": "docker",                    // omit to force every machine to answer
      "choices": ["docker", "podman"],        // enforced when set AND when resolved
      "description": "Which runtime runs this project's containers",
      "prompt": "Runtime?"                    // asked when there is no default
    }
  },
  "vendor_token": { "machine": { "prompt": "Vendor token" }, "secret": true }
}
```

`machine` is legal **only in `vars`** — an `env` map has no name for
`veld config set` to address. `default` is an ordinary value (so it may be
`{ "env": … }`) but can never be another machine var.

```sh
veld config vars                       # value + which scope it came from
veld config set NAME VALUE             # this machine, every worktree of the repo
veld config set NAME --env VAR         # store a pointer instead (how a secret is answered)
veld config set NAME VALUE --worktree  # this checkout only
veld config unset NAME
veld start --var NAME=VALUE            # this run only, never stored
```

Precedence: `--var` → worktree scope → project scope → `default` → error naming
the var and the command. Answers are keyed by the repo's **main checkout**, so
every worktree shares one; moving a checkout orphans them.

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
"ports": null                                  // no ports at all (portless long_running)
"files": { ".secrets/k.pem": { "env": "CERT", "secret": true, "mode": "0400" } }
```

Three authorings, and they are three different things:

| `ports` | meaning |
|---|---|
| absent | one auto-allocated `http` port — the default, unchanged |
| `null` | no ports at all: no allocation, no `${veld.port}`, no URL, no route |
| `{ … }` | that map, merged node → variant per key; `"name": null` erases one entry (erasing them all lands on "no ports", not back on the default) |

**`19899` (veld's own daemon port) is never allocated to a node, and naming it
explicitly is an error rather than a substitution** — a node that bound it would
break the daemon's next start.

An entry is either shorthand (`"auto"`, `5432`) or the long form
`{ "port": …, "protocol": "http" | "tcp", "host": "<template>", "share": { … } }`:

```jsonc
"ports": {
  "http":     "auto",                                  // primary  → protocol "http"
  "admin":    { "port": "auto", "protocol": "http" },  // own hostname: api-admin.<run>.…
  "postgres": { "port": 5432,   "protocol": "tcp",
                "share": { "expose": ["peer"] } },     // consent lives on the PORT
  "debug":    "auto"                                   // secondary → protocol "tcp"
}
```

**Default protocol is `http` for the primary port and `tcp` for every other**, so
an existing multi-port config gains no new hostname. An `http` port gets a
hostname and a Caddy route, plus `${veld.urls.<name>}` (with `.hostname`, `.host`,
`.origin`, `.scheme`, `.port`), `${nodes.<n>.urls.<name>}` and `VELD_URL_<NAME>`.
Its `{service}` label is the node name for the primary and `<node>-<port>` for a
secondary — a sibling at the same depth, so a wildcard cert covering the node
covers its extra ports too. Per-port `host` overrides that template and is the way
out of a collision. A `tcp` port is allocated and exported and deliberately never
routed: a raw TCP connection carries no hostname to demultiplex on, and
`*.veld.localhost` already resolves to 127.0.0.1.

`${veld.port}` stays the primary — the one named `http`, the sole entry marked
`"protocol": "http"`, or the sole entry when it states no protocol. Several ports
with none of those is `ambiguous-primary-port` (error).

Each port also carries its own **sharing consent** — see [Sharing](#sharing). A
node/variant-level `share` is shorthand for the *primary* port's policy and never
spreads to the others, so a node can serve an app port, an ops console and a
database and expose only the one it named.

A delivered file is created with its mode (default `0600`), never chmod-ed
afterwards. It is **not** removed when the run ends — git-ignore the path. veld
warns at start if a `secret` file is not ignored.

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
- Expansion is **bounded**: at most 256 levels of `@preset` nesting and 4096
  expansion steps, else the preset is refused ("cannot be expanded"). A preset
  referenced from several places is expanded once per reference, so a *tree* that
  doubles at each level costs 2^depth even though every path through it is acyclic —
  and expansion runs in the daemon, on an endpoint the desktop app polls. Both
  limits are far above any hand-written preset; hitting one means a generated config
  is fanning out, and the fix is to flatten the tree.
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

A node's `type` describes its **lifecycle only** — runs to completion, or stays
running. Whether it serves anything is a property of its `ports`.

### `long_running` — Supervised processes

By default the node gets one auto-allocated `http` port and must bind to
`${veld.port}`. A readiness probe (`probes.readiness` or legacy `health_check`) is
always required.

`start_server` is the historical spelling and remains a **permanent alias**,
exactly as `bash` is for `command`. Configs written either way load forever and
veld never rewrites one; write `long_running` in anything new.

```json
{
  "type": "long_running",
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

#### Portless (`"ports": null`)

A long-running process that serves nothing — an Electron shell, a file watcher, a
background compiler. No port, no `${veld.port}`, no URL, no route; veld starts it,
keeps it in the graph, and stops it with the rest. Readiness is still required,
and an `http`/`port` probe here is a `probe-needs-port` error.

```json
{
  "type": "long_running",
  "shell": "electron .",
  "ports": null,
  "depends_on": { "web": "dev" },
  "env": { "APP_URL": "${nodes.web.url}" },
  "probes": { "readiness": { "type": "settle", "seconds": 5 } }
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

Every `long_running` variant requires a readiness probe — including a portless one. Use `probes.readiness` (preferred) or legacy `health_check`. Four types:

```json
{ "type": "http", "path": "/health", "expect_status": 200, "timeout_seconds": 30 }
{ "type": "port", "timeout_seconds": 15 }
{ "type": "port", "port": "admin" }
{ "type": "command", "argv": ["./scripts/check-ready.sh"], "timeout_seconds": 45 }
{ "type": "settle", "seconds": 3 }
```

- `http`: Two-phase — TCP port check first, then HTTP. Default status: 200, path: `/`.
- `port`: Just checks TCP connection.
- `command`: Exit 0 = healthy.
- `settle`: The process was still alive `seconds` after spawn (default 3). For a **portless** node. Deliberately a weak claim, but raced against process exit, so a command that dies on startup still fails the run. Prefer `command` whenever the process publishes something observable (a socket, a built file, a pid file).
- `http`/`port` probe the **primary** port unless `"port": "<name>"` names another one from the node's `ports` map. On a node with no such port they are a `probe-needs-port` error and fail the run — they no longer report healthy. An unrecognised `type` is `unknown-probe-type`, for the same reason: a typo must never be the quiet way to turn a probe off.
- **Probe fields are interpolated**: an `http` probe's `path` and a `command` probe's `argv`/`shell` resolve `${…}` with the node's context, like the node's own `argv`/`env` (`schema/v3/examples/vars-and-values.json` proves it).
- A **`command` probe runs with the node's declared `env`**, the veld-owned `VELD_*` variables (including `VELD_PORT`), and the node's outputs as `$KEY`, so it can be parameterised. On failure its **stderr and exit code are included in the timeout error** rather than discarded.
- Defaults: `timeout_seconds`: 60, `interval_ms`: 1000 (min: 100).

### Liveness (ongoing)

Runs continuously after a node becomes healthy. Available for both `command` and `long_running` types. Same check types as readiness minus `settle`: `http`, `port`, `command` (arbitrary shell command, exit 0 = healthy). `settle` describes startup, so as a liveness check it would report healthy forever — veld rejects it there.

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
  "variants": { "dblab": { "type": "long_running", "shell": "..." } }
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
- The node's declared `env` (as `$KEY`, below the node outputs in precedence) and the veld-owned `VELD_*` variables — `VELD_RUN`, `VELD_RUN_ID`, `VELD_ROOT`, `VELD_PROJECT`, `VELD_NODE`, `VELD_VARIANT`, plus `VELD_PORT`/`VELD_URL`/`VELD_PORT_<NAME>`/`VELD_URL_<NAME>`/`VELD_HOST_<NAME>` on a node that has ports

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
| `ports` | node, variant | Named ports: `{"http": "auto", "debug": "auto"}`, or an entry's long form `{"port": …, "protocol": "http"\|"tcp", "host": "<template>"}`. `${veld.ports.<name>}`, `VELD_PORT_<NAME>`; an `http` port also gets a hostname, a route, `${veld.urls.<name>}` and `VELD_URL_<NAME>`. `${veld.port}` = primary. Protocol defaults: `http` for the primary, `tcp` for the rest. **`null` = no ports at all** (portless `long_running`); absent = one auto http port. |
| `files` | node, variant | Values delivered to disk: `{"<path>": {source, secret?, mode?}}`. Mode defaults `0600`. |
| `hooks` | project (any file) | **Reserved.** Parsed and stored, NOT executed by this version. `veld lint` emits a notice. |
| `ide` | project (any file) | Veld's own IDE surfaces (Veld Desktop, `/ide`). `ide.quicklinks`, `ide.permissions`, `ide.externalOrigins`, `ide.panes` and `ide.news` are rendered; **every other key under `ide` is reserved** — parsed, stored, NOT rendered. See the section below. |

Any **other** top-level key is an error reported by `veld lint` and `veld start`
(rule `unknown-top-level-key`) — deliberately not a load failure, so a typo cannot
strand `veld stop`. The pre-JSONC `"//": "…"` comment idiom lands here; make it a
real `//` comment.
| `cwd` | node, variant | Working directory. Relative paths resolve from project root. Variant overrides node. Supports `${...}` substitution. |
| `hidden` | node | Hide from `veld nodes` output |
| `client_log_levels` | project, node, variant | Browser log levels: `["log", "warn", "error", "info", "debug"]`. Exceptions always captured. |
| `features` | project, node, variant | `{"feedback_overlay": bool, "client_logs": bool, "inject": bool}`. All default `true`. |
| `proxy` | project, node, variant | `{request?: {remove?: [str], set?: {k: v}}, response?: {...}}`. Reverse-proxy header rules for the local Caddy proxy + web gateway (NOT peer shares). Cascades: `remove` lists union, `set` maps merge (variant > node > project). Absent = no manipulation. See [Proxy](#proxy). |
| `type` | node, variant | `long_running` (alias: `start_server`) or `command`. Lifecycle only — ports decide whether the node serves anything. Declare once on the node if all its variants agree. |
| `argv` / `shell` | node, variant | What to run — exactly one of them. |
| `on_stop` | node, variant | Per-node teardown, run on `veld stop`, in reverse dependency order. `{argv\|shell}`. Cross-node `${nodes.<node>.<field>}` references resolve at stop time from the run's persisted state, and the veld-owned `VELD_*` vars are exported. A hook whose command **or environment** cannot be resolved is skipped loudly (it never runs with an empty env). |
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

**Consent is per port.** `share` is a field on a port entry — that is where exposure happens, and the only place a config grants it. `veld share` refuses everything else, naming candidates as `node:variant#port`.

```jsonc
{
  "sharing": { "relays": "public" },
  "nodes": {
    "api": {
      "variants": {
        "dev": {
          "type": "long_running",
          "shell": "api --port ${veld.port} --admin ${veld.ports.admin}",
          "ports": {
            "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer", "web"] } },
            "admin":    { "port": "auto", "protocol": "http" },                      // never shared
            "postgres": { "port": 5432,   "protocol": "tcp",  "share": { "expose": ["peer"] } }
          },
          "probes": { "readiness": { "type": "port" } }
        }
      }
    },
    "frontend": {
      "variants": {
        // Node/variant-level `share` still works: shorthand for the PRIMARY port only.
        "local": { "type": "long_running", "argv": ["npm", "run", "dev"], "share": { "expose": ["peer"] } }
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
- `share.expose` — `peer` (Veld-to-Veld via `veld share`) and/or `web` (any browser via `veld share --web` + the gateway; real public URL, best-effort fidelity). Empty list or absent = not shareable. Peer and web are separate shares with separate capabilities — revoking one never touches the other.
- **Where `share` is written, and how it resolves.** On the port entry. A node/variant-level `share` is *defined* as shorthand for the **primary** port's policy, so every config written before per-port consent means exactly what it meant — and the same words can never spread to an ops console or a database the author never mentioned. A port's own `share` **replaces** the shorthand for that port (no merge; the more specific declaration wins), an absent `share` is always "not shared", and nothing anywhere widens a port that declared none. A node with no primary (all-`tcp`, or `"ports": null`) has nowhere to fold the shorthand into, so it grants nothing.
- **`web` requires `"protocol": "http"`** — lint rule `web-share-needs-http` (error). The gateway speaks HTTP/1.1 over the tunnel and a browser cannot speak a raw protocol through it: that is what the `web` audience *is*, not a limitation to be lifted. Enforced three times — `veld lint`/`veld start`, the daemon at share time (which names the excluded port instead of dropping it silently), and the gateway, which discards any non-routed manifest entry.
- **Raw `tcp` sharing is `peer`-only, and the joiner's port differs from yours.** A `tcp` port opted into `peer` rides the same encrypted iroh tunnel and is reproduced on the joining machine as a bare local TCP port — no Caddy route, since a raw connection carries no hostname to match on. Nothing preserves the origin's port number, so the address is the joiner's local listener: `veld join` prints it as `host:port  (tcp)` apart from the URLs, and `--json` returns it in `addresses` (URLs stay in `urls`). Quote the printed address, never the origin's `veld.json` port. `proxy` header rules are an HTTP concept and do not apply. A joiner on an older veld **refuses the entire join** when the manifest carries a tcp endpoint (the old wire format required `url`) — deliberate fail-closed behaviour, so a peer that cannot represent an endpoint never reproduces it as an HTTP route. There is no `udp`.
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

## `ide` — quicklinks, permissions, external origins, panes and news

Per-project settings for Veld's own IDE surfaces (Veld Desktop, and `/ide` in a
browser). Absent from most configs, and never affects a run. Every key under `ide`
other than the ones below is **reserved**: parsed, stored, not rendered, and
`veld lint` emits a notice naming it.

> Spelled `ui` before it was interpreted. A config still using `ui` fails
> `veld lint` with `unknown-top-level-key`, and the message names the rename —
> renaming the key is the whole migration.

```jsonc
"ide": {
  "quicklinks": [
    { "label": "Staging", "url": "https://staging.example.com" }
  ],
  "externalOrigins": ["https://accounts.google.com", "https://*.okta.com"],
  "permissions": [
    { "origin": "https://*.veld.localhost:*", "allow": ["notifications"] },
    { "origin": "http://localhost:*", "allow": ["geolocation"] },
    { "origin": "https://staging.example.com", "deny": ["display-capture"] }
  ],
  "panes": [
    {
      "id": "claude",
      "type": "terminal",
      "label": "Claude",
      "icon": "sparkles",
      "requires_bin": ["claude"],
      "argv": ["claude", "--session-id", "${veld.pane.token}"],
      "resume": { "argv": ["claude", "--resume", "${veld.pane.token}"] },
      "auto_resume": true
    }
  ]
}
```

### `ide.externalOrigins`

Origins that must open in the user's **system** browser rather than in a Veld
browser pane. A URL a terminal produces — clicked in its output, or opened by a
program running in the shell (Veld points `$BROWSER` at itself, which `gh`, `git`,
Claude Code, vite and next all consult) — otherwise becomes a pane beside that
terminal.

The reason is cookies: a pane has its own jar, so an SSO or bank flow started in
one begins from scratch. List the hosts *this app's* sign-in goes through.

Same grammar as `ide.permissions[].origin` (same parser, same lint treatment):
`scheme://host[:port]`, http(s) only, no path, leading `*.` for any depth of
subdomain, `*` port for any port, omitted port = the scheme's default exactly.
Matching is on the **origin**, so a path cannot be exempted on its own, and an
internationalised host must be written punycoded.

**Unioned** with the user's `browser.externalOrigins` setting rather than
replacing it. A project cannot remove a user's entry, and cannot switch the
feature off — that is the user's `terminal.openUrlsInApp`.

Not to be confused with the two user settings that ride the same shell handoff
and that a project cannot influence either: `terminal.shellIntegration` (a
terminal reports when a command started and how it ended, which marks a
worktree in the rail) and `terminal.agentIntegration` (an agent wrapper —
`claude`, `codex` — installs lifecycle hooks so an agent waiting on the user
reaches the same glyph). Both sit under *Settings → Activity* with
`activity.showWorking` and the
four `activity.notify*` rows. All of these switches are independent of one
another.

### `ide.quicklinks`

Project links that are **not** veld's — staging, a dashboard, a wiki — offered
behind a **Bookmarks** button on a browser pane's start page and on the new-pane
chooser, where the list itself is the run's own URLs. Typing in the address bar
still matches them inline. `label` and `url` are both required; `url` must be
`http://` or `https://` (other schemes are refused, because a click hands the
string to the OS). **Literal only** — `${...}` is not interpolated here, since
the start page renders with no run to resolve against.

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

### `ide.news`

Cards the project shows its own team, through the channel Veld uses for its own
announcements. Merge one with the change it describes; a teammate pulls, and the
next time they open the IDE they are told once. Reading clears it; dismissing stops
the prompt but keeps it counted; everything is revisitable from *What's new…* in the
project ⋯ menu.

```jsonc
"ide": {
  "news": [
    {
      "id": "one-command-tests",
      "since": "2026-08-12",
      "eyebrow": "Heads up",
      "headline": "Stop guessing which test script works",
      "body": "The wrappers are gone — `just test` runs everything, and your old local alias is the one thing that will still fail today.",
      "glyph": "terminal"
    }
  ]
}
```

| Field | Notes |
|---|---|
| `id` | **Required.** Kebab-case (`^[a-z0-9]+(-[a-z0-9]+)*$`), ≤64 chars, unique in the project. **Never renamed, never reused** — it is what each teammate's read state is stored against; both failures are silent. Namespaced per project before storage, so another repo's identical slug is not a collision. |
| `since` | **Required**, `YYYY-MM-DD`, no default. Shown on the card, and it decides who is too new to need it: a teammate who imported the project after that day never sees it. |
| `eyebrow` | **Required**, 1–24 chars. |
| `headline` | **Required**, 1–44 chars. What a teammate can now *do*, or stop doing. |
| `body` | **Required**, 1–160 chars. One sentence. |
| `glyph` | `terminal` \| `panes` \| `device` \| `inbox`. Default `inbox`. Not the `ide.panes` icon set. |

Rules that are enforced rather than advised:

- **At most 5 live items.** Extras are dropped with a `veld lint` warning.
  **Retiring an item is deleting it.**
- **Only the repo's main checkout is read** — the primary clone, at whatever it has
  checked out. A card drafted in a *worktree* prompts nobody until it lands, and news
  stays silent until somebody pulls on main. A card on a branch in the main clone
  itself IS live; the isolation is per worktree, not per branch.
- **Every item is a change and its date gates it.** There is no evergreen kind:
  standing practice belongs in the repo's docs (point at them with
  `ide.quicklinks`), so a card can never outlive the change it describes.
- Malformed entries are `veld lint` warnings, never load errors.
- The reader can turn the whole channel off (*Settings → General → Show news from
  your projects*), which hides cards without marking them read.

**Write the outcome, not the mechanism** — ✗ "Test wrappers removed", ✓ "Stop
guessing which test script works". If the sentence would read as true to somebody
who will never touch that part of the repo, it describes the change instead of
their day. Ask the user before adding one unless they asked: it is a message
published to their colleagues in their name.

### `ide.panes`

Pane types the project adds to Veld Desktop's dock — the `+` menu, the pane
chooser and the ⌘K palette. Only `type: "terminal"` exists today: a pane that
runs the project's command inside your login shell. A browser tab has no panes,
so none of this applies there.

The chooser renders them as **equal cards in declaration order**, beside veld's plain
Terminal, each showing its `description` under its label — so write descriptions;
they are what tells four agent panes apart. Nothing is promoted and there is no
`primary` flag. An entry whose `requires_bin` is missing keeps its card, disabled,
with the reason on that second line.

| Field | Notes |
|---|---|
| `id` | **Required.** `[A-Za-z0-9_-]`, ≤64 chars, unique in the project. |
| `type` | **Required.** `terminal`. An unknown type is skipped with a lint problem, and the *rest* of `ide.panes` still applies — so a config written for a newer veld costs one pane, not the block. |
| `label` / `description` | Menu text (defaults to `id`) and tooltip. |
| `icon` | An emoji, or a Tabler name from the allowlist (`sparkles`, `robot`, `bolt`, `terminal-2`, …). ASCII means "name", so a typo is a lint problem, not a tab labelled `sparkle`. |
| `requires_bin` | Executable **names** on `PATH`. Never paths, and never a command veld runs — deciding whether to draw a menu item must not execute anything. |
| `argv` / `shell` | **Required**, exactly one. |
| `resume` | `{ argv }` or `{ shell }` — what to run when the pane is restored and its shell is gone. |
| `auto_resume` | Default `false`. Ignored (with a lint problem) without `resume`. |
| `close_on_exit` | Default `true`. Closes the pane on a **clean** exit only; a non-zero exit always keeps it so the error stays readable. Only fires on an exit someone saw, so it never competes with `auto_resume`. Note it also means a deliberate `/exit` never shows the Resume button — set `false` to stop and choose. |
| `allow_terminal_renaming` | Default `false`. Whether the process in the pane may rename its own tab with the terminal title it sets (OSC 0/2). A plain terminal always adopts its title; this opts a config pane in, because its `label` is how you navigate a rail full of agent panes. |

**`${veld.pane.token}` is the whole trick.** Veld mints a UUID the first time a
pane launches, remembers it against that pane in its database, and interpolates
it into the command. Hand it to a tool's session flag and the pane's `resume`
command reopens that exact conversation after a reboot — the shell died, the
conversation did not.

- **The token never leaves the daemon.** Not to the browser, not to the app, not
  into browser storage.
- **A fresh launch always mints a new token**, because `--session-id` is a
  *create*. "Start fresh" therefore means a genuinely new conversation.
- Two panes of the same type in one worktree get **different** tokens. That is
  the case worth the machinery: plain `--continue` would have the second pane
  reopen the first one's conversation.
- **A tool that cannot take an externally-chosen id still works, with no token
  at all.** `codex` is that shape: no launch-time session-id flag, but
  `codex resume --last` continues the most recent session and its resume is
  cwd-filtered unless `--all`, so "most recent" means "in this worktree".
  `{"argv": ["codex"], "resume": {"argv": ["codex","resume","--last"]}}` gets a
  Resume button and `auto_resume` like any other pane. The cost is per-pane
  identity: two such panes in one worktree resume the *same* session, which is
  precisely what the token prevents for tools that cooperate.

**`auto_resume` is narrower than it sounds.** It fires only when a pane *comes
into being* with its shell already gone (app start after a reboot, or after the
detach grace reaped the session). It is never consulted while you are watching
the pane: an exit you saw always waits for a click. A daemon restart or
`veld update` is not a trigger at all, because the shell survives those and the
pane just reattaches. Dragging a pane to another window spawns nothing. The
default is `false` because these commands launch coding agents, and an
unattended one spends money and runs tools with nobody watching.

**A failed `resume` is never retried as a fresh launch** — that would start a new
billable conversation and present as data loss. The pane reports it and offers
*Start fresh* as a separate button.

**`auto_resume` is trust in the repo, not in the command you clicked.** veld
remembers that a pane launched; it does not pin what it ran. The `resume`
command is re-read from `veld.json` at every restore, so a `git pull` that
rewrites it — or flips the flag on a pane started once by hand — changes what
runs unattended on the next app start. This is the only place a config command
runs on app launch rather than on `veld start` or a click.

**Quote your interpolations in a `shell` pane, and prefer `argv`.**
`${veld.branch}` is the one pane variable an outsider can choose (check out
someone's PR branch and the name is theirs), so unquoted in a `shell` command a
branch named `` `curl …` `` executes at pane launch. `argv` interpolates per
element after the array is fixed and cannot change the argument count.

**Variable scope is small** (a pane has no run, no node, no ports); anything else
is a lint problem: `${veld.pane.id}`, `${veld.pane.label}`, `${veld.pane.token}`,
`${veld.worktree}`, `${veld.root}`, `${veld.branch}`, `${veld.project}`,
`${veld.username}`. `VELD_PANE_ID` and `VELD_PANE_TOKEN` are in the environment
too, for a `shell` pane.

**A pane command runs inside your login+interactive shell** (`<shell> -l -i -c
'<command>'`) — the same shell a plain terminal opens, i.e. the one the
`terminal.shell` setting names (default: your login shell) — so it inherits
**everything** your `.zprofile`/`.zshrc`/`.bashrc` export (model tokens,
`JAVA_HOME`, tool paths), not just `PATH`. That is why an agent pane picks up your environment.
The wrapper shell exits with the command's status, so `close_on_exit` and exit
reporting are unaffected.

**There are no pane variants.** Two modes of a tool are two entries. Node
`variants` exist because a node is a graph vertex a preset selects across; a pane
is neither, and one token per pane is what keeps "which conversation is this"
answerable.
