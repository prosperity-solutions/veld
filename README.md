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
- **Machine-overridable vars** — some values in a committed config are facts about the *laptop*, not the project: which of two installed container runtimes to use, a memory ceiling a 16 GB machine and a 64 GB machine disagree about, the path to a locally installed tool. A var declares `machine: { default, choices, description, prompt }` and each developer answers it once with `veld config set` — stored in Veld's database, **shared across every worktree of the repo** rather than asked again in each one, narrowable to one checkout with `--worktree`. A var with no default stops the run *before anything spawns*, asks when there is a terminal (or a UI), and otherwise refuses with the exact command — it never resolves a default nobody chose and never persists a guess
- **Named ports** — `"ports": { "http": "auto", "debug": "auto" }` for debug adapters and multi-port containers, so nothing needs a hand-picked literal port that breaks parallel worktrees
- **Structured output** — all commands support `--json` for scripting and CI
- **Browser dashboard** — management UI at `https://veld.localhost` with service health, logs, search, stop/restart
- **Every step's output is collected** — `command` nodes (a `docker build`, a `pnpm install`) log to their node's stream exactly like servers do, and project `setup`/`teardown` steps to the run's; nothing a step prints is thrown away. Lines also stream into `veld start`'s progress output as they arrive, instead of scribbling over it
- **Client-side logs** — captures browser `console.log/warn/error`, exceptions, and promise rejections; view with `veld logs --source client`
- **Internal logs** — liveness probe outcomes (with stderr), recovery decisions, health state transitions; view with `veld logs --source internal`
- **Resource monitoring that doesn't lie, including while a run is still starting** — the daemon samples every node's whole process tree every 5s, from the moment the run begins rather than once it is fully up: a dev server's boot-up allocation ramp is recorded, not skipped. `command` steps — builds, installs, codegen — are sampled too, every 2s, by the `veld start` that runs them, because their processes never outlive that command and no PID for them exists anywhere else. So "what did that `cargo build` peak at" and "how much does the dev server hold before it serves its first request" are questions with answers. (A `docker build` is the exception: the work happens inside `dockerd`/`buildkitd`, outside the step's process tree, so the client is all veld can see.) The headline memory figure is the tree's **footprint** (proportional set size on Linux, `phys_footprint` on macOS), because summing RSS over a tree counts each page shared inside it once per process, so a five-process `npm run dev` reported far more than it occupied. On Linux the footprint splits by page class — private dirty (the heap, what grows when a node leaks), private clean, shared dirty, shared clean, swap, wired — and every node splits by **subprocess**, so "which child is eating the RAM" is a question with an answer. `veld stats` shows it in the terminal (`--processes`, `--history`, `--cpu`, `--memory <metric>`); the dashboard expands any node into a scrubbable chart you can flip between memory and CPU, and between total, by-type and by-process. Where a bucket averages several samples, the chart plots the peak alongside the mean — a mean over a six-minute bucket hides the spike a 5s sample caught. Totals are kept 24h, per-process detail 2h (both reported by the API, so nothing hardcodes them); `VELD_STATS_MEMORY_DETAIL=off` and `VELD_STATS_CMDLINE=off` turn off the detailed probe and argv capture respectively — set in the *daemon's* environment (`launchctl setenv` / `systemctl --user set-environment`, then restart it; a shell `export` does not reach a launchd/systemd service), see [skills/veld/SKILL.md](skills/veld/SKILL.md)
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
| `veld status [--name <n>] [--json]` | Show environment status: current run id, what the run was started from (the preset, flagged if that preset has been edited since), per-node CPU/memory footprint, and (when the environment isn't live) the last run's outcome. URLs are shown only while live |
| `veld stats [--name <n>] [--node <n>] [--processes] [--history] [--cpu] [--window <d>] [--memory <m>] [--all-metrics] [--json]` | Detailed resource usage. `--processes` breaks a node down by subprocess (pid, CPU, memory, cumulative CPU time); `--history` draws a sparkline over `--window` (`30s`/`15m`/`2h`), of memory or — with `--cpu` — of CPU; `--memory` picks which figure to show — `footprint` (default), `resident`, `private_dirty`, `private_clean`, `shared_dirty`, `shared_clean`, `swap`, `wired`, `virtual`; `--all-metrics` shows a column per available metric |
| `veld urls [--name <n>] [--json]` | Show URLs for a running environment; errors if the environment is stopped (routes are torn down with the run) |
| `veld open-url <URL>` | Open a web page in the Veld window that owns this terminal (a browser pane beside it). Falls back to your system browser outside a Veld terminal, for an origin on the exempt list, or when no window is attached. A Veld terminal points `$BROWSER` at this, so most CLIs reach it without being told to |
| `veld action <name> [--name <n>] [--node <n>] [--print] [--json]` | Run a node-defined action (e.g. open the database in a GUI client); `--print` emits the resolved command |
| `veld actions [--json]` | List the actions defined across the project's nodes |
| `veld logs [--name <n>] [--node <n>] [--lines <n>] [-f] [--since <d>] [--run <id-prefix>] [-p] [--all-runs] [--source <s>] [-s <term>] [-C <n>] [--utc] [--local] [--json]` | View logs, scoped to the latest run by default (`-f` follow — exits 0 when the followed run ends, `--run` targets a past run by id prefix, `-p`/`--previous` the run before the latest, `--all-runs` restores the old interleaved-across-runs behavior, `-s` search, `-C` context lines, `--source` is one of `all` (default), `server` — node output, `client`, `setup` — project setup/teardown steps, `internal`). Timestamps print in your **local** time zone; `--utc` shows them as stored and `--local` forces local, either one overriding the `logs.timeZone` setting for that command. `--json` always emits UTC |
| `veld graph [NODE:VARIANT...]` | Print dependency graph |
| `veld nodes [--json]` | List all nodes and variants, with the file and line each is defined in |
| `veld presets [--json] [--pin]` | List presets with their keys, labels, and `when_to_use`. `--pin` prints the current numbering as a block to paste, freezing auto-assigned keys |
| `veld lint [--json]` | Check the config for semantic problems — unknown or out-of-scope `${veld.*}`, a `${nodes.X}` no preset's plan contains, secrets heading for a command line, broken presets. Exits 1 on any error, 0 when only warnings and notices remain — the CI-facing half of the config checks. `veld start` refuses on the same errors |
| `veld runs [--name <n>] [--json]` | List run history — one row per execution instance (short id, started/ended, duration, outcome), newest first. Without `--name`, all environments' runs grouped |
| `veld runs show <id> [--json]` | One run in full: outcome, what it was started from (preset or explicit selections), node results with exit codes, and the graph snapshot it was started with (raw commands, env key names, URL templates — never resolved values) |
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
| `veld config vars [--json]` | Every machine-overridable var: effective value, and which scope it came from |
| `veld config set <name> <value\|--env N\|--file P\|--shell C> [--worktree]` | Answer a machine-overridable var on this machine. Shared by every worktree of the project unless `--worktree` |
| `veld config unset <name> [--worktree]` | Forget this machine's answer, falling back to the next scope and then the config default |
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

### Service logs

On macOS a launchd job's stdout and stderr are **discarded** unless its plist names a file, so:

- **The daemon** logs to `~/.veld/veld-daemon.log`, owner-only. In the user's own directory rather than beside the binary, because the daemon is a user LaunchAgent and a legacy `/usr/local` install's lib dir is root-owned — and launchd does not run a job whose log file it cannot create, it exits it `EX_CONFIG` before the program starts.
- **The privileged helper** logs to `<lib dir>/veld-helper.log` (`~/.local/lib/veld/veld-helper.log` for a default install). It runs as root, so that directory is writable whichever prefix it is. The *unprivileged* helper has no log file yet.

On Linux systemd captures unit output itself: `journalctl --user -u veld-daemon`. `veld doctor` prints the daemon's log location in its Installation block — read out of the service definition, so it names the file that is really being written — and says plainly when a machine is not capturing it, which is the case for any install set up before this existed. `veld setup <mode>` writes the current definition.



### Storage

All CLI/daemon state — run state, the project registry, service logs, per-node and per-process resource samples, feedback threads and screenshots, relay auth tokens — lives in one SQLite database at `<data_dir>/veld/veld.db` (macOS: `~/Library/Application Support/veld/veld.db`; Linux: `~/.local/share/veld/veld.db`; override with `VELD_DB_PATH`). The file is `0600` (it holds secrets) and runs in WAL mode, so the CLI, daemon, and detached log writers read and write concurrently without file locking. The schema is versioned (`PRAGMA user_version`) and migrates forward automatically on upgrade — a CLI update never orphans or stops running environments because the data shape changed. A database created by a *newer* veld is refused with an error instead of being modified.

## Extensions

### Management UI

Veld includes a browser-based dashboard at `https://veld.localhost` (or `https://veld.localhost:18443` in unprivileged mode). It shows all environments with:

- **Services tab** — nodes with health status indicators, URLs with copy/open, variant, PID, and live resource usage (memory footprint + CPU per process tree, with a sparkline). Click a node's stats to expand a **scrubbable resource chart**: switch between **memory and CPU**, pick the window (5m → 24h) and the memory metric, and choose whether to plot the total, the page-class split, or one band per subprocess — with the live process table (pid, CPU, footprint, RSS, cumulative CPU time) below it. On a stopped environment it shows the last run's final node states
- **Logs tab** — terminal viewer with search + highlighting, context lines (grep -C), auto-scroll, node filter, source filter (server/client/all), and a run picker to read past runs' logs (latest, any ended run, or all interleaved)
- **Run history** — a top-level **Active | History** switcher keeps the everyday view to live environments; ended ones live under History with their last run's outcome (`crashed`/`failed` in red) and a one-click Restart. The **history horizon** (Settings → Runs) keeps that list short: it defaults to the last **3 days**, and anything older is hidden from History and from the past-run pickers — with a line saying how many, because a list that quietly omits things is indistinguishable from data loss. Clear the field to show everything. Nothing is deleted by it, and nothing is kept longer either: housekeeping already removes ended runs after 7 days, which is why 7 is the maximum
- **Stop/Restart** — control environments directly from the browser
- **Sharing** — start/stop peer shares and public web shares per run; each live tunnel shows its transport (`direct`, or `relayed via <relay>` — throughput capped by the relay) so slow shares are diagnosable at a glance. The two kinds of share say which they are: **Share privately** is the direct peer tunnel (they need Veld), **Share to the web** is the public URL (they need a browser). A public web share is laid out as a **row per service** — the URL, *Copy link*, *Copy link + QR*, *Copy QR*, and a **QR code shown right there** for opening it on a phone without retyping. The code encodes the same link the copy button gives you, password fragment included, so treat it like the password it carries. *Copy link + QR* puts both the text and a picture of the code on the clipboard, so pasting into Slack gives your reader something to point a phone at (which flavour a chat app takes is its own choice — the text is still on the clipboard), and *Copy QR* is the image alone, for sending the code after the link

Open it with `veld ui` or visit the URL directly.

An experimental second-generation management UI is served at `/ide` (worktree mode: import git repositories, manage `git worktree` checkouts with aliases, and drive veld runs per worktree). It is also the web core of **Veld Desktop**, an Electron shell in `desktop/` — see [desktop/ARCHITECTURE.md](desktop/ARCHITECTURE.md).

It has **terminal panes**: real shells in the selected worktree's directory, in a dock of two tab strips you can split, reorder and drag tabs between. Dragging a tab onto a pane's **left or right edge** splits there, and dropping it anywhere else in a pane moves it into that pane — the same gesture, with the target read off where you let go. A tab strip is one keyboard stop: `←`/`→` move between its tabs (`Home`/`End` to the ends) without switching panes as they go, `Enter` switches to the focused tab, and `Delete` closes it. The shell runs in the daemon and reaches the browser over a WebSocket, so terminals work in a plain browser and not only in the Electron app.

Sessions outlive the page. Reloading (or Electron reloading its window) reattaches to the same shells with their scrollback intact, and output produced while you were away is replayed — a build keeps running and keeps logging. `Shift+Enter` inserts a newline instead of submitting, which is what Claude Code and other coding agents read as a line break in their input box.

They also **outlive the daemon**. `veld update` restarts it, and a daemon can crash; neither ends your shells. Each terminal's pseudo-terminal is owned by a small holder process of its own rather than by the daemon, so the new daemon finds the running shells and picks them back up with their scrollback — an update is no longer something to schedule around, and an agent working in a pane keeps working through one. Quitting **Veld Desktop** and reopening it restores your panes too, so the tabs come back attached to the same shells. A reboot is the one thing nothing survives.

Each open terminal has a small holder process of its own, and its socket lives in `~/.veld/pty-<daemon port>/` — `veld doctor` reports how many are running, which is where to look if a terminal did not come back. A socket nobody answers is a holder that is gone; the next daemon start sweeps it. If that directory's path is too long for a unix socket (104 bytes — a deep `$HOME` can reach it), `veld doctor` fails that check and names `VELD_PTY_DIR` as the way out, rather than leaving you with a terminal that will not open.

**Closing a terminal pane** is what ends a shell; closing the browser window or quitting the app only detaches, and a session nobody comes back to is hung up after 30 minutes by default (Settings → Terminal) — as is one whose daemon never returns, so nothing is left running for a daemon that is gone for good. Opening the same terminal in a second window takes it over rather than mirroring it, so there is only ever one writer. Up to 48 sessions can be live at once; past that, opening a terminal reports the limit and closing a pane frees a slot. Selecting a worktree no longer opens a terminal by itself: its pane offers Terminal, Browser and the run's URLs, so browsing the rail costs nothing and a shell starts when you ask for one.

**A URL in the terminal opens in a pane.** Addresses in the output are links: click one and it opens as a browser tab in the same dock, next to the shell that printed it. The same goes for a program *in* the terminal that wants to open a browser — Veld points `$BROWSER` at itself, which is what Claude Code, `gh`, `git`, vite and next all consult, so an agent's login link or a dev server's `--open` lands in a pane instead of pulling you out of the app. (`veld open-url <URL>` is the same thing to invoke by hand, or from a script.)

Two escape hatches, because a pane is *not* the browser you are logged into — it has its own cookie jar, so an SSO flow started in one begins from scratch. Hold ⌘/Ctrl while clicking a link to send that one to your real browser. And an **exempt list** of origins always goes there: yours in Settings → Terminal, and the project's in its `veld.json` (`ide.externalOrigins`), unioned rather than one overriding the other — you name the sign-ins you use, the repo names the ones its app goes through. The whole behaviour is one switch (Settings → Terminal) if you would rather every link left the app.

**Tools that call `open`/`xdg-open` directly are caught too**, which matters because that is what an agent's shell tool does — `Bash(open "https://…")` never looks at `$BROWSER`, and Claude Code even sets `BROWSER=true` for its children. Those need Veld's shim directory on `PATH`, and `PATH` set before your login shell starts does not survive it: macOS's `path_helper` (in `/etc/zprofile`) rebuilds `PATH` with the system directories first, and Debian's `/etc/profile` overwrites it outright. So `PATH` has to be set *after* your startup files, and Veld arranges that without editing any of them:

> It points `ZDOTDIR` at a directory of its own containing **one** file, a `.zshenv`. That file hands `ZDOTDIR` straight back, sources your real `~/.zshenv` (whose place in the startup order it took), and registers a `precmd` hook that prepends the shim directory. Your `.zprofile`, `.zshrc` and `.zlogin` are then read normally, in order, unmodified, with your own `$ZDOTDIR` visible to them. Veld owns one file, in its own directory, and never writes to your home.

The shims route a single http(s) URL and hand **anything else** to the real tool untouched, with the original arguments — `open .`, `open report.pdf`, `open -a Safari …` behave exactly as they always did. This is zsh only (it is the one shell with both a startup file that runs before `$ZDOTDIR` matters and a hook that runs after every rc file); bash, fish and the rest keep `$BROWSER`, and can opt in by hand with one line, since every Veld terminal exports the directory:

```sh
# ~/.bashrc — route open/xdg-open inside Veld terminals to embedded panes
[ -n "$VELD_SHIM_DIR" ] && PATH="$VELD_SHIM_DIR:$PATH"
```

Anything that runs inside your shell's startup gets an off switch: *Also catch programs that call open / xdg-open* (Settings → Terminal). Turn it off and Veld sets no `ZDOTDIR` at all; turn off *Open links from the terminal in Veld* and Veld puts **nothing** in the shell — no `$BROWSER`, no `ZDOTDIR`, no round trip. Both take effect for new terminals; a shell already open keeps the environment it started with.

`veld doctor` reports whether this is actually working, because its failure mode is otherwise silent: the little scripts are written once when the daemon starts, and they carry the absolute path of the `veld` CLI belonging to that daemon — a sibling binary for a dev build, `<prefix>/bin/veld` for an installed one. A daemon that can find neither (a moved install, an interrupted update) leaves the feature off, and the row says which case it is and what fixes it — restarting the daemon, or reinstalling.

**Settings** (`⌘,`, or the gear in the top bar) covers terminal font, cursor, scrollback, the Shift+Enter behaviour, whether terminal links open in a pane, whether `open`/`xdg-open` are caught as well, and which origins are exempt, how long detached shells are kept, how worktrees are marked in the rail, how long trashed worktrees are kept before they are deleted for good, how far back the run history views reach, which time zone log timestamps are shown in, and which quick switches a browser pane's toolbar carries. They are stored by the daemon rather than the browser, so Veld Desktop and a browser tab against the same daemon agree, and every window sees a change. There is no Save button — each control writes as you change it.

**Creating a worktree starts with its name.** *New worktree…* asks first what this checkout is *called* — `Checkout V2` — and derives the rest: the rail name (`checkout-v2`) and the branch, both shown in the dialog before anything is created, because both derivations drop characters a git ref or a hostname cannot carry. Umlauts and accents are transliterated rather than thrown away — `Größe ändern` becomes `groesse-aendern`, `café` becomes `cafe` — since `groesse` is what a German speaker writes when they have to reach ASCII, and `gr-e` is not. The branch follows the name until you type over it, and clearing that field hands it back to the derivation; unticking *Create the branch* switches it to checking out a branch git already has, named exactly. The rail marker is picked in the same dialog rather than on a second trip through the context menu, and opens on a **random free** colour and glyph — random rather than "the next unused one", which proposed the same marker for every new checkout in a repo and made a week's rail look dealt off the top of the deck. If the name you typed collides with a checkout this repo already has, the dialog says so before the button rather than quietly appending a `-2`.

**Organising the rail.** With a dozen checkouts, alphabetical stops helping. **Lanes** are groups you name yourself — `review`, `spikes`, `client-x` — created from the rail's folder button or a worktree's context menu → *Move to lane*, and a worktree is dragged between them. Within a lane you can **drag worktrees into any order** you like; anything you have not placed by hand stays alphabetical underneath, so a checkout you make tomorrow appears at the end of its group rather than wedged silently into the middle of an order you built. The rail is also **resizable** — drag its right edge, or focus it and use `←`/`→` — and the width is remembered *per window*, because two windows on two monitors want different rails. Dragging cannot narrow the rail into its collapsed mode: collapsing hides the alias and the branch, so it stays a deliberate click on the chevron rather than something a drag can do to you by accident.

Lanes and order are **yours, not the repository's**. Nothing is written to `veld.json`, so your private rail layout never turns up as a review comment on someone else's pull request. (The project-declared counterpart already exists and is a different thing: `ide.quicklinks` is for links every checkout of the repo should see.)

**Removing a worktree puts it in the trash.** *Move to trash…* marks the checkout and hands you straight back the app — **nothing is deleted**, the directory stays on disk, and the row moves to a **Trash** section at the bottom of the rail. Restoring it from there is a real undo, not a race. That also makes the operation instant: awaiting `git worktree remove` on a large checkout is what froze the UI before, and now the request does no slow work at all.

A trashed worktree is deleted for good when you say so — *Delete permanently* on its row, or the trash-can button on the Trash header to empty the whole thing — or automatically once its **retention period** runs out (Settings → Worktrees). That setting is `0` by default, which means *keep until I empty it*: the only thing veld ever deletes without asking twice is something you put in the trash yourself and then left there for as long as you told it to.

Deleting always goes through `git worktree remove`, un-forced. Any run in the worktree is stopped first. If git refuses — uncommitted changes, a locked worktree, a file open in another process — the worktree **comes back into the rail with the reason on its row**, and says so once in a toast; it is never a silent failure. Forcing (which discards uncommitted changes) is a second, explicit click, offered only after git has already told you why it said no. The branch itself is always kept.

**Worktree markers** are a colour by default, or the animal emoji if you prefer (Settings → Appearance). Both are stored for every worktree, so switching between them never loses the one you picked, and either can be chosen per worktree from its context menu → *Change marker…*. The alias is always shown next to the marker, so the colour is a scanning aid rather than the only way to tell two checkouts apart.

It also has **browser panes**: a tab in the same dock that renders one of the run's URLs, with a back/forward/reload row and an address bar. Each pane runs in a **colour-coded session** — its own cookie jar, named after an animal (otter, wombat, gecko…) rather than numbered — so you can be logged in as two different users of your own app side by side and tell which is which from the tab's dot. Sessions are added and removed from the pane's session menu — up to eight alongside the default, remembered per worktree — and the same menu clears any session's data, signing out every pane on it. A pane that can't load tells you *which* problem it hit: nothing listening (start the run), a hostname that doesn't resolve or an untrusted certificate (`veld doctor`), a timeout, or a crash. A browser pane with no URL **is** the run's start page: it lists your veld URLs, each row opening here on click with copy and open-externally beside it. A worktree opens on a **single undecided pane** — not a split. The chooser already lists the run's URLs, so opening split showed them twice and imposed two columns on every checkout the first time you looked at it, including the many that want one full-width terminal; splitting is a drag away, un-splitting something you never asked for is busywork. The top bar's globe opens a browser pane, and `+` in the tab strip opens an undecided pane that offers Terminal, Browser and the same list at pane size (hover `+` for one-click shortcuts), or ⌘K ("Open &lt;service&gt; in a pane"). In **Veld Desktop** the pane is a real Chromium view, which means working history, page titles, isolated cookie jars, and pages that refuse to be framed still render. In a plain browser it falls back to an `<iframe>`: good enough for a preview, but a page sending `X-Frame-Options` shows blank, and history and separate sessions are unavailable — the pane says so, and offers to open the URL in your system browser instead.

Each browser pane can also **emulate a device**. The presets are **size classes, not model names** — small/medium/large phone, three tablet sizes, a 14″ laptop, a 24″ monitor, a 27″ widescreen — because a list of named handsets is long, out of date within a year, and never answers the only question you were asking, which is *how wide*. Each class carries the metrics a current device of that class actually reports. Beside them is **Responsive**: it starts at whatever the pane can hold and you **drag its edges**, which is how you find the width your layout actually breaks at. The page reflows *while* you drag — the screen grows from its centre, and the size under your pointer reads out in the toolbar. Any device can be dragged, not just that one — a phone dragged narrower keeps its touch events and user agent and becomes a custom size. Plus rotate, a mobile-user-agent toggle, an explicit custom size, and touch events, so a swipe gesture or a `@media (hover: none)` rule is testable without leaving the app. The useful half is the *large* sizes: a pane is small and a desktop layout is not, so a 1440-wide viewport is rendered at 1440 and **scaled to fit the pane**, which is a view no browser window can give you without a second monitor. **Page zoom** sits in the same control, with a reset. Both are remembered per pane and survive a reload, a session switch and a worktree switch. **DevTools** opens per pane in its own window (the ⟨⟩ button) — detached, because a docked inspector and an embedded view fight over the same box. Touch emulation needs Chromium's debugging channel, which something else can hold, so the pane reports whether touch is actually in force rather than assuming — its device menu says so when it isn't. The mobile user agent is the *string* only: `navigator.userAgentData` and the `Sec-CH-UA` headers still report your desktop, so an app that branches on client hints rather than the UA string keeps serving its desktop bundle. The menu says that too. A device only takes effect once the pane has a page: an empty pane has no view to emulate against, so the device you pick applies to the first thing it opens. In a plain browser the *sizes* work (the frame really is that many CSS pixels wide, so your media queries respond), but the user agent, touch, device pixel ratio, page zoom and DevTools all need the desktop app — the menu says so rather than pretending otherwise.

A pane can also **emulate the page's media features** — `prefers-color-scheme`, `prefers-reduced-motion` and `forced-colors` — from the same device menu. It is the same question a device width asks, put to a preference: *what does this look like for someone whose OS says dark, or who has asked for less motion?* There is no reload; a media query is live, so the page re-evaluates in place. The control is worded as being about the **page**, because Veld themes itself light and dark too. These ride the same debugging channel touch does, so the pane reports whether they are actually in force, and the browser build says so rather than offering a control that would do nothing.

The two you reach for constantly get **one-click switches in the pane's toolbar**, beside the device button: **Responsive**, and the page's **colour scheme**. Both are shortcuts into the device menu, which keeps every one of these controls — turning a switch off costs reach, never capability, which is why *which switches appear* is a setting (Settings → Browser panes) rather than two more buttons for everyone. It is a standing choice about whether you want the shortcut at all, not a per-pane one: the setting is global, so off is off in every pane and window. The colour scheme **cycles System → Dark → Light → System**, so a light-only layout bug is one click away too, and **System is the absence of an override** rather than a third value — which is also what releases the debugging channel. Responsive's off is **no emulation at all**, so that switch answers one question ("am I in the resizable viewport") rather than restoring a device you picked earlier. The colour-scheme switch reports what the pane *achieved*: it shows as paused, not as set, while something else holds Chromium's debugger. In a plain browser the responsive switch works and the colour-scheme one is shown inert, saying why in its tooltip and its accessible name.

**Permissions in a browser pane** used to be refused across the board, for a defensible reason: a prompt raised inside an embedded pane has no chrome to attribute it to, and "example.com wants your camera" is a lie when the window says Veld. Now the prompt is *in the pane*, where it can name the site, the pane and its session colour — and where blocking it costs nothing but the page's request. It sits above the page rather than over it, so it never hides what asked. Answers are remembered per session and origin the way a browser remembers them, and the shield beside the address bar opens that site's settings: every permission, each Allow / Ask / Block, and where the current answer came from.

A project can also **pre-answer permissions for its own origins**, in `veld.json` — so a dev server that needs geolocation or a camera works for everyone who clones the repo instead of prompting each of them one at a time:

```jsonc
"ide": {
  "permissions": [
    { "origin": "http://localhost:*", "allow": ["geolocation", "clipboard-read"] }
  ],
  "quicklinks": [
    { "label": "Staging", "url": "https://staging.example.com" }
  ]
}
```

A grant you make by hand always beats the file, in both directions, and anything from the file is labelled *set by veld.json* in the site panel where you can revoke it — so this is a default, not a decision taken out of your hands. Understand what you are declaring, though: an entry for a *remote* origin hands that server's JavaScript a standing capability on the machine of anyone who opens it in a pane, which is a step beyond what the rest of a config does. See [docs/configuration.md](docs/configuration.md#ide-quicklinks-permissions-external-origins-and-panes) for the origin syntax, the full id list and the defaults.

One permission is answered without asking anyone: a pane may **capture its own contents** at an origin veld serves. That is what `veld feedback` needs to take a screenshot, and a pane that could not screenshot the page it is showing was the one place the feature should have worked best.

`ide.externalOrigins` is the third key in that block: origins a URL from a terminal must open in the *system* browser rather than in a pane — the sign-ins your app's login goes through, which need the browser the developer is already logged into. It is unioned with each user's own exempt list rather than replacing it.

`ide.quicklinks` is the other half of the same block, and the other half of a pane's start page: veld lists the URLs it made, and these are the ones it didn't — staging, a dashboard, an internal wiki — versioned with the repo so a teammate who clones it gets the same links.

**A project can add its own panes** (`ide.panes`), which is how the dock stops being a fixed set of four kinds. A declared pane is a terminal that runs *your* command instead of a login shell, and it appears in the `+` menu, the pane chooser and ⌘K alongside veld's own:

```jsonc
"ide": {
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

Nothing there is a vendor veld knows about. It knows how to run a command in a terminal, how to check that `claude` is on your `PATH` before offering the pane, and how to hand the command a stable identity — `${veld.pane.token}`, a UUID it mints per pane and remembers in its own database. Which tool that is, and what its resume flag is called, lives in your repo.

A tool that won't accept an id you chose needs no token at all. `codex` mints its own, so its pane is just `"argv": ["codex"]` with `"resume": { "argv": ["codex", "resume", "--last"] }` — same Resume button, same `auto_resume`, and codex's own resume is scoped to the working directory, so "most recent" means most recent in that worktree. What the token buys, and this shape gives up, is *per-pane* identity: two Codex panes in one worktree resume the same session, where two Claude panes hold two separate conversations. This repo's own `veld.json` carries both, precisely so the difference is visible.

That token is what lets a pane **fake surviving a reboot**. Veld's terminals already outlive a daemon restart and a `veld update`, because each shell sits in its own holder process — but nothing carries a process across a reboot. The *conversation* can be carried, though: a coding agent keeps its own transcript keyed by the session id it was given, so re-launching with the same id brings it back. A fresh launch always mints a new token, so "start fresh" really is a new conversation, and two Claude panes in one worktree hold two separate ones — which is precisely what a bare `--continue` cannot do.

`auto_resume` is deliberately narrow. It fires only when a pane **comes into being** with its shell already gone — the app starting up and restoring your layout — and never while you are watching: a tool that exits in front of you always waits for a click, whatever the config says. It defaults to `false`, because these commands launch coding agents and an unattended one spends money and runs tools with nobody there. For the same reason a `resume` that fails is never quietly retried as a fresh launch; the pane says so and offers *Start fresh* as its own button.

`close_on_exit` is the other half, and defaults to **true**: quit the tool and the pane closes, the way a terminal emulator does. Two things bound it. A **non-zero** exit never closes the pane — the reason a tool died is printed on the screen it dies on, and a pane that disappears takes the error with it. And it only fires on an exit somebody was there to see, so it never competes with `auto_resume`: a reboot, a quit app or a reaped session leave the pane to be restored from the layout instead. When a pane *is* sitting there not running, it says so across the whole pane — icon, label, what happened, Resume and Start fresh as real buttons — with a *Show output* link back to whatever is underneath, because by then the pane has usually failed and that output is the reason.

The dock also holds the run's **diagnostics**, so a worktree that is misbehaving can be diagnosed without leaving it. A **Logs** pane is the same viewer the dashboard has — search with ±N context lines, node filter, source filter (server/client/setup/internal), a run picker over history and auto-scroll, and **colour**: a line's ANSI colours are rendered rather than printed as escape codes, other escape sequences are dropped, a progress line that overwrites itself shows its last state instead of every frame at once, and search matches the text you can see rather than the bytes underneath it — and a **Nodes** pane is the per-node health table: status, failure/recovery counts and the last liveness error, URL with copy/open, variant, PID, live CPU and memory with a sparkline that expands into the scrubbable resource chart, and each node's configured actions. In IDE mode each node's URL also carries an **open-in-a-pane** button, so a service opens in an embedded browser beside the terminal instead of in another application (copy and open-in-your-browser sit next to it). They are literally the dashboard's two views, not lookalikes: runs mode is a run's controls plus a Nodes|Logs switcher over the same components, so the two surfaces cannot drift. Both read whichever run the *selected* worktree has, so switching worktrees re-points every open diagnostics pane; both reach past runs (the logs pane's run picker, a picker in the nodes pane's header); and both keep working after a run ends — a crashed run's logs and last node states are exactly what you want then. The nodes view is a card per node rather than a table — a table has columns to lose, and this view has to be readable in a 300px pane and in a 1080px dashboard card alike; here nothing is dropped as the width changes, only rewrapped. Open them from `+` in the tab strip, the pane chooser, or ⌘K.
The dock also holds the run's **diagnostics**, so a worktree that is misbehaving can be diagnosed without leaving it. A **Logs** pane is the same viewer the dashboard has — search with ±N context lines, node filter, source filter (server/client/setup/internal), a run picker over history and auto-scroll, and **colour**: a line's ANSI colours are rendered rather than printed as escape codes, other escape sequences are dropped, a progress line that overwrites itself shows its last state instead of every frame at once, and search matches the text you can see rather than the bytes underneath it — and a **Nodes** pane is the per-node health table: status, failure/recovery counts and the last liveness error, URL with copy/open, variant, PID, live CPU and memory with a sparkline that expands into the scrubbable resource chart, and each node's configured actions. In IDE mode each node's URL also carries an **open-in-a-pane** button, so a service opens in an embedded browser beside the terminal instead of in another application (copy and open-in-your-browser sit next to it). They are literally the dashboard's two views, not lookalikes: runs mode is a run's controls plus a Nodes|Logs switcher over the same components, so the two surfaces cannot drift. Both read whichever run the *selected* worktree has, so switching worktrees re-points every open diagnostics pane; both reach past runs (the logs pane's run picker, a picker in the nodes pane's header); and both keep working after a run ends — a crashed run's logs and last node states are exactly what you want then. The nodes view is a card per node rather than a table — a table has columns to lose, and this view has to be readable in a 300px pane and in a 1080px dashboard card alike; here nothing is dropped as the width changes, only rewrapped. Open them from `+` in the tab strip, the pane chooser, or ⌘K — or from the rail itself: a worktree whose run has **failed**, or one whose nodes veld is **recovering**, carries a warning on its row that says which, and clicking it brings you to that worktree with its Nodes pane in front. The rest of the row's run state is on its start/stop control, which spins while a run is coming up or going down — including a run you started from the terminal or from another window, not only one you clicked here.

**Sharing** is one surface in the top bar: it starts and stops the peer share and the public `--web` share for the selected worktree's run, offers the join link and the `veld join` command, toggles auto-accept, and shows each live connection's transport (`direct`, or `relayed via <relay>`, with RTT) — so a slow share is diagnosable here too. Join requests are not hidden behind it: while someone is waiting for approval, a prompt sits above the panes naming who wants to join which run, with Approve and Deny. Sharing is opt-in per service (`share.expose`) and needs a relay (`sharing.relays`), so a run that has neither is *refused* rather than shared — the daemon says exactly what to add to `veld.json`, and both UIs now show that text instead of a bare status code.

Because a terminal is a shell on your machine, `/api/pty/attach` is gated more tightly than the rest of the daemon's API: WebSocket handshakes cannot carry the `X-Veld-Request` CSRF header, so an attach needs a single-use ticket minted through a CSRF-gated `POST` **and** an `Origin` on the allowlist, failing closed when `Origin` is absent. Details and the reasoning are in `crates/veld-daemon/src/pty.rs`.

### Veld Desktop

The same `/ide` UI as a desktop app: a native window with a menu-bar icon, real Chromium browser panes (working history, page titles, isolated cookie jars, and pages that refuse to be framed), device emulation and per-pane DevTools. Everything else works identically in a browser — the app is a shell around the daemon, not a second implementation.

**Windows.** `⌘N` opens another full window — one worktree per monitor, rather than switching back and forth in one. Right-click a worktree in the rail for *Open in a new window* to send it straight there.

**A worktree has one set of panes, and one window shows them.** Windows are for working on *different* worktrees side by side, not for opening the same one twice: pick a worktree another window already has and Veld brings you to that window instead of growing a second set of terminals. Those rows are marked in the rail before you click one, and the switch says so, so a window coming forward is never a surprise. Close that window and the worktree is free again — open it anywhere and its panes come back, still attached to the same shells, because the layout belongs to the worktree rather than to the window that happened to show it.

A pane can also leave its window: right-click a tab → *Open in a new window*, or **drag it out** and drop it — on your second screen, and that is where the window appears. A dropped pane can go back the same way, or into any other Veld window showing that worktree: drag it over and the target shows its own drop indicators, so it lands at the tab position or pane edge you aimed at rather than at the end. That bare dock window can be split and hold tabs of its own, and closing it **returns** its tabs rather than ending them — closing a *pane* is still what ends a shell. Detaching a browser pane reloads its page (a Chromium view belongs to one window, so moving it is a rebuild); the URL, session, device and zoom all survive. Every window comes back on the next launch with its panes attached to the same shells. Up to eight windows.

**It needs the veld CLI**, which it does not ship. On a machine that has never had veld the app shows the two commands that get you there — the installer and `veld setup unprivileged` — and waits for the daemon to appear.

Download the `.dmg` (macOS) or `.AppImage` / `.deb` (Linux x64) from the [latest release](https://github.com/prosperity-solutions/veld/releases/latest) — `checksums.txt` on the same release page has a SHA-256 for every artifact if you want to verify what you downloaded. The app ships with every veld release and carries the same version number as the CLI — one tag, one version, so the app and the daemon it talks to are halves of the same thing. When they drift apart (you updated one and not the other) the app says so and names the fix: `veld update` for the CLI, its own updater for itself.

> **macOS: not code-signed yet.** Gatekeeper refuses the first launch. Open it once, let the warning appear, then go to **System Settings → Privacy & Security** and click *Open Anyway* — or clear the quarantine flag from a terminal: `xattr -dr com.apple.quarantine /Applications/Veld.app`. (Right-click → *Open* used to be the shortcut for this; macOS 15 removed it for apps that aren't notarized.) Developer ID signing and notarization are [tracked in #167](https://github.com/prosperity-solutions/veld/issues/167).

Updates are checked in the background and offered, never applied behind your back — applying one restarts the app. The Linux AppImage installs the update itself; on macOS and on `.deb` installs the app opens the release page instead, because macOS only lets an app replace itself when the replacement carries the same code signature (and there isn't one yet) and a `.deb`'s files belong to your package manager. *Check for Updates…* is in the menu bar icon (macOS) and the application menu.

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
