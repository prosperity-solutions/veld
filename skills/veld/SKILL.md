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
compatibility: Requires veld v10.7.0+
allowed-tools: Read, Edit, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "10.7.0"
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

### Active runs
!`veld runs 2>&1`

## CLI

!`veld --help 2>&1`

Run `veld <subcommand> --help` for flags and options.

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
      "command": "PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER $DB_NAME"
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

> **Secrets — prefer `$KEY` over `${output.KEY}`.** `${output.DB_PASS}` is
> interpolated into the command string, so the value is visible in the process
> list (`ps`). `$DB_PASS` is passed as an environment variable and expanded by
> the shell at runtime, so it never appears in argv — the `psql` example above
> leaks nothing. GUI clients are the exception: launching e.g.
> `open -a Postico "postgresql://$DB_USER:$DB_PASS@$DB_HOST:$DB_PORT/$DB_NAME"`
> expands the URL into `open`'s argv regardless. For local dev against ephemeral
> clones that's usually fine; to avoid it, drop the password and let the client
> prompt: `open -a Postico "postgresql://$DB_USER@$DB_HOST:$DB_PORT/$DB_NAME"`.

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
`{ "command": ... }` (resolved on the daemon at share time; keep secrets out of
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

For the full config schema, variables, and node types, see [reference/config.md](reference/config.md).

Quick reference for the two node types:

**`start_server`** — long-running process. Must bind to `${veld.port}`. Requires a readiness probe (`probes.readiness` or legacy `health_check`).
```json
{
  "type": "start_server",
  "command": "npm run dev -- --port ${veld.port}",
  "probes": {
    "readiness": { "type": "http", "path": "/health" },
    "liveness": { "type": "http", "path": "/health", "interval_ms": 5000 }
  },
  "depends_on": { "database": "docker" },
  "env": { "DATABASE_URL": "${nodes.database.DATABASE_URL}" }
}
```

**`command`** — run-to-completion. Emits outputs via `$VELD_OUTPUT_FILE`. Supports liveness probes for long-lived resources (e.g., SSH tunnels).
```json
{
  "type": "command",
  "script": "./scripts/setup.sh",
  "outputs": ["DATABASE_URL"],
  "skip_if": "./scripts/check.sh",
  "probes": {
    "liveness": { "type": "command", "command": "pg_isready", "interval_ms": 5000 }
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
  `veld start e2e --oneshot && deploy`. Ctrl+C aborts and exits `130`.
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
process_count, sampled_at }`) in `--json`. Values are sampled by the daemon
every ~5s, so they're absent (`–` / omitted) until the first sample lands, and
go absent again shortly after a node dies or the daemon stops. The management UI
shows the same figures live with a memory sparkline.

To debug liveness probe failures and recovery decisions:
```sh
veld logs --source internal --name my-feature     # shows probe stderr, recovery attempts
veld logs --source internal -f --name my-feature  # follow mode
```

**Outputs can change after a recovery restart.** When a liveness probe triggers recovery (e.g., SSH tunnel drops and the DB clone restarts), the restarted node may produce new outputs (different port, new password, new connection string). Always re-read outputs with `veld status --outputs` after a restart rather than caching them. If you observe connection failures to a previously-working service, check whether a recovery happened and refresh your outputs.

## Gotchas

- **Readiness probe is required** on every `start_server` variant — use `probes.readiness` (preferred) or legacy `health_check`
- **`skip_if` replaces `verify`** — `verify` still works as an alias but `skip_if` is the canonical name
- **Outputs are volatile** — after a recovery restart, outputs like `DATABASE_URL` may change. Never cache outputs long-term; re-read with `veld status --outputs` when needed
- **`depends_on` needs the variant** — write `"backend": "local"`, not just `"backend"`
- **`${...}` vs `{...}`** — `${veld.port}` in commands/env, `{service}` in URL templates. Mixing them up silently produces wrong values.
- **`outputs` shape differs by type** — object (`{"KEY": "template"}`) for `start_server`, array (`["KEY"]`) for `command`
- **`${veld.port}` is only for `start_server`** — `command` variants don't get an allocated port
- **`--oneshot` needs a `command` node** — the terminal node must run to completion; a `start_server` is rejected. Its exit code becomes veld's exit code; only its logs stream to stdout unless `--all-logs`
- **`setup`/`teardown` are not nodes** — they have no variants, no health checks, no outputs. Only project-level variables (`${veld.name}`, `${veld.root}`, `${veld.run}`) are available, not `${veld.port}` or `${nodes.*}`
- **No default header stripping** — Veld no longer strips `Origin` by default (it used to, for dev-server WS HMR). `Origin` now passes through the local proxy and is rewritten coherently by the gateway. If a Next.js dev server rejects WS HMR, set `allowedDevOrigins` in `next.config.js`; the escape hatch is `"proxy": { "request": { "remove": ["Origin"] } }`. Proxy header rules never apply to direct peer shares (`veld share` without `--web`)
- **Ports are dynamic** (19000–29999) — never hardcode a port in veld.json or dependent config
- **Commands run from veld.json directory**, not your CWD — use `cwd` field if a node needs a different working directory
- **Name resolution** — if `--name` omitted: one run → auto-selects, multiple → prompts, none → errors
- **`--json`** — most commands accept it for machine-readable output, prefer it when parsing results
- **Sharing needs matching setup modes** — both people must have veld installed and be in the *same* mode (both privileged → clean URLs, or both unprivileged → `:18443` in URLs), or the shared URLs won't match
- **Local URL wins on collision** — if the joiner already runs the same environment, their local URL is kept; that shared node is skipped and reported as a warning
- **`--approve manual` vs `first`** — manual (interactive default) needs `veld approve <REQ_ID>` (or the dashboard) per join; first (default with `--json`) auto-pins the first token-valid joiner and rejects the rest
- **Share via the join URL** — `veld share` prints `https://veld.localhost/join#<ticket>` (or `:18443` unprivileged); the recipient opens it in a browser to join, or uses `veld join <ticket>` in a terminal
- **`unshare`/`leave` ids are optional** — omit the id to resolve the sole active share/join; `veld stop` auto-unshares the run's shares and a consumer's join self-tears-down when the tunnel closes
- **Shares are in-memory** — if the daemon stops, shares stop (fail-closed); a ticket alone doesn't grant access without host approval

## Troubleshooting

If something isn't working (WebSocket failures, CSP errors, overlay disappearing, port conflicts, cert warnings), see [reference/troubleshooting.md](reference/troubleshooting.md).
