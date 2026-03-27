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
| `veld logs [--name <n>] [--node <n>] [--lines <n>] [-f] [--since <d>] [--source <s>] [-s <term>] [-C <n>]` | View logs (`-f` follow, `-s` search, `-C` context lines) |
| `veld graph [NODE:VARIANT...]` | Print dependency graph |
| `veld nodes` | List all nodes and variants |
| `veld presets` | List presets |
| `veld runs` | List all runs |
| `veld feedback listen [--name <n>] [--after <seq>]` | Listen for feedback events (agent-facing) |
| `veld feedback answer --thread <id> "<msg>"` | Reply to a feedback thread |
| `veld feedback ask "<msg>"` | Ask the reviewer a question |
| `veld feedback threads [--name <n>]` | List feedback threads |
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

## Extensions

### Management UI

Veld includes a browser-based dashboard at `https://veld.localhost` (or `https://veld.localhost:18443` in unprivileged mode). It shows all environments with:

- **Services tab** — nodes with health status indicators, URLs with copy/open, variant, PID
- **Logs tab** — terminal viewer with search + highlighting, context lines (grep -C), auto-scroll, node filter, source filter (server/client/all)
- **Stop/Restart** — control environments directly from the browser

Open it with `veld ui` or visit the URL directly.

### Hammerspoon (macOS)

If you use [Hammerspoon](https://www.hammerspoon.org/), Veld ships a menu bar widget that shows running environments at a glance.

```sh
veld setup hammerspoon
```

This installs the `Veld.spoon` into `~/.hammerspoon/Spoons/` and offers to patch your `init.lua` to load it automatically. No sudo required. The menu includes an "Open Management UI" item for quick access to the browser dashboard.

Check extension status with `veld doctor`.

## Requirements

- macOS (arm64/x64) or Linux (x64/arm64)
- Optional: sudo access for `veld setup privileged` (clean URLs without port numbers, custom apex domains)

## Agent Skills

Veld ships skills for AI coding agents (Claude Code, Cursor, Codex, Windsurf, and [40+ more](https://github.com/vercel-labs/skills#supported-agents)). Install them so your agent knows how to configure, use, and collaborate through Veld:

```sh
npx skills add prosperity-solutions/veld
```

This installs the **veld** skill — a single skill covering CLI usage, `veld.json` configuration, and the bidirectional feedback workflow. It loads live project state (nodes, presets, active runs, current config) at invocation time so your agent can act immediately without discovery steps.

## Contributing

We only accept agentic contributions — see [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## License

[MIT](LICENSE)
