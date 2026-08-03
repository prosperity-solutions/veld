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
  Covers any `veld` CLI command.
triggers:
  - veld
  - veld.json
  - start the environment
  - show the user
  - get feedback
  - listen for comments
  - wait for feedback
  - let them review
  - preview the UI
  - feedback loop
  - "*.localhost"
compatibility: Requires veld v12.8.0+
allowed-tools: Read, Edit, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "12.8.0"
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

**Opt-in is required.** A service is shareable only if its variant declares
`share.expose` in `veld.json`; `veld share` errors on anything that hasn't opted
in. Add `"share": { "expose": ["peer"] }` to the variant(s) you want to share.

```sh
veld share my-feature                       # print a join URL to send (plus a veld join command)
veld share my-feature --node frontend       # share only specific nodes (repeatable)
veld share my-feature --ttl 3600            # TTL in seconds (default 7200)
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

**Public web sharing** (`veld share --web`): exposes services whose variant has
`web` in `share.expose` to anyone with a browser — no Veld on the viewer's side.
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
— give API nodes `"web": { "access": "link" }`. Web shares default to a 3600s
TTL (peer: 7200s). Web and peer are separate shares with separate
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

**`start_server`** — long-running process. Must bind to `${veld.port}`. Requires a readiness probe (`probes.readiness` or legacy `health_check`).
```jsonc
{
  "type": "start_server",
  "argv": ["npm", "run", "dev", "--", "--port", "${veld.port}"],
  "probes": {
    "readiness": { "type": "http", "path": "/health" },
    "liveness": { "type": "http", "path": "/health", "interval_ms": 5000 }
  },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" }
}
```

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
  "type": "start_server",
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
- The node **must be `command` type** (a `start_server` never exits) **and must
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

Two escape hatches, read from **the daemon's** environment:

> A plain `export VELD_STATS_CMDLINE=off` in your shell does **not** reach them.
> The sampler runs inside the user daemon, which is a launchd LaunchAgent (macOS)
> or a `systemd --user` unit (Linux); neither inherits an interactive shell's
> environment — the same reason veld has to inject `PATH` into daemon-spawned
> commands. Set them where the service can see them, then restart the daemon
> (the values are read once per daemon lifetime):
>
> ```sh
> # macOS — set it for the session, then restart the agent so it picks it up
> launchctl setenv VELD_STATS_CMDLINE off
> launchctl kickstart -k "gui/$(id -u)/dev.veld.daemon"
>
> # Linux
> systemctl --user set-environment VELD_STATS_CMDLINE=off
> systemctl --user restart veld-daemon
> ```
>
> Verify it took effect with `veld stats --processes --json`: with argv capture
> off, every process's `cmd` is `null` while `name` still reports.


| variable | effect |
|---|---|
| `VELD_STATS_MEMORY_DETAIL=off` | Fall back to RSS-only sampling. For a process with a pathological number of memory mappings, where reading `smaps_rollup` is not cheap. `footprint` then equals RSS and every page class reports `null`. |
| `VELD_STATS_CMDLINE=off` | Stop recording each process's argv. The process *name* is still recorded. veld's own rules forbid secrets on a command line because the process table is world-readable — but on macOS argv is restricted to the owning uid, so recording it does move that data into the database and the daemon's localhost API. On by default (a command line is often the only way to tell two `node` children apart); this turns it off. |

`veld status --json` additionally carries `live` (whether the environment
occupies the live run slot), `end_reason`/`end_detail` (populated once the run
has ended), and `ended_at` (also emitted as the deprecated alias `stopped_at`,
for scripts written against the old shape).

To debug liveness probe failures and recovery decisions:
```sh
veld logs --source internal --name my-feature     # shows probe stderr, recovery attempts
veld logs --source internal -f --name my-feature  # follow mode
```

Log sources, and where each kind of output lands:

| `--source` | Contains |
|---|---|
| `server` | Node output — both `start_server` processes and `command` steps (a `docker build`'s progress is here, under that node). Read one node with `--node <name>` |
| `client` | Browser `console.*` from the client-log collector |
| `setup` | Project-level `setup`/`teardown` step output, labelled `setup:<step name>` |
| `internal` | Liveness probe outcomes, recovery decisions |
| `all` (default) | All four, interleaved by timestamp |

Step output is recorded verbatim and never redacted, so a node or step that
echoes a secret from its environment puts it in that run's log. A `command`
step's stdin is `/dev/null` — one that prompts fails on EOF instead of hanging.

**Outputs can change after a recovery restart.** When a liveness probe triggers recovery (e.g., SSH tunnel drops and the DB clone restarts), the restarted node may produce new outputs (different port, new password, new connection string). Always re-read outputs with `veld status --outputs` after a restart rather than caching them. If you observe connection failures to a previously-working service, check whether a recovery happened and refresh your outputs.

## Gotchas

- **Readiness probe is required** on every `start_server` variant — use `probes.readiness` (preferred) or legacy `health_check`
- **`skip_if` replaces `verify`** — `verify` still works as an alias but `skip_if` is the canonical name
- **Outputs are volatile** — after a recovery restart, outputs like `DATABASE_URL` may change. Never cache outputs long-term; re-read with `veld status --outputs` when needed
- **`depends_on` needs the variant** — write `"backend": "local"`, not just `"backend"`
- **"Works in my terminal, fails from the UI" is an environment difference, not a flake** — a run started from the management UI or Veld Desktop is spawned by the daemon, which passes node commands only `PATH` (resolved from the user's login shell, so `npx`/`pg_isready`/version-manager shims *are* found) and not the rest of the shell environment. `veld start` in a terminal passes everything through. A node depending on a variable exported from a shell rc file must declare it in `env` to behave the same both ways
- **`${...}` vs `{...}`** — `${veld.port}` in commands/env, `{service}` in URL templates. Mixing them up silently produces wrong values.
- **`outputs` shape** — a map (`{"KEY": "template"}`) publishes computed values, an array (`["KEY"]`) declares names captured from the node's own output. Both work on both node types now; on a `command` node the map is interpolated *after* the command runs, with its captured outputs in scope
- **`veld lint` is the fast feedback loop** — it reports every semantic problem at once and exits 1 on any error. `veld start` refuses on the same errors, but only one at a time
- **`schemaVersion` must be `"3"`; `command` is gone** — use `argv` (array, spawned directly) or `shell` (string, via `sh -c`), exactly one. A `"1"`/`"2"` config, or a config containing `command`, fails to load — the error names every offending position. veld ships no converter: apply the rules in `docs/migrating-to-v3.md` and verify with `veld lint`
- **`veld.*` is a closed set** — node outputs are `${output.KEY}` / `${nodes.<node>.KEY}`, never `${veld.KEY}`. This changed: `${veld.<OUTPUT>}` used to work inside `on_stop` and now fails, which would silently skip the teardown hook — `veld lint` catches it
- **A built-in that exists is not a built-in that is *populated*** — `veld lint` reports `builtin-not-in-scope` for a real name written where the context does not have it. Availability: run/name/project/root/worktree/branch/username everywhere; `run_id` everywhere except `setup`/`teardown`; `node`/`variant` on nodes only; `port`/`url`/`url.*`/`ports.*` on `start_server` nodes only. A node's `on_stop` has exactly what the node had, URL family included — so `docker rm ${veld.project}-${veld.node}-${veld.run}` in `argv` and in `on_stop` cannot drift
- **A `vars` value is run-scoped, and interpolated** — every `${…}` in a var literal is veld's to resolve, so `${HOME}` is a `var-unresolvable-reference` error (write `$HOME` unbraced, or use `{ "env": "HOME" }`). `${veld.run}`, `${veld.branch}` and friends resolve; `${veld.port}`, `${veld.url}`, `${veld.node}` in one are a lint error, because a var is one value for the whole run. Compose per-node values at the use site. A var backed by a source (`file`/`env`/`argv`/`shell`) is resolved only when the plan reaches it, so a credential-helper var costs nothing on a run that does not use it
- **`${nodes.X.…}` is checked against each preset's plan** — `veld lint` reports `unknown-node-ref` (no such node) and `preset-missing-node-ref` (real node, not in that preset's plan), naming the preset. This is the "works with preset A, dies with preset B" class; a node pulled in transitively by `depends_on` counts as present
- **A node is defined in exactly one file** — with `include` globs, the same node name in two files is an error naming both. `veld config --files` prints the glob → file → node chain when a node seems missing
- **Relative paths resolve from the project root**, never from the file that declares them, even in an included file
- **A preset entry starting with `@` references another preset** — `"ci": ["@core", "e2e:dev"]` is "everything in `core`, plus one more". Selections de-duplicate, and a cycle is an error naming the path. `veld lint` catches a dangling `@ref`, a cycle, and a selection naming a node or variant that does not exist
- **Pick a preset by reading `when_to_use`, not by guessing from its name** — the `veld presets` output above carries each preset's label, intent, and selections. When the user's request doesn't clearly match one, ask rather than starting a 90-second Docker build they didn't want
- **A preset's `key` is stable; its list position is not** — `veld start --preset <key-or-name>` both work, and a pinned `key` keeps meaning the same preset as the config grows. Never tell a user "pick option 3" from a list you sorted yourself; quote the key veld printed
- **`default_preset` is the answer to "just start it"** — a bare `veld start` uses it directly without a TTY, so in an agent shell it starts the project's default instead of failing with "No selections provided". If a project has many presets and no `default_preset`, suggest adding one
- **`depends_on` names must be literal** — no `${...}` in either the node or the variant name; the graph is read before variables exist
- **A `secret` value must not be *substituted* into `argv`/`shell`** — a command line lands in the process table. `secret-in-command` (**error**) fires on the forms veld resolves and only those: `${vars.x}`, `${output.x}`, `${nodes.a.x}`. Deliver the value via `env` or `files` instead. A bare `$SECRET_NAME` is a `secret-shell-expansion` **warning**, not an error: the *shell* expands it, so it leaks only when the expansion becomes another program's argument — `PGPASSWORD=$DB_PASS psql …` and `echo $DB_PASS` are safe, `psql "postgres://u:$DB_PASS@h/db"` is not, because the shell then `execve`s `psql` with the password in *its* argv. Prefer handing the program the variable name. `["docker","run","-e","NAME","img"]` is silent — no `$`, nothing expands
- **`${veld.port}` and `${veld.url}` are only for `start_server`** — a `command` variant gets no allocated port and no route. Reach a server's address as `${nodes.<node>.url}`, which also works from that server's own `env` (the `NEXTAUTH_URL` / `BASE_URL` case)
- **`--oneshot` needs a `command` node** — the terminal node must run to completion; a `start_server` is rejected. Its exit code becomes veld's exit code; only its logs stream to stdout unless `--all-logs`
- **`setup`/`teardown` are not nodes** — they have no variants, no health checks, no outputs. Project-level variables only (`${veld.name}`, `${veld.root}`, `${veld.run}`, `${veld.worktree}`, `${veld.branch}`, `${veld.username}`, `${vars.*}`) — not `${veld.port}`, `${veld.node}`, `${veld.run_id}` (a teardown can outlive the run row), or `${nodes.*}`
- **The root config may be `veld.json` or `veld.jsonc`** — both are read identically. Both in one directory: `veld.json` wins and lint reports `ambiguous-root-config` (an error, so `start` refuses — but a *finding*, so `stop` still works and teardown hooks still run). `veld init` writes `veld.json`. Included files were always glob-matched, so `nodes/*.jsonc` already worked
- **`hooks` and `ui` are reserved** — they parse and are stored but are **not executed** by this version; `veld lint` emits a notice saying so. Every *other* unknown top-level key is an error reported by `veld lint`/`veld start` — not a load failure, so a typo never blocks `veld stop`
- **No default header stripping** — Veld no longer strips `Origin` by default (it used to, for dev-server WS HMR). `Origin` now passes through the local proxy and is rewritten coherently by the gateway. If a Next.js dev server rejects WS HMR, set `allowedDevOrigins` in `next.config.js`; the escape hatch is `"proxy": { "request": { "remove": ["Origin"] } }`. Proxy header rules never apply to direct peer shares (`veld share` without `--web`)
- **Ports are dynamic** (19000–29999) — never hardcode a port in veld.json or dependent config
- **Commands run from veld.json directory**, not your CWD — use `cwd` field if a node needs a different working directory
- **Name resolution** — if `--name` omitted: one run → auto-selects, multiple → prompts, none → errors
- **`veld logs` defaults to the latest run** — after a restart, `veld logs` no longer reaches into the previous generation's lines. Use `--run <id-prefix>` for a specific past run, `-p`/`--previous` for the run before the latest, or `--all-runs` to restore the old interleaved-across-runs behavior
- **`veld logs -f` exits 0 when the run ends** — it no longer hangs forever on a run that crashed or was stopped; it prints history then a stderr note and returns
- **`veld status`/`veld urls` on a stopped environment** — `status` still works and shows the last run's outcome, but hides the URL column (routes are torn down); `urls` errors outright instead of printing dead links
- **`veld urls --json` shape** — `{ "urls": [{node, variant, url}...], "live": bool }` (no longer a bare array; stopped environments add `"ended_at"`); check `.live` first, then read `.urls`
- **`--json`** — most commands accept it for machine-readable output, prefer it when parsing results
- **Sharing needs matching setup modes** — both people must have veld installed and be in the *same* mode (both privileged → clean URLs, or both unprivileged → `:18443` in URLs), or the shared URLs won't match
- **Local URL wins on collision** — if the joiner already runs the same environment, their local URL is kept; that shared node is skipped and reported as a warning
- **`--approve manual` vs `first`** — manual (interactive default) needs `veld approve <REQ_ID>` (or the dashboard) per join; first (default with `--json`) auto-pins the first token-valid joiner and rejects the rest
- **Share via the join URL** — `veld share` prints `https://veld.localhost/join#<ticket>` (or `:18443` unprivileged); the recipient opens it in a browser to join, or uses `veld join <ticket>` in a terminal
- **`unshare`/`leave` ids are optional** — omit the id to resolve the sole active share/join; `veld stop`, `veld restart` **and a `veld start` that replaces a live same-named environment** auto-unshare the run's shares (each mints a new run id, so the old share would point at torn-down ports) and a consumer's join self-tears-down when the tunnel closes. Re-share afterwards if you still need the URL. A share whose run ended some other way (a crash) is listed in both dashboards' top panel as a "share without a run" with an Unshare button, so it never becomes unreachable
- **Shares are in-memory** — if the daemon stops, shares stop (fail-closed); a ticket alone doesn't grant access without host approval
- **Two checkouts can't serve one URL** — the hostname is `{service}.{run}.{project}.localhost` and `{project}` comes from `veld.json`, so two clones/worktrees of one repo running the same environment name mint the *same* URL. `veld start` refuses this with an error naming the other project and its run instead of hijacking the route; start under a different name (`--name <other>`) or stop the other run. Different repos are unaffected — their `{project}` differs
- **Run names are unique per project, not globally** — two repos both checked out on `main` each get an environment called `main`, so a bare name is not an address. `veld share` resolves the run against the project directory you run it from; a name that project doesn't run is a `404` naming where it does run (`cd` there), and from outside any project an ambiguous name is a `409` naming the candidate roots rather than a silent guess. `veld share` requires the run to be **running** — not just started. A stopped environment's stored URLs point at ports that are gone, and a still-starting one would silently share only the services already up, so both are refused (the error says which, and whether to start or wait). A scripted `veld start` then `veld share` must wait for `running`. Same rule in the daemon's HTTP API: `stop`/`restart`/`action`/`logs` take a `project_root` query parameter and `404` on a mismatch.

## Troubleshooting

If something isn't working (WebSocket failures, CSP errors, overlay disappearing, port conflicts, cert warnings), see [reference/troubleshooting.md](reference/troubleshooting.md).
