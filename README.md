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
- **Health checks** — two-phase checks (TCP port + HTTP endpoint) before marking services healthy
- **Multiple variants** — same node, different behaviors (local server, Docker, remote URL)
- **Named runs** — multiple environments coexist; re-running by name is idempotent
- **Presets** — named shortcuts for common selections (`fullstack`, `ui-only`)
- **Variable interpolation** — `${veld.port}`, `${nodes.backend.url}`, git branch, etc.
- **Structured output** — all commands support `--json` for scripting and CI

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
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "myproject",
  "url_template": "{service}.{run}.{project}.localhost",
  "nodes": {
    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "npm run dev -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 30 }
        }
      }
    },
    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "npm run dev -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/", "timeout_seconds": 30 },
          "depends_on": { "backend": "local" }
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
| `veld logs [--name <n>] [--node <n>] [--lines <n>]` | View logs |
| `veld graph [NODE:VARIANT...]` | Print dependency graph |
| `veld nodes` | List all nodes and variants |
| `veld presets` | List presets |
| `veld runs` | List all runs |
| `veld feedback [--name <n>] [--wait] [--history]` | Read or wait for in-browser feedback |
| `veld gc` | Clean up stale state and logs |
| `veld setup [unprivileged\|privileged]` | One-time system setup |
| `veld init` | Create a new veld.json |

## Configuration

### Step types

- **`start_server`** — long-running process. Veld allocates a port (`${veld.port}`), starts the process, and runs health checks.
- **`command`** — runs a command to completion. Can emit outputs via `VELD_OUTPUT key=value` on stdout. Optional `verify` command for idempotency.

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

### Variable interpolation

Commands and env values support `${veld.port}`, `${veld.run}`, `${veld.root}`, `${nodes.backend.url}`, `${nodes.backend.port}`, etc.

## Architecture

Three binaries work together:

- **`veld`** — CLI. Parses commands, orchestrates environments, displays output.
- **`veld-helper`** — manages DNS entries and Caddy routes via a minimal Unix socket API. Runs as either a system daemon (privileged, for clean URLs on ports 80/443) or a user process (unprivileged, on port 18443).
- **`veld-daemon`** — user-space daemon. Monitors health, runs garbage collection, broadcasts state updates.

Caddy handles HTTPS termination and reverse proxying. Its internal CA is trusted in the system keychain during setup so browsers accept certificates without warnings.

## Extensions

### Hammerspoon (macOS)

If you use [Hammerspoon](https://www.hammerspoon.org/), Veld ships a menu bar widget that shows running environments at a glance.

```sh
veld setup hammerspoon
```

This installs the `Veld.spoon` into `~/.hammerspoon/Spoons/` and offers to patch your `init.lua` to load it automatically. No sudo required.

Check extension status with `veld doctor`.

## Requirements

- macOS (arm64/x64) or Linux (x64/arm64)
- Optional: sudo access for `veld setup privileged` (clean URLs without port numbers, custom apex domains)

## Agent Skills

Veld ships skills for AI coding agents (Claude Code, Cursor, Codex, Windsurf, and [40+ more](https://github.com/vercel-labs/skills#supported-agents)). Install them so your agent knows how to configure, use, and collaborate through Veld:

```sh
npx skills add prosperity-solutions/veld
```

This installs three skills:

| Skill | Description |
|-------|-------------|
| **veld-config** | Write and edit `veld.json` — nodes, health checks, dependencies, URL templates |
| **veld-feedback** | Human-in-the-loop feedback workflow — request reviews, read comments, iterate |
| **veld-usage** | CLI reference — start, stop, logs, status, and all other commands |

## Contributing

We only accept agentic contributions — see [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## License

[MIT](LICENSE)
