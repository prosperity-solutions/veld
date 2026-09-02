---
name: veld
description: >
  Orchestrate local dev environments with veld. Use this skill when the user wants to
  start, stop, or restart services; check run status or logs; configure veld.json
  (nodes, services, dependencies, presets, health checks, ports, URL templates); or
  debug environment issues like port conflicts or health-check failures. Also use when
  the user wants to show their UI to a human for review, get visual feedback on
  changes, watch for comments, or run a feedback loop — even if they say
  "let me check," "show the user," "wait for feedback," or "let them review it."
  Also use when they want to customize the Veld IDE for a project — add a status
  badge (pull request state, CI, a deploy tag), a button that opens the worktree in
  an editor, a menu of project actions, or tell the team something changed.
  Covers any `veld` CLI command.
triggers:
  - veld
  - veld.json
  - customize the veld ide
  - ide.extensions
  - status badge in the top bar
  - show pr status in veld
  - open worktree in editor
  - start the environment
  - show the user
  - get feedback
  - listen for comments
  - wait for feedback
  - let them review
  - preview the UI
  - feedback loop
  - "*.localhost"
compatibility: Requires veld v16.63.0+
allowed-tools: Read, Edit, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "16.63.0"
---

# Veld

Veld orchestrates local dev environments. It starts services from `veld.json`, wires dependencies, and gives each service an HTTPS URL like `https://frontend.my-feature.myproject.localhost`.

## Version Check

Installed:
!`veld -V 2>&1`

If the output above shows "command not found" or "No such file", veld is not installed. Guide the user through installation — see [reference/install.md](reference/install.md). Do NOT attempt to run any `veld` commands until it is installed.

If the installed version is older than what `compatibility` requires, tell the user: "This project requires a newer veld. Run `veld update` to upgrade."

## Live State

### Configuration
!`veld config 2>&1`

### Nodes & presets
!`veld nodes 2>&1`
!`veld presets 2>&1`

### Run history
!`veld runs 2>&1`

## CLI

!`veld --help 2>&1`

Run `veld <subcommand> --help` for flags and options.

## Environments and runs

An **environment** is the durable named slot (`--name dev`) — what `veld start`,
`veld stop`, and `veld status` address. A **run** is one execution instance of
an environment: it has an id, a start/end time, and an outcome. Stopping,
crashing, or replacing a run doesn't erase it — it persists as history (last 10
runs per environment, 7 days) along with its logs.

Post-mortem workflow — "why did last night's run die?":

```sh
veld runs --name dev                # list past runs for dev, newest first
veld logs --run a3f8c12             # logs for that specific run (id prefix, like git)
veld runs show a3f8c12              # full detail: node results + the graph snapshot it started with
veld runs diff a3f8c12              # config diff vs its predecessor ("what changed since it worked?")
```

Every run stores a **graph snapshot** at start: raw (pre-interpolation)
command strings, cwd, env variable *names* (never values — they can be
secrets), URL templates, and a hash of veld.json. `veld runs diff <old> <new>`
(or one id, against its predecessor) reports node added/removed and per-field
changes — the fastest answer to "did the config change between the run that
worked and the run that didn't?"

`veld runs --json` gives the machine-readable outcome: `end_reason` is one of
`stopped | failed | crashed | replaced | completed`, and `end_detail` carries
the specifics (`failed_step`, `failed_node`, `exit_code`, `message`) — a
crashed run tells you which node's process died, a failed setup step tells you
which step and its exit code. `crashed` (process died unexpectedly) is now
distinguishable from `stopped` (clean `veld stop`). A `--oneshot` run records
`completed` (exit 0) or `failed` (non-zero).

## Node actions

A node can declare **actions** — shell commands that the CLI and dashboard
expose generically. Veld injects the node's live outputs so the rotating clone
port and password never have to be copied by hand.

```jsonc
// in veld.json, under a node:
"database": {
  "variants": { "dblab": { /* … */ } },
  "actions": [
    {
      "name": "psql",
      "label": "psql",
      "description": "Open a psql shell to the DB clone",
      "requires_outputs": ["DB_HOST", "DB_PORT", "DB_NAME", "DB_USER", "DB_PASS"],
      "shell": "PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER $DB_NAME"
    }
  ]
}
```

Actions are **node-scoped**: a command sees only the outputs of the node it's
attached to. Inside `command` you can reference:

- `$KEY` — the node's live outputs, injected as environment variables and expanded by the shell at runtime
- `${output.KEY}` — the same outputs, interpolated by Veld into the command string before it runs
- `${param.KEY}` — the action's static `parameters`
- `${veld.run}`, `${veld.node}`, `${veld.project}`, `${veld.root}`, `${veld.port}`, `${veld.url}`
- The node's declared `env` (as `$KEY`, below the outputs in precedence) and the veld-owned `VELD_*` variables (`VELD_RUN`, `VELD_ROOT`, `VELD_NODE`, `VELD_VARIANT`, `VELD_PROJECT`, and the port/url/host family) — so an action can read the `CONTAINER_NAME` its node was started with

> **Secrets — `$KEY` is better than `${output.KEY}`, but it is not automatically
> safe.** `${output.DB_PASS}` is interpolated by Veld into the command string, so
> the value is in `ps` for certain — that is a `secret-in-command` **error**.
> `$DB_PASS` is expanded by the *shell* instead, and where the expansion ends up
> decides whether anything leaks:
>
> | Form | Leaks? |
> |---|---|
> | `echo $DB_PASS` (shell builtin, no `execve`) | no |
> | `PGPASSWORD=$DB_PASS psql -U u db` (environment assignment) | no |
> | `psql "postgres://u:$DB_PASS@host/db"` | **yes** — the shell `execve`s `psql` with the expanded value in *its* argv |
> | `open -a Postico "postgresql://$DB_USER:$DB_PASS@$DB_HOST/$DB_NAME"` | **yes**, same reason |
>
> The shell's own `ps` entry shows the literal `$DB_PASS`; the program it then
> runs shows the value. Veld cannot tell the cases apart, so `$KEY` naming a
> secret is a `secret-shell-expansion` **warning**, not an error. Prefer handing
> the program the variable *name* and letting it read the environment
> (`PGPASSWORD=`, `--password-file`, `-e NAME` for a container). For a GUI client,
> drop the password and let it prompt:
> `open -a Postico "postgresql://$DB_USER@$DB_HOST:$DB_PORT/$DB_NAME"`.

Run actions from the CLI:

```sh
veld actions                   # list configured actions
veld action psql               # run it against the only active run
veld action psql --name dev    # target a specific run
veld action psql --node database  # disambiguate when several nodes define it
veld action psql --print       # print the resolved command instead of running it
veld action psql --json        # resolved command as JSON (does not run)
```

`requires_outputs` gates availability: the action only runs (and only appears as
a dashboard button) when the node is running and exposes all listed outputs.

The management dashboard (`veld ui`) shows a button for each available action on
the node's row. Clicking it runs the action server-side via the CLI, so any
credentials never reach the browser.

## Sharing environments (peer-to-peer)

Share a running environment with a colleague so they open the **same** URLs on
their own machine, over an encrypted P2P tunnel (iroh: QUIC + NAT hole-punching
+ n0 relay fallback). No accounts, no Veld-hosted server.

**Opt-in is required, and it is per PORT.** `share` is a field on a port entry —
that is where exposure happens, and it is the only place consent is granted.
`veld share` errors on anything that hasn't opted in, listing candidates as
`node:variant#port`.

```jsonc
"ports": {
  "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer", "web"] } },
  "admin":    { "port": "auto", "protocol": "http" },                     // absent → NEVER shared
  "postgres": { "port": 5432,   "protocol": "tcp",  "share": { "expose": ["peer"] } }
}
```

A node/variant-level `"share": { "expose": ["peer"] }` still works and is
**defined as shorthand for the PRIMARY port's policy** — it never spreads to the
node's other ports, so every pre-existing config means exactly what it meant. A
port's own `share` replaces the shorthand for that port (it does not merge), and
absent is always "not shared": nothing anywhere widens a port that declared none.
So **do not** add a node-level `share` when the user asks to share one port of a
multi-port node — put it on that port. And a node with no primary (all-`tcp`, or
`"ports": null`) has nowhere to fold the shorthand into, so it grants nothing.

**`"expose": ["web"]` requires `"protocol": "http"`** — lint rule
`web-share-needs-http` (error). The gateway speaks HTTP/1.1 and a browser cannot
speak a raw protocol through it; this is what `web` *means*, not a gap to be
lifted later. A database goes to `peer`.

**Raw `tcp` sharing is peer-only, and the joiner's port number is different from
yours.** A `tcp` port opted into `peer` is reproduced on the joining machine as a
bare local TCP port with no Caddy route, so nothing preserves the original number.
`veld join` prints these separately from URLs as `host:port  (tcp)`, and `--json`
puts them in `addresses` (URLs stay in `urls`). Tell the user to use the printed
address — never the origin's port from `veld.json`. There is no `udp`.
A joiner on an older veld refuses the *whole* join when the manifest carries a
tcp endpoint (its `url` is absent and the old wire format required it) — that is
deliberate fail-closed behaviour, not a bug; both sides must upgrade.

```sh
veld share my-feature                       # print a join URL to send (plus a veld join command)
veld share my-feature --node frontend       # narrow to specific nodes (repeatable; never widens consent)
veld share my-feature --ttl 3600            # TTL in seconds, one share; no upper
                                            # bound, floored at 60s
                                            # Defaults: 14400 peer / 7200 web, from
                                            # sharing.peerTtlMinutes / .webTtlMinutes
                                            # (veld settings), overridable per project
                                            # via sharing.peer_ttl_minutes in veld.json
veld share my-feature --approve first        # first|manual|auto (default: manual, or first with --json)
veld join veldshare_… --label alice         # terminal join by ticket; blocks until the host approves
veld shares                                  # list active shares, joins, pending requests
veld approve <REQ_ID>                        # resolve a pending join request
veld deny <REQ_ID>
veld unshare [SHARE_ID]                      # stop hosting a share (id optional → sole active share)
veld leave [JOIN_ID]                         # disconnect from a joined share (id optional → sole active join)
```

`veld share` prints a **join URL** as the primary way to share:
`https://veld.localhost/join#<ticket>` (or `:18443` in unprivileged mode), plus a
`veld join <ticket>` command as an alternative; `--json` adds a `join_url` field.
The recipient **opens the URL in their browser** — it loads their own Veld
dashboard, which connects, waits for host approval, then shows the shared URLs as
clickable links. The ticket is short and constant-size regardless of how many URLs
the run exposes — the manifest is sent over the tunnel after approval, not embedded
in the ticket. You can also share from the **dashboard**: each running run's card
has a **Share** button (which also copies the join link to your clipboard); once
shared it shows **Copy link** / **Copy command** buttons, a live joiner count, an
**auto-accept** toggle, and **Stop sharing**, with pending join requests
(Approve/Deny) and joined shares in a panel.

Two gates protect a share: a capability token in the ticket, plus host approval.
Approval modes: `manual` (host approves each join via the dashboard — which opens
automatically — or `veld approve`), `first` (auto-approve + pin the first
token-valid joiner, reject the rest), `auto` (approve any token-valid joiner).
Traffic is end-to-end encrypted; a relay only forwards sealed bytes and never
sees URLs or content. Relay selection is a config compliance control and must be
opted into explicitly (no implicit default): set `sharing.relays` to `"public"`
or an array of self-hosted relay URLs, else `veld share` is refused. **`"public"`
(n0's relays) is dev/testing only** — rate-limited, best-effort, no guarantees;
production or high-volume sharing should self-host relays (n0's fair-use guidance,
not a license limit; iroh is MIT/Apache-2.0). Config wins
over the legacy `VELD_SHARE_RELAY` env var (read from the daemon's env, not your
shell; not an enforceable floor). The daemon binds one iroh endpoint per relay
policy on demand, so shares on different relays run concurrently. A self-hosted
relay can require an auth token: write the relay as `{ "url": ..., "token": ... }`
where `token` is a literal string or `{ "env": ... }` / `{ "file": ... }` /
`{ "argv": [...] }` / `{ "shell": "..." }` (resolved on the daemon at share time; keep secrets out of
`veld.json` with the non-literal forms). A joiner auto-confines to the relay(s) in
the ticket (a custom-relay share is never joined over public relays); to reach a
token-gated relay it is prompted for the token (browser overlay / `veld join`
terminal; cached per relay in the veld database (`<data_dir>/veld/veld.db`, 0600); wrong
token re-prompts; `--json` returns `needs_relay_token`). The token can also come
from `VELD_SHARE_RELAY` + `VELD_SHARE_RELAY_TOKEN` (sent only when the URL matches
the ticket's relay), or the host sets `sharing.dangerouslyEmbedRelayTokensInTicket:
true` to embed it in the ticket (DANGER: relay secret then rides in every share
link — disposable tokens only). Stopping the run (`veld stop`) auto-unshares its
shares, and a consumer's join self-tears-down when the tunnel closes.

**Public web sharing** (`veld share --web`): exposes the `http` ports that have
`web` in their `share.expose` to anyone with a browser — no Veld on the viewer's side.
Requires `sharing.gateway` in config (a URL, or `{ "url", "token" }` where
`token` is a secret source like relay tokens; the org's self-hosted
`veld-gateway` container serves the public URLs — see docs/gateway.md). The
command prints deterministic `https://<slug>.<gateway-domain>` URLs and —
**by default — a viewer password**: the gateway shows a password page before
serving, a session cookie (12 h, capped at the share TTL) keeps the viewer in.
`--password <pw>` chooses the password (min 8 chars); the printed one-link
(`https://…/#veld-key=…`) carries it in the URL fragment (never hits DNS/logs).
Opt a service out with `"web": { "access": "link" }` in its `share` block (or
`--access link` for config-silent services; explicit config always wins over
the flag) — then the unguessable slug is the only gate, treat the link as a
secret. Multi-service caveat: the session cookie is per public host, so a
password-protected API called cross-origin from the shared frontend gets 401s
— give API nodes `"web": { "access": "link" }`. Web shares default to a 60-minute
TTL and peer shares to 120 (`sharing.webTtlMinutes` / `sharing.peerTtlMinutes`, or the
project's `sharing.web_ttl_minutes` / `peer_ttl_minutes`; `--ttl` overrides both). Web and peer are separate shares with separate
capabilities: `veld unshare` on one never affects the other. The toolbar arc
menu has a top-level **Sharing** item (dot when the page is web-shared) whose
submenu covers **Start/Stop sharing** (toggle a web share for the page's run
from the browser), **Copy public URL** (turn the current page into its public
deep link, path + query + hash preserved), and **Sharing status**. Transport
detail is not shown in the toolbar — `veld shares` prints each live tunnel's
transport (`relayed via <relay>` means throughput is capped by that relay, the
usual cause of slow shares; `direct` is full bandwidth), as does the management
UI.
Fidelity is best-effort:
the app sees its own origin `Host` (Vite allowedHosts pass), public host
rides in `X-Forwarded-Host`, redirects between shared services are rewritten,
WebSockets/HMR work; hard-coded absolute URLs / CORS / OAuth redirect URIs
are the operator's domain setup.

## Editing veld.json

For the full config schema, variables, and node types, see [reference/config.md](reference/config.md) — **read its "Authoring principles" section first.**

Two capabilities under `ide` are worth knowing you have before a user asks for
something you think veld cannot do: [customizing the IDE's top
bar](#customizing-the-ide-for-this-project-ideextensions) with the project's own
badges, buttons and menus, and [telling the team something
changed](#telling-this-projects-team-something-changed-idenews).

The short version, because the wrong instinct here is expensive:

- **Deduplicate values, never structure.** Which keys a node has stays written in
  that node. `rg <ENV_VAR_NAME>` must still find the line that sets it.
- **No inheritance, no mixins, no `extends`, no templates, no loops, no
  conditionals.** Do not reach for patterns from other config systems. If you want
  one, you want a node-level default or a `var`.
- **Prefer `argv` over `shell`.** `argv` is spawned directly, so an interpolated
  value can never change the argument count.
- **A secret is a pointer plus a flag, never custody** — a value source plus
  `secret: true`, delivered via the environment or `files`, never a command line.
- **Run `veld lint` after editing.** It reports every problem at once; `veld start`
  refuses on the same errors.

Comments and trailing commas are legal in every config file. Editors need
`"files.associations": {"veld.json": "jsonc"}` to stop flagging them.

Quick reference for the two node types :

**`long_running`** — a process veld supervises for the life of the run. `type` names the **lifecycle only**; whether the node serves anything is a property of its `ports`. By default it gets one auto-allocated http port and must bind to `${veld.port}`. A readiness probe (`probes.readiness` or legacy `health_check`) is always required. `start_server` is a permanent alias for the same type — old configs load forever, but write `long_running` in anything new.
```jsonc
{
  "type": "long_running",
  "argv": ["npm", "run", "dev", "--", "--port", "${veld.port}"],
  "probes": {
    "readiness": { "type": "http", "path": "/health" },
    "liveness": { "type": "http", "path": "/health", "interval_ms": 5000 }
  },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" }
}
```

**Portless `long_running`** — `"ports": null` supervises a process that serves nothing: an Electron shell, a file watcher, a background compiler. No port, no `${veld.port}`, no URL, no route. Readiness is still mandatory, and an `http`/`port` probe here is a `probe-needs-port` error — use `command` when the process publishes something observable, `settle` otherwise.
```jsonc
{
  "type": "long_running",
  "shell": "electron .",
  "ports": null,
  "depends_on": { "web": "dev" },
  "env": { "APP_URL": "${nodes.web.url}" },
  "probes": { "readiness": { "type": "settle", "seconds": 5 } }
}
```

**Named ports with protocols** — shorthand (`"auto"`, `5432`) or the long form. Default protocol: `http` for the primary port, `tcp` for every other, so existing multi-port configs gain no new hostname. An `http` port gets its own hostname (`api-admin.<run>.<project>.localhost` for a secondary — a sibling of the node's own, not a deeper label) and a Caddy route, plus `${veld.urls.<name>}` / `VELD_URL_<NAME>`. A `tcp` port is allocated and exported (`${veld.ports.<name>}`, `VELD_PORT_<NAME>`) and never routed.
```jsonc
{
  "type": "long_running",
  "shell": "api --port ${veld.port} --admin ${veld.ports.admin}",
  "ports": {
    "http":     "auto",                                  // primary → http
    "admin":    { "port": "auto", "protocol": "http" },  // own hostname
    "postgres": { "port": 5432,   "protocol": "tcp" },
    "debug":    "auto"                                   // secondary → tcp
  },
  "env": { "ADMIN_ORIGIN": "${veld.urls.admin.origin}" },
  "probes": { "readiness": { "type": "port" } }
}
```
Add `"host": "<template>"` to an `http` entry to override the derived hostname — the documented way out of a collision.

**`command`** — run-to-completion. Emits outputs via `$VELD_OUTPUT_FILE`. Supports liveness probes for long-lived resources (e.g., SSH tunnels).
```jsonc
{
  "type": "command",
  "script": "./scripts/setup.sh",
  "outputs": ["DATABASE_URL"],
  "skip_if": { "shell": "./scripts/check.sh" },
  "probes": {
    "liveness": { "type": "command", "argv": ["pg_isready"], "interval_ms": 5000 }
  }
}
```

**Node-level defaults** — declare a field once for every variant of a node
(`schemaVersion: "3"`). Any variant overrides it; `"KEY": null` erases an inherited
map entry. See the merge table in [reference/config.md](reference/config.md#node-level-defaults) — the strategies differ per field.
```jsonc
{
  "type": "long_running",
  "probes": { "readiness": { "type": "http", "path": "/healthz" } },
  "env": { "LOG_LEVEL": "info" },
  "variants": {
    "dev":   { "argv": ["node", "server.js"] },
    "debug": { "argv": ["node", "--inspect", "server.js"], "env": { "LOG_LEVEL": "debug" } }
  }
}
```

**Reverse-proxy header rules** — optional `proxy` block at project/node/variant level (most specific wins; `remove` lists union, `set` maps merge). Applies to the local Caddy proxy **and** the web gateway (`veld share --web`), NOT to direct peer shares (`veld share`). Veld does no header manipulation by default.
```json
{
  "proxy": {
    "request":  { "remove": ["Origin"] },
    "response": { "set": { "X-Frame-Options": "DENY" } }
  }
}
```

**Staleness sensitivity** — `ide.git.stalenessSensitivity` (default `1`) tunes
how urgently the top-bar "update main" pill colours when the main checkout is
behind `origin/<default>`. It scales two thresholds: a commit reads red at ~7
days old (`1`), and ~50 commits reads red; doubling `s` halves both, halving it
doubles them. When a user says the pill is always orange on a freshly cloned
repo or a long-lived release branch, lower it (`0.5`/`0.25`); when worktrees are
being born stale and PRs conflict late, raise it (`2`–`3`). See
[reference/config.md](reference/config.md) and
[configuration.md](docs/configuration.md#idegit-per-project-git-knobs).

## Telling this project's team something changed (`ide.news`)

**A capability worth knowing you have.** If you change how this project runs — the
test command moves, a service needs a new env var, the branch convention changes —
you can leave the team a card in the Veld IDE instead of hoping they read the
commit. Merge it with the change; a teammate pulls; the next time they open the IDE
they are told, once.

**Read this paragraph before the example below.** These interrupt everybody on the
team, exactly once, whether or not it was worth it — so writing one is mostly
"don't". Write one when somebody would otherwise be *bitten* by the change or would
never find it: the command they type daily now does something else, a step they must
take once before their next pull works, a convention that has changed. Never for a
bug fix, a refactor, a dependency bump, a new optional flag, or anything only the
person who made the change would look for. **Ask the user before adding one** unless
they asked for it — it is a message to their colleagues, published in their name.

Once that is settled:

```jsonc
// veld.json, or veld.d/news.jsonc — `ide` may be split across include files
"ide": {
  "news": [
    {
      "id": "one-command-tests",       // kebab-case, ≤64 chars, NEVER renamed or reused
      "since": "2026-08-12",           // required, YYYY-MM-DD, never in the future;
                                       // also decides who sees it
      "eyebrow": "Heads up",           // ≤24 chars
      "headline": "Stop guessing which test script works",   // ≤44 chars
      "body": "The wrappers are gone — `just test` runs everything, and your old local alias is the one thing that will still fail today.",  // ≤160
      "glyph": "terminal"              // terminal | panes | device | inbox (default inbox)
    }
  ]
}
```

**Write the outcome, not the mechanism.** This is the rule agents get wrong by
default, because you have just spent an hour inside the change and the change is
the most available thing in your head. The headline is what a teammate can now
*do, or stop doing* — in their words:

- ✗ *"Test wrappers removed"* — "The `scripts/test-*.sh` wrappers were deleted and
  replaced by a `just test` recipe."
- ✓ *"Stop guessing which test script works"* — "The wrappers are gone — `just
  test` runs everything, and your old local alias is the one thing that will still
  fail today."

The check: if the sentence would still read as true to somebody who will never
touch that part of the repo, it is describing your change instead of their day.
Never open with the project name, a feature name, or a list of the new options —
that is documentation, and the docs already have it.

**Rules with teeth:**

- **Five live items, maximum.** Over the cap, the entries with the **oldest
  `since`** are dropped with a `veld lint` warning naming them — so a card dated
  today survives and you never have to delete a good one to make room. By date, not
  by position, so a backdated entry can be the one dropped. **Retiring an item is deleting it** — when you add one, delete anything
  that has stopped being news.
- **Never rename an id, never reuse a retired one.** The id is what each
  teammate's read state is stored against: a rename re-shows the card to the whole
  team, and reusing a retired slug suppresses the new card for everyone who saw the
  old one. Both fail silently.
- **`since` gates the card.** A teammate who imported the project after that day
  never sees it, so a new hire meets no back-catalogue. There is no evergreen kind:
  standing practice ("how we work in this repo") belongs in the repo's docs, which
  `ide.quicklinks` can point at. Only a *change* goes here.
- **Only the main checkout counts, by default.** Veld reads this from the repo's
  main checkout — the primary clone, at whatever it has checked out — so a card
  drafted in a *worktree* prompts nobody until it lands, and the card is silent
  until somebody pulls on main. A branch in the main clone itself is live: the
  isolation is per worktree, not per branch. The **`news.source`** setting
  (Settings → General, `main` by default) makes this switchable — `worktree`
  unions every checked-out worktree's own `ide.news` instead, for previewing a
  card before it merges.
- **`veld lint` reports every mistake here**; nothing under `ide.news` can fail a
  config load.

See [reference/config.md](reference/config.md) and
[docs/promotions.md](docs/promotions.md).

## Customizing the IDE for this project (`ide.extensions`)

**A capability worth knowing you have.** A project can put its own badges, buttons
and menus in the Veld IDE's top bar: the state of this branch's pull request, a
button that opens the worktree in the user's editor, the deploy tag currently live.
Each one is backed by a command veld runs in that worktree, and **veld never learns
your code host's name** — the command prints a small contract and veld renders it,
so the provider-specific half stays in the repo. A GitHub project ships a script
calling `gh`; a GitLab project ships the same declaration with `glab` behind it.

**Read this before writing one.** The bar already carries around sixteen controls,
and a badge is *permanently* on screen — unlike a notification it never goes away,
so one nobody reads is worse than none: it teaches people to ignore that row. Add a
badge only when somebody looks the thing up several times a day, the answer is short
enough to read without stopping, and it is about *this worktree* (a project-wide
fact belongs in the README). An `action` is much cheaper — it costs nothing when
idle — and inside a `menu` it costs almost nothing, so prefer actions and menus when
in doubt. **Ask the user before adding extensions they did not request**: these are
committed to a shared repo and show up in every teammate's IDE.

Once that is settled:

```jsonc
// veld.json, or veld.d/*.jsonc — `ide` may be split across include files
"ide": {
  "extensions": [
    // A badge. Its command prints one JSON object; veld renders it.
    { "id": "pr", "slot": "topBar", "type": "status", "label": "PR",
      "icon": "git-pull-request",
      "argv": ["scripts/veld/pr-badge.sh"],
      "refresh_seconds": 60,              // default 60, floored at 15
      "requires_bin": ["gh"],             // names on PATH — never for a GUI app
      "when_missing": "hint",             // hint (default) | disable | hide
      "hint": { "text": "Install the GitHub CLI to see this branch's pull request.",
                "href": "https://cli.github.com" } },

    // One control instead of one button per editor. Group at three.
    { "id": "open-in", "slot": "topBar", "type": "menu",
      "label": "Open this worktree in", "icon": "external-link",
      "items": ["vscode", "webstorm"] },

    // No `slot`: declared to be *referenced* — by the menu above, or by a
    // badge's own output. This is how three editors cost one control.
    { "id": "vscode", "type": "action", "label": "VS Code",
      "shell": "command -v code >/dev/null 2>&1 && exec code \"${veld.root}\" || exec open -a \"Visual Studio Code\" \"${veld.root}\"" }
  ]
}
```

The four things that decide whether it works:

1. **The badge's stdout is the contract**, and its tolerances do most of the work:
   non-contract output becomes the text (so `git rev-parse --short HEAD` needs no
   adapter), **exit 0 with no output hides the badge** — use that for "not
   applicable to this worktree" instead of printing `n/a` — and a non-zero exit
   renders it red with your **last stderr line** as the tooltip, so write a real
   message there.
2. **`actions` in the output are ids of declared `action` entries, never commands.**
   Veld resolves them against the config, so a command can *choose* among your
   commands and never contribute one. This is what makes the empty state useful: no
   pull request yet → `{"text":"No PR","actions":[{"id":"create-pr"}]}`.
3. **`open_in` is a question about whose session the page belongs to.** Behind a
   login the developer holds (code host, CI, cloud console, error tracker) →
   `system`, the default, because a pane has its own cookie jar and lands them on a
   sign-in page. Served by the run itself (localhost, staging on the same session, a
   local report) → `pane`. In doubt: are they already signed in to it elsewhere?
4. **`${veld.branch}` is slugified, so it is not a git ref.** `feat/foo` arrives as
   `feat-foo`. `gh pr view "${veld.branch}"` is therefore not an error but a *wrong
   answer* — on any branch with a `/`, a `.` or a capital it reports no pull request
   and offers to create a second one. Use `${veld.branch_raw}` in `argv` instead
   (`"argv": ["gh", "pr", "view", "${veld.branch_raw}"]`). The slugging on
   `${veld.branch}` is deliberate: the branch name belongs to whoever opened the
   pull request you checked out, so a `shell` command interpolating it raw would be
   running their string — and `veld lint` refuses `${veld.branch_raw}` in `shell`
   for the same reason (`git check-ref-format --branch` accepts `foo$(id)` and
   `foo'bar`, so no quoting closes that hole; `argv` closes it because the element
   count is fixed before substitution). A branch starting with `-` is different:
   `argv` has no shell hole there, but the name is still text a flag parser can
   read as an option, so veld omits `${veld.branch_raw}` entirely for one rather
   than hand a command a flag. The scope is otherwise the pane one minus
   `pane.*`: `${veld.root}`, `${veld.worktree}`, `${veld.project}`,
   `${veld.username}`.
5. **`requires_bin` asks `PATH`, so never use it for a GUI application.** `code`,
   `webstorm` and `idea` are launchers installed *separately* from the editor, so
   the check hides the option on a machine where the app is right there. Leave it
   off and let the command fall back to the bundle, as the `vscode` entry above
   does.

A `status` badge can also add `"display": "icon"` to render its glyph **alone**,
with the label kept as the accessible name (a badge is a real `<button>`) and the
tooltip's fallback. Falls back to `text` if there is no glyph to show. Overridable
per value the same way `open_in` is.

Then verify — `veld lint` is the **only** check that a declaration took, because
everything under `ide` is lenient by design (a malformed entry is a warning and a
dropped entry, never a load error):

```sh
veld lint                       # unknown key, dangling reference, bad variable, clamped interval
./scripts/veld/pr-badge.sh      # run it yourself: is stdout one JSON object?
```

**Full authoring guide — worked adapters, the tone and icon vocabularies, grouping,
and the bounds veld enforces (only the visible worktree, one child process across
windows, 20s deadline, 24 extensions max, the `extensions.autoRefresh` switch) — is
in [reference/extensions.md](reference/extensions.md).** Field table:
[reference/config.md](reference/config.md#ideextensions).

## Feedback Loop

For the full feedback workflow, the `next` output schema, thread fields, and the resolve policy, see [reference/feedback.md](reference/feedback.md).

Core pattern — a single agent draining a linear queue, no cursor to track:

```
loop:
  out = veld feedback next --wait --name <run> --json
  → "item"    : fix it, then `veld feedback reply <id> "..."` (or resolve on explicit approval)
  → "timeout" : call next again
  → "ended"   : reviewer clicked "Done" → stop
```

`next` is a pure read (same item until you reply/resolve), so it's safe to
re-run and resumes cleanly after a restart. Reply parks a thread on the human
and drops it off the queue; a new human comment brings it back automatically.

## One-off runs (`--oneshot`) — e2e tests, CI

`veld start <node> --oneshot` runs a `command` node as the run's **terminal
node**: it starts the node's dependencies, runs the node to completion
(streaming its output), then tears the whole environment down in reverse order
and exits with the node's exit code. The local/CI analog of
`docker compose run --rm --abort-on-container-exit`.

```sh
# Bring up e2e's deps (web, api, db), run the suite, tear down, exit w/ its code.
veld start e2e --oneshot
veld start e2e --oneshot --all-logs   # also interleave dependency logs (stderr)
```

- **stdout = only the terminal node's stdout.** Veld's chrome (summary,
  progress NDJSON, teardown lines) and dependency logs all go to **stderr**, so
  an agent/CI capturing stdout gets just the program output. Dep logs are
  recorded (`veld logs --node <dep>`); `--all-logs` interleaves them live.
- Ports are dynamic, so pass dep URLs into the runner via `${nodes.<node>.url}`
  in the command or its `env` (e.g. `"env": { "BASE_URL": "${nodes.web.url}" }`).
- The node **must be `command` type** (a `long_running` never exits) **and must
  terminate** — a server mistyped as `command` hangs the run. Exactly **one**
  selection is required (no multi-node preset); its deps start automatically.
- A non-zero exit (failing tests) becomes veld's own exit code — chain it:
  `veld start e2e --oneshot && deploy`. Ctrl+C aborts and exits `130`. The run
  is recorded with `end_reason: completed` (exit 0) or `failed` (non-zero,
  `end_detail.exit_code` set) — visible via `veld runs --name e2e`.
- Teardown (`on_stop` hooks, project `teardown`) always runs — on completion
  and on Ctrl+C — and runs to completion once started. Deps aren't
  health-monitored while the node runs.

## Reading Outputs

After starting an environment, read node outputs (database URLs, ports, credentials, etc.):

```sh
veld status --outputs --name my-feature        # human-readable
veld status --outputs --json --name my-feature  # machine-readable
```

`veld status` also reports per-node resource usage (CPU % and memory, summed
over each node's whole process tree) — a `CPU`/`MEM` column in the table, and a
top-level `stats` map (`"node:variant"` → `{ cpu_percent, memory_bytes,
process_count, cpu_seconds, memory: { ... }, sampled_at }`) in `--json`. Values
are sampled by the daemon every ~5s, so they're absent (`–` / omitted) until the
first sample lands, and go absent again shortly after a node dies or the daemon
stops. The management UI shows the same figures live with a sparkline.

**Sampling covers the start phase, and who samples depends on the step type.**
Long-lived nodes (`start_server`) are sampled by the daemon from the moment they
spawn — including the whole boot-up window before the node is healthy, which is
where a dev server does most of its allocating. `command` steps (builds,
installs, codegen) are sampled every ~2s by the `veld start` process that runs
them, because a `command` step's process is spawned, awaited and reaped inside
that command and its PID never exists anywhere else. `veld restart` samples the
same way, since it re-runs the same steps. Three consequences worth knowing when
reading the data:

- A `command` node has **no live reading once it finishes** — it stops being
  sampled the moment it exits. Its curve is still in `veld stats --history` and
  in the dashboard chart for the retention window; that history is the only place
  to read a build's peak.
- A step shorter than one sampling interval is represented by the single sample
  taken when it spawned, and a peak between two ticks isn't seen. This is a
  sampler, not kernel accounting. **That first sample's CPU is 0% by
  construction** — CPU is derived from the delta between two refreshes, and there
  has only been one — so for a sub-interval step, memory is the usable figure and
  the 0% is an artefact, not a measurement.
- A `docker build` reports almost nothing: the work happens in
  `dockerd`/`buildkitd`, which are not descendants of the step's process.

### Detailed resources: `veld stats`

`veld status`'s `MEM` column is the tree's **footprint**, not RSS. This matters
when reading the numbers: `memory_bytes` in `--json` is RSS summed over the
tree, which counts every page shared *inside* the tree once per process — a
five-process `npm run dev` reports far more than it occupies. Use
`memory.footprint` (proportional set size on Linux, `phys_footprint` on macOS),
which is the only memory figure that sums correctly over a tree.

`veld stats` is the detailed view:

```sh
veld stats --json --name my-feature                      # breakdown per node
veld stats --processes --json --name my-feature           # + one row per subprocess
veld stats --history --window 1h --json --name my-feature # + bucketed history
veld stats --history --cpu --window 1h --name my-feature   # CPU instead of memory
# Is this node leaking, and which child? A leak is a TREND, so --history is
# what answers it — a single reading only tells you the value is large.
veld stats --node web --memory private_dirty --processes --history --window 1h
```

`--json` gives, per node: `cpu_percent`, `cpu_seconds` (cumulative),
`process_count`, `resident`, and a `memory` object with `footprint`,
`virtual_bytes`, and the page classes `private_clean`/`private_dirty`/
`shared_clean`/`shared_dirty`/`swap`/`wired`. A page class is `null` where the
platform can't measure it — **`null` means "not measurable here", never zero**,
so don't sum or chart it as 0. Linux reports the full split
(`/proc/<pid>/smaps_rollup`); macOS reports totals plus `wired` only. The
top-level `available_metrics` tells you which are usable without probing each
node. `--processes` adds a `processes` array (`pid`, `parent_pid`, `depth`,
`name`, `cmd`, `cpu_percent`, `cpu_seconds`, `memory_bytes`, `memory`) in
pre-order — indent by `depth`, since the parent may be absent (the sampler
records at most 64 processes per node, keeping the heaviest). `--history` adds
`history` buckets averaged server-side; a bucket with no samples is **omitted,
not zero-filled**, so consecutive entries are not necessarily adjacent in time.
Each bucket carries `cpu_percent` and `cpu_peak` as well as the memory fields, so
one request answers both dimensions — `--cpu` only changes which one the terminal
sparkline draws. Use `cpu_peak`/`footprint_peak` when `samples > 1`: a mean over a
wide bucket hides the spike a 5s sample caught.

Which memory number answers which question:

| question | metric |
|---|---|
| what does this node cost the machine? | `footprint` |
| is it leaking? | `private_dirty` **with `--history`** — one reading shows size, only a rising trend shows a leak |
| why does `top` say 4 GB? | `virtual` / `resident` |
| is it thrashing? | `swap` climbing while `resident` is flat |
| which subprocess is it? | `--processes` |
| is it burning CPU, and in bursts? | `cpu_percent` vs `cpu_peak` (`--cpu` to graph it) |

Retention: node totals 24h, per-process rows 2h — the API reports both
(`retention_secs`, `process_retention_secs`) so a client never has to hardcode
them. A by-process view over a window longer than the per-process horizon is
legitimately empty for the older part of the range.

Two escape hatches. **There are two samplers, so each switch has to be set in two
places** — the daemon's service environment (for `start_server` nodes) *and* the
shell you run `veld start` from (for `command` steps, which the CLI samples).
Setting only one leaves the other half capturing.

> A plain `export VELD_STATS_CMDLINE=off` in your shell does **not** reach the
> daemon. It runs as a launchd LaunchAgent (macOS) or a `systemd --user` unit
> (Linux); neither inherits an interactive shell's environment — the same reason
> veld has to inject `PATH` into daemon-spawned commands. And the reverse is just
> as true: `launchctl setenv` / `systemctl --user set-environment` does **not**
> reach an already-running interactive shell, so a terminal-launched `veld start`
> keeps capturing its build steps' argv unless you export it there too. (A run
> started from the dashboard or Veld Desktop inherits the daemon's environment,
> so that half is covered by the service form alone — which is exactly how this
> ends up looking like "works from the UI, not from my terminal".)
>
> ```sh
> # 1. the daemon — macOS: set it, then restart the agent so it picks it up
> launchctl setenv VELD_STATS_CMDLINE off
> launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon"
>
> # 1. the daemon — Linux
> systemctl --user set-environment VELD_STATS_CMDLINE=off
> systemctl --user restart veld-daemon
>
> # 2. the CLI — in your shell profile, so every `veld start` sees it
> export VELD_STATS_CMDLINE=off
> ```
>
> Verify with `veld stats --processes --json` **against a `command` node**, not a
> server one: a server node reads the daemon's setting and will report `cmd:
> null` even when the CLI half is still on. With argv capture off, every
> process's `cmd` is `null` while `name` still reports.


| variable | effect |
|---|---|
| `VELD_STATS_MEMORY_DETAIL=off` | Fall back to RSS-only sampling. For a process with a pathological number of memory mappings, where reading `smaps_rollup` is not cheap. `footprint` then equals RSS and every page class reports `null`. |
| `VELD_STATS_CMDLINE=off` | Stop recording each process's argv. The process *name* is still recorded. veld's own rules forbid secrets on a command line because the process table is world-readable — but on macOS argv is restricted to the owning uid, so recording it does move that data into the database and the daemon's localhost API. On by default (a command line is often the only way to tell two `node` children apart); this turns it off. |

`veld status --json` additionally carries `live` (whether the environment
occupies the live run slot), `end_reason`/`end_detail` (populated once the run
has ended), and `ended_at` (also emitted as the deprecated alias `stopped_at`,
for scripts written against the old shape).

**What a run was started from** lives at
`graph_snapshot.started_from = {preset, selections}` in `veld status --json` and
`veld runs show --json`. `preset` is the config name (absent for an
explicit-selection start), and `selections` is the sorted `node:variant` set that
name expanded to *at start time*. The expansion is stored beside the name because
presets are re-read from disk on every use, so the name alone can be stale.

**Which surface answers "is the live run still what this preset means?"**: the
human `veld status` and `veld runs show` do — they re-expand the preset and print
`Started from: preset \`x\` (redefined since start)`, `(no longer defined)`, or
`(cannot be expanded — see \`veld lint\`)`. The `--json` shapes carry the *record*
(`started_from`) but not that verdict, and `veld presets --json` gives raw
`selections` with `@preset` refs unexpanded — so an agent that wants the
comparison should read the human line, or diff two runs with
`veld runs diff <old> <new> --json`, which reports `origin_changed`. Do not
compare `started_from.selections` against `veld presets --json` output directly:
they are different shapes and will disagree for every preset written without
explicit variants. `started_from` is absent on runs started by a veld older than
this feature.

To debug liveness probe failures and recovery decisions:
```sh
veld logs --source internal --name my-feature     # shows probe stderr, recovery attempts
veld logs --source internal -f --name my-feature  # follow mode
```

Log sources, and where each kind of output lands:

| `--source` | Contains |
|---|---|
| `server` | Node output — both `long_running` processes and `command` steps (a `docker build`'s progress is here, under that node). Read one node with `--node <name>` |
| `client` | Browser `console.*` from the client-log collector |
| `setup` | Project-level `setup`/`teardown` step output, labelled `setup:<step name>` |
| `internal` | Liveness probe **transitions** and recovery decisions — see below |
| `all` (default) | All four, interleaved by timestamp |

**Two `internal`-stream lines that mean something specific.**

- `[log] dropped N line(s) from <node>:<variant>` — Veld could not keep up writing
  that node's output to the database and **lost N lines**. It is on the `internal`
  stream rather than the node's own precisely so it cannot be forged by the
  process being watched: if you see it on `server`, the program printed it. Treat
  a gap in a node's log as explained only when this line is present.
- `database reports as damaged — skipping log retention and page reclaim` from
  `veld gc`, and the matching daemon warning. While the database reads as damaged
  Veld deliberately stops pruning logs, reclaiming pages and emptying the worktree
  trash, so `veld gc` reporting `0` pruned is not a bug — run `veld doctor` and
  `veld backup restore`. The database can grow in the meantime.

**A quiet `internal` stream means healthy, not broken.** The liveness prober logs
*changes*: a probe that starts failing, a node that recovers, a probe that cannot
run, a recovery attempt. A node whose probe simply keeps passing writes one
"probe passing" line an hour and nothing else. It used to write two lines per
node per poll — on one real machine that was 22.7% of every log row in the
database, all of them the same sentence — so do not read a long gap between
`internal` lines as the prober having stopped. If you want to know a node is
being probed right now, read its status (`veld status --json`), not its log
volume.

**Timestamps.** Lines are stored in UTC and printed in the machine's **local** time
zone, so a human reading `veld logs` sees the clock on their wall. `--utc` prints the
stored value verbatim, `--local` forces local, and either overrides the `logs.timeZone`
setting for that command. **`--json` always emits UTC RFC 3339** regardless of the
flags or the setting — parse `timestamp` and convert on your side rather than reaching
for `--local`, which does nothing to JSON output.

Step output is recorded verbatim and never redacted, so a node or step that
echoes a secret from its environment puts it in that run's log. A `command`
step's stdin is `/dev/null` — one that prompts fails on EOF instead of hanging.

**Outputs can change after a recovery restart.** When a liveness probe triggers recovery (e.g., SSH tunnel drops and the DB clone restarts), the restarted node may produce new outputs (different port, new password, new connection string). Always re-read outputs with `veld status --outputs` after a restart rather than caching them. If you observe connection failures to a previously-working service, check whether a recovery happened and refresh your outputs.

## Gotchas

- **Readiness probe is required** on every `long_running` variant — use `probes.readiness` (preferred) or legacy `health_check`. This holds for a **portless** node too (`"ports": null`), where there is no port to probe: use `{ "type": "command", "shell": "…" }` when the process publishes something observable (a socket, a built file, a pid file), and `{ "type": "settle", "seconds": 3 }` otherwise. `settle` claims only "the process was still running after N seconds", but it is raced against process exit, so a command that dies on startup still fails the run. Lint rule: `long-running-needs-readiness`
- **`long_running` is the type; `start_server` is a permanent alias** — same as `bash` for `command`. `type` names the *lifecycle only*; whether a node serves anything is a property of its `ports`. Old configs load forever and veld never rewrites one, but write `long_running` in anything you author
- **`"ports": null` is not the same as omitting `ports`** — absent means one auto http port (the default), `null` means no ports at all: no allocation, no `${veld.port}`, no URL, no route. `{"http": null}` erasing the last entry lands on `null` too
- **A probe with no port to probe now fails instead of passing** — an `http`/`port` probe on a portless node, one naming a port that isn't declared, or one on a **`command` node** (which never gets an allocated port, whatever its `ports` map says) is `probe-needs-port`; an unrecognised `"type"` is `unknown-probe-type`. Both used to silently report healthy forever. `settle` is readiness-only; as a liveness check it is rejected
- **A port name is a DNS label and an env-var suffix** — letters, digits, `-` and `_` only (`port-name`), and two names that collapse to one `VELD_PORT_<NAME>` (`a-b` vs `a_b`, `admin` vs `Admin`) are `port-name-collision`. It becomes `<node>-<port>.…` in DNS, `${veld.ports.<name>}`, and the `#` half of every `node:variant#port` consent label
- **A port's `protocol` decides whether it gets a hostname** — `http` mints a hostname (`<node>` for the primary, `<node>-<port>` for a secondary) plus a Caddy route, `${veld.urls.<name>}` and `VELD_URL_<NAME>`; `tcp` is allocated and exported (`${veld.ports.<name>}`, `VELD_PORT_<NAME>`) and never routed, because a raw TCP connection carries no hostname to route on and `*.veld.localhost` already resolves to 127.0.0.1. Default: `http` for the primary, `tcp` for the rest — so adding a second port never quietly puts HTTPS in front of a debugger port. Override a derived hostname with a per-port `"host": "<template>"`
- **`19899` is veld's own daemon port and a node never gets it** — auto-allocation skips it, and naming it explicitly is refused rather than substituted. A node that bound it would break the daemon's next start, and the symptom would surface later and elsewhere
- **`skip_if` replaces `verify`** — `verify` still works as an alias but `skip_if` is the canonical name
- **Outputs are volatile** — after a recovery restart, outputs like `DATABASE_URL` may change. Never cache outputs long-term; re-read with `veld status --outputs` when needed
- **`depends_on` needs the variant** — write `"backend": "local"`, not just `"backend"`
- **"Works in my terminal, fails from the UI" is an environment difference, not a flake** — a run started from the management UI or Veld Desktop is spawned by the daemon, which passes node commands only `PATH` (resolved from the user's login shell, so `npx`/`pg_isready`/version-manager shims *are* found) and not the rest of the shell environment. `veld start` in a terminal passes everything through. A node depending on a variable exported from a shell rc file must declare it in `env` to behave the same both ways
- **`${...}` vs `{...}`** — `${veld.port}` in commands/env, `{service}` in URL templates. Mixing them up silently produces wrong values.
- **`outputs` shape** — a map (`{"KEY": "template"}`) publishes computed values, an array (`["KEY"]`) declares names captured from the node's own output. Both work on both node types now; on a `command` node the map is interpolated *after* the command runs, with its captured outputs in scope
- **`veld lint` is the fast feedback loop** — it reports every semantic problem at once and exits 1 on any error. `veld start` refuses on the same errors, but only one at a time
- **`schemaVersion` must be `"3"`; `command` is gone** — use `argv` (array, spawned directly) or `shell` (string, via `sh -c`), exactly one. A `"1"`/`"2"` config, or a config containing `command`, fails to load — the error names every offending position. veld ships no converter: apply the rules in `docs/migrating-to-v3.md` and verify with `veld lint`. There is no v4 — `long_running`, port-less nodes, `protocol`/`host` on a port, `settle`, and per-port `share` are all additive within `"3"` (`docs/adopting-long-running-and-ports.md`)
- **`veld.*` is a closed set** — node outputs are `${output.KEY}` / `${nodes.<node>.KEY}`, never `${veld.KEY}`. This changed: `${veld.<OUTPUT>}` used to work inside `on_stop` and now fails, which would silently skip the teardown hook — `veld lint` catches it
- **A built-in that exists is not a built-in that is *populated*** — `veld lint` reports `builtin-not-in-scope` for a real name written where the context does not have it. Availability: run/name/project/root/worktree/branch/username everywhere; `run_id` everywhere except `setup`/`teardown`; `node`/`variant` on nodes only; `port`/`url`/`url.*`/`ports.*`/`hosts.*`/`urls.*` on `long_running` nodes only, and only for the ports that node actually declares — `${veld.urls.<name>}` needs that port to be `protocol: "http"` (use `${veld.hosts.<name>}`, which works for both protocols), and `${veld.port}`/`${veld.url}` need a primary, which a `"ports": null` node and an all-`tcp` node do not have. Lint says all of this by name rather than reporting an unknown built-in. A node's `on_stop` has exactly what the node had, URL family included — so `docker rm ${veld.project}-${veld.node}-${veld.run}` in `argv` and in `on_stop` cannot drift
- **A `vars` value is run-scoped, and interpolated** — every `${…}` in a var literal is veld's to resolve, so `${HOME}` is a `var-unresolvable-reference` error (write `$HOME` unbraced, or use `{ "env": "HOME" }`). `${veld.run}`, `${veld.branch}` and friends resolve; `${veld.port}`, `${veld.url}`, `${veld.node}` in one are a lint error, because a var is one value for the whole run. Compose per-node values at the use site. A var backed by a source (`file`/`env`/`argv`/`shell`) is resolved only when the plan reaches it, so a credential-helper var costs nothing on a run that does not use it
- **A machine-overridable var is answered per machine, never in the config** — `"x": { "machine": { "default": …, "choices": […] } }` declares that the answer is a fact about the developer's computer. Answer it with `veld config set x <value>` (or `--env NAME` / `--shell 'cmd'` to store a pointer rather than a value, which is how a `secret` one is answered); `veld config vars --json` lists every one with its effective value and the scope it came from. **Do not tell a user to edit `veld.json` to change one** — the declaration is shared and committed, the answer is not. A var with no `default` blocks `veld start` before anything spawns: with no TTY (which is where you usually are) it fails with the exact `veld config set` command rather than guessing, so read the error and run that command instead of retrying. `veld start --var NAME=VALUE` answers one for a single run without storing it — the right choice in CI, and the wrong place for a secret, since it lands in the process table
- **Veld's own settings are `veld settings`, and they are not `veld config`** — `veld config` is about a *project's* `veld.json`; `veld settings` is about the machine's Veld: which shell terminals open (`terminal.shell`), where new worktrees land (`worktree.storageMode`/`storageDir`), whether Veld may keep the machine awake (`keepAwake.*`), how long a share link lives (`sharing.peerTtlMinutes` / `sharing.webTtlMinutes` — the one pair a project's `veld.json` can override), and whether the feedback overlay is injected into routed sites at all (`feedback.suppressOverlay`, machine-wide, overrides a project's `features.feedback_overlay`), what is allowed to interrupt the user (`activity.*`, `focus.*`), how often Veld copies its own database (`backup.*`), and whether the desktop app keeps its macOS menu-bar icon (`desktop.menuBarIcon`, on — off leaves the app in the Dock and only costs the ambient run status). `veld settings --json` lists every one with its effective value and whether it was set or is still the default. **Run `veld settings describe <key>` before `veld settings set`**, because the two failure modes are not symmetrical: a number out of range is **clamped, not refused** (`set keepAwake.sharingOnBatteryMinutes 900` stores `480`), so a wrong guess exits 0 and looks like a success — `describe` gives you the range, the default, the allowed values, and any setting this one depends on. Values are written bare (`true`, `60`, `/bin/bash`), not as JSON; a list setting takes either a JSON array or a comma-separated string. `set --json` reports `clamped: true` when it happened. Do not tell a user to click through the settings dialog for something you can set here, and do not edit the database directly
- **`open <file>` in a Veld terminal shows the file in a browser pane** — which is the answer when a user asks you to write them an HTML report or a slide deck and then wants to look at it: write the file, run `open <path>`, and it appears beside the terminal instead of in their other browser. It is served over http from an origin of its own, so ES modules, `fetch` and relative subresources work — none of which they would from a `file://` URL, and building a deck that depends on them is therefore fine. Web pages and PDFs are on by default; **images and plain text (including `.md`) are off**, so `open notes.md` and `open diagram.png` fall through to the system opener until the user turns them on (`veld settings set files.viewPlainText true`, `files.viewImages`), `files.viewPatterns` selects among the kinds veld can display (`reports/*.xml`, `*.log`) and cannot add one, so do not reach for it to view a format veld has no content type for. Anything Veld will not show — a directory, `open .`, two arguments, a flag, a path outside the worktree — reaches the real `open` untouched and **silently**, so a fall-through is not an error you should report or retry. The pane reloads itself when you rewrite the file, so iterating on a deck needs no further action from either of you — and in the desktop app it watches whichever file the pane is *showing*, so a set of decks that link to each other with relative hrefs all reload as you rewrite them (in a browser tab the pane cannot see the navigation, so it keeps watching the file it was opened on). Gitignored paths are included on purpose (`notes/`, scratch directories), so writing a report where the repo tells you to write working documents does not hide it
- **Every registered repo, worktree, lane, layout, run and setting is one SQLite file, and `veld backup` is what stands between the user and losing all of it** — the daemon copies `<data_dir>/veld/veld.db` every `backup.intervalMinutes` (60 by default) into `<data_dir>/veld-backups`, keeping `backup.keep` recent copies plus one per day for `backup.keepDaily` days. Point `backup.dir` somewhere else — an external drive, a synced folder — if off-disk matters; no local default survives the disk failing. `veld backup` lists what is there **and whether each copy can actually be restored**, which is not the same question: the newest file can be the one that copied the damage. `veld backup restore` puts the newest *restorable* one back — checked with SQLite's full integrity check, not the cheap one the listing uses, so the copy it picks is one it will actually accept — and **renames** the database it replaced rather than deleting it, because that file is the only evidence of what went wrong. All three work when the database itself will not open, which is the only day they matter. Nothing is backed up until the database holds something: a fresh install with no repositories, no settings changes and no runs has nothing to lose, and backing it up would only produce an empty copy that outranks the real ones. Take one before anything risky with `veld backup now`. Do not hand-copy `veld.db` with `cp` — a copy of a live SQLite file is not guaranteed consistent, and producing exactly that kind of unopenable file is what this exists to prevent
- **A damaged database is silent, so veld probes for it — and `Db::open()` succeeding is not health** — most of a corrupted SQLite file keeps working, because only the statements that touch the damaged page fail. A real incident ran for 17 hours this way: one page of `pane_layouts` went bad, 440 identical `database disk image is malformed` lines went into the daemon log, **not one backup was written in all that time**, and the only visible symptom was one project labelled `unavailable` in the IDE. So the daemon runs SQLite's `quick_check` against the live file every five minutes (~15 ms), `veld doctor`'s database row runs it too and reports `Database DAMAGED` with what SQLite said (it used to report `Database OK` for any file that merely *opened*), and the IDE shows an undismissable banner naming the fault plus a *Restore newest backup…* button. `POST /api/db-health/restore` refuses unless a fault is recorded **and** a human confirms in a native OS dialog — the IDE shares an origin with any page a veld run serves, so the CSRF header is not a barrier there and replacing the database is not something a page may do alone; on a headless machine it refuses and points at `veld backup restore`. `GET /api/db-health` is the machine-readable form — `database.state` is `"ok"`/`"corrupt"`/`"io"`, `backups.state` is `"ok"`/`"failing"`/`"overdue"`/`"off"`/`"unknown"`, and `restore.candidate` is the copy a restore would actually put back. **Treat any `database.state` you do not recognise as a fault**, not as health: the field is an open set and a newer daemon can name a fault an older client has never heard of
- **"Backups are failing" is answered by attempts, not by file age** — every attempt records its outcome including the one that cannot open the database (the arm whose silence hid the incident above), so a broken schedule shows up within an interval rather than after the 12 hours `veld doctor`'s generous staleness rule waits. `veld backup` prints a warning line **above** the table when nothing has been written for two intervals, and reports it as `overdue` in `--json`. A table full of `ok` rows is not evidence that anything is still writing them — that is exactly what the incident's machine showed
- **A repository being unavailable and the database being damaged are different things** — `repository unavailable` in the top bar means the checkout is gone from disk or git cannot read it, and nothing else. It used to also fire when the daemon's reconciling *write* failed, which is how a damaged page took the start/stop controls away from a perfectly healthy repo. If a user reports that label, check the checkout first; the database has its own banner and its own endpoint
- **`${nodes.X.…}` is checked against each preset's plan** — `veld lint` reports `unknown-node-ref` (no such node) and `preset-missing-node-ref` (real node, not in that preset's plan), naming the preset. This is the "works with preset A, dies with preset B" class; a node pulled in transitively by `depends_on` counts as present
- **A node is defined in exactly one file** — with `include` globs, the same node name in two files is an error naming both. `veld config --files` prints the glob → file → node chain when a node seems missing
- **Relative paths resolve from the project root**, never from the file that declares them, even in an included file
- **A preset entry starting with `@` references another preset** — `"ci": ["@core", "e2e:dev"]` is "everything in `core`, plus one more". Selections de-duplicate, and a cycle is an error naming the path. `veld lint` catches a dangling `@ref`, a cycle, and a selection naming a node or variant that does not exist
- **Pick a preset by reading `when_to_use`, not by guessing from its name** — the `veld presets` output above carries each preset's label, intent, and selections. When the user's request doesn't clearly match one, ask rather than starting a 90-second Docker build they didn't want
- **A preset's `key` is stable; its list position is not** — `veld start --preset <key-or-name>` both work, and a pinned `key` keeps meaning the same preset as the config grows. Never tell a user "pick option 3" from a list you sorted yourself; quote the key veld printed
- **`default_preset` is the answer to "just start it"** — a bare `veld start` uses it directly without a TTY, so in an agent shell it starts the project's default instead of failing with "No selections provided". If a project has many presets and no `default_preset`, suggest adding one
- **`depends_on` names must be literal** — no `${...}` in either the node or the variant name; the graph is read before variables exist
- **A `secret` value must not be *substituted* into `argv`/`shell`** — a command line lands in the process table. `secret-in-command` (**error**) fires on the forms veld resolves and only those: `${vars.x}`, `${output.x}`, `${nodes.a.x}`. Deliver the value via `env` or `files` instead. A bare `$SECRET_NAME` is a `secret-shell-expansion` **warning**, not an error: the *shell* expands it, so it leaks only when the expansion becomes another program's argument — `PGPASSWORD=$DB_PASS psql …` and `echo $DB_PASS` are safe, `psql "postgres://u:$DB_PASS@h/db"` is not, because the shell then `execve`s `psql` with the password in *its* argv. Prefer handing the program the variable name. `["docker","run","-e","NAME","img"]` is silent — no `$`, nothing expands
- **`${veld.port}` and `${veld.url}` are only for a `long_running` node with ports** — a `command` variant and a `"ports": null` variant both get no allocated port and no route. Reach a server's address as `${nodes.<node>.url}`, which also works from that server's own `env` (the `NEXTAUTH_URL` / `BASE_URL` case), and a non-primary http port as `${nodes.<node>.urls.<port>}`
- **`--oneshot` needs a `command` node** — the terminal node must run to completion; a `long_running` is rejected. Its exit code becomes veld's exit code; only its logs stream to stdout unless `--all-logs`
- **`setup`/`teardown` are not nodes** — they have no variants, no health checks, no outputs. Project-level variables only (`${veld.name}`, `${veld.root}`, `${veld.run}`, `${veld.worktree}`, `${veld.branch}`, `${veld.username}`, `${vars.*}`) — not `${veld.port}`, `${veld.node}`, `${veld.run_id}` (a teardown can outlive the run row), or `${nodes.*}`
- **The root config may be `veld.json` or `veld.jsonc`** — both are read identically. Both in one directory: `veld.json` wins and lint reports `ambiguous-root-config` (an error, so `start` refuses — but a *finding*, so `stop` still works and teardown hooks still run). `veld init` writes `veld.json`. Included files were always glob-matched, so `nodes/*.jsonc` already worked
- **`hooks` is reserved**, and so is every key under **`ide`** except `quicklinks`, `permissions`, `externalOrigins` and `panes` — the reserved ones parse and are stored but are **not executed or rendered** by this version; `veld lint` emits a notice naming them. (`ide` was spelled `ui` until it gained a meaning; the old spelling now errors as an unknown top-level key, and the message names the rename.)
- **`ide.permissions` origins need a `*.` wildcard for veld's own URLs.** They are `{service}.{run}.{project}.localhost`, so the **run name is in the hostname** and a pinned host matches exactly one run — write `https://*.veld.localhost:*` — the host wildcard matches any depth of subdomain, and the `:*` is needed because an unprivileged install serves on 18443 while an omitted port means 443. `*.com`-style wildcards over a single label are refused, `*.x` does not match `x` itself, and an omitted port means the scheme's *default* port, not any port. A user's own answer always beats the config, and malformed rules are dropped with a lint warning rather than failing the load — so `veld lint` is the check that a rule actually took. Full rules: `reference/config.md`
- **`ide.panes` adds pane types to Veld Desktop's dock** — `{ id, type: "terminal", label, icon, requires_bin, argv|shell, resume, auto_resume, allow_terminal_renaming }`. `requires_bin` is executable *names* looked up on PATH (never paths, never a command veld runs to decide). `${veld.pane.token}` is a UUID veld mints per pane and remembers in its database — pass it to a tool's session flag (`claude --session-id`) and the pane's `resume` command (`claude --resume`) picks that conversation back up after a reboot. **The token never leaves the daemon.** A fresh launch always mints a new one, so "start fresh" really is a new conversation. A tool that will not accept an id needs no token: `codex` pairs `{"argv":["codex"]}` with `{"resume":{"argv":["codex","resume","--last"]}}`, trading per-pane identity (two such panes resume the same session) for the same Resume button. **A pane command runs inside your login+interactive shell** (`<shell> -l -i -c '<command>'`, where `<shell>` is the `terminal.shell` setting — default: your login shell) — the same shell a plain terminal opens — so it inherits everything `.zprofile`/`.zshrc` export (model tokens, tool paths), not just the injected `PATH`. The wrapper shell exits with the command's status, so `close_on_exit` is unaffected The pane chooser shows every declared pane as an **equal card in declaration order** beside veld's plain Terminal, each with its `description` under the label — so always write a `description`; it is what distinguishes four agent panes. Nothing is promoted, there is no `primary` flag, and an unavailable pane keeps its card with the missing binary named on that line
- **`auto_resume` only fires when a pane comes into being** — app start with the shell already gone. Never while you are watching: an exit you saw always waits for a click, whatever the config says. It defaults to `false` because these commands launch coding agents, and a failed `resume` is never retried as a fresh launch. **`auto_resume` is trust in the repo, not in the command you clicked** — the `resume` command is re-read from `veld.json` at every restore, so a `git pull` changes what runs unattended; it is the only place a config command runs on app launch rather than on `veld start` or a click. In a `shell` pane, quote interpolations and prefer `argv`: `${veld.branch}` is attacker-choosable via a PR branch name. Need the unslugified name? `${veld.branch_raw}`, `argv` only — `veld lint` refuses it in `shell` (a branch can legally be `foo$(id)` or `foo'bar`, so no quoting is safe there). Pane commands see a *small* scope: `${veld.pane.id|label|token}`, `${veld.worktree}`, `${veld.root}`, `${veld.branch}`, `${veld.branch_raw}`, `${veld.project}`, `${veld.username}` — anything else is a lint problem. There are no pane *variants*; two modes are two entries. `close_on_exit` (default **true**) closes a pane on a *clean* exit only — a non-zero exit always keeps it so the error is readable, and it only fires on an exit someone saw, so it never competes with `auto_resume`
- **`ide.extensions` badges are a *contract*, not an integration — and three things about them surprise people.** A `status` command prints `{ text, tone, icon, tooltip, href, open_in, actions }` on stdout and veld renders it, so veld never learns a code host's name. (1) **Output that is not the contract becomes the badge text**, first line only, so `git rev-parse --short HEAD` is already a working badge; **exit 0 with no output hides the badge** ("not applicable here"); **a non-zero exit renders it red with your last stderr line as the tooltip** — so write a real message to stderr rather than swallowing errors. (2) A badge's `actions` are **ids of declared `action` entries, never commands**: veld resolves each against the on-disk config, so a running command chooses among declared commands and cannot introduce one. (3) **`open_in` defaults to `system`** — the *opposite* of `ide.quicklinks` — because a badge's link is normally behind a login the developer holds and a pane has its own cookie jar; only write it to say `pane`. Also: **never put `requires_bin` on something with a GUI** (`code`/`webstorm`/`idea` are launchers installed separately from the editor, so a PATH check hides the option on a machine that has the app). **Declarations come from the project's main checkout by default** — the `extensions.source` setting (`main` by default) — so every worktree sees whatever has reached main, regardless of when it was cloned; commands still execute in the worktree you're looking at. Set it to `worktree` to restore the original per-branch behaviour: declarations come from the checked-out worktree, testable before merging (an extension keeps no persisted state), with the flip side that checking out somebody else's branch runs their badge commands — `extensions.autoRefresh` is the lever for reviewing untrusted ones regardless of source. `ide.news` follows the same `main`-by-default shape via its own `news.source` setting, for the same underlying reason. Full authoring guide, worked adapters and the bounds veld enforces: [reference/extensions.md](reference/extensions.md)
- **`ide.externalOrigins` is the exempt list for terminal URLs, not a block list.** A URL a Veld terminal produces (clicked in the output, or opened by a program in the shell via `$BROWSER`) becomes an embedded browser pane; an origin listed here goes to the user's real browser instead, because a pane has its own cookie jar and an SSO flow in one starts from scratch. Same origin grammar as `ide.permissions`, same lint treatment. It is **unioned** with the user's `browser.externalOrigins` setting — a project cannot remove a user's entry, and cannot turn the feature off (that is the user's `terminal.openUrlsInApp`)
- **Every *other* unknown top-level key is an error** reported by `veld lint`/`veld start` — not a load failure, so a typo never blocks `veld stop`
- **No default header stripping** — Veld no longer strips `Origin` by default (it used to, for dev-server WS HMR). `Origin` now passes through the local proxy and is rewritten coherently by the gateway. If a Next.js dev server rejects WS HMR, set `allowedDevOrigins` in `next.config.js`; the escape hatch is `"proxy": { "request": { "remove": ["Origin"] } }`. Proxy header rules never apply to direct peer shares (`veld share` without `--web`)
- **Ports are dynamic** (19000–29999) — never hardcode a port in veld.json or dependent config
- **Commands run from veld.json directory**, not your CWD — use `cwd` field if a node needs a different working directory
- **Name resolution** — if `--name` omitted: one run → auto-selects, multiple → prompts, none → errors
- **One directory can hold several live environments, and that is a supported state** — `veld start --preset api --name api` beside `veld start --name web` in the same project root gives two independent runs with their own nodes, ports, hostnames (the run name is in the hostname), logs and stats. This is the shape an agent creates by starting a preset in a project the human is also running, so pass `--name` explicitly rather than relying on resolution, and say which environment you started. Veld Desktop shows a **run selector** in its top bar: one control per window naming the bound environment, with `1/2` when there are live siblings. ▶ is a toggle bound to that run (it re-runs an ended one under its own name); the dropdown's **Start another run** entry is what creates a *second* environment while one is live, and it shows the name it will use (`dev-2`) first. The list holds **live** environments only — ended ones sit behind a "Show N ended" disclosure, or appear outright when nothing is live, since run history is Runs mode's job
- **Terminals open the shell `terminal.shell` names** (*Settings → Terminal → Shell* in `/ide`) — `"auto"` by default, meaning the user's login shell. Someone whose integrations live in `~/.bashrc` while `$SHELL` is zsh sets an absolute path here; `GET /api/shells` lists what the machine has. It governs terminal panes, config-declared `ide.panes` commands, and the shell veld consults for the user's `PATH`. A terminal already open keeps the shell it started with, so a change needs a new pane, not a restart. It also decides how `open`/`xdg-open` are caught: zsh via `ZDOTDIR`, bash via posix-mode `$ENV` (probed — macOS's bash 3.2 ignores it), nothing for fish/nu, which keep `$BROWSER` plus the `$VELD_SHIM_DIR` one-liner. `veld doctor` names the mechanism actually in force
- **A worktree's rail row carries one activity glyph for its terminals** — the *worktree inbox*, worst-state-first: an ear (waiting for you, pulsing) > a triangle (a command failed) > a tick (finished) > a spinner (running). Not a count, and clicking it does nothing but select the worktree; *Mark read* is in the row's context menu, and the pane's own tab carries a dot so you can tell which one. Two producers, each with an independent off switch, both default on. `terminal.shellIntegration` registers OSC 133 hooks in the shell veld opens, so a command that ends in an unwatched pane reports `finished`/`failed` with its real exit code (zsh, and bash ≥ 4.4 — macOS's `/bin/bash` is 3.2 and cannot carry it); a shell at a prompt and a watcher that has not ended never count. `terminal.agentIntegration` puts an agent wrapper (`claude`, `codex`, `pi`) on the terminal's `PATH` that hands the real binary a throwaway hook configuration — an ephemeral `--settings` file for Claude, a literal `-c notify=[...]` override for Codex, an ephemeral extension module loaded via `-e` for Pi (Pi has no settings key or config override to reach for, only an extension API) — because **none of the three tools' output says whether it is working or waiting** — measured for Claude: OSC 0, OSC 8, OSC 9;4 progress and OSC 52, and nothing about its state. **Only the session's own state is reported, never a sub-agent's** — a Claude sub-agent starting, finishing or failing files no event and does not clear the pane's `working`, because it is not the user's to act on and not the pane's state; the one exception is a sub-agent asking for input, which still reports `blocked`, since that *is* the user's to answer. Also, the only start-of-turn signal either tool has that doesn't fire per tool call is Claude's `UserPromptSubmit`, so a turn that resumes without a prompt (an agent picking up work after a background command) has no `working` signal until the next prompt — the spinner is missing, no event is wrong. Never merges into `~/.claude/settings.json`, writes a `.claude/` into the project, touches `~/.codex/config.toml`, or touches `~/.pi/agent/settings.json`/`~/.pi/agent/extensions/`, and passes through untouched for `claude mcp`/`update`/`-p`, `codex exec`, `pi install`/`update`/`-p`, or a user's own settings/config/extension flag (`codex resume`/`fork` count as interactive, not as subcommands to leave alone). Codex reports much less than Claude, **on purpose**: it has a richer `hooks` system too, mirroring Claude's own event names, but using it means either an interactive hook-trust prompt on first use or a flag that waives trust for every configured hook, not just this one — so veld uses the older `notify` hook instead, which fires on exactly one event, end-of-turn. A Codex pane therefore has no `waiting`/`blocked` signal at all — launched and finished are the whole story — and its `-c notify=[...]` *replaces* rather than merges a user's own configured `notify`. **Pi has no `waiting`/`blocked` signal either, but for a different reason**: it has no permission prompts or plan mode to observe at all, not a deliberate trade. It does get a run-level `agent_start`/`agent_settled` pair, so — unlike Codex — a Pi pane's `working` spinner is accurate for the whole run, not just at launch; `-e` is documented repeatable and additive, so it never replaces anything of the user's the way Codex's `-c` does. Its `session_shutdown` event tells `quit` apart from `/new`/`/resume`/`/fork` (which continue in the same pane and correctly report nothing), so a Pi pane's session-end tracking has none of Codex's shell-integration-fallback caveat. Unread events survive a page reload (per-tab `sessionStorage`); the live "running" state deliberately does not, since a reload is when it stops being knowable. Closing the tab loses that worktree's news entirely, because the marker parser is the terminal in the page. Looking at a pane reads it, typing in it reads it, *Mark read* in the row's context menu clears the worktree. `activity.showWorking` (**on** by default — accurate for shell commands, Claude and Pi, whose wrappers report `ready` at launch and then a real `working` signal when work actually starts, but blank for an agent veld has no integration for, and — for Codex specifically — blank for the entire working phase too, since nothing reports a turn *starting*; on by default because a missing spinner is a missing hint rather than a wrong one) governs the spinner; five `activity.notify*` keys decide which events also raise a **system notification** (command finished/failed, agent waiting, agent finished, and a program's OSC 9), and each fires as an in-app toast when Veld is focused or an OS banner when it is not — never both. It works in `ide.panes` panes as well as plain terminals — a pane runs under `-c`, which never prompts, so it gets finished/failed from the process exit rather than from OSC 133, and veld prepends its shim dir to `PATH` inside the pane command so the agent wrapper is still reachable. All of these live under *Settings → Activity*
- **Focus mode is the top-bar toggle (search ↔ settings) that silences the notification half of the above** — `focus.enabled` (**off**), with three independent sub-switches under *Settings → Activity → Focus mode*, all **on** once the master switch is: suppress the terminal bell, suppress the in-app toast, suppress the OS banner. Reaches only the background-activity channel described above (`notifyTerminal`/`showSystemNotification`/the bell) — never a toast that is feedback for something you clicked yourself. A suppressed toast or banner is discarded, not queued — both are built from the rail's own OSC 9/133 marks, so the rail glyph is still the permanent record. The bell is the exception: a plain terminal BEL never reached the rail in the first place, so silencing it leaves nothing behind, on or off. "OS banner" means Veld's own notification path (Veld Desktop's native notification, or a browser tab's Web Notification), not a system-wide Do Not Disturb.
- **Keep-awake sits beside search in the top bar, and has no keep-awake subcommand** — a coffee-cup menu holding the machine awake for 30m/1h/2h/4h/8h or with no limit, so a long build or an agent left running is not killed by the laptop suspending. Machine-wide, not per run: every window and every client sees one state. There is deliberately **no `veld` subcommand for it**, so an agent asked to "keep this machine awake" has nothing to call — say so rather than reaching for `caffeinate` yourself, since a bare `caffeinate` you spawn dies with your shell and is invisible to the menu. Coverage is not uniform and the menu states which case a machine is in: on Linux one unprivileged `systemd-inhibit` lock covers everything including a closed lid on battery; on macOS `caffeinate -s -i` stops at mains power, and the battery/lid-closed case needs `veld setup privileged`, because the only lever is `pmset disablesleep` and that needs root. That setting is durable rather than an assertion, so the helper takes it on a renewed lease, only ever reverts a value veld itself set, and hands it back when the daemon stops renewing — `veld doctor`'s helper row reports `holding sleep off` when a lease is live. **Sharing arms it by itself**, which is the part worth knowing before you diagnose a machine that will not sleep: starting a share holds the machine awake with no button pressed — `keepAwake.sharingOnPower` (on, 120 minutes) and `keepAwake.sharingOnBattery` (on, 30 minutes), two settings because the answer differs by power source. The cap is a ceiling per sharing session, not a countdown: the deadline is `min(cap, latest share expiry)`, and unplugging or plugging in restarts the allowance for the source you moved to. **Which of the two binds is worth checking before you believe a number.** With defaults everywhere the *cap* is the shorter one — a 2h mains cap under a 4h peer link holds the machine for two hours and the link then outlives it — so the cup's countdown is the cap's. The share's expiry binds instead when it is shorter: a `--ttl`, a project that shortens the TTL fields, or a cap raised past the link's life. The cup, the sharing panel, `veld share` and `veld doctor` all name the sharing as the deadline when that is the case, so a countdown that does not match the keep-awake setting says why. The link's life is its own pair of settings — `sharing.peerTtlMinutes` / `sharing.webTtlMinutes` (`veld settings`), overridable per project by `sharing.peer_ttl_minutes` / `sharing.web_ttl_minutes` in `veld.json`, and per share by `veld share --ttl`. **An automatic hold never asks the privileged helper for anything** — on mains it covers a shut lid for free (`caffeinate -s` is AC-only, so no root is involved), and on battery it holds idle sleep and nothing else, so a laptop that goes in a bag mid-share still sleeps. `keepAwake.manualOnBattery` (on) is a per-machine off switch for the privileged half; off means veld never writes `pmset disablesleep` there on any path. Two read-only CLI surfaces exist and neither is a keep-awake command: `veld share` prints an `Awake:` line beside `Stop:` (and an `Expires:` line for the *link's* own lifetime, which is the number to check when a `--ttl` or a `veld.json` TTL was clamped), and `veld doctor` reports a `Keep awake:` row naming the reason and the time left — that row, not the helper's, is the one that answers "why won't this Mac sleep" for an ordinary unprivileged hold. **The five `keepAwake.*` settings are readable and writable from the CLI** — `veld settings keepAwake` lists them with their effective values, and `veld settings set keepAwake.sharingOnBatteryMinutes 60` changes one. That is the surface to reach for; the dashboard's *Settings → Keep awake* edits the same values. Note the asymmetry that remains: there is still no command that *takes* a hold, only ones that read and configure the policy — so "keep this machine awake" is still something to say you cannot do, while "is keep-awake on, and for how long" now has an answer
- **A browser refusing a veld URL with `ERR_CERT_DATE_INVALID` is Caddy's certificate, and `veld doctor` now names it** — the `Certificate:` row and the `HTTPS certificate` check read the leaf Caddy is actually serving, which is a different question from the `CA:` row above it (whether this machine trusts the authority). Certificates last a week and Caddy renews them itself; when that stops, the helper restarts Caddy within about two minutes (two consecutive bad probes, one minute apart, before it drops every live connection) and gives up after three fruitless restarts, because a config reload cannot renew a certificate Caddy already holds in its cache. Don't reach for a fix before reading the row — and if it stays expired, the reason is in `~/.local/lib/veld/caddy-data/caddy.log`, whose path doctor prints
- **`veld logs` defaults to the latest run** — after a restart, `veld logs` no longer reaches into the previous generation's lines. Use `--run <id-prefix>` for a specific past run, `-p`/`--previous` for the run before the latest, or `--all-runs` to restore the old interleaved-across-runs behavior
- **`veld logs -f` exits 0 when the run ends** — it no longer hangs forever on a run that crashed or was stopped; it prints history then a stderr note and returns
- **`veld logs` prints local time, `veld logs --json` prints UTC** — human output is converted to the machine's zone (`--utc` opts out, `--local` forces it, both override the `logs.timeZone` setting); the `timestamp` field in `--json` is always UTC so a parser has one format to handle. Don't compare a `--json` timestamp against a human-output one without converting
- **`veld status`/`veld urls` on a stopped environment** — `status` still works and shows the last run's outcome, but hides the URL column (routes are torn down); `urls` errors outright instead of printing dead links
- **`veld urls --json` shape** — `{ "urls": [{node, variant, url}...], "live": bool }` (no longer a bare array; stopped environments add `"ended_at"`); check `.live` first, then read `.urls`
- **`--json`** — most commands accept it for machine-readable output, prefer it when parsing results
- **Sharing consent is per port, and the node-level `share` is only the primary's** — `share` on a node/variant is *defined* as shorthand for the primary port's policy and never spreads; a port with no `share` of its own is never shared, and a port that has one ignores the shorthand entirely. To share a second port, write `share` on **that port**. A node with no primary (all-`tcp`, or `"ports": null`) folds the shorthand into nothing. `veld share` lists what it excluded and why, per `node:variant#port`
- **`web` needs `protocol: "http"`; raw `tcp` is peer-only** — `"expose": ["web"]` on a tcp port is the `web-share-needs-http` lint error, because the gateway serves HTTP and a browser can't speak a raw protocol through it. That is permanent. Share a database or debugger port with `"expose": ["peer"]` instead. There is no `udp` protocol and no udp sharing
- **A joined `tcp` endpoint's port is the joiner's, not the origin's** — nothing routes a raw port, so the local listener binds whatever it gets. `veld join` prints these as `host:port  (tcp)` below the URLs (`addresses` in `--json`, separate from `urls`); quote *that* address to the user, never the port from the origin's `veld.json`. A joiner running an older veld refuses the whole join rather than mis-reproducing a tcp endpoint as an HTTP route — fail-closed by design, so upgrade both sides
- **Sharing needs matching setup modes** — both people must have veld installed and be in the *same* mode (both privileged → clean URLs, or both unprivileged → `:18443` in URLs), or the shared URLs won't match
- **Local URL wins on collision** — if the joiner already runs the same environment, their local URL is kept; that shared node is skipped and reported as a warning
- **`--approve manual` vs `first`** — manual (interactive default) needs `veld approve <REQ_ID>` (or the dashboard) per join; first (default with `--json`) auto-pins the first token-valid joiner and rejects the rest
- **Share via the join URL** — `veld share` prints `https://veld.localhost/join#<ticket>` (or `:18443` unprivileged); the recipient opens it in a browser to join, or uses `veld join <ticket>` in a terminal
- **`unshare`/`leave` ids are optional** — omit the id to resolve the sole active share/join; `veld stop`, `veld restart` **and a `veld start` that replaces a live same-named environment** auto-unshare the run's shares (each mints a new run id, so the old share would point at torn-down ports) and a consumer's join self-tears-down when the tunnel closes. Re-share afterwards if you still need the URL. A share whose run ended some other way (a crash) is listed in both dashboards' top panel as a "share without a run" with an Unshare button, so it never becomes unreachable
- **Shares are in-memory** — if the daemon stops, shares stop (fail-closed); a ticket alone doesn't grant access without host approval
- **Two checkouts can't serve one URL** — the hostname is `{service}.{run}.{project}.localhost` and `{project}` comes from `veld.json`, so two clones/worktrees of one repo running the same environment name mint the *same* URL. `veld start` refuses this with an error naming the other project and its run instead of hijacking the route; start under a different name (`--name <other>`) or stop the other run. Different repos are unaffected — their `{project}` differs
- **Run names are unique per project, not globally** — two repos both checked out on `main` each get an environment called `main`, so a bare name is not an address. `veld share` resolves the run against the project directory you run it from; a name that project doesn't run is a `404` naming where it does run (`cd` there), and from outside any project an ambiguous name is a `409` naming the candidate roots rather than a silent guess. `veld share` requires the run to be **running** — not just started. A stopped environment's stored URLs point at ports that are gone, and a still-starting one would silently share only the services already up, so both are refused (the error says which, and whether to start or wait). A scripted `veld start` then `veld share` must wait for `running`. Same rule in the daemon's HTTP API: `stop`/`restart`/`action`/`logs` take a `project_root` query parameter and `404` on a mismatch.

- **Exit code 75 means "an update is running", not "your command is wrong"** — `veld update` holds a single-flight lock while it replaces the binaries and restarts the daemon and helper, and nearly every other veld command refuses with `EX_TEMPFAIL` (75) for the one to four minutes that takes. The message names who is updating and which phase they are on; `veld update --status --json` is the machine-readable form, and `veld doctor` reports it too. **Retry rather than diagnose** — this is the one veld exit code that means "try again shortly". `update`, `doctor`, `version`, `config`, `lint`, `init` and `desktop status` keep working throughout, as do the internal log sinks a running environment depends on. A lock is cleared automatically when its holder dies or after 30 minutes without progress; `veld update --force` takes over sooner. **An update never prompts for a password** — the privileged helper restarts itself over its own socket, so a `veld update` driven by an agent does not hang on `sudo`; and `veld update --verbose` shows the install script's raw output when you need to see why one failed

- **Veld Desktop is optional, and an agent-driven update never downloads it** — the Mac app is a recorded answer in `~/.veld/desktop.json`, not a default. A run with nobody to ask (no TTY, or `VELD_NON_INTERACTIVE`) keeps an app that is *already* installed in step and does **not** fetch one that isn't, so a scripted `veld update` on a machine that has never had the app costs no ~113 MB download and no prompt. `veld desktop install` records "yes", `veld desktop uninstall` records "no" (and works with nothing installed — that is how an orchestrator-only machine opts out permanently), and `veld desktop status --json` reports it as `preference`: `"wanted"`, `"unwanted"`, or `null` for never asked. `VELD_DESKTOP=0` skips the app for one run and records nothing. A deliberately stale app is therefore a legitimate state — `veld doctor` says so on the row rather than telling you to update it

## Troubleshooting

If something isn't working (WebSocket failures, CSP errors, overlay disappearing, port conflicts, cert warnings), see [reference/troubleshooting.md](reference/troubleshooting.md).
