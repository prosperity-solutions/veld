# Veld — Product Requirements Document (v1)

## Overview

Veld is a local development environment orchestrator for monorepos. It enables developers to spin up fully wired local preview environments — with real HTTPS URLs, automatic DNS, TLS termination, Caddy routing, database clones, and multiple dev servers — from a single config file and a single command.

Veld abstracts away all port management, process coordination, and environment wiring. Users never deal with port numbers. They only ever see clean, stable, human-readable local HTTPS URLs.

v1 ships as three binaries (CLI, helper, user-space daemon) distributed via a single install script. No sudo required for install. No GUI. No MCP server. No Windows support.

---

## What v1 Is Not

The following are explicitly out of scope and must not be built:

- **Tauri GUI / desktop app** — v3 at earliest
- **System tray** — follows from no GUI
- **MCP server** — v2, abstraction layer stubbed from day one
- **Windows support** — macOS and Linux only
- **`veld init --ai`** — v2
- **`veld migrate`** — v2, only one schema version exists
- **Homebrew tap** — v2
- **Notarized macOS binaries** — v2, requires Apple Developer account
- **Cross-compilation in CI** — native builds only until v1 is stable

---

## Problem

Modern monorepos with multiple frontend apps, a backend, and cloud dependencies create significant local development friction:

- Multiple dev servers on different ports with no unified URL scheme
- No standard way to wire up local TLS, reverse proxies, or hostnames
- Database clones require manual setup scripts run in the right order
- No shared contract for "what is running and where"
- Git worktrees multiply this complexity — each worktree needs its own isolated environment
- Port numbers are arbitrary, forgettable, and collide across worktrees

---

## Vision

A developer runs `veld start frontend:local admin:local --name my-feature` from any directory in a monorepo worktree. Veld creates a named run, resolves the dependency graph, allocates ports internally, starts every process on its assigned port, configures Caddy to route clean HTTPS URLs, writes exact DNS entries, and produces a fully running environment at stable URLs like `https://frontend.my-feature.my-project.localhost`.

No ports. No manual config. No mkcert commands. No hosts file editing.

---

## Installation

### One Command Install
```sh
curl -fsSL https://veld.oss.life.li/get | bash
```

The install script handles everything end to end. A developer runs this once and their machine is fully configured. The install script:

1. Detects platform (macOS arm64/x64, Linux arm64/x64)
2. Downloads `veld`, `veld-daemon`, and `veld-helper` binaries from the latest GitHub Release as a matched versioned set
3. Verifies SHA-256 checksums against `checksums.txt` from the release
4. Places `veld` at `~/.local/bin/veld` (with a PATH reminder if needed)
5. Places `veld-daemon` and `veld-helper` at `~/.local/lib/veld/`
6. Prints success with next steps

The install script does NOT auto-run `veld setup`; commands auto-bootstrap on first use.

### Binary Versioning
All three binaries (`veld`, `veld-daemon`, `veld-helper`) are versioned together and released as a matched set. `veld --version` reports all three versions:

```
veld          1.2.0
veld-daemon   1.2.0
veld-helper   1.2.0
```

A version mismatch between binaries is detected at startup and treated as a fatal error with a clear message directing the user to run `veld update`.

### `veld update`
Re-runs the full install script, which downloads all three binaries, replaces them, and restarts services in a mode-aware fashion (detecting the current setup mode and restarting the appropriate daemons). One command updates everything — CLI, daemon, helper, and Caddy.

---

## Setup

### `veld setup` — Two Modes

`veld setup` is an optional system configuration command that installs Veld's infrastructure dependencies and registers its daemons. Commands auto-bootstrap on first use if setup has not been run explicitly.

`veld setup` operates in two modes:

- **`veld setup unprivileged`** — no sudo required. Installs Caddy and daemons as user-level processes. Services run on port 18443 instead of 80/443. Ideal for environments without root access.
- **`veld setup privileged`** — requires one-time sudo. Installs Caddy and daemons as system services on ports 80/443 for clean URLs without port numbers. The single sudo cost is paid once during setup.

Running `veld setup` without a mode argument auto-detects: it uses unprivileged mode by default and prompts for privileged if conditions allow.

Both modes are:
- **Idempotent** — re-running detects what is already correctly installed and skips those steps
- **Transparent** — prints clear per-step progress with success/failure indicators
- **Verified** — each step is confirmed to have actually worked before proceeding
- **Recoverable** — partial failures halt immediately with an actionable error message
- **Atomic** — `veld-helper` is verified running before setup marks itself complete

Steps performed (privileged mode shown):

```
[1/6] Checking port availability...       ✓ ports 80, 443, 2019 are free
[2/6] Installing Caddy...                  ✓ caddy 2.8.4 → ~/.local/lib/veld/caddy
[3/6] Trusting Caddy CA...                 ✓ Caddy CA added to system trust store
[5/6] Installing veld-helper daemon...     ✓ registered as LaunchDaemon (macOS)
[6/6] Installing veld-daemon...            ✓ registered as LaunchAgent, running
[6/6] Starting veld-helper...             ✓ running (pid 1234)

✓ Veld setup complete (privileged mode).

  Run `veld init` in a project to get started.
```

If any step fails, setup halts immediately with the exact failure and a suggested fix. It never silently proceeds past a broken step.

### Port Conflict Detection

Before touching anything, `veld setup` checks that ports 80, 443, and 2019 (Caddy admin API) are available. If any port is occupied, Veld identifies the exact process holding it and exits with a clear, actionable error:

```
✗ Port 443 is already in use by Caddy (pid 4821).

  Veld needs exclusive use of port 443 for HTTPS routing.
  Veld manages its own Caddy instance — it cannot share with
  an existing one.

  To stop it:
    brew services stop caddy
    sudo launchctl stop caddy

  Then re-run `veld setup`.
```

Veld does not forcefully kill processes it does not own. It surfaces the conflict and stops.

For a previous Veld install: `veld setup` detects its own running Caddy instance, checks the version against the newly downloaded binary, and restarts if the version changed. Fully idempotent.

### Caddy — Native Binary, Veld-Owned

Veld downloads a custom Caddy build from `caddyserver.com/api/download` with the `replace-response` plugin, and installs it to `~/.local/lib/veld/caddy`. This is a Veld-owned installation — completely separate from any system or Homebrew Caddy.

**Why not Docker for Caddy?** Docker on macOS runs inside a Linux VM. A Caddy container cannot reach `localhost:{port}` on the host directly — `localhost` inside the container is the container itself. The workaround (`host.docker.internal`) only works on Docker Desktop, not Linux. The Caddy config would have to be platform-aware just for this reason. A native binary sidesteps all of this — `localhost:{port}` always means what you expect, the config is identical across platforms, and there is no dependency on Docker Desktop being running.

**Why not Homebrew?** Homebrew is not guaranteed to be installed, `brew install` is slow and can fail for unrelated reasons, and the installed version lives in the user's global environment where version conflicts or upgrades can break Veld unexpectedly. A Veld-owned binary under `~/.local/lib/veld/` is fully controlled and updated atomically with the rest of Veld.

Veld configures Caddy entirely via its JSON admin API — no config files, no manual reloads. Routes are added and removed dynamically per run. Caddy returns 502 gracefully for processes not yet bound.

### Setup State Enforcement — Veld Screams If Not Set Up

Every command that touches environments — `veld start`, `veld stop`, `veld restart` — checks for a valid setup state as its very first action. If setup is incomplete or `veld-helper` is not running, commands auto-bootstrap using unprivileged mode. Users can also run setup explicitly:

```
✗ Veld setup has not been completed. Bootstrapping in unprivileged mode...

  For clean URLs without port numbers, run:
    veld setup privileged
```

- In `--json` mode:
```json
{
  "error": "setup_required",
  "message": "Run `veld setup` to complete one-time system setup.",
  "missing": ["veld-helper", "caddy", "mkcert"]
}
```

This applies to all callers — humans, scripts, and agents all get a clear, structured signal.

---

## The Helper: `veld-helper`

`veld-helper` is a small background daemon installed during `veld setup`. It can run in two modes:

- **Privileged mode** (`veld setup privileged`): runs as a root-owned system daemon. Binds Caddy to ports 80/443 for clean URLs. This is the **only component that ever runs with elevated privileges**.
- **Unprivileged mode** (`veld setup unprivileged`): runs as a user-level process. Binds Caddy to port 18443. No root access required at any point.

### Why a Permanent Daemon, Not a Short-Lived subprocess

A short-lived `sudo -n` subprocess requires cached sudo credentials at the time of every `veld start`. If credentials are not cached — after a reboot, in CI, in agent environments — sudo prompts mid-command, breaking scripted and agent use entirely. In privileged mode, a permanent launchd/systemd daemon requires no credentials after setup. The single sudo cost is paid once during install. In unprivileged mode, no sudo is ever needed.

### Responsibilities

Owns all operations requiring root:
- Writing and removing exact DNS host entries (dnsmasq include file or `/etc/hosts`)
- Adding and removing Caddy routes via Caddy's JSON admin API
- Reloading dnsmasq when entries change
- Starting/stopping the Veld-managed Caddy instance

### Command Surface (Minimal and Auditable)
```
add_host      <hostname> <ip>
remove_host   <hostname>
add_route     <caddy_route_json>
remove_route  <route_id>
reload_dns
caddy_start
caddy_stop
caddy_reload
status
```

No shell execution. No file system access beyond its specific managed paths. No network access beyond the local Caddy admin socket and dnsmasq config directory.

### Communication
`veld-helper` exposes a Unix socket at a fixed path, owned by the installing user. The `veld` CLI and `veld-daemon` communicate with it via this socket — no auth token, no sudo, no prompts, ever.

### Installation
**Privileged mode:**
- **macOS:** `/Library/PrivilegedHelperTools/dev.veld.helper`, registered via `SMJobBless` as a LaunchDaemon. Auto-starts on boot.
- **Linux:** systemd system unit. Auto-starts on boot.

**Unprivileged mode:**
- **macOS:** `~/.local/lib/veld/veld-helper`, registered as a LaunchAgent. Auto-starts on login.
- **Linux:** `~/.local/lib/veld/veld-helper`, registered as a user-level systemd unit. Auto-starts on login.

### Uninstallation
`veld uninstall` stops and removes the helper, its LaunchDaemon/systemd registration, all managed DNS entries, and all Caddy config. Leaves the machine completely clean.

---

## The User-Space Daemon: `veld-daemon`

An unprivileged background service that:
- Monitors running environments and polls health checks on a schedule
- Updates run statuses in the global registry
- Runs `veld gc` periodically
- Broadcasts state changes to connected CLI processes via a local Unix socket

Auto-starts on user login (not boot — runs as the user, not root):
- **macOS:** registered as a LaunchAgent by `veld setup`
- **Linux:** registered as a user-level systemd unit by `veld setup`

`veld-daemon` is transparent. The CLI works correctly without it, but the daemon improves health monitoring responsiveness and enables background state updates.

---

## URL Management System

**Users never see or deal with port numbers.** This is the defining feature of Veld.

### How It Works

1. Before any server starts, Veld allocates a free port from its managed internal range for the node.
2. Veld generates the stable HTTPS URL from the URL template.
3. Veld tells `veld-helper` to write an exact DNS entry: `{url} → 127.0.0.1`.
4. Veld tells `veld-helper` to add a Caddy route: `{url} → localhost:{port}`.
5. Veld starts the process, injecting `${veld.port}` via the command string or environment. **The process must bind to this port.** If it does not, Caddy returns 502 and the health check fails — this is a config authoring error, clearly surfaced to the user.
6. Veld runs the two-phase health check (see below) until both phases pass, then marks the node healthy and proceeds to dependent nodes.

DNS and Caddy are configured before the process starts. The URL is immediately valid — returning 502 gracefully until the process binds and becomes healthy.

### Two-Phase Health Check

Each node undergoes two health check phases in sequence before being marked healthy:

**Phase 1 — Port Check:** Verifies the process actually bound to `${veld.port}` via a direct TCP connection to `localhost:{port}`. If this fails, the error is clearly a process issue — the command didn't start, crashed, or ignored the injected port.

**Phase 2 — HTTPS URL Check:** Verifies the full stack end-to-end — DNS resolves, Caddy is routing, TLS cert is valid, upstream responds over HTTPS. Uses the `health_check` path declared in the config, checked against the full HTTPS URL. If phase 1 passes but phase 2 fails, the error is clearly a Veld infrastructure issue (Caddy routing, DNS, mkcert) rather than a process issue. This distinction produces much better error messages.

Both phases must pass for a node to be considered healthy. The health check config applies to both phases:

```json
{ "type": "http", "path": "/health", "expect_status": 200 }
{ "type": "port" }
{ "type": "command", "command": "./scripts/check.sh" }
```

Configurable `timeout_seconds` (default: 60) and `interval_ms` (default: 1000).

### URL Template

Defined in `veld.json` at the project level. Fully custom — the project embeds its own identity, naturally preventing collisions across projects:

```json
"url_template": "{service}.{branch ?? run}.my-project.life.li"
```

#### Default Template
If no `url_template` is declared, Veld defaults to:
```json
"{service}.{run}.{project}.localhost"
```

`.localhost` subdomains resolve to `127.0.0.1` automatically on modern macOS and Linux (RFC 6761) — no DNS configuration needed for the default case. The only setup required is mkcert for TLS. This is the recommended path for most users and requires no dnsmasq configuration at all.

Custom apex domains opt into full dnsmasq management via `veld-helper`.

#### Template Variables

| Variable | Value |
|---|---|
| `{service}` | Node name |
| `{run}` | Run name |
| `{worktree}` | Slugified worktree directory name |
| `{branch}` | Current git branch name, slugified (empty string if not in git) |
| `{project}` | Project name from `veld.json` |
| `{username}` | OS username |
| `{hostname}` | Machine hostname |

`{branch}` and `{worktree}` are evaluated at run creation time and **frozen into run state** — URLs never change if you switch branches mid-run.

#### The `??` Fallback Operator
```json
"url_template": "{service}.{branch ?? run}.my-project.localhost"
```
Left to right — first non-empty value wins. `{run}` is always guaranteed non-empty and should be the final fallback.

### DNS Strategy
Veld writes **exact host entries only** — never wildcard rules. Real domains and unrelated subdomains continue resolving normally via public DNS. Only the exact URLs Veld generates are intercepted locally.

- **`.localhost` apex (default):** No DNS writes at all. RFC 6761 handles resolution automatically.
- **Custom apex:** `veld-helper` writes exact entries to a Veld-managed dnsmasq include file. On run stop, entries are removed atomically. No stale entries accumulate in normal operation — `veld gc` handles cleanup from crashed runs.

---

## Core Concepts

### `veld.json`
Lives in the project root, committed to version control. Declares `$schema` and `schemaVersion`. Defines nodes, variants, and optional presets. Editors and agents load the schema for full autocomplete and validation.

**Path resolution:** All relative paths resolve relative to the directory containing `veld.json` — never the current working directory. `${veld.root}` is always available as the absolute path to this directory.

**Config discovery:** Veld walks up the file tree from the current working directory until it finds `veld.json`, exactly like Git discovers `.git`. If none is found, Veld exits with a clear error suggesting `veld init`.

### Runs
A **run** is a named, stateful instantiation of a node+variant selection.

- Runs have a **name** — user-specified (`--name my-feature`) or defaulting to the slugified worktree/workspace directory name
- Multiple runs can coexist within the same project simultaneously
- Runs are **idempotent by name** — `veld start --name my-feature` on a healthy run is a no-op
- Each run has a stable UUID for programmatic reference alongside the human-readable name
- Run names are frozen at creation — renaming is not supported

**Run name slugification:** lowercase, non-alphanumeric characters → `-`, consecutive `-` collapsed, leading/trailing `-` stripped, max 48 characters. If two runs in the same project produce the same default slug, Veld appends a short 4-character hash to the newer one and notifies the user.

### Node Graph Model
The config describes a directed graph of nodes. Each node has one or more variants. A run is defined by selecting end nodes with their desired variants — Veld resolves the full dependency graph from that selection, parallelizing independent branches.

There are no static profiles. A **preset** is a named shortcut for a node+variant selection — convenience only, not a core concept.

### Variants
A variant defines *how* a node behaves in a given context. The step type (`command` or `start_server`) is declared at the **variant level** — the same node might be a running server in one variant and a script exporting a remote URL in another.

### Dependency Declaration
Each variant declares its dependencies as explicit `node:variant` pairs. Default variants are never silently assumed — every dependency names its variant. The graph is always fully deterministic.

If two selected end nodes transitively require the same dependency node with *different* variants, Veld starts both as independent processes — each with its own port, URL, and state. Variable references must use the qualified form `${nodes.backend:local.url}` when two variants of the same node are running. Veld validates all variable references for ambiguity at graph resolution time and fails fast with a precise error before starting anything.

---

## Step Types

### `command`
Runs a shell script or inline command to completion. Used for setup tasks — database cloning, seeding, exporting remote service URLs, etc.

- Working directory defaults to `${veld.root}`
- Declares outputs via `VELD_OUTPUT key=value` written to stdout
- Built-in outputs: `exit_code`
- Optional `verify` command for idempotency of steps with external side effects:
  - Exit `0` → skip, previous result still valid
  - Non-zero → re-run
  - Receives the previous run's output variables as environment variables
  - If `verify` itself errors unexpectedly, the step re-runs (safe default)
- `sensitive_outputs: ["KEY"]` — encrypted at rest, masked in all output

### `start_server`
Starts and manages a long-lived process.

- Veld allocates a port and injects it as `${veld.port}` in `command` or `env`
- Working directory defaults to `${veld.root}`
- The process **must** bind to `${veld.port}` — Veld does not detect or verify the actual bound port. If the process ignores it, the two-phase health check fails with a clear error distinguishing process vs infrastructure failure.
- Built-in outputs: `url` (full HTTPS URL), `port` (allocated port, rarely needed)
- Optional `outputs` map: synthetic outputs whose values are string templates interpolated after the port is allocated. Used when a downstream node needs a constructed string like a database connection URL that incorporates `${veld.port}`:

```json
"outputs": {
  "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app"
}
```

This is particularly useful for Docker infrastructure nodes where stdout cannot be used for `VELD_OUTPUT`.

- stdout/stderr streamed to the run's log store (see Logging)

### Docker Pattern
Docker works natively as a `start_server` step — no special type needed. Veld injects `${veld.port}` and the command maps it to the container's internal port:

```json
{
  "type": "start_server",
  "command": "docker run --rm --name veld-db-${veld.run} -p ${veld.port}:5432 postgres:16",
  "health_check": { "type": "port" },
  "outputs": {
    "DATABASE_URL": "postgresql://postgres:postgres@localhost:${veld.port}/app"
  }
}
```

Docker is particularly well-suited for infrastructure nodes (databases, queues, caches) — strong isolation, consistent versions, clean teardown via container name.

---

## Built-in Variables

Available to all node variants without declaration:

| Variable | Value |
|---|---|
| `${veld.port}` | Allocated port for this node in this run |
| `${veld.url}` | Full HTTPS URL for this node (`start_server` only) |
| `${veld.url.hostname}` | DNS name only (e.g. `app.my-run.proj.localhost`) |
| `${veld.url.host}` | hostname:port (omits port when HTTPS port is 443) |
| `${veld.url.origin}` | scheme + host (same as `${veld.url}`) |
| `${veld.url.scheme}` | Protocol scheme (`https`) |
| `${veld.url.port}` | HTTPS port (note: `${veld.port}` is the backend bind port) |
| `${veld.run}` | Run name |
| `${veld.run_id}` | Stable run UUID |
| `${veld.root}` | Absolute path to directory containing `veld.json` |
| `${veld.project}` | Project name from `veld.json` |
| `${veld.worktree}` | Slugified worktree directory name |
| `${veld.branch}` | Git branch, slugified (empty string if not in git) |
| `${veld.username}` | OS username |

Node output references available to downstream nodes:

```
${nodes.database.DATABASE_URL}        # custom command or outputs declaration
${nodes.backend.url}                  # start_server built-in (unambiguous)
${nodes.backend.url.hostname}         # DNS name only
${nodes.backend.url.host}             # hostname:port
${nodes.backend:local.url}            # qualified form (two variants running)
${nodes.backend:local.port}           # internal port (rarely needed)
${nodes.clone-db.exit_code}           # command built-in
```

Short form `${nodes.backend.url}` is valid when only one variant of `backend` is active in the current graph. Qualified form is required when two variants of the same node are running simultaneously. Veld validates this at graph resolution time and fails fast with a precise, actionable error.

**Environment variable precedence:** The `env` block takes strict precedence over the inherited shell environment. Shell variables not overridden by `env` are passed through unchanged.

---

## Idempotency

- `start_server` — if process is running and both health check phases pass, skip
- `command` without `verify` — if previously completed successfully with the same input variable values, skip
- `command` with `verify` — run the verify command; exit 0 = skip, non-zero = re-run

`veld start --name my-feature` always converges to a healthy state. Safe to call repeatedly.

---

## Teardown

`veld stop` tears down the run in **reverse dependency order** — servers stop before the resources they depend on. All Caddy routes and DNS entries written by the run are removed via `veld-helper` atomically. Teardown is safe to re-run.

Runs are stopped as a whole unit — no per-node stop. After stop, the run record remains in state as `stopped` and can be restarted by name. Runs can be fully purged (state + logs) via `veld runs purge --name my-feature`.

---

## Logging

### Storage
Each run's process output is piped directly to files in the project's `.veld/` directory:

```
.veld/
  logs/
    {run-name}/
      {node}-{variant}.log          # start_server stdout+stderr, merged
      {node}-{variant}-setup.log    # command step stdout+stderr
      veld-debug.log                # orchestration trace, only with --debug
```

Logs are plain text, line-buffered. The Veld log writer prepends an ISO 8601 timestamp to every line. When a process exits, the log writer appends a final structured line:

```
[2026-03-11T14:23:01Z] [VELD] Process exited with code 1
```

No log daemon. No special storage layer. Files are directly readable by humans, `tail -f`, and agents.

`veld gc` prunes log directories for runs older than the configured retention period (default: 7 days).

### CLI Access
```sh
veld logs                                        # tail all nodes, current run
veld logs --node frontend                        # specific node
veld logs --node frontend --lines 100            # last N lines
veld logs --node frontend --since 5m             # since duration
veld logs --name my-feature --node backend       # specific run + node
veld logs --json                                 # structured per-line output
```

`--json` wraps each line: `{ "timestamp", "run", "node", "variant", "line" }`. Primary mechanism for agents to inspect failures without interactive sessions. Raw log files in `.veld/logs/` are also directly readable.

### Debug Mode
`--debug` on any command writes `veld-debug.log` to the run's log directory: full orchestration trace including variable resolution steps, graph traversal order, `veld-helper` socket calls, Caddy API payloads, DNS write operations, and health check attempts with timing and response details. Sensitive values masked throughout.

---

## Sensitive Variables

Outputs declared in `sensitive_outputs` are:
- Masked as `[REDACTED]` in all terminal output, debug logs, and run logs
- Stored encrypted at rest in `state.json` using a machine-local key derived from the machine's hardware UUID — no passphrase, no manual key management
- Never visible in `veld graph` output

---

## State

### Local State
`.veld/` directory per project, gitignored:
```
.veld/
  state.json          # all run states: statuses, PIDs, ports, variable values
  logs/
    {run-name}/
      {node}-{variant}.log
      {node}-{variant}-setup.log
      veld-debug.log
```

### Global Registry
JSON file in OS app data directory. One entry per known project: project root path, run names and statuses, URL maps, pointer to local `state.json`. Updated by the CLI on every start/stop.

### Garbage Collection
`veld gc`:
- Removes global registry entries whose project root paths no longer exist
- Removes Caddy routes and DNS entries for stale runs via `veld-helper`
- Kills any orphaned processes tracked in stale state
- Prunes log directories older than the retention period (default: 7 days)

Runs on a schedule when `veld-daemon` is active. Also available as an explicit CLI command.

---

## `veld init`

Bootstraps a `veld.json` for an existing project. Checks setup state first — if setup is incomplete, prompts to run `veld setup` before proceeding.

### Interactive Mode
```sh
veld init
```
1. Detects project structure (pnpm/npm/yarn workspaces, Cargo workspace)
2. Lists discovered services and asks which to include
3. For each service: dev command, port hint, dependencies
4. Detects database patterns (Prisma, Drizzle) and suggests command steps
5. Proposes URL template based on project name
6. Writes `veld.json` and adds `.veld/` to `.gitignore`

`veld init --ai` is out of scope for v1.

---

## Schema Versioning

`veld.json` declares `"schemaVersion": "1"`. Only one schema version exists in v1 — `veld migrate` is not built. Veld validates the declared version on every command and exits with a clear error if it encounters an unknown version, directing the user to `veld update`.

---

## CLI Reference

### Setup Gating
Every environment command checks setup state first. Setup not complete = immediate loud error. See Setup section.

### `veld start` With No Arguments
- TTY with presets: interactive preset selector
- TTY without presets: interactive node+variant picker
- Non-TTY (agent/CI): exits non-zero immediately with structured JSON listing available presets and nodes — never blocks on input

### Commands
```sh
# Run management
veld start [<node:variant>...] [--preset <n>] [--name <n>] [--debug]
veld stop [--name <n>] [--all]
veld restart [--name <n>] [--debug]
veld runs [--all] [--name <n>]
veld runs purge --name <n>
veld status [--name <n>]
veld urls [--name <n>]
veld logs [--name <n>] [--node <n>] [--lines <n>] [--since <d>] [--json]
veld graph [<node:variant>...]

# Project
veld nodes
veld presets
veld init

# System
veld list [--urls]
veld gc
veld setup
veld update
veld uninstall
```

All commands support `--json` for structured, stable output. `veld urls --json` is the canonical discovery call.

---

## Integration Test Suite

**A first-class v1 deliverable, not an afterthought.**

### Mock Test Project
```
testproject/
  veld.json
  backend/
    server.py     # python3 -m http.server, responds to /health
  frontend/
    server.py     # python3 -m http.server
```

No npm. No node_modules. No external dependencies. `python3` is available everywhere.

```json
{
  "$schema": "../../schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "testproject",
  "url_template": "{service}.{run}.testproject.localhost",
  "nodes": {
    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "python3 -m http.server ${veld.port}",
          "health_check": { "type": "port" }
        }
      }
    },
    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "python3 -m http.server ${veld.port}",
          "health_check": { "type": "port" },
          "depends_on": { "backend": "local" }
        }
      }
    }
  }
}
```

### Assertions (runnable as `./tests/integration.sh`)
1. `veld setup` completes without error
2. `veld setup` is idempotent — run twice, no failure
3. `veld start frontend:local --name inttest` exits 0 and prints URLs
4. `veld status --name inttest --json` shows all nodes healthy
5. `veld urls --name inttest --json` returns two HTTPS URLs
6. Both HTTPS URLs are reachable — `curl -f {url}` exits 0 (validates DNS, Caddy, TLS end-to-end)
7. `veld logs --name inttest --node backend --lines 10` returns output
8. `veld stop --name inttest` exits 0
9. Both process PIDs are dead after stop
10. Both HTTPS URLs return connection refused or 502 after stop
11. `veld gc` exits 0 and removes stale Caddy/DNS entries
12. Re-running `veld start --name inttest` succeeds (idempotency after stop)
13. Running any environment command before `veld setup` exits non-zero with `setup_required` in `--json` output

These tests run in CI on every PR.

---

## CI/CD Pipeline

### On Every PR
```yaml
- cargo fmt --check
- cargo clippy -- -D warnings
- cargo test --workspace
- ./tests/integration.sh    # macOS runner, requires real setup
```

### On Tag Push (`v*`)
```yaml
- cargo build --release      # macOS arm64 (native runner)
- cargo build --release      # macOS x64 (native runner)
- cargo build --release      # Linux x64 (native runner)
- sha256sum all binaries → checksums.txt
- GitHub Release assets:
    veld-macos-arm64.tar.gz
    veld-macos-x64.tar.gz
    veld-linux-x64.tar.gz
    checksums.txt
```

No cross-compilation until v1 is stable. No Tauri. No GTK. No npm in CI.

---

## Full Example `veld.json`

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "my-project",
  "url_template": "{service}.{branch ?? run}.my-project.localhost",
  "presets": {
    "fullstack": ["frontend:local", "admin:local"],
    "ui-only":   ["frontend:staging", "admin:staging"]
  },
  "nodes": {
    "database": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "command",
          "script": "./scripts/clone-db.sh",
          "verify": "./scripts/verify-db.sh",
          "outputs": ["DATABASE_URL"],
          "sensitive_outputs": ["DATABASE_URL"]
        },
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-db-${veld.run} -e POSTGRES_PASSWORD=veld -p ${veld.port}:5432 postgres:16",
          "health_check": { "type": "port" },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/app"
          }
        }
      }
    },
    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter backend dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "database": "local" },
          "env": {
            "DATABASE_URL": "${nodes.database.DATABASE_URL}"
          }
        },
        "staging": {
          "type": "command",
          "script": "./scripts/export-staging-backend-url.sh",
          "outputs": ["BACKEND_URL"]
        }
      }
    },
    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter frontend dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:local.url}"
          }
        },
        "staging": {
          "type": "start_server",
          "command": "pnpm --filter frontend dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "staging" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:staging.BACKEND_URL}"
          }
        }
      }
    },
    "admin": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter admin dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:local.url}"
          }
        },
        "staging": {
          "type": "start_server",
          "command": "pnpm --filter admin dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "staging" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:staging.BACKEND_URL}"
          }
        }
      }
    }
  }
}
```

---

## Technical Architecture

### Cargo Workspace
```
veld/
  crates/
    veld-core/      # all shared logic
    veld/           # CLI binary
    veld-helper/    # privileged daemon
    veld-daemon/    # user-space daemon
```

No Tauri crate. No MCP crate. Both are v2 — MCP abstraction layer stubbed in `veld-core` as a trait so adding it requires only a thin adapter.

### `veld-core`
All logic shared across binaries:
- Setup state detection and enforcement
- Config parsing and JSON schema validation
- Path resolution (always relative to `veld.json` location)
- Node graph construction, topological sort, cycle detection, variant resolution
- Variable reference validation and ambiguity detection at graph resolution time
- Parallel execution engine (dependency-ordered startup, reverse-order teardown)
- Bash runner (`VELD_OUTPUT` stdout parsing, `verify` execution)
- Server launcher (process management, port allocation from managed range)
- `outputs` interpolation for `start_server` (post-port-allocation)
- Two-phase health check polling (port check + HTTPS URL check)
- Port allocator (managed internal range, persisted per-run)
- `veld-helper` Unix socket client
- Run management (create, resume, list, stop, purge, gc)
- State persistence and global registry management
- Variable resolution and template interpolation (`??` operator)
- URL template evaluation, frozen at run creation time
- Sensitive variable encryption (hardware-derived machine-local key)
- Log file writer (timestamped lines, exit code annotation)
- Log reader (tail, lines, since, json)
- Debug trace collection and log writing
- MCP abstraction layer (trait, stubbed — implemented in v2)

### `veld` (CLI)
Thin wrapper over `veld-core`. Argument parsing, interactive TTY selectors, ASCII dependency graph rendering, `--json` mode, log streaming to terminal.

### `veld-helper` (daemon)
Minimal. DNS and Caddy management only. Unix socket interface. Installed by `veld setup`. Runs as root-owned system daemon (privileged mode) or user-level process (unprivileged mode). Does nothing else.

### `veld-daemon` (user-space)
Unprivileged. Health monitoring. GC scheduling. State broadcasts. LaunchAgent / user systemd unit. Auto-starts on login.

---

## Non-Goals (v1)

- Tauri GUI
- MCP server (trait stubbed, not implemented)
- System tray
- Windows support
- Homebrew tap
- Notarized macOS binaries
- `veld init --ai`
- `veld migrate`
- Cross-compilation in CI
- Multi-repo workspace documentation (architecture supports it, docs do not)

---

## Future (v2+)

- **MCP server** — thin adapter over the `veld-core` MCP trait. `veld setup-mcp` prints paste-ready config for Claude Desktop, Cursor, etc.
- **GUI** — Tauri app, observer only. Live dependency graph, URL launcher, log viewer.
- **`veld init --ai`** — AI-assisted config generation from repo structure description
- **`veld migrate`** — schema version upgrade with colored diff and confirmation
- **Homebrew tap** — `brew install veld-dev/tap/veld`
- **Notarized macOS binaries**
- **Multi-repo workspace UX** — `veld.json` outside any repo, `veld init --workspace`

---

## Success Criteria

- `curl -fsSL https://veld.oss.life.li/get | bash` installs Veld without sudo on a fresh machine; commands auto-bootstrap on first use
- `veld setup` is idempotent, verifies each step, and never silently proceeds past a failure
- Port conflicts are detected before setup proceeds with a precise, actionable error message
- Any environment command run before setup completes exits non-zero with a clear structured error
- `veld start frontend:local --name test` on the mock test project produces working HTTPS URLs reachable in the browser in under 2 minutes
- No user ever sees or types a port number
- Re-running `veld start --name test` on a healthy environment completes in under 5 seconds
- `veld stop --name test` cleanly removes all processes, Caddy routes, and DNS entries with no orphaned state
- Both health check phases pass — port bound AND HTTPS URL reachable — before any node is marked healthy
- The full integration test suite passes on every PR in CI
- `veld update` updates all three binaries and restarts daemons atomically
