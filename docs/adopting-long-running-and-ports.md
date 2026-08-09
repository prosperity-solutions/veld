# Long-running nodes, port protocols, and per-port sharing

**You do not have to change anything.** Everything on this page is additive
*within* `schemaVersion: "3"`. There is no v4, no new spelling to adopt by a
deadline, and no config that loads today and stops loading tomorrow.

Read it as a menu, not a checklist — then read [the behaviour
changes](#four-behaviour-changes-worth-a-veld-lint) at the end, which are the
only part that can move under a config you never touch.

If you are coming from `schemaVersion: "1"` or `"2"`, do
[that migration](migrating-to-v3.md) first — it is the one that is mandatory.

## What changed, in one table

| You want | Before | Now |
|---|---|---|
| A supervised process with no port | give it a port it never binds | `"ports": null` |
| A second HTTP port with its own URL | one URL per node, full stop | `{ "port": "auto", "protocol": "http" }` |
| A database port with a name | port number only | `{ "port": 5432, "protocol": "tcp" }` |
| Readiness for a process that binds nothing | no honest option | `{ "type": "settle", "seconds": 3 }` |
| Share one port of a node, not all of it | `share` on the node, all-or-nothing | `share` on the port |

## The fastest way: hand this page to your coding agent

```
Adopt veld's long-running/port features in my veld.json, following
https://github.com/prosperity-solutions/veld/blob/main/docs/adopting-long-running-and-ports.md
Change nothing that already works. Then run `veld lint` and fix everything it reports.
```

`veld lint` is what makes this safe to delegate: it reads the result with the real
parser and reports every problem with a file, line, and rule name.

---

## `start_server` is now spelled `long_running`

Both spellings load, forever, exactly as `bash` still loads as an alias for
`command`. Nothing rewrites your file, and `veld lint` does not flag the old one —
a permanent alias sets no deadline, so there is no rule to write.

```jsonc
"type": "start_server"    // still correct, not deprecated
"type": "long_running"    // canonical — what veld's messages say and what run history records
```

The rename is about what the field *means*. A node's type has only ever decided
**lifecycle** — runs to completion, or stays running. `start_server` named the
common case instead of the contract, and once a long-running node was allowed to
have no ports at all, "server" described the minority of them.

## `"ports": null` — a long-running node that serves nothing

The reason the rename happened. A supervised process with no ports: no
allocation, no `${veld.port}`, no URL, no DNS host, no Caddy route.

```jsonc
"electron": {
  "depends_on": { "web": "dev" },
  "variants": { "dev": {
    "type": "long_running",
    "shell": "electron .",
    "ports": null,
    "env": { "APP_URL": "${nodes.web.url}" },
    "probes": { "readiness": { "type": "settle", "seconds": 5 } }
  }}
}
```

This is the Electron shell, the file watcher, the background compiler — anything
veld should start, keep, report on and stop with the run, but that nobody
connects to. Before, these had to be given a port they never bound, or run
outside veld entirely.

`ports` now has three authorings, and only the middle one is new:

| `ports` | Meaning |
|---|---|
| absent | one auto-allocated `http` port — **unchanged** |
| `null` | no ports at all |
| `{ … }` | that map, merged node → variant per key, `"name": null` erasing one entry |

## `protocol` on a named port

The scalar shorthand is unchanged and is not going anywhere. The long form adds
three fields:

```jsonc
"ports": {
  "http":     "auto",                                   // shorthand — exactly as before
  "admin":    { "port": "auto", "protocol": "http" },   // gets its own hostname
  "postgres": { "port": 5432,   "protocol": "tcp" },    // allocated + exported, never routed
  "debug":    "auto"
}
```

- **`http`** mints a hostname for that port and registers a Caddy route, so it is
  reachable as a URL and has a `${veld.urls.<name>}` family (plus `.hostname`,
  `.host`, `.origin`, `.scheme`, `.port`; `VELD_URL_<NAME>` in the environment;
  `${nodes.<node>.urls.<name>}` across nodes). `${veld.url}` still means the
  primary, permanently.
- **`tcp`** is allocated and exported — `${veld.ports.<name>}`,
  `VELD_PORT_<NAME>` — and deliberately never routed. A raw TCP connection
  carries no hostname for a proxy to match on, and every `*.veld.localhost` name
  already resolves to 127.0.0.1 without veld's help, so `db.myapp.veld.localhost:5432`
  reaches the process with Caddy out of the path entirely.
- **`host`** overrides the `url_template` for one port, replacing it wholesale. It
  takes the same placeholders a `url_template` does — `{service}`, `{variant}`,
  `{run}`, `{project}`, `{branch}`, `{worktree}`, `{username}`, `{hostname}`, with
  `{a ?? b}` for a fallback — and **not** `${vars.…}` or any `${veld.…}`: the
  hostname has to be known before the first var is resolved. `{service}` is the
  one to know about, because on a secondary port it is already `<node>-<port>`.

Every port gets a **hostname**, whatever its protocol — naming and routing are
separate concerns, and `tcp` uses the name without the route. That is what
another node addresses a raw port by:

```jsonc
// db declares { "ports": { "pg": { "port": 5432, "protocol": "tcp" } } }
"api": { "variants": { "dev": {
  "env": { "DATABASE_URL": "postgres://app@${nodes.db.hosts.pg}:${nodes.db.ports.pg}/app" }
}}}
```

`${veld.hosts.<name>}` is the same value for the node's *own* ports. Both are
pre-computed before any node starts, so no `depends_on` edge is needed to read
them — that is unchanged from `${nodes.<node>.url}`.

**Your existing multi-port nodes gain no new URL.** The default is `http` for the
primary port and `tcp` for every other, and that asymmetry is the whole point: it
is what stops a `{"http": "auto", "debug": "auto"}` node from suddenly minting an
HTTPS route in front of its Node inspector the first time it runs on a newer
veld. A secondary port that *should* be a URL has to say so.

They do gain a **name**: naming and routing are separate, so every port now
claims a hostname whether or not anything routes it — see [behaviour change
4](#four-behaviour-changes-worth-a-veld-lint).

One thing to know if you adopt a secondary `http` port: it is served at
`<node>-<port>.…` — `web-admin.dev.veld.localhost`, a sibling of the node's own
hostname rather than a deeper label, so a wildcard cert or dnsmasq rule that
already covers the node covers it too. If that collides with a node genuinely
named `web-admin`, `veld start` refuses before spawning anything and names both
owners; the way out is `host` on the port.

## The `settle` readiness probe

For a long-running node that binds no port. Readiness stays **mandatory** on
`long_running` — `"ports": null` does not exempt a node from proving it started.

```jsonc
"probes": { "readiness": { "type": "settle", "seconds": 3 } }
```

Its claim is deliberately weak and it says so: *the process was still running N
seconds after it was spawned*. That is worth having anyway, because it is raced
against process exit exactly as the port probe is — a command that dies on
startup still fails the run rather than letting dependents start behind a corpse.

Prefer `command` whenever the process publishes something observable — a socket,
a built file, a pid file:

```jsonc
"probes": { "readiness": { "type": "command", "shell": "test -f ./generated/index.ts" } }
```

`settle` is the honest fallback, not the recommendation, and it is readiness
only: as a liveness probe it would report healthy forever, so veld rejects it
there.

`seconds` is named for `settle`, where it is the whole check, but it sets the
settle window for **any** readiness probe on a portless node — that window is
what races the process's own exit, and a `command` probe on a portless node
needs it just as much. It is ignored where the node has a port, because
readiness then waits for the listener instead.

## Consent moved to the port

`share` is now a field on a **port entry**. That is where exposure happens: a node
may contribute its app port and withhold its admin console and its database, and
each of those is a separate decision.

```jsonc
"ports": {
  "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer", "web"] } },
  "admin":    { "port": "auto", "protocol": "http" },                      // never shared
  "postgres": { "port": 5432,   "protocol": "tcp",  "share": { "expose": ["peer"] } }
}
```

**Your existing `share` means exactly what it always meant.** A node-level `share`
is defined as shorthand for *the node's primary port*, and nothing else:

```jsonc
"share": { "expose": ["peer"] }     // === "ports": { "http": { …, "share": { "expose": ["peer"] } } }
```

So adding a `postgres` port to a node that already opts into `peer` shares
nothing new. That is the whole reason the shorthand is defined narrowly rather
than as "every port": the alternative would have let a word written months ago
spread to a database the author added last week.

Two rules to know:

- **`web` requires `http`.** The gateway speaks HTTP/1.1 over the tunnel and a
  browser cannot speak a raw protocol through it, so `"protocol": "tcp"` with
  `"expose": ["web"]` is `web-share-needs-http`, a lint error — refused at
  authoring time rather than silently dropped at share time.
- **A node-level `share` needs a primary port to land on.** On a node with
  `"ports": null`, or one whose ports are all `tcp`, the shorthand has no target,
  and it is `share-without-primary-port` — an error, not a silent no-op. Writing
  "share" and getting nothing, with nothing to say so, is the failure mode this
  whole design exists to prevent.

Sharing a `tcp` port to a **peer** works: `veld join` binds a local listener,
splices it over the tunnel, and prints the address separately from the URLs. The
address is `<the origin's hostname>:<the joiner's local port>` — the hostname
resolves locally because the join mints it in DNS, and the port is the joiner's
own listener, never the origin's number.

---

## Four behaviour changes worth a `veld lint`

These are the only items on this page that can affect a config you do not edit.
The first two are the same fix: a check that could not run used to answer
"healthy", and now answers "no".

1. **A probe that cannot check anything now fails instead of passing.** An
   unknown `type` — `{"type": "htpp"}` — used to mean "always healthy" on both
   the readiness and the liveness path, so a typo was the silent way to turn a
   probe off. So did a `port` or `http` probe on a node with no such port.
   Both are now errors: `unknown-probe-type` and `probe-needs-port` at lint time,
   and a real failure at runtime. If `veld lint` reports one, that probe has
   never checked anything — decide what it should have checked.

   Run `veld lint` *before* restarting the daemon, not after. An environment
   that is already running keeps running across `veld update`, and a *liveness*
   probe of this shape flips from permanently healthy to permanently failing —
   with recovery restarts behind it — the next time the daemon reads it.

2. **A variant that erases its last port no longer gets a fresh one.** A variant
   writing `"ports": { "http": null }` over a node that declared only `http` now
   has zero ports. It used to collapse back to "nothing declared" and silently
   allocate a *new* port, which is the opposite of what the author wrote. If you
   have such a variant and it genuinely wanted a port, declare one; if it wanted
   none, it now needs a `command` or `settle` readiness probe.

3. **A node-level `share` with nowhere to land is now an error.** Previously it
   was accepted and granted nothing. See `share-without-primary-port` above.

4. **Every port claims a hostname, including a `tcp` one.** Naming and routing
   are separate concerns, so `{"http": "auto", "debug": "auto"}` now claims
   `<node>-debug.…` in DNS as well as `<node>.…` — with no route in front of it,
   which is why it gains no URL. Two consequences on an unchanged config: the
   name enters the collision checks, so a run holding a node called `web-debug`
   alongside a node `web` with a port `debug` is refused at `veld start` (naming
   both owners, with per-port `host` as the way out); and on a custom apex domain
   it is a real DNS entry rather than something `*.localhost` resolves for free.

`ambiguous-primary-port` also became *more permissive*: a node whose ports are
**all** explicitly `tcp` legitimately has no primary, and no longer trips it.
Marking exactly one port `"protocol": "http"` answers the question the rule is
asking; marking two does not, and neither does naming a port `http` while
declaring it `"protocol": "tcp"` — a name cannot outvote the protocol beside it.

---

## Where to look next

- A worked config using all of it:
  [`schema/v3/examples/long-running-and-port-protocols.json`](../schema/v3/examples/long-running-and-port-protocols.json)
- The reference: [`docs/configuration.md`](configuration.md) — `ports`, `probes`,
  and *Consent is per port*
- Sharing end to end: [`docs/gateway.md`](gateway.md)
