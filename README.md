# Veld

> This thing is 100% vibe coded with [Claude Code](https://claude.com/claude-code).

Local development environment orchestrator for monorepos. Spin up fully configured preview environments with real HTTPS URLs from a single command.

```sh
veld start frontend:local --name my-feature
# => https://frontend.my-feature.myproject.localhost
# => https://backend.my-feature.myproject.localhost
```

No port numbers. No manual wiring. Just clean, stable, human-readable URLs.

## Features

- **No port numbers** — work with stable HTTPS URLs instead of `localhost:3847`
- **Dependency graph** — resolves node dependencies, parallelizes startup, reverse-order teardown
- **TLS by default** — Caddy's internal CA handles TLS termination, auto-trusted during setup
- **Health checks** — readiness probes (two-phase: TCP port + HTTP/command) gate startup; liveness probes detect failures after startup (e.g., dropped SSH tunnels)
- **Automatic recovery** — when liveness probes detect failure, the environment is automatically restarted (configurable failure threshold and max recovery attempts)
- **Multiple variants** — same node, different behaviors (local server, Docker, remote URL)
- **Named runs** — multiple environments coexist; re-running by name is idempotent
- **Setup / teardown** — project-level lifecycle steps that gate startup (check Docker, create networks) and clean up after stop
- **Presets** — named shortcuts for common selections (`fullstack`, `ui-only`)
- **Variable interpolation** — `${veld.port}`, `${nodes.backend.url}`, git branch, etc.
- **Structured output** — all commands support `--json` for scripting and CI
- **Browser dashboard** — management UI at `https://veld.localhost` with service health, logs, search, stop/restart
- **Client-side logs** — captures browser `console.log/warn/error`, exceptions, and promise rejections; view with `veld logs --source client`
- **Internal logs** — liveness probe outcomes (with stderr), recovery decisions, health state transitions; view with `veld logs --source internal`
- **Peer-to-peer sharing** — share a running environment with a colleague over an encrypted P2P tunnel (`veld share`); they open the same URLs on their own machine. Services opt in explicitly in config, and relays are configurable (public or self-hosted) for compliance. No accounts, no Veld-hosted server.
- **Public web sharing** — expose a service to someone *without* Veld (`veld share --web`): a self-hosted gateway (`veld-gateway`, one Docker container) mints a real public URL anyone can open in a browser. The overlay's **Copy public URL** action translates your current page (path + query preserved) into the public link.

## Install

Download the latest release for your platform:

```sh
curl -fsSL https://veld.oss.life.li/get | bash
```

This detects your OS and architecture, downloads the latest release, and installs:
- `veld` to `~/.local/bin/`
- `veld-helper` and `veld-daemon` to `~/.local/lib/veld/`

No sudo required. Ensure `~/.local/bin` is on your `PATH`.

Setup is optional — commands auto-bootstrap on first use with HTTPS on port 18443.
For the full experience with clean URLs (no port numbers), run the one-time privileged setup:

```sh
veld setup privileged
```

This registers system services and binds ports 80/443, so your URLs are just
`https://frontend.my-feature.myproject.localhost` — no `:18443` suffix. Requires
sudo once; you won't be asked again.

Alternatively, `veld setup unprivileged` does a no-sudo setup with HTTPS on port 18443.
Both modes support the full feature set with one difference: unprivileged mode uses port 18443 in URLs and only supports `.localhost` domains (RFC 6761). Custom apex domains (e.g. `{service}.mycompany.dev`) require `veld setup privileged` since they need `/etc/hosts` or dnsmasq management.

To install a specific version: `VELD_VERSION=1.0.0 curl -fsSL https://veld.oss.life.li/get | bash`

In containers or CI images without a working launchd/systemd, `veld setup` fails
service registration on purpose (an unmanaged helper dies permanently on the next
update). Set `VELD_ALLOW_UNMANAGED_HELPER=1` to let setup direct-spawn the helper
anyway — it will not survive reboots or binary updates.

### Build from source

```sh
git clone https://github.com/prosperity-solutions/veld.git
cd veld
cargo build --release
# Binaries: target/release/veld, target/release/veld-helper, target/release/veld-daemon
```

## Quick start

1. Create a `veld.json` in your project root:

```json
{
  "$schema": "https://veld.oss.life.li/schema/v2/veld.schema.json",
  "schemaVersion": "2",
  "name": "myproject",
  "url_template": "{service}.{run}.{project}.localhost",
  "nodes": {
    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "npm run dev -- --port ${veld.port}",
          "probes": { "readiness": { "type": "http", "path": "/health", "timeout_seconds": 30 } }
        }
      }
    },
    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "npm run dev -- --port ${veld.port}",
          "probes": { "readiness": { "type": "http", "path": "/", "timeout_seconds": 30 } },
          "depends_on": { "backend": "local" },
          "env": { "NEXT_PUBLIC_API_URL": "${nodes.backend.url}" }
        }
      }
    }
  }
}
```

2. Start the environment:

```sh
veld start frontend:local --name dev
```

Veld resolves the dependency graph (backend first, then frontend), allocates ports, starts processes, runs health checks, configures Caddy routes, and gives you HTTPS URLs.

3. Check status:

```sh
veld status --name dev
veld urls --name dev
```

4. Stop:

```sh
veld stop --name dev
```

## CLI reference

| Command | Description |
|---------|-------------|
| `veld start [NODE:VARIANT...] --name <n>` | Start an environment |
| `veld stop [--name <n>] [--all]` | Stop a running environment |
| `veld restart [--name <n>]` | Restart an environment |
| `veld status [--name <n>] [--json]` | Show run status |
| `veld urls [--name <n>] [--json]` | Show URLs for a run |
| `veld action <name> [--name <n>] [--node <n>] [--print] [--json]` | Run a node-defined action (e.g. open the database in a GUI client); `--print` emits the resolved command |
| `veld actions [--json]` | List the actions defined across the project's nodes |
| `veld logs [--name <n>] [--node <n>] [--lines <n>] [-f] [--since <d>] [--source <s>] [-s <term>] [-C <n>]` | View logs (`-f` follow, `-s` search, `-C` context lines) |
| `veld graph [NODE:VARIANT...]` | Print dependency graph |
| `veld nodes` | List all nodes and variants |
| `veld presets` | List presets |
| `veld runs` | List all runs |
| `veld feedback next [--wait] [--name <n>] [--json]` | Get the next feedback item to work on (agent-facing; pure read, no cursor) |
| `veld feedback reply <thread-id> "<msg>"` | Reply to a feedback thread (parks it on the reviewer) |
| `veld feedback resolve <thread-id>` | Resolve a thread (agent-facing; only on explicit approval) |
| `veld feedback ask "<msg>"` | Ask the reviewer a question |
| `veld feedback threads [--name <n>]` | List feedback threads |
| `veld share [RUN] [--node <n>]... [--ttl <secs>] [--approve <first\|manual\|auto>] [--web] [--access <password\|link>] [--password <pw>] [--json]` | Share a running env over an encrypted P2P tunnel; prints a join URL (and `veld join` command). `--web`: publish the `web`-opted services via the configured gateway and print public URLs — password-protected by default (`--access link` to opt config-silent nodes out, `--password` to choose the password) |
| `veld join <TICKET> [--label <n>] [--no-remember] [--json]` | Join a shared env by ticket; materializes the shared URLs locally (blocks until approved). `--no-remember`: don't cache a relay auth token entered at the prompt |
| `veld shares [--json]` | List active shares, joins, and pending join requests. Each live tunnel shows its transport: `direct` (full bandwidth) or `relayed via <relay>` (throughput limited by the relay) plus RTT |
| `veld approve <REQ_ID> [--json]` | Approve a pending join request |
| `veld deny <REQ_ID> [--json]` | Deny a pending join request |
| `veld unshare [SHARE_ID] [--json]` | Stop hosting a share (defaults to the sole active share) |
| `veld leave [JOIN_ID] [--json]` | Disconnect from a joined share (defaults to the sole active join) |
| `veld ui` | Open the management dashboard in the browser |
| `veld gc` | Clean up stale state and logs |
| `veld setup [unprivileged\|privileged]` | One-time system setup |
| `veld init` | Create a new veld.json |

## Configuration

### Step types

- **`start_server`** — long-running process. Veld allocates a port (`${veld.port}`), starts the process, and runs health checks.
- **`command`** — runs a command to completion. Can emit outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE` (preferred) or via `VELD_OUTPUT key=value` on stdout (legacy, discouraged). Optional `skip_if` command for idempotency.

### Setup & teardown

Project-level lifecycle steps that run outside the dependency graph. Setup steps run sequentially before any node starts; teardown steps run after all nodes stop.

```json
{
  "setup": [
    { "name": "docker", "command": "docker info", "failureMessage": "Docker must be running" },
    { "name": "veld-network", "command": "docker network create ${veld.name}-net 2>/dev/null || true" }
  ],
  "teardown": [
    { "name": "veld-network", "command": "docker network rm ${veld.name}-net 2>/dev/null || true" }
  ]
}
```

Setup steps that fail (non-zero exit) abort startup with the `failureMessage` if provided. Teardown is best-effort — failures are logged but don't block stop. Commands support shell env vars and project-level Veld variables: `${veld.name}`, `${veld.project}`, `${veld.root}`, `${veld.run}`.

### Health checks

```json
{ "type": "http", "path": "/health", "expect_status": 200, "timeout_seconds": 30 }
{ "type": "port", "timeout_seconds": 10 }
{ "type": "command", "command": "curl -sf http://localhost:${veld.port}/ready" }
```

### URL template variables

| Variable | Description |
|----------|-------------|
| `{service}` | Node name |
| `{run}` | Run name |
| `{project}` | Project name from veld.json |
| `{branch}` | Current git branch (slugified) |
| `{worktree}` | Worktree directory name (slugified) |
| `{username}` | OS username |
| `{hostname}` | Machine hostname |

Fallback operator: `{branch ?? run}` uses the first non-empty value.

### Client-side log levels

Veld automatically captures browser `console.log`, `console.warn`, `console.error`, unhandled exceptions, and promise rejections from `start_server` nodes. Configure which levels to capture with `client_log_levels` at the project, node, or variant level (most specific wins):

```json
"client_log_levels": ["log", "warn", "error"]
```

Valid levels: `"log"`, `"warn"`, `"error"`, `"info"`, `"debug"`. Default: `["log", "warn", "error"]`. Unhandled exceptions are always captured regardless of this setting.

View client logs with `veld logs --source client` or filter by source in the management UI.

### Feature toggles

Control which Veld capabilities are injected into `start_server` nodes' HTML responses with `features` at the project, node, or variant level (most specific wins):

```json
"features": {
  "feedback_overlay": false,
  "client_logs": true
}
```

Available features: `feedback_overlay` (toolbar/comments UI), `client_logs` (browser log collector), `inject` (auto-inject bootstrap scripts). All default to `true`.

### Environment variables

Declare `env` at the project, node, or variant level. Variables cascade: variant > node > project (per-key merge, most specific wins). Values support `${...}` variable substitution.

```json
{
  "env": { "FEATURE_FLAG": "1" },
  "nodes": {
    "api": {
      "env": { "LOG_LEVEL": "debug" },
      "variants": {
        "local": {
          "env": { "PORT": "${veld.port}" }
        }
      }
    }
  }
}
```

### Variable interpolation

Commands, env values, and output templates support `${veld.port}`, `${veld.url}`, `${veld.run}`, `${veld.root}`, `${nodes.backend.url}`, `${nodes.backend.port}`, etc.

For `start_server` nodes, individual URL location pieces are also available (mirrors the Web URL API):

| Variable | Example | Description |
|----------|---------|-------------|
| `${veld.url.hostname}` | `app.my-run.proj.localhost` | DNS name only |
| `${veld.url.host}` | `app.my-run.proj.localhost:19443` | hostname:port (omits port if 443) |
| `${veld.url.origin}` | `https://app.my-run.proj.localhost:19443` | scheme + host (same as `${veld.url}`) |
| `${veld.url.scheme}` | `https` | Protocol scheme |
| `${veld.url.port}` | `19443` | HTTPS port (note: `${veld.port}` is the backend bind port) |

These are also available as cross-node references: `${nodes.backend.url.hostname}`, `${nodes.backend.url.host}`, etc.

Ports and URLs for all `start_server` nodes are pre-computed before execution, so `${nodes.X.url}` works everywhere — even across nodes with no dependency relationship. Frontend can reference backend's URL and backend can reference frontend's URL without a cycle.

## Architecture

Three binaries work together:

- **`veld`** — CLI. Parses commands, orchestrates environments, displays output.
- **`veld-helper`** — manages DNS entries and Caddy routes via a minimal Unix socket API. Runs as either a system daemon (privileged, for clean URLs on ports 80/443) or a user process (unprivileged, on port 18443).
- **`veld-daemon`** — user-space daemon. Monitors health, runs garbage collection, broadcasts state updates.

Caddy handles HTTPS termination and reverse proxying. Its internal CA is trusted in the system keychain during setup so browsers accept certificates without warnings.

### Storage

All CLI/daemon state — run state, the project registry, service logs, feedback threads and screenshots, relay auth tokens — lives in one SQLite database at `<data_dir>/veld/veld.db` (macOS: `~/Library/Application Support/veld/veld.db`; Linux: `~/.local/share/veld/veld.db`; override with `VELD_DB_PATH`). The file is `0600` (it holds secrets) and runs in WAL mode, so the CLI, daemon, and detached log writers read and write concurrently without file locking. The schema is versioned (`PRAGMA user_version`) and migrates forward automatically on upgrade — a CLI update never orphans or stops running environments because the data shape changed. A database created by a *newer* veld is refused with an error instead of being modified.

## Extensions

### Management UI

Veld includes a browser-based dashboard at `https://veld.localhost` (or `https://veld.localhost:18443` in unprivileged mode). It shows all environments with:

- **Services tab** — nodes with health status indicators, URLs with copy/open, variant, PID
- **Logs tab** — terminal viewer with search + highlighting, context lines (grep -C), auto-scroll, node filter, source filter (server/client/all)
- **Stop/Restart** — control environments directly from the browser
- **Sharing** — start/stop peer shares and public web shares per run; each live tunnel shows its transport (`direct`, or `relayed via <relay>` — throughput capped by the relay) so slow shares are diagnosable at a glance

Open it with `veld ui` or visit the URL directly.

### Hammerspoon (macOS)

If you use [Hammerspoon](https://www.hammerspoon.org/), Veld ships a menu bar widget that shows running environments at a glance.

```sh
veld setup hammerspoon
```

This installs the `Veld.spoon` into `~/.hammerspoon/Spoons/` and offers to patch your `init.lua` to load it automatically. No sudo required. The menu includes an "Open Management UI" item for quick access to the browser dashboard.

Check extension status with `veld doctor`.

## Sharing

Share a running environment with a colleague so they open the **same** URLs on their own machine, over an encrypted peer-to-peer tunnel (iroh: QUIC with NAT hole-punching and an n0 relay fallback). No accounts, no Veld-hosted server.

**Services must opt in.** A service is shareable only if its variant declares `share.expose` in `veld.json` — `veld share` refuses to expose anything that hasn't. This makes what leaves your machine explicit and auditable:

```json
{
  "sharing": { "relays": "public" },
  "nodes": {
    "frontend": {
      "variants": {
        "local": { "type": "start_server", "command": "npm run dev", "share": { "expose": ["peer"] } }
      }
    }
  }
}
```

```sh
veld share my-feature        # prints a join URL to send (plus a veld join command)
```

`veld share` prints a **join URL** as the primary way to share: `https://veld.localhost/join#<ticket>` (or `:18443` in unprivileged mode). Send it to a colleague — they **open it in their browser**, which loads their own Veld dashboard, connects, waits for your approval, then shows the shared URLs as clickable links. The `veld join <ticket>` command is an alternative for a terminal-only join, and `--json` output includes a `join_url` field. The ticket is short and constant-size no matter how many URLs the run exposes — the URL manifest is sent over the tunnel after approval, not embedded in the ticket.

You can also drive sharing from the **dashboard**: each running run's card has a **Share** button (which also copies the join link to your clipboard); once shared it exposes **Copy link** / **Copy command** buttons, a live joiner count, an **auto-accept** toggle, and **Stop sharing**. Pending join requests (Approve/Deny) and joined shares appear in a panel.

Both people must have Veld installed and be in the **same setup mode** — both privileged (clean URLs) or both unprivileged (`:18443` in URLs) — so the URLs match. The consumer's own Caddy issues a locally-trusted cert, so there's no cert warning.

Two gates protect a share: a capability token embedded in the ticket, plus host approval. Approval modes (`--approve`):

- **`manual`** (default for interactive use) — you approve each join via the dashboard (which opens automatically) or `veld approve <REQ_ID>`
- **`first`** (default with `--json`) — auto-approves and pins the first token-valid joiner, rejecting the rest
- **`auto`** — approves any token-valid joiner

Traffic is end-to-end encrypted between the two velds; a relay only forwards sealed bytes and never sees your URLs or content. Relay selection is a config-level compliance control and **must be opted into explicitly** — there is no implicit default, so nothing is ever routed over n0's public relays by accident. Set `sharing.relays` to `"public"` (n0's public relays) or to an array of self-hosted relay URLs to confine share traffic to relays you run (a single Docker container). `veld share` refuses to share a run whose config sets no relay. Config wins over the legacy `VELD_SHARE_RELAY` env var — which is read from the daemon's environment, not your shell, and is not an enforceable floor (a project setting `"relays": "public"` overrides it). The custom-relay guarantee covers **both legs**: the joining side automatically confines to the relay(s) advertised in the ticket, so a custom-relay share is never joined over n0's public relays — a joiner only needs `VELD_SHARE_RELAY` + `VELD_SHARE_RELAY_TOKEN` on their daemon to supply a **token** for a token-gated relay. The daemon binds **one iroh endpoint per relay policy** on demand, so shares on different relays (e.g. one project on public, another on your private relay) run side by side — no conflict, no restart.

A self-hosted relay can require an **authorization token** so it isn't open to anyone. Write a relay as `{ "url": ..., "token": ... }` and Veld sends the token as an `Authorization: Bearer` header. The token can be a literal string, or — to keep the secret out of `veld.json` — `{ "env": "VAR" }`, `{ "file": "/run/secrets/…" }` (Docker/K8s mounts), or `{ "command": "op read op://vault/relay/token" }` (1Password/Vault CLI). It's resolved on the daemon at share time; if it can't be resolved, the share fails rather than connecting unauthenticated. Config tokens apply to **hosting** only. The join side derives the relay from the ticket automatically; if that relay is token-gated, the joiner is **prompted** for the token (browser overlay or `veld join` terminal prompt) and it's **cached** per relay so future joins don't re-ask. A wrong token re-prompts. The token can also come from `VELD_SHARE_RELAY` + `VELD_SHARE_RELAY_TOKEN` (sent only when it matches the ticket's relay), or — to skip joiner setup entirely — the host can set `sharing.dangerouslyEmbedRelayTokensInTicket: true` to embed the token in the ticket, which is **dangerous** (the relay secret then travels in every share link; disposable tokens only). See [Relay auth tokens](docs/configuration.md#relay-auth-tokens).

`share.expose` is a list of audiences. `peer` (Veld-to-Veld, described above) reproduces the origin URL verbatim. `web` exposes a service to **anyone with a browser** — no Veld required — via a self-hosted gateway.

### Public web sharing

Point the environment at your org's gateway and opt services into the `web` audience:

```json
{
  "sharing": {
    "relays": ["https://relay.acme.internal"],
    // token is resolved in the daemon's environment (not your shell) — see the note below
    "gateway": { "url": "https://share.acme.internal", "token": { "file": "/run/secrets/gw-token" } }
  },
  "nodes": {
    "frontend": {
      "variants": {
        "local": { "type": "start_server", "command": "npm run dev", "share": { "expose": ["peer", "web"] } }
      }
    }
  }
}
```

```sh
veld share --web            # prints https://<slug>.share.acme.internal per service + a password
```

The gateway `token` (and any relay token) is resolved in the **daemon's** environment, not your interactive shell — a bare `export …` won't reach a background daemon, so use a literal (quick start), a `file` secret mount (production), or set the variable in the daemon's service definition. Same rule as [relay auth tokens](docs/configuration.md#relay-auth-tokens).

`veld share --web` mints a **separate** share scoped to the `web`-opted services (its own capability — revoking the web audience never touches peer shares), registers it with the gateway, and prints the public URLs. The gateway joins over iroh like any peer and reverse-proxies the tunneled service onto `https://<slug>.<gateway-domain>`. URLs are **deterministic** (a hash bound to your machine, the service, and the share) and survive gateway restarts; a new share mints new URLs. The daemon keeps the registration alive with heartbeats; `veld unshare` (or the share's TTL) kills the public URLs.

**Web shares are password-protected by default.** `veld share --web` generates a share password (or takes yours via `--password`) and prints it next to the URLs; the first visit shows a password page, then a session cookie keeps the viewer in for up to 12 hours (never longer than the share). Send URL and password over different channels for real secrecy — or use the printed **one-link** (`https://…/#veld-key=…`), which carries the password in the URL *fragment*: it never appears in DNS, TLS, server logs, or `Referer`, so even the convenient form beats a bare link. To opt a service out (anyone with the link is served — the unguessable 128-bit slug is then the only gate), set `"share": { "expose": ["web"], "web": { "access": "link" } }` in config, or pass `--access link` for services whose config doesn't pin a mode — an explicit config value always wins over the flag. Viewer sessions are stateless (signed with a key derived from the share's capability), so a gateway restart doesn't log viewers out, and revoking the share invalidates every session instantly.

WebSockets (HMR) work through the gateway; redirects to shared sibling services are rewritten to their public URLs. Fidelity is best-effort by design: the app sees its own origin hostname (dev-server host allow-lists pass untouched), the public host arrives in `X-Forwarded-Host`, and response cookies scoped to origin hostnames are made host-only. Apps with hard-coded absolute URLs, strict CORS allow-lists, or OAuth redirect URIs need those configured for the public host — that's the operator's domain setup, not something Veld rewrites. One password caveat for multi-service shares: the session cookie is per public host, so a password-protected API called cross-origin from a shared frontend will get 401s — give API nodes `"web": { "access": "link" }` (their slugs stay unguessable and only the app's code ever uses them).

In the browser, the toolbar's arc menu has a top-level **Sharing** item (a green dot marks it when the current page is already on the public web) that opens a submenu: **Start sharing** / **Stop sharing** toggle a web share for the current page's run without touching the terminal, **Copy public URL** swaps the host of your *current* page for the public one — keeping path, query, and hash, so a deep link to the exact screen you're looking at lands on your recipient's screen too — and **Sharing status** reports whether the page is shared and its public URL. Transport detail (`direct` vs `relayed via <relay>`, RTT, throughput warnings) lives in `veld shares` and the management UI, not the in-page toolbar.

Deploying the gateway is one container (`ghcr.io/prosperity-solutions/veld-gateway`) plus a wildcard DNS record — see the [gateway operator guide](docs/gateway.md).

> **Upgrading:** opt-in is a behavior change. Before, `veld share` exposed every URL-bearing service in a run; now it shares only services whose variant declares `share.expose`, and errors (naming the candidates) if none have opted in. Add `"share": { "expose": ["peer"] }` to the variants you previously relied on sharing. Password-by-default is a second behavior change: existing web shares gain a password on upgrade, and a freshly-upgraded daemon refuses `veld share --web` against a gateway too old to enforce it (clear error) — upgrade the gateway image, or share with `--access link`.

If the consumer already runs the same environment, the local URL wins — that node is skipped and reported as a warning. Shares live in the daemon's memory: if the daemon stops, shares stop (fail-closed). Stopping the run (`veld stop`) also auto-unshares its shares, and the consumer's join self-tears-down when the tunnel closes. `veld unshare` and `veld leave` take the id optionally, resolving the sole active share/join when omitted. Default TTL is 7200s (3600s for `--web` — the audience is the open internet, so idle web shares die sooner).

## Requirements

- macOS (arm64/x64) or Linux (x64/arm64)
- Optional: sudo access for `veld setup privileged` (clean URLs without port numbers, custom apex domains)

## Agent Skills

Veld ships skills for AI coding agents (Claude Code, Cursor, Codex, Windsurf, and [40+ more](https://github.com/vercel-labs/skills#supported-agents)). Install them so your agent knows how to configure, use, and collaborate through Veld:

```sh
npx skills add prosperity-solutions/veld
```

This installs the Veld skills: **`veld`** — CLI usage, `veld.json` configuration, and the bidirectional feedback workflow, loading live project state (nodes, presets, active runs, current config) at invocation time so your agent can act without discovery steps — and **`veld-launch-feedback-loop`**, a focused skill that parks an agent on the `veld feedback next` loop to work in-browser review comments one at a time.

## Contributing

We only accept agentic contributions — see [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## License

[MIT](LICENSE)
