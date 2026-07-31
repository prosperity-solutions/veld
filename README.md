# Veld

> **Like if Kubernetes and localhost had a magic, agentic baby — with a gorgeous UI.**

Real HTTPS URLs for every service you run — one command, no YAML, no cloud bill. Then you and your coding agent build on it, together.

veld is a local development environment orchestrator — for a monorepo, a pile of separate repos, or any set of services you run together. Declare them in one `veld.json`; veld resolves the dependency graph, starts everything, runs health checks, and hands you clean, stable HTTPS URLs.

```sh
veld start frontend:local --name my-feature
# => https://frontend.my-feature.myproject.localhost
# => https://backend.my-feature.myproject.localhost
```

No port numbers. No manual wiring. Just clean, stable, human-readable URLs.

> This thing is 100% vibe coded with [Claude Code](https://claude.com/claude-code).

## Features

- **No port numbers** — work with stable HTTPS URLs instead of `localhost:3847`
- **Dependency graph** — resolves node dependencies, parallelizes startup, reverse-order teardown
- **TLS by default** — Caddy's internal CA handles TLS termination, auto-trusted during setup
- **Health checks** — readiness probes (two-phase: TCP port + HTTP/command) gate startup; liveness probes detect failures after startup (e.g., dropped SSH tunnels)
- **Automatic recovery** — when liveness probes detect failure, the environment is automatically restarted (configurable failure threshold and max recovery attempts)
- **Multiple variants** — same node, different behaviors (local server, Docker, remote URL)
- **Named environments** — multiple environments coexist (`--name dev`); re-running by name is idempotent
- **Run history** — a stopped, failed, or crashed run persists as history (last 10 per environment, 7 days) with its logs; `veld runs` lists it, `veld logs --run/--previous` targets it, and a crash is distinguishable from a clean stop
- **Setup / teardown** — project-level lifecycle steps that gate startup (check Docker, create networks) and clean up after stop
- **Presets** — named shortcuts for common selections (`fullstack`, `ui-only`); a preset can compose others with `@name`. Every preset shows a **key**, the number you type at `veld start` — pin it and it never moves again, whatever is added or renamed around it, so it stays valid in muscle memory and in a runbook (`veld presets --pin` freezes the current numbering). Add an optional `label`, `group`, and `when_to_use` and a list of thirty becomes pickable by someone who didn't write it — and by a coding agent, which reads the same text from `veld presets --json`. `default_preset` gives a bare `veld start` a defined answer, including in a non-interactive shell
- **Variable interpolation** — `${veld.port}`, `${nodes.backend.url}`, git branch, etc. Which built-ins exist in which context is checked by `veld lint`, so `${veld.url}` on a `command` node is refused before the run instead of failing mid-start
- **Config that scales to a monorepo** (`schemaVersion: "3"`) — comments and trailing commas in `veld.json` (or name the root file `veld.jsonc` and your editor works it out); split the config across per-directory files with `include` globs so teams own their own; declare a field once at node level and override it per variant; `vars` for one definition point per value, interpolated and resolved only when the plan reaches them. Deduplicates *values*, never structure — `rg <ENV_VAR>` still finds the line that sets it. `schemaVersion: "3"` is required — an older config fails to load with an error stating every change needed. See [docs/migrating-to-v3.md](docs/migrating-to-v3.md), written to hand to a coding agent, with `veld lint` as the check
- **`argv` or `shell`** — one vocabulary everywhere veld runs something. `argv` is spawned directly, so an interpolated value containing spaces or globs can never change the argument count; `shell` is the permanently-supported escape hatch
- **Value sources and secrets** — read a value from the environment, a file, or a command's stdout, and mark it `secret`. Veld carries a *pointer* and a flag, never custody: a secret reaches the process's environment or a file (`files:`) and is refused in a command line, where it would land in the process table
- **Named ports** — `"ports": { "http": "auto", "debug": "auto" }` for debug adapters and multi-port containers, so nothing needs a hand-picked literal port that breaks parallel worktrees
- **Structured output** — all commands support `--json` for scripting and CI
- **Browser dashboard** — management UI at `https://veld.localhost` with service health, logs, search, stop/restart
- **Every step's output is collected** — `command` nodes (a `docker build`, a `pnpm install`) log to their node's stream exactly like servers do, and project `setup`/`teardown` steps to the run's; nothing a step prints is thrown away. Lines also stream into `veld start`'s progress output as they arrive, instead of scribbling over it
- **Client-side logs** — captures browser `console.log/warn/error`, exceptions, and promise rejections; view with `veld logs --source client`
- **Internal logs** — liveness probe outcomes (with stderr), recovery decisions, health state transitions; view with `veld logs --source internal`
- **Resource monitoring that doesn't lie** — the daemon samples every node's whole process tree every 5s. The headline memory figure is the tree's **footprint** (proportional set size on Linux, `phys_footprint` on macOS), because summing RSS over a tree counts each page shared inside it once per process, so a five-process `npm run dev` reported far more than it occupied. On Linux the footprint splits by page class — private dirty (the heap, what grows when a node leaks), private clean, shared dirty, shared clean, swap, wired — and every node splits by **subprocess**, so "which child is eating the RAM" is a question with an answer. `veld stats` shows it in the terminal (`--processes`, `--history`, `--cpu`, `--memory <metric>`); the dashboard expands any node into a scrubbable chart you can flip between memory and CPU, and between total, by-type and by-process. Where a bucket averages several samples, the chart plots the peak alongside the mean — a mean over a six-minute bucket hides the spike a 5s sample caught. Totals are kept 24h, per-process detail 2h
- **Reverse-proxy header rules** — add or strip request/response headers on the local proxy and the public web gateway with a `proxy` config block (project/node/variant). Veld does no header manipulation by default.
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

1. Create a `veld.json` (or `veld.jsonc` — both are read) in your project root:

```json
{
  "$schema": "https://veld.oss.life.li/schema/v3/veld.schema.json",
  "schemaVersion": "3",
  "name": "myproject",
  "url_template": "{service}.{run}.{project}.localhost",
  "nodes": {
    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "argv": ["npm", "run", "dev", "--", "--port", "${veld.port}"],
          "probes": { "readiness": { "type": "http", "path": "/health", "timeout_seconds": 30 } }
        }
      }
    },
    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "argv": ["npm", "run", "dev", "--", "--port", "${veld.port}"],
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
| `veld start [NODE:VARIANT...] --name <n>` | Start an environment. With nothing to start: prompts (enter takes `default_preset`), or without a TTY uses `default_preset` directly |
| `veld start --preset <NAME-OR-KEY>` | Start a preset by name or by the number `veld presets` shows |
| `veld start <NODE:VARIANT> --oneshot [--all-logs]` | Run a command node as a one-off: start its dependencies, run it to completion (streaming its output), tear everything down in reverse order, and exit with its exit code. Ideal for end-to-end test runs. |
| `veld stop [--name <n>] [--all]` | Stop a running environment |
| `veld restart [--name <n>]` | Restart an environment |
| `veld status [--name <n>] [--json]` | Show environment status: current run id, per-node CPU/memory footprint, and (when the environment isn't live) the last run's outcome. URLs are shown only while live |
| `veld stats [--name <n>] [--node <n>] [--processes] [--history] [--cpu] [--window <d>] [--memory <m>] [--all-metrics] [--json]` | Detailed resource usage. `--processes` breaks a node down by subprocess (pid, CPU, memory, cumulative CPU time); `--history` draws a sparkline over `--window` (`30s`/`15m`/`2h`), of memory or — with `--cpu` — of CPU; `--memory` picks which figure to show — `footprint` (default), `resident`, `private_dirty`, `private_clean`, `shared_dirty`, `shared_clean`, `swap`, `wired`, `virtual`; `--all-metrics` shows a column per available metric |
| `veld urls [--name <n>] [--json]` | Show URLs for a running environment; errors if the environment is stopped (routes are torn down with the run) |
| `veld action <name> [--name <n>] [--node <n>] [--print] [--json]` | Run a node-defined action (e.g. open the database in a GUI client); `--print` emits the resolved command |
| `veld actions [--json]` | List the actions defined across the project's nodes |
| `veld logs [--name <n>] [--node <n>] [--lines <n>] [-f] [--since <d>] [--run <id-prefix>] [-p] [--all-runs] [--source <s>] [-s <term>] [-C <n>] [--json]` | View logs, scoped to the latest run by default (`-f` follow — exits 0 when the followed run ends, `--run` targets a past run by id prefix, `-p`/`--previous` the run before the latest, `--all-runs` restores the old interleaved-across-runs behavior, `-s` search, `-C` context lines, `--source` is one of `all` (default), `server` — node output, `client`, `setup` — project setup/teardown steps, `internal`) |
| `veld graph [NODE:VARIANT...]` | Print dependency graph |
| `veld nodes [--json]` | List all nodes and variants, with the file and line each is defined in |
| `veld presets [--json] [--pin]` | List presets with their keys, labels, and `when_to_use`. `--pin` prints the current numbering as a block to paste, freezing auto-assigned keys |
| `veld lint [--json]` | Check the config for semantic problems — unknown or out-of-scope `${veld.*}`, a `${nodes.X}` no preset's plan contains, secrets heading for a command line, broken presets. Exits 1 on any error, 0 when only warnings and notices remain — the CI-facing half of the config checks. `veld start` refuses on the same errors |
| `veld runs [--name <n>] [--json]` | List run history — one row per execution instance (short id, started/ended, duration, outcome), newest first. Without `--name`, all environments' runs grouped |
| `veld runs show <id> [--json]` | One run in full: outcome, node results with exit codes, and the graph snapshot it was started with (raw commands, env key names, URL templates — never resolved values) |
| `veld runs diff <old> <new> [--json]` | What changed in the config between two runs (node added/removed, command/cwd/env/url changes, veld.json hash). With one id, diffs that run against its predecessor |
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
| `veld config [--path] [--files] [--why <pointer>] [--json]` | Print the config. `--files`: each `include` glob, the files it matched, and the nodes each defines. `--why`: one effective value and where it was defined (a `secret` is described, never printed) |
| `veld init` | Create a new veld.json (veld also reads veld.jsonc) |

## Configuration

### Step types

- **`start_server`** — long-running process. Veld allocates a port (`${veld.port}`), starts the process, and runs health checks.
- **`command`** — runs a command to completion. Can emit outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE` (preferred) or via `VELD_OUTPUT key=value` on stdout (legacy, discouraged). Optional `skip_if` command for idempotency.

### Setup & teardown

Project-level lifecycle steps that run outside the dependency graph. Setup steps run sequentially before any node starts; teardown steps run after all nodes stop.

```json
{
  "setup": [
    { "name": "docker", "argv": ["docker", "info"], "failureMessage": "Docker must be running" },
    { "name": "veld-network", "shell": "docker network create ${veld.name}-net 2>/dev/null || true" }
  ],
  "teardown": [
    { "name": "veld-network", "shell": "docker network rm ${veld.name}-net 2>/dev/null || true" }
  ]
}
```

Setup steps that fail (non-zero exit) abort startup with the `failureMessage` if provided. Teardown is best-effort — failures are logged but don't block stop. Commands support shell env vars, project-level Veld variables (`${veld.name}`, `${veld.project}`, `${veld.root}`, `${veld.run}`, `${veld.worktree}`, `${veld.branch}`, `${veld.username}`) and `${vars.*}`. Node-scoped built-ins (`${veld.port}`, `${veld.url}`, `${veld.node}`) and `${veld.run_id}` are not available — a project step belongs to no node, and a teardown can outlive the run row.

### Health checks

```json
{ "type": "http", "path": "/health", "expect_status": 200, "timeout_seconds": 30 }
{ "type": "port", "timeout_seconds": 10 }
{ "type": "command", "argv": ["curl", "-sf", "http://localhost:${veld.port}/ready"] }
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

### Reverse-proxy header rules

Add or strip HTTP headers as requests and responses pass through the reverse proxy with `proxy` at the project, node, or variant level (most specific wins). Rules apply to **both** the local Caddy proxy (local dev) and the public web gateway (`veld share --web`). They do **not** apply to direct iroh peer sharing (`veld share` without `--web`) — that path is a transport-level byte splice with no HTTP layer, so header rules cannot be applied there.

```json
"proxy": {
  "request":  { "remove": ["Origin"] },
  "response": { "set": { "X-Frame-Options": "DENY" } }
}
```

- `request` rules apply to the request forwarded upstream; `response` rules to the response returned to the browser.
- `remove` strips the listed headers; `set` sets each header to the given value (replacing any existing value). Header names are matched case-insensitively.
- Across project → node → variant, `remove` lists are unioned (case-insensitive) and `set` maps are merged per key (most specific level wins). Absent `proxy` means no manipulation — the default.

**Default behavior changed.** Veld no longer manipulates any headers by default. Previously it stripped the `Origin` header so dev-server WebSocket HMR (Next.js, etc.) worked; now `Origin` passes through the local proxy, and the gateway rewrites `Origin` *coherently* to the origin host on all requests (including WebSocket upgrades) rather than dropping it. If your dev server gates WS HMR on `Origin` and rejects the passed-through value, set [`allowedDevOrigins`](https://nextjs.org/docs/app/api-reference/config/next-config-js/allowedDevOrigins) in `next.config.js` (the recommended fix). For frameworks with no allow-list, use the escape hatch of stripping `Origin` at the proxy:

```json
"proxy": { "request": { "remove": ["Origin"] } }
```

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

These are also available as cross-node references: `${nodes.backend.url.hostname}`, `${nodes.backend.url.host}`, etc., and in the node's own `on_stop` hook — so a container named after `${veld.url.hostname}` in `argv` is removed by the identical string at teardown.

Ports and URLs for all `start_server` nodes are pre-computed before execution, so `${nodes.X.url}` works everywhere — even across nodes with no dependency relationship. Frontend can reference backend's URL and backend can reference frontend's URL without a cycle.

Availability is not uniform — a `command` node has no port or URL of its own, a project `setup` step belongs to no node, and a `vars` value is one value for the whole run. `veld lint` reports a real built-in written where it is not populated (`builtin-not-in-scope`) rather than letting the run fail with `unknown built-in variable`. See [Availability](docs/configuration.md#availability).

## Architecture

Three binaries work together:

- **`veld`** — CLI. Parses commands, orchestrates environments, displays output.
- **`veld-helper`** — manages DNS entries and Caddy routes via a minimal Unix socket API. Runs as either a system daemon (privileged, for clean URLs on ports 80/443) or a user process (unprivileged, on port 18443).
- **`veld-daemon`** — user-space daemon. Monitors health, runs garbage collection, broadcasts state updates.

Caddy handles HTTPS termination and reverse proxying. Its internal CA is trusted in the system keychain during setup so browsers accept certificates without warnings.

### Storage

All CLI/daemon state — run state, the project registry, service logs, per-node and per-process resource samples, feedback threads and screenshots, relay auth tokens — lives in one SQLite database at `<data_dir>/veld/veld.db` (macOS: `~/Library/Application Support/veld/veld.db`; Linux: `~/.local/share/veld/veld.db`; override with `VELD_DB_PATH`). The file is `0600` (it holds secrets) and runs in WAL mode, so the CLI, daemon, and detached log writers read and write concurrently without file locking. The schema is versioned (`PRAGMA user_version`) and migrates forward automatically on upgrade — a CLI update never orphans or stops running environments because the data shape changed. A database created by a *newer* veld is refused with an error instead of being modified.

## Extensions

### Management UI

Veld includes a browser-based dashboard at `https://veld.localhost` (or `https://veld.localhost:18443` in unprivileged mode). It shows all environments with:

- **Services tab** — nodes with health status indicators, URLs with copy/open, variant, PID, and live resource usage (memory footprint + CPU per process tree, with a sparkline). Click a node's stats to expand a **scrubbable resource chart**: switch between **memory and CPU**, pick the window (5m → 24h) and the memory metric, and choose whether to plot the total, the page-class split, or one band per subprocess — with the live process table (pid, CPU, footprint, RSS, cumulative CPU time) below it. On a stopped environment it shows the last run's final node states
- **Logs tab** — terminal viewer with search + highlighting, context lines (grep -C), auto-scroll, node filter, source filter (server/client/all), and a run picker to read past runs' logs (latest, any ended run, or all interleaved)
- **Run history** — a top-level **Active | History** switcher keeps the everyday view to live environments; ended ones live under History with their last run's outcome (`crashed`/`failed` in red) and a one-click Restart
- **Stop/Restart** — control environments directly from the browser
- **Sharing** — start/stop peer shares and public web shares per run; each live tunnel shows its transport (`direct`, or `relayed via <relay>` — throughput capped by the relay) so slow shares are diagnosable at a glance

Open it with `veld ui` or visit the URL directly.

An experimental second-generation management UI is served at `/ide` (worktree mode: import git repositories, manage `git worktree` checkouts with aliases, and drive veld runs per worktree). It is also the web core of **Veld Desktop**, an Electron shell in `desktop/` — see [desktop/ARCHITECTURE.md](desktop/ARCHITECTURE.md).

It has **terminal panes**: real shells in the selected worktree's directory, in a dock of two tab strips you can split, reorder and drag tabs between. The shell runs in the daemon and reaches the browser over a WebSocket, so terminals work in a plain browser and not only in the Electron app.

Sessions outlive the page. Reloading (or Electron reloading its window) reattaches to the same shells with their scrollback intact, and output produced while you were away is replayed — a build keeps running and keeps logging. **Closing a terminal pane** is what ends a shell; closing the browser window or quitting the app only detaches, and a session nobody comes back to is hung up after 30 minutes (restarting the daemon, as `veld update` does, ends them immediately — the terminal says so instead of quietly handing you a fresh shell). Opening the same terminal in a second window takes it over rather than mirroring it, so there is only ever one writer. Up to 48 sessions can be live at once; past that, opening a terminal reports the limit and closing a pane frees a slot.

It also has **browser panes**: a tab in the same dock that renders one of the run's URLs, with a back/forward/reload row and an address bar. Each pane runs in a **colour-coded session** — its own cookie jar, named after an animal (otter, wombat, gecko…) rather than numbered — so you can be logged in as two different users of your own app side by side and tell which is which from the tab's dot. Sessions are added and removed from the pane's session menu — up to eight alongside the default, remembered per worktree — and the same menu clears any session's data, signing out every pane on it. A pane that can't load tells you *which* problem it hit: nothing listening (start the run), a hostname that doesn't resolve or an untrusted certificate (`veld doctor`), a timeout, or a crash. A browser pane with no URL **is** the run's start page: it lists your veld URLs, each row opening here on click with copy and open-externally beside it. So a worktree opens on a terminal next to one, the top bar's globe opens another, and `+` in the tab strip opens an undecided pane that offers Terminal, Browser and the same list at pane size (hover `+` for one-click shortcuts), or ⌘K ("Open &lt;service&gt; in a pane"). In **Veld Desktop** the pane is a real Chromium view, which means working history, page titles, isolated cookie jars, and pages that refuse to be framed still render. In a plain browser it falls back to an `<iframe>`: good enough for a preview, but a page sending `X-Frame-Options` shows blank, and history and separate sessions are unavailable — the pane says so, and offers to open the URL in your system browser instead.

Each browser pane can also **emulate a device**. The presets are **size classes, not model names** — small/medium/large phone, three tablet sizes, a 14″ laptop, a 24″ monitor, a 27″ widescreen — because a list of named handsets is long, out of date within a year, and never answers the only question you were asking, which is *how wide*. Each class carries the metrics a current device of that class actually reports. Beside them is **Responsive**: it starts at whatever the pane can hold and you **drag its edges**, which is how you find the width your layout actually breaks at. The page reflows *while* you drag — the screen grows from its centre, and the size under your pointer reads out in the toolbar. Any device can be dragged, not just that one — a phone dragged narrower keeps its touch events and user agent and becomes a custom size. Plus rotate, a mobile-user-agent toggle, an explicit custom size, and touch events, so a swipe gesture or a `@media (hover: none)` rule is testable without leaving the app. The useful half is the *large* sizes: a pane is small and a desktop layout is not, so a 1440-wide viewport is rendered at 1440 and **scaled to fit the pane**, which is a view no browser window can give you without a second monitor. **Page zoom** sits in the same control, with a reset. Both are remembered per pane and survive a reload, a session switch and a worktree switch. **DevTools** opens per pane in its own window (the ⟨⟩ button) — detached, because a docked inspector and an embedded view fight over the same box. Touch emulation needs Chromium's debugging channel, which something else can hold, so the pane reports whether touch is actually in force rather than assuming — its device menu says so when it isn't. The mobile user agent is the *string* only: `navigator.userAgentData` and the `Sec-CH-UA` headers still report your desktop, so an app that branches on client hints rather than the UA string keeps serving its desktop bundle. The menu says that too. A device only takes effect once the pane has a page: an empty pane has no view to emulate against, so the device you pick applies to the first thing it opens. In a plain browser the *sizes* work (the frame really is that many CSS pixels wide, so your media queries respond), but the user agent, touch, device pixel ratio, page zoom and DevTools all need the desktop app — the menu says so rather than pretending otherwise.

The dock also holds the run's **diagnostics**, so a worktree that is misbehaving can be diagnosed without leaving it. A **Logs** pane is the same viewer the dashboard has — search with ±N context lines, node filter, source filter (server/client/setup/internal), a run picker over history and auto-scroll — and a **Nodes** pane is the per-node health table: status, failure/recovery counts and the last liveness error, URL with copy/open, variant, PID, live CPU and memory with a sparkline that expands into the scrubbable resource chart, and each node's configured actions. In IDE mode each node's URL also carries an **open-in-a-pane** button, so a service opens in an embedded browser beside the terminal instead of in another application (copy and open-in-your-browser sit next to it). They are literally the dashboard's two views, not lookalikes: runs mode is a run's controls plus a Nodes|Logs switcher over the same components, so the two surfaces cannot drift. Both read whichever run the *selected* worktree has, so switching worktrees re-points every open diagnostics pane; both reach past runs (the logs pane's run picker, a picker in the nodes pane's header); and both keep working after a run ends — a crashed run's logs and last node states are exactly what you want then. The nodes view is a card per node rather than a table — a table has columns to lose, and this view has to be readable in a 300px pane and in a 1080px dashboard card alike; here nothing is dropped as the width changes, only rewrapped. Open them from `+` in the tab strip, the pane chooser, or ⌘K.

**Sharing** is one surface in the top bar: it starts and stops the peer share and the public `--web` share for the selected worktree's run, offers the join link and the `veld join` command, toggles auto-accept, and shows each live connection's transport (`direct`, or `relayed via <relay>`, with RTT) — so a slow share is diagnosable here too. Join requests are not hidden behind it: while someone is waiting for approval, a prompt sits above the panes naming who wants to join which run, with Approve and Deny. Sharing is opt-in per service (`share.expose`) and needs a relay (`sharing.relays`), so a run that has neither is *refused* rather than shared — the daemon says exactly what to add to `veld.json`, and both UIs now show that text instead of a bare status code.

Because a terminal is a shell on your machine, `/api/pty/attach` is gated more tightly than the rest of the daemon's API: WebSocket handshakes cannot carry the `X-Veld-Request` CSRF header, so an attach needs a single-use ticket minted through a CSRF-gated `POST` **and** an `Origin` on the allowlist, failing closed when `Origin` is absent. Details and the reasoning are in `crates/veld-daemon/src/pty.rs`.

### Veld Desktop

The same `/ide` UI as a desktop app: a native window with a menu-bar icon, real Chromium browser panes (working history, page titles, isolated cookie jars, and pages that refuse to be framed), device emulation and per-pane DevTools. Everything else works identically in a browser — the app is a shell around the daemon, not a second implementation.

**It needs the veld CLI**, which it does not ship. On a machine that has never had veld the app shows the two commands that get you there — the installer and `veld setup unprivileged` — and waits for the daemon to appear.

Download the `.dmg` (macOS) or `.AppImage` / `.deb` (Linux x64) from the [latest release](https://github.com/prosperity-solutions/veld/releases/latest) — `checksums.txt` on the same release page has a SHA-256 for every artifact if you want to verify what you downloaded. The app ships with every veld release and carries the same version number as the CLI — one tag, one version, so the app and the daemon it talks to are halves of the same thing. When they drift apart (you updated one and not the other) the app says so and names the fix: `veld update` for the CLI, its own updater for itself.

> **macOS: not code-signed yet.** Gatekeeper refuses the first launch. Open it once, let the warning appear, then go to **System Settings → Privacy & Security** and click *Open Anyway* — or clear the quarantine flag from a terminal: `xattr -dr com.apple.quarantine /Applications/Veld.app`. (Right-click → *Open* used to be the shortcut for this; macOS 15 removed it for apps that aren't notarized.) Developer ID signing and notarization are [tracked in #167](https://github.com/prosperity-solutions/veld/issues/167).

Updates are checked in the background and offered, never applied behind your back — applying one restarts the app. The Linux AppImage installs the update itself; on macOS and on `.deb` installs the app opens the release page instead, because macOS only lets an app replace itself when the replacement carries the same code signature (and there isn't one yet) and a `.deb`'s files belong to your package manager. *Check for Updates…* is in the menu bar icon (macOS) and the application menu.

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
        "local": { "type": "start_server", "argv": ["npm", "run", "dev"], "share": { "expose": ["peer"] } }
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

> **n0's public relays are for development and testing, not production.** They're a free community service shared across all iroh users worldwide — rate-limited, best-effort, and carry no uptime or throughput guarantees (a share stuck on `relayed` is capped by that relay). Per [iroh's own guidance](https://docs.iroh.computer/concepts/relays), production or high-volume sharing should run on **self-hosted relays** (`"relays": ["https://relay.example.com"]`, one Docker container — see [Relay auth tokens](docs/configuration.md#relay-auth-tokens)) or n0's managed relays. This is n0's fair-use recommendation, not a licensing restriction — iroh is dual-licensed MIT/Apache-2.0 and free to use commercially.

A self-hosted relay can require an **authorization token** so it isn't open to anyone. Write a relay as `{ "url": ..., "token": ... }` and Veld sends the token as an `Authorization: Bearer` header. The token can be a literal string, or — to keep the secret out of `veld.json` — `{ "env": "VAR" }`, `{ "file": "/run/secrets/…" }` (Docker/K8s mounts), or `{ "argv": ["op", "read", "op://vault/relay/token"] }` (1Password/Vault CLI). It's resolved on the daemon at share time; if it can't be resolved, the share fails rather than connecting unauthenticated. Config tokens apply to **hosting** only. The join side derives the relay from the ticket automatically; if that relay is token-gated, the joiner is **prompted** for the token (browser overlay or `veld join` terminal prompt) and it's **cached** per relay so future joins don't re-ask. A wrong token re-prompts. The token can also come from `VELD_SHARE_RELAY` + `VELD_SHARE_RELAY_TOKEN` (sent only when it matches the ticket's relay), or — to skip joiner setup entirely — the host can set `sharing.dangerouslyEmbedRelayTokensInTicket: true` to embed the token in the ticket, which is **dangerous** (the relay secret then travels in every share link; disposable tokens only). See [Relay auth tokens](docs/configuration.md#relay-auth-tokens).

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
        "local": { "type": "start_server", "argv": ["npm", "run", "dev"], "share": { "expose": ["peer", "web"] } }
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

If the consumer already runs the same environment, the local URL wins — that node is skipped and reported as a warning. Shares live in the daemon's memory: if the daemon stops, shares stop (fail-closed). Stopping the run (`veld stop`) auto-unshares its shares, and so do `veld restart` and a `veld start` that replaces a live environment of the same name — each tears down the ports a share points at and mints a new run, so the old share is released rather than left pointing at nothing. A share whose run ended some other way (a crash) is listed as a "share without a run" in the dashboard, with an Unshare button, so it never becomes unreachable. The consumer's join self-tears-down when the tunnel closes. `veld unshare` and `veld leave` take the id optionally, resolving the sole active share/join when omitted. Run names are unique per project, not across your machine — two repos both on `main` each have an environment named `main` — so `veld share` resolves the run against the project directory you run it from. (Two *checkouts of one repo* are the exception worth knowing: they share a `veld.json`, so the same run name there means the same URL. `veld start` refuses the second one with an error naming the other project rather than hijacking its route — start it under a different `--name`.) A name that project doesn't run is an error naming where it *does* run, so `cd` there; run from outside any project, an ambiguous name is rejected naming the candidates rather than guessed at. Sharing also requires the run to be **running**, not merely started: a stopped environment's URLs point at ports that are gone, and one still coming up would share only the services that happen to be up already. Default TTL is 7200s (3600s for `--web` — the audience is the open internet, so idle web shares die sooner).

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
