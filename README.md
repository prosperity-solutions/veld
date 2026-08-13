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
- **The project's own badges and buttons in the IDE** (`ide.extensions`) — a repo declares what its top bar carries: a badge for this branch's pull request, a menu that opens the worktree in WebStorm or VS Code, a coverage number. Each is backed by a command veld runs in that worktree, and **veld never learns your code host's name** — the command prints a tiny contract (`text`, `tone`, `href`, and the ids of actions to offer) and veld renders it, so a GitHub project ships a `gh` script and a GitLab project ships the same declaration with `glab` behind it. A badge whose tool is missing says so with your own install hint rather than vanishing, which is how a fresh clone teaches a new joiner what to set up. Bounded on purpose, because a badge is the only thing veld runs from a config without you clicking anything: only the worktree on screen, only while a window is open, one child process however many windows ask, no terminal attached (so a CLI that would prompt for credentials fails instead of hanging), a hard deadline, a floor on the refresh interval, a cap per project, every command logged — and one switch (Settings → General) that turns the unattended half off machine-wide
- **Named ports, with protocols** — `"ports": { "http": "auto", "admin": { "port": "auto", "protocol": "http" }, "debug": "auto" }` for debug adapters and multi-port containers, so nothing needs a hand-picked literal port that breaks parallel worktrees. Every `http` port gets its own hostname (`web-admin.dev.veld.localhost`); a `tcp` port is allocated and exported (`VELD_PORT_DEBUG`) but never routed, because a raw TCP connection carries no hostname to route on
- **Supervise a process that serves nothing** — `"ports": null` on a `long_running` node: an Electron shell, a file watcher, a background compiler. No port, no URL, no route — just a process veld starts, keeps in the graph, and stops with the rest. Readiness is still required; `{ "type": "settle", "seconds": 3 }` accepts "it was still running after 3s" when there is nothing better to probe
- **Structured output** — all commands support `--json` for scripting and CI
- **Browser dashboard** — management UI at `https://veld.localhost` with service health, logs, search, stop/restart
- **Every step's output is collected** — `command` nodes (a `docker build`, a `pnpm install`) log to their node's stream exactly like servers do, and project `setup`/`teardown` steps to the run's; nothing a step prints is thrown away. Lines also stream into `veld start`'s progress output as they arrive, instead of scribbling over it
- **Client-side logs** — captures browser `console.log/warn/error`, exceptions, and promise rejections; view with `veld logs --source client`
- **Internal logs** — liveness probe outcomes (with stderr), recovery decisions, health state transitions; view with `veld logs --source internal`
- **Resource monitoring that doesn't lie, including while a run is still starting** — the daemon samples every node's whole process tree every 5s, from the moment the run begins rather than once it is fully up: a dev server's boot-up allocation ramp is recorded, not skipped. `command` steps — builds, installs, codegen — are sampled too, every 2s, by the `veld start` that runs them, because their processes never outlive that command and no PID for them exists anywhere else. So "what did that `cargo build` peak at" and "how much does the dev server hold before it serves its first request" are questions with answers. (A `docker build` is the exception: the work happens inside `dockerd`/`buildkitd`, outside the step's process tree, so the client is all veld can see.) The headline memory figure is the tree's **footprint** (proportional set size on Linux, `phys_footprint` on macOS), because summing RSS over a tree counts each page shared inside it once per process, so a five-process `npm run dev` reported far more than it occupied. On Linux the footprint splits by page class — private dirty (the heap, what grows when a node leaks), private clean, shared dirty, shared clean, swap, wired — and every node splits by **subprocess**, so "which child is eating the RAM" is a question with an answer. `veld stats` shows it in the terminal (`--processes`, `--history`, `--cpu`, `--memory <metric>`); the dashboard expands any node into a scrubbable chart you can flip between memory and CPU, and between total, by-type and by-process. Where a bucket averages several samples, the chart plots the peak alongside the mean — a mean over a six-minute bucket hides the spike a 5s sample caught. Totals are kept 24h, per-process detail 2h (both reported by the API, so nothing hardcodes them); `VELD_STATS_MEMORY_DETAIL=off` and `VELD_STATS_CMDLINE=off` turn off the detailed probe and argv capture respectively — and because there are two samplers, each has to be set **twice**: in the *daemon's* environment (`launchctl setenv` / `systemctl --user set-environment`, then restart it; a shell `export` does not reach a launchd/systemd service) for long-running services, *and* exported in the shell you run `veld start` from for build/install steps, which the CLI samples. Verify against a `command` node, since a server node reads only the daemon's half — see [skills/veld/SKILL.md](skills/veld/SKILL.md)
- **Reverse-proxy header rules** — add or strip request/response headers on the local proxy and the public web gateway with a `proxy` config block (project/node/variant). Veld does no header manipulation by default.
- **Peer-to-peer sharing** — share a running environment with a colleague over an encrypted P2P tunnel (`veld share`); they open the same URLs on their own machine, and a shared `tcp` port (a database, a debugger) shows up as a local port on theirs. **Consent is per port**, declared in config, so a node can expose its app and withhold its ops console. Relays are configurable (public or self-hosted) for compliance. No accounts, no Veld-hosted server.
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
The two modes differ in three ways, all of them about what needs root. Unprivileged mode uses port 18443 in URLs; it only supports `.localhost` domains (RFC 6761), so custom apex domains (e.g. `{service}.mycompany.dev`) require `veld setup privileged` since they need `/etc/hosts` or dnsmasq management; and on **macOS** the keep-awake button covers a closed lid on mains power only — holding a MacBook awake with the lid shut *on battery* is `pmset` territory, which needs root (see the keep-awake paragraphs under *Management UI*). Everything else is identical. Linux is unaffected by that last one: its sleep inhibitor covers battery without privileges.

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
          "type": "long_running",
          "argv": ["npm", "run", "dev", "--", "--port", "${veld.port}"],
          "probes": { "readiness": { "type": "http", "path": "/health", "timeout_seconds": 30 } }
        }
      }
    },
    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "long_running",
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
| `veld update [--force] [--verbose]` | Update the whole release — CLI, daemon, helper, and on macOS Veld Desktop. Asks before closing a running app and reopens it afterwards; a non-interactive run leaves it alone. Never asks for a password: the privileged helper restarts itself on request over its own socket, and sudo is offered only if that and its binary watcher have both failed. Only one update runs at a time; `--force` takes over from a run you know is dead, `--verbose` shows the install script's own output instead of veld's step summary |
| `veld update --status [--json]` | What the update that is currently running is doing — who started it, which release, which phase. Installs nothing |
| `veld desktop [status] [--json]` | Where Veld Desktop is installed and whether it matches the CLI. `--json` also lists what this CLI can be asked to do (`capabilities`) |
| `veld desktop install` | Install the Mac app (macOS). Skips the Gatekeeper detour a browser download gets, since curl sets no quarantine flag |
| `veld desktop update [--relaunch]` | Update the installed app *only*, to this CLI's version. `veld update` covers this — reach for it when you want the app half on its own |
| `veld gc` | Clean up stale state and logs |
| `veld setup [unprivileged\|privileged]` | One-time system setup |
| `veld config [--path] [--files] [--why <pointer>] [--json]` | Print the config. `--files`: each `include` glob, the files it matched, and the nodes each defines. `--why`: one effective value and where it was defined (a `secret` is described, never printed) |
| `veld config vars [--json]` | Every machine-overridable var: effective value, and which scope it came from |
| `veld config set <name> <value\|--env N\|--file P\|--shell C> [--worktree]` | Answer a machine-overridable var on this machine. Shared by every worktree of the project unless `--worktree` |
| `veld config unset <name> [--worktree]` | Forget this machine's answer, falling back to the next scope and then the config default |
| `veld init` | Create a new veld.json (veld also reads veld.jsonc) |

## Configuration

### Step types

A node's `type` describes its **lifecycle only** — whether it runs to completion or stays running. Whether it serves anything is a property of its `ports`.

- **`long_running`** — a process veld supervises for the life of the run. By default veld allocates one port (`${veld.port}`), starts the process, and gates the graph on a readiness probe, which is mandatory. Declare `"ports": null` for a long-running process that serves nothing — an Electron shell, a file watcher, a background compiler — and it gets no port, no URL and no route. (`start_server` is the historical spelling and remains a permanent alias, exactly as `bash` is for `command`. Configs written either way load forever, and nothing nags you about the old one: a permanent alias sets no deadline, so there is deliberately no lint rule for it.)
- **`command`** — runs a command to completion. Can emit outputs by writing `key=value` lines to `$VELD_OUTPUT_FILE` (preferred) or via `VELD_OUTPUT key=value` on stdout (legacy, discouraged). Optional `skip_if` command for idempotency.

### Ports and protocols

`ports` has three authorings:

```jsonc
// absent  → one auto-allocated http port. The default, unchanged.
// null    → no ports at all: no allocation, no ${veld.port}, no URL, no route.
"ports": null

// a map   → named ports, shorthand or long form. `"name": null` erases one entry.
"ports": {
  "http":     "auto",                                   // primary → protocol "http"
  "admin":    { "port": "auto", "protocol": "http" },   // its own hostname
  "postgres": { "port": 5432,   "protocol": "tcp" },    // allocated, exported, never routed
  "debug":    "auto"                                    // secondary → protocol "tcp"
}
```

- **The default protocol is `http` for the primary port and `tcp` for every other**, so an existing multi-port config gains no new hostname the first time it runs on a newer veld.
- An **`http`** port gets a hostname and a Caddy route. The primary's `{service}` is the node name; a secondary's is `<node>-<port>` — `web-admin.dev.veld.localhost`, a sibling of the node's own hostname at the same depth, so a wildcard cert that already covers the node covers its extra ports too. Per-port `"host": "<template>"` overrides the template, which is the way out of a collision.
- A **`tcp`** port is allocated and exported as `${veld.ports.<name>}` / `VELD_PORT_<NAME>`, and deliberately not routed: a raw TCP connection carries no hostname for a proxy to demultiplex on, and every `*.veld.localhost` name already resolves to 127.0.0.1 anyway, so the port number is the whole address.
- `${veld.port}` is the primary — the port named `http`, the sole entry marked `"protocol": "http"`, or the sole entry when it states no protocol. Several ports with none of those is `ambiguous-primary-port`, a `veld lint` error rather than a guess.
- **`19899` is veld's own daemon port and is never given to a node** — auto-allocation skips it, and naming it explicitly is refused rather than substituted. A node that bound it would take the port from the daemon's next start, which fails later and elsewhere with nothing pointing back at the config.
- A port entry also carries its own `share` opt-in — see [Sharing](#sharing). Consent is per port, because a node that serves an app, an ops console and a database must be able to expose one of them without exposing all three.

All of this is additive within `schemaVersion: "3"` — there is no v4 and nothing to rewrite. [docs/adopting-long-running-and-ports.md](docs/adopting-long-running-and-ports.md) is the page to hand a coding agent if you want to adopt it, and it ends with the behaviour changes that can move under a config you never touch.

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
{ "type": "port", "port": "admin" }
{ "type": "command", "argv": ["curl", "-sf", "http://localhost:${veld.port}/ready"] }
{ "type": "settle", "seconds": 3 }
```

`http` and `port` probe the primary port unless `"port": "<name>"` names another. On a node with no such port they are a `probe-needs-port` lint error and fail the run rather than reporting healthy — an unknown `type` likewise (`unknown-probe-type`), because a typo must never be the quiet way to turn a probe off.

**`settle`** is the readiness probe for a portless `long_running` node. Its claim is deliberately weak and it says so: *the process was still running `seconds` after it was spawned* (default 3). That is worth having anyway, because it is raced against process exit exactly as the port probe is, so a node whose command dies on startup still fails the run instead of letting its dependents start behind a corpse. Prefer `{ "type": "command", … }` whenever the process publishes something observable — a socket, a built file, a pid file. `settle` is readiness only; as a liveness check it would report healthy forever, so veld rejects it there.

### URL template variables

| Variable | Description |
|----------|-------------|
| `{service}` | Node name — for a node's *secondary* `http` port, `<node>-<port>` |
| `{run}` | Run name |
| `{project}` | Project name from veld.json |
| `{branch}` | Current git branch (slugified) |
| `{worktree}` | Worktree directory name (slugified) |
| `{username}` | OS username |
| `{hostname}` | Machine hostname |

Fallback operator: `{branch ?? run}` uses the first non-empty value.

### Client-side log levels

Veld automatically captures browser `console.log`, `console.warn`, `console.error`, unhandled exceptions, and promise rejections from `long_running` nodes. Configure which levels to capture with `client_log_levels` at the project, node, or variant level (most specific wins):

```json
"client_log_levels": ["log", "warn", "error"]
```

Valid levels: `"log"`, `"warn"`, `"error"`, `"info"`, `"debug"`. Default: `["log", "warn", "error"]`. Unhandled exceptions are always captured regardless of this setting.

View client logs with `veld logs --source client` or filter by source in the management UI.

### Feature toggles

Control which Veld capabilities are injected into `long_running` nodes' HTML responses with `features` at the project, node, or variant level (most specific wins):

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

Veld also injects a `VELD_*` environment on **every** surface of a node — the node's process, its readiness and liveness probes, its `on_stop` hook, and `veld action`: `VELD_RUN`, `VELD_RUN_ID`, `VELD_ROOT`, `VELD_PROJECT`, `VELD_NODE`, `VELD_VARIANT`, plus `VELD_PORT`/`VELD_URL`/`VELD_PORT_<NAME>`/`VELD_URL_<NAME>`/`VELD_HOST_<NAME>` on a `long_running` node that has ports. A `command` probe also gets the node's declared `env` and outputs as `$KEY`, and so does an `action` on the node. Probe fields (`path`, `argv`/`shell`) are interpolated like the node's own `argv`/`env`. See [Environment variables](docs/configuration.md#env) in the config reference.

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

For `long_running` nodes with an `http` port, individual URL location pieces are also available (mirrors the Web URL API):

| Variable | Example | Description |
|----------|---------|-------------|
| `${veld.url.hostname}` | `app.my-run.proj.localhost` | DNS name only |
| `${veld.url.host}` | `app.my-run.proj.localhost:19443` | hostname:port (omits port if 443) |
| `${veld.url.origin}` | `https://app.my-run.proj.localhost:19443` | scheme + host (same as `${veld.url}`) |
| `${veld.url.scheme}` | `https` | Protocol scheme |
| `${veld.url.port}` | `19443` | HTTPS port (note: `${veld.port}` is the backend bind port) |

These are also available as cross-node references: `${nodes.backend.url.hostname}`, `${nodes.backend.url.host}`, etc., and in the node's own `on_stop` hook — so a container named after `${veld.url.hostname}` in `argv` is removed by the identical string at teardown.

`${veld.url}` is the **primary** port's URL. Every *other* `http` port has the same family under its own name — `${veld.urls.<name>}` plus `.hostname`, `.host`, `.origin`, `.scheme`, `.port`, cross-node as `${nodes.<node>.urls.<name>.origin}`, and in the environment as `VELD_URL_<NAME>`. A `tcp` port has a `${veld.ports.<name>}` but no URL, and `veld lint` says so by name rather than reporting an unknown built-in.

Ports and URLs for all `long_running` nodes are pre-computed before execution, so `${nodes.X.url}` works everywhere — even across nodes with no dependency relationship. Frontend can reference backend's URL and backend can reference frontend's URL without a cycle.

Availability is not uniform — a `command` node has no port or URL of its own, nor does a `long_running` node that declared `"ports": null`, a project `setup` step belongs to no node, and a `vars` value is one value for the whole run. `veld lint` reports a real built-in written where it is not populated (`builtin-not-in-scope`) rather than letting the run fail with `unknown built-in variable`. See [Availability](docs/configuration.md#availability).

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

All CLI/daemon state — run state, the project registry, service logs, per-node and per-process resource samples, feedback threads and screenshots, relay auth tokens, the desktop worktree registry, and the IDE's pane layouts — lives in one SQLite database at `<data_dir>/veld/veld.db` (macOS: `~/Library/Application Support/veld/veld.db`; Linux: `~/.local/share/veld/veld.db`; override with `VELD_DB_PATH`). The file is `0600` (it holds secrets) and runs in WAL mode, so the CLI, daemon, and detached log writers read and write concurrently without file locking. The schema is versioned (`PRAGMA user_version`) and migrates forward automatically on upgrade — a CLI update never orphans or stops running environments because the data shape changed. A database created by a *newer* veld is refused with an error instead of being modified.

## Extensions

### Management UI

Veld includes a browser-based dashboard at `https://veld.localhost` (or `https://veld.localhost:18443` in unprivileged mode). It shows all environments with:

- **Services tab** — nodes with health status indicators, URLs with copy/open, variant, PID, and live resource usage (memory footprint + CPU per process tree, with a sparkline). Click a node's stats to expand a **scrubbable resource chart**: switch between **memory and CPU**, pick the window (5m → 24h) and the memory metric, and choose whether to plot the total, the page-class split, or one band per subprocess — with the live process table (pid, CPU, footprint, RSS, cumulative CPU time) below it. On a stopped environment it shows the last run's final node states
- **Logs tab** — terminal viewer with search + highlighting, context lines (grep -C), auto-scroll, node filter, source filter (server/client/all), and a run picker to read past runs' logs (latest, any ended run, or all interleaved)
- **Run history** — a top-level **Active | History** switcher keeps the everyday view to live environments; ended ones live under History with their last run's outcome (`crashed`/`failed` in red) and a one-click Restart. The **history horizon** (Settings → General) keeps that list short: it defaults to the last **3 days**, and anything older is hidden from History and from the past-run pickers — with a line saying how many, because a list that quietly omits things is indistinguishable from data loss. Clear the field to show everything. Nothing is deleted by it, and nothing is kept longer either: housekeeping already removes ended runs after 7 days, which is why 7 is the maximum
- **Stop/Restart** — control environments directly from the browser
- **Sharing** — start/stop peer shares and public web shares per run; each live tunnel shows its transport (`direct`, or `relayed via <relay>` — throughput capped by the relay) so slow shares are diagnosable at a glance. The two kinds of share say which they are: **Share privately** is the direct peer tunnel (they need Veld), **Share to the web** is the public URL (they need a browser). A public web share is laid out as a **row per service** — the URL, *Copy link*, *Copy link + QR*, *Copy QR*, and a **QR code shown right there** for opening it on a phone without retyping. The code encodes the same link the copy button gives you, password fragment included, so treat it like the password it carries. *Copy link + QR* puts both the text and a picture of the code on the clipboard, so pasting into Slack gives your reader something to point a phone at (which flavour a chat app takes is its own choice — the text is still on the clipboard), and *Copy QR* is the image alone, for sending the code after the link

Open it with `veld ui` or visit the URL directly.

An experimental second-generation management UI is served at `/ide` (worktree mode: import git repositories, manage `git worktree` checkouts, and drive veld runs per worktree). It is also the web core of **Veld Desktop**, an Electron shell in `desktop/` — see [desktop/ARCHITECTURE.md](desktop/ARCHITECTURE.md).

Opened with no projects, `/ide` is a **start screen** rather than an empty page: the Veld wordmark, three things Veld is for, and a single *Import your first project* button (which the top bar carries too, in place of the project selector there is nothing yet to select between). Once you have a project, Veld occasionally shows a short **What's new** card — only for changes that alter how you work, never for fixes or flags. Closing one with *Got it* marks it read; dismissing it with Esc stops it reappearing but keeps it on the count, so clearing a dialog mid-task doesn't lose it. The ⋯ menu at the **end** of the top bar carries a dot and an unread count, and *What's new…* there reopens everything any time. News that shipped before your first session is never put in front of you — updating into a new version doesn't dump a back-catalogue on you — but it is still there to read. See [docs/promotions.md](docs/promotions.md).

**Working several projects at once has its own column.** Turn it on with **⌘B**, or the stacked-layers button in the top bar (left of the project selector, `ui.showProjectColumn`, **off** by default — most installs have one project and a column of one square answers nothing) and every project gets a square down the left edge, beside the worktree rail. **⌘1…⌘9** go straight to one, **⌘\`** flips back to the one you came from, and squares are **dragged to reorder** — that order is the daemon's, so it is the same in every window, which is what makes ⌘2 mean one project rather than two. A square is the project's initials — greyscale, so it never competes with the worktree markers beside it, and there is no marker to pick. Right-click one for switch / open-in-a-new-window / remove, and the `+` at the bottom imports another.

The column exists because of the thing a menu cannot do: each square carries the same activity glyph the rail puts on a worktree — an ear when something is *waiting for you*, a triangle when a command *failed*, a tick when something *finished* — aggregated over that project's checkouts, worst-state-first, and it is **on screen without a click**. A second, quieter dot in the corner means some other window or browser tab already has one of that project's checkouts.

**The project menu carries every project action**, and is the whole surface when the column is off. First in the top bar: *Import repository…*, then one row per project, each opening a submenu with **Open**, *Open in a new window* (*Open in a new tab* in a plain browser — a real second view, since a tab takes part in ownership exactly like a window), *Move up* / *Move down*, and *Remove project…*. Move up/down are the keyboard's half of the column's drag and write the same order, so either one moves ⌘1…⌘9 with it. Each row shows its own activity glyph and either where the project already is — *open in Veld Desktop*, *open in Chrome* — or the ⌘N that reaches it; the away note is a warning rather than a reason to grey the row out, because picking one of that project's held checkouts is exactly what brings the other window forward. A dot on the selector button says one of the projects you are **not** looking at has news; it ignores *running*, since something merely in flight somewhere would light it more or less permanently. Going to a *specific* checkout in another project is ⌘K's job: it lists every project's worktrees now, not just the selected project's.

⌘1…⌘9 are **Veld Desktop only** — Chrome and Safari reserve them for their own tabs and a page cannot intercept them. ⌘\` is best-effort in both: macOS and most browsers bind it to "cycle windows in this app", and where they do, that wins.

The rail down the left side is the list of checkouts *in the selected project*, grouped into **groups** you name. **Each group has its own "＋"**, so a new worktree is created straight into the section you want it in rather than created and then dragged. Name it whatever you like — "Hello test", "Checkout V2 (final)" — and that is what the rail shows; an identifier (`hello-test`) is derived underneath for the branch and the run name, and the dialog shows you both before it creates anything. Either can be changed later from the row's ⋮ menu, and clearing the name goes back to showing the identifier.

It has **terminal panes**: real shells in the selected worktree's directory, in a dock of two tab strips you can split, reorder and drag tabs between. Dragging a tab onto a pane's **left or right edge** splits there, and dropping it anywhere else in a pane moves it into that pane — the same gesture, with the target read off where you let go. A tab strip is one keyboard stop: `←`/`→` move between its tabs (`Home`/`End` to the ends) without switching panes as they go, `Enter` switches to the focused tab, and `Delete` closes it. The shell runs in the daemon and reaches the browser over a WebSocket, so terminals work in a plain browser and not only in the Electron app.

Sessions outlive the page. Reloading (or Electron reloading its window) reattaches to the same shells with their scrollback intact, and output produced while you were away is replayed — a build keeps running and keeps logging. `Shift+Enter` inserts a newline instead of submitting, which is what Claude Code and other coding agents read as a line break in their input box.

They also **outlive the daemon**. `veld update` restarts it, and a daemon can crash; neither ends your shells. Each terminal's pseudo-terminal is owned by a small holder process of its own rather than by the daemon, so the new daemon finds the running shells and picks them back up with their scrollback — an update is no longer something to schedule around, and an agent working in a pane keeps working through one. Quitting **Veld Desktop** and reopening it restores your panes too, so the tabs come back attached to the same shells. A reboot is the one thing nothing survives.

A dropped pipe is usually the same story: the machine slept or a proxy timed out, and the shell is still running. So when the connection to a terminal breaks, Veld **reconnects on its own** a few times (near-immediately first, then with a backoff — Settings → Terminal → Auto-reconnect) before it settles and offers the Reconnect button, so a transient blip never interrupts a build in front of you.

Each open terminal has a small holder process of its own, and its socket lives in `~/.veld/pty-<daemon port>/` — `veld doctor` reports how many there are, which is where to look if a terminal did not come back. **It only knocks on them when the daemon is down**, and says so in the row: connecting to a holder is how a daemon arrives, so a diagnostic that knocks while your terminals are attached is a diagnostic that can cost you one. With no daemon there is nothing to disturb, and that is also the moment the answer matters — a socket nobody answers is a holder that is gone, and the next daemon start sweeps it. If that directory's path is too long for a unix socket (104 bytes — a deep `$HOME` can reach it), `veld doctor` fails that check and names `VELD_PTY_DIR` as the way out, rather than leaving you with a terminal that will not open.

**Closing a terminal pane** is what ends a shell; closing the browser window or quitting the app only detaches, and a session nobody comes back to is hung up after 30 minutes by default (Settings → Terminal) — as is one whose daemon never returns, so nothing is left running for a daemon that is gone for good. If a foreground process is still running in a terminal, closing it asks first — the same "are you sure, that process will be terminated" a real terminal asks — so an accidental X on a tab running a build or an editor doesn't lose it. Opening the same terminal in a second window takes it over rather than mirroring it, so there is only ever one writer. Up to 48 sessions can be live at once; past that, opening a terminal reports the limit and closing a pane frees a slot. Selecting a worktree no longer opens a terminal by itself: its pane offers the project's panes, a plain shell and the run's URLs, so browsing the rail costs nothing and a shell starts when you ask for one.

**A URL in the terminal opens in a pane.** Addresses in the output are links: click one and it opens as a browser tab in the same dock, next to the shell that printed it. The same goes for a program *in* the terminal that wants to open a browser — Veld points `$BROWSER` at itself, which is what Claude Code, `gh`, `git`, vite and next all consult, so an agent's login link or a dev server's `--open` lands in a pane instead of pulling you out of the app. (`veld open-url <URL>` is the same thing to invoke by hand, or from a script.)

**Terminals are polite about finishing.** The basics of terminal ergonomics are covered: a `BEL` rings the bell (volume set in Settings → Terminal), a process can rename its own tab with the terminal title it sets (OSC 0/2), and the OSC 9 "notify" sequence raises a notification — and rings the bell with it, at the same volume, because "notify me" is the stronger of the two sequences and should not be the quieter one. A notification names the **worktree and pane** — and the **project**, when the event came from one you do not have selected — so it is navigable across several open checkouts, and clicking it focuses the pane that produced it. The pane is named by *its own* name — "Terminal 2", or a config pane's `label` — never by the title the shell set for itself: a shell renames its tab to the command it is running, which is useful in the strip you are looking at and noise in a banner you read an hour later on another desktop — the in-app toast works in the window you are in, and the OS banner (with the same click-to-focus) surfaces whether the worktree lives in this window, another Veld window, or a browser tab. It is deliberately **stateless** — there is no inbox, nothing is persisted, and it is not a dashboard of what an agent is doing; it is just "something in a terminal finished". A plain terminal (a login shell) always adopts its title and notifies; a config-declared pane keeps its configured `label` unless the repo opts in with `allow_terminal_renaming`, because a fixed label is how you navigate a rail full of agent panes.

Two escape hatches, because a pane is *not* the browser you are logged into — it has its own cookie jar, so an SSO flow started in one begins from scratch. Hold ⌘/Ctrl while clicking a link to send that one to your real browser. And an **exempt list** of origins always goes there: yours in Settings → Links, and the project's in its `veld.json` (`ide.externalOrigins`), unioned rather than one overriding the other — you name the sign-ins you use, the repo names the ones its app goes through. The whole behaviour is one switch (Settings → Links) if you would rather every link left the app.

**Tools that call `open`/`xdg-open` directly are caught too**, which matters because that is what an agent's shell tool does — `Bash(open "https://…")` never looks at `$BROWSER`, and Claude Code even sets `BROWSER=true` for its children. Those need Veld's shim directory on `PATH`, and `PATH` set before your login shell starts does not survive it: macOS's `path_helper` (in `/etc/zprofile`) rebuilds `PATH` with the system directories first, and Debian's `/etc/profile` overwrites it outright. So `PATH` has to be set *after* your startup files, and Veld arranges that without editing any of them:

> **zsh** — Veld points `ZDOTDIR` at a directory of its own containing **one** file, a `.zshenv`. That file hands `ZDOTDIR` straight back, sources your real `~/.zshenv` (whose place in the startup order it took), and registers a `precmd` hook that prepends the shim directory. Your `.zprofile`, `.zshrc` and `.zlogin` are then read normally, in order, unmodified, with your own `$ZDOTDIR` visible to them.
>
> **bash** — bash has no hook that runs after its rc files (`BASH_ENV` is non-interactive shells only, and `--rcfile` is ignored for the login shell a terminal opens). What it does have is posix mode: started with `--posix`, an interactive bash reads `$ENV` and *no other startup file*. So Veld takes that one file, leaves posix mode on its first line, replays your own startup in bash's documented order — `/etc/profile`, then the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile` — and adds its line at the end, where nothing can rebuild `PATH` after it. `shopt login_shell` stays true throughout, so your files see the shell they expect.
>
> **…and then `~/.bashrc`, which is the one place Veld deliberately departs from bash.** A *login* bash never reads `~/.bashrc`; the convention is that your profile sources it. macOS ships no `~/.bash_profile` at all, so a machine with `~/.profile` and `~/.bashrc` gets a login bash with none of your aliases or functions — which would make picking bash here load none of the config you picked it for. Veld therefore sources `~/.bashrc` too, **unless the profile it just sourced already mentions it**, so a conventional setup is never sourced twice. Every other terminal emulator runs a plain login bash and stops at the profile; Veld does not, because choosing bash in this picker is a statement about where your config lives.

In both cases Veld owns one file, in its own directory, and never writes to your home.

The shims route a single http(s) URL and hand **anything else** to the real tool untouched, with the original arguments — `open .`, `open report.pdf`, `open -a Safari …` behave exactly as they always did.

**bash support is probed, not assumed.** macOS still ships bash **3.2** as `/bin/bash`, and that version ignores `$ENV` in posix mode entirely — so Veld asks your bash — a ~10ms `bash --posix -i -c ':'` with `$ENV` pointed at a marker file, cached per binary — and only uses the handoff on a bash that honours it. A Homebrew or Linux bash 4+ works; the system one on macOS does not.

Everything else — fish, nushell, and a bash too old for the handoff — keeps `$BROWSER` and can opt in by hand with one line, since every Veld terminal exports the directory:

```sh
# ~/.bashrc — route open/xdg-open inside Veld terminals to embedded panes
[ -n "$VELD_SHIM_DIR" ] && PATH="$VELD_SHIM_DIR:$PATH"
```

**You do not have to guess whether it worked.** *Settings → Terminal → Shell* asks your actual shell what `open` resolves to and, if the shim did not win, says so and shows the exact line to paste and the file to paste it in. `veld doctor` reports the same thing, naming the mechanism in force (`ZDOTDIR`, `$ENV`, or none) for the shell you chose.

Anything that runs inside your shell's startup gets an off switch: *Also catch programs that call open / xdg-open* (Settings → Links). Turn it off and the `open`/`xdg-open` wrappers leave your `PATH`; turn off *Open links from the terminal in Veld* and the `$BROWSER` half goes too. Both take effect for new terminals; a shell already open keeps the environment it started with.

That one file Veld owns is also where [the worktree inbox](#the-worktree-inbox-what-happened-while-you-werent-looking)'s command markers come from, and the two features are **separately switchable** even though they share the mechanism: each half of the file runs only when its own variable is in the environment. The first version gated them together, so turning off *Also catch programs that call open* silently removed the rail's activity glyph — a coupling nothing in either setting's description implied. With every switch that rides the file off, Veld sets no `ZDOTDIR` and no `$ENV` at all.

`veld doctor` reports whether this is actually working, because its failure mode is otherwise silent: the little scripts are written once when the daemon starts, and they carry the absolute path of the `veld` CLI belonging to that daemon — a sibling binary for a dev build, `<prefix>/bin/veld` for an installed one. A daemon that can find neither (a moved install, an interrupted update) leaves the feature off, and the row says which case it is and what fixes it — restarting the daemon, or reinstalling.

### The worktree inbox: what happened while you weren't looking

Twelve open panes across four worktrees, and the only way to find out whether the build passed was to go and look at each one. So each worktree's row in the rail carries **one glyph for what its terminals have to say**, worst-state-first: an ear when something is *waiting for you*, a triangle when a command *failed*, a tick when something *finished*, a spinner when something is *running*. Not a count — nobody acts differently on three unseen events than on five, so the row's scarce space goes on which *kind* of news. The pane's own **tab** carries a dot too, so you can tell which one.

**Looking at a pane reads it**, and so does typing in it — answering a `sudo` prompt in place makes the mark go away without a second gesture. To dismiss a whole worktree, *Mark read* in its right-click menu. Clicking the glyph does nothing special: it selects the worktree, like any other part of the row.

It is an inbox, not a status light. A status light answers "what is this pane doing", which you can see by looking at it. This answers the question you cannot: *of the panes I am not looking at, which finished, which failed, and which needs me?*

Two things fill it, each with its own off switch, and both **on** by default:

- **Plain shell commands** — *Settings → Activity → Notice when a command finishes* (`terminal.shellIntegration`). Veld registers two hooks in the shell it opens, which print an invisible marker when a command starts and when it ends, carrying the exit code. So `cargo build` in a pane you walked away from shows up as finished or failed, exactly, with no guessing. A shell sitting at a prompt never counts, and neither does a watcher that has not ended — `pnpm dev` and `tsc --watch` are silent until you stop them. zsh, and bash 4.4 or newer (the same probe as above: macOS's own `/bin/bash` is 3.2 and cannot carry it).
- **Coding agents** — *Settings → Activity → Notice when a coding agent is waiting for you* (`terminal.agentIntegration`). Works in an ordinary terminal **and in a config-declared pane** (`ide.panes`), for **Claude Code, Codex CLI and Pi**. None of the three's output says whether it is thinking or waiting — measured: Claude's inline TUI emits a title sequence, hyperlinks and a progress report, and nothing about its state. So being told is the only honest way to know. Veld puts a wrapper on the terminal's `PATH` in front of the real binary, handing it an extra, throwaway hook configuration; a Claude session stopped at a permission prompt then shows amber beside its worktree, and a turn that merely *ended* counts as finished, not as waiting — the difference between a badge you trust and one you learn to ignore. **Only the session reports, never its sub-agents.** A Claude session that farms work out to sub-agents announces each one finishing, and none of those is yours to act on: the reported state is the session's turn, so a sub-agent starting, finishing or failing produces no news and does not touch the pane's *running* spinner. Its counterpart survives on purpose — a sub-agent that needs an answer still shows as *waiting for you*, because the question is whether you have something to do, not which agent asked. **Codex tells veld less, deliberately**: it also has a richer `hooks` system with signals as fine-grained as Claude's, but using it means either an interactive "new hook, review required" prompt the first time Codex sees it, or a flag that waives that review for *every* hook configured, not just Veld's — both worse than the gap. So Veld uses Codex's older `notify` hook instead, which fires on exactly one event, the end of a turn: a Codex pane can go straight from *launched* to *finished* with no *waiting* signal in between, and that's the trade. **Pi has no gap to trade around, and no signal to fill it with either**: it has no permission prompts, no plan mode, and no built-in tool approval at all, so there is nothing waiting ever fires on — not a choice Veld made, a fact about Pi. What it does have is a run-level pair, `agent_start` and `agent_settled`: `agent_start` is when the agent starts working on a prompt, and `agent_settled` is Pi's documented signal that the run is fully settled — no retry, compaction retry, or queued follow-up remains — which is the true end of the run. That is the same whole-run shape Claude's `UserPromptSubmit`/`Stop` cover minus the permission prompt: a Pi pane goes *launched* → *working* → *finished* once per prompt, and *finished* arrives exactly when the agent actually became idle — not, as a per-turn signal would, after every single step of a run still in progress.

**Your own configuration is never edited.** No `~/.claude/settings.json` merge, no `.claude/` written into your project, no `~/.codex/config.toml` touched, no `~/.pi/agent/settings.json`, `.pi/settings.json`, or either of Pi's own extension directories (`~/.pi/agent/extensions/`, `.pi/extensions/`) touched either. For Claude, the hooks live in a file Veld owns inside its own per-daemon directory, named after the terminal session and rewritten on every launch, and `--settings` *merges* into Claude Code's normal hierarchy rather than replacing it — a `notify` hook of your own in `~/.claude/settings.json` still runs alongside Veld's. For Codex there is no file at all: Veld passes `-c notify=[...]`, which overrides that one config key for that one launch. **This one is a replacement, not a merge** — if you have your own `notify` configured in `~/.codex/config.toml`, it does not fire in a Veld terminal, silently, for as long as this feature is on; there is no cheap way to chain the two without Veld resolving and re-invoking your own notifier itself, which it does not do today. Pi has neither a settings key nor a config override to reach for — the only hook mechanism it has at all is an extension module, loaded with `-e <path>` — so Veld writes one, into the same per-daemon directory, and loads it once. `-e` is documented as repeatable and purely additive, the same "nothing of yours is touched" property `--settings` gives Claude, by a third route: code on disk instead of a JSON merge or a config replacement. All three wrappers are also deliberately hard to reach: each passes through untouched for anything that is not a plain interactive launch (`claude mcp …`, `claude update`, `claude -p …`, `codex exec …`, `pi install …`/`pi update …`/`pi -p …`, or a settings/config/extension flag of your own), and if it cannot find the real binary, or Veld cannot produce the hook configuration, it `exec`s the real tool exactly as you typed it. `codex resume`/`codex fork` count as interactive too, not as subcommands to leave untouched — otherwise the `ide.panes` resume example just below would silently get no badge.

A third switch, *Show what is working* (`activity.showWorking`, **off**), adds a spinner for a worktree with a command running in it. Claude reports it properly: its wrapper says *"an agent lives here and it is idle"* before it even starts — the one fact only the thing launching it knows, and what stops a pane running an agent looking like a pane running a long shell command — and a hook then says when each turn begins. **Codex only gets the first half.** Its wrapper reports the same "idle" launch, but nothing ever tells Veld a Codex turn *started* — so with this switch on, a Codex pane shows no spinner for its entire working phase, not just before its first prompt. That is worse than the plain shell fallback this setting replaces (which would have spun for as long as `codex` stayed running), and it is the direct cost of the `notify`-over-`hooks` trade above. **Pi gets both halves, like Claude**: its wrapper reports the same idle-at-launch, and its `agent_start` event says when each run actually begins — a different mechanism (an extension event, not a hook) landing on the same result. An agent Veld has no integration for at all still reports nothing either, which is why this ships **off**: a spinner that is right for builds, for Claude and for Pi but blank for Codex and for anything else is not something to put in front of everybody by default. Turn it on when what you want is "is that still going", and expect it to under-report for Codex specifically. Either way it is the quietest thing the rail shows: it loses to every unseen event, so a worktree with an agent waiting still reads as waiting.

These switches are **independent of each other and of the link-routing pair above**, even though they reach your shell through the same one file Veld owns. Turning off *Also catch programs that call `open`* does not turn off the rail glyph.

**And it can tell you when you are somewhere else.** *Settings → Activity → Notifying* is a five-row table — a command finished, a command failed, a coding agent is waiting for you, a coding agent finished, a program asked to be noticed — because those are not the same event and one switch for all of them is wrong in one direction or the other. The agent rows say *agent* on purpose; the last row is any program ringing the terminal's notification sequence, which Veld cannot tell apart from one merely *waiting* for input (that is not observable from a browser at all, so a program has to say so itself). Everything is on except *a command finished*, which is news the rail already carries rather than a reason to interrupt you. **One channel, chosen by where you are**: a focused window gets an in-app toast — the right weight for something you can already see — and an unfocused one gets the OS banner, which is the only thing that reaches across windows and apps. Never both, and clicking either focuses the pane. Worth knowing before you leave *a coding agent finished* on — for Claude and Codex the end-of-turn signal fires after every *response*, so a long session can ring more than you expect; Pi's fires once per settled run instead, so a Pi session does not.

**And it reaches across projects.** A coding agent's state is reported to Veld by the agent itself, so it arrives wherever you are — including a window that has a different project selected. Such a notification names the **project** as well as the worktree and pane, because branch names and rail markers repeat across projects by design (two projects both on `main` is the ordinary case), and clicking it switches project, selects the checkout and brings its pane to the front. There is one honest limit: what a *shell* reports — a command finishing or failing, a program ringing the notify sequence — is read by the terminal in the window that has that pane open, so a project **no window is showing at all** produces coding-agent events only. Two windows, one project each, is unaffected: each window reads its own panes.

**Focus mode is the top-bar toggle for all of that, between search and settings** (`focus.enabled`, **off**). On, it silences whichever of three channels *Settings → Activity → Focus mode* has checked — the terminal bell, the in-app toast, and the OS banner — all three checked by default the moment the master switch goes on. It reaches exactly the channel described above and nothing else: a toast reporting something you clicked yourself (a failed action, a copy confirmation) is not an interruption from elsewhere, so it is never gated by this. A suppressed toast or banner is **discarded, not queued** — both ride the same OSC 9/133 marks the rail glyph is built from, so the rail is already the permanent record that something happened, and a second one held back for a summary popup is more likely to be ignored than read once it finally appears. The bell is different: a plain terminal BEL was never one of those marks and never reached the rail, on or off — silencing it removes exactly what it always was, a sound in the moment and nothing more. And "OS-level" means only the banner Veld's own notification path would have raised (Veld Desktop's native notification, or a browser tab's Web Notification) — it is not a system-wide Do Not Disturb, and nothing else on the machine is muted.

**And if you are walking away, the coffee cup beside search keeps the machine awake.** A run that outlives your attention — an overnight agent, a long build, a watcher — dies when the laptop suspends, and the machine suspending is not something a run can do anything about. The button is a menu rather than a plain toggle, because *how long* is the interesting half: 30 minutes, 1, 2, 4 or 8 hours, or until you turn it off. Turning it off is the first item in the same menu once it is on, and the tooltip carries the time remaining. It is **machine-wide, not per run and not per window** — the thing being held awake is the laptop — so every browser tab and every Desktop window agrees, and switching it off in one switches it off everywhere.

Under it, veld holds the inhibition the way each platform supports: `caffeinate -s -i` on macOS, and a `systemd-inhibit` lock over `handle-lid-switch:sleep:idle` on Linux. **It can never outlive the daemon** — the inhibitor is wrapped around a pipe veld holds, so the daemon exiting for any reason, including being killed outright, releases it; nothing is persisted, so a reboot never comes back with the machine pinned awake by a choice you made last week. On a machine with no way to ask at all (a Linux box without systemd) the button is greyed with the reason rather than failing on click.

**A closed lid on battery is the one case that needs root, and only on macOS.** `caffeinate -s` is valid on AC power only, and macOS offers no unprivileged API for the battery case at all — the single lever is `pmset -b disablesleep`. So veld reaches it through the privileged helper, when you have one: with `veld setup privileged` the coffee menu covers a shut lid on battery too, and says so; without it, the menu says the machine will still sleep on battery and names the command that changes that. Linux needs none of this — `handle-lid-switch` is a real logind inhibitor and holds on battery already.

That lever is a *durable* system setting rather than an assertion, which is the one thing worth understanding about it: set it and walk away and you have a Mac that never sleeps, with nothing running to explain why. So veld never simply sets it. **The helper holds it on a short lease that the daemon must keep renewing** — the daemon renews every 30s against a 90s lease, and the helper's watchdog puts the setting back the moment renewals stop. A daemon that is killed, wedged, updated or uninstalled stops renewing and the Mac sleeps again on its own; a helper that is itself killed mid-lease clears the setting when it comes back, rather than inheriting one nothing is tracking. Every failure path lands on "the machine can sleep", which is the only safe direction for a setting that survives a reboot. If the lease is ever lost while the keep-awake is still on, the menu stops claiming battery coverage instead of quietly leaving you with a promise nobody is keeping.

**Config-declared panes work too, and they get there differently.** An `ide.panes` entry runs as `<shell> -l -i -c '<command>'`, and `-c` never prints a prompt — so the OSC 133 hooks, which fire around a *prompt*, never run in one. A custom pane therefore reports *finished*/*failed* from its process exiting rather than from a command mark, which is the same answer by another route. What it does get is the agent half: Veld prepends its shim directory to `PATH` inside that command. **That line is a zsh fix and a no-op for bash**, measured both ways — bash puts the directory on `PATH` with a plain assignment in the file it reads, which a `-c` shell reads fine, while zsh does it from a prompt hook that never fires. Without it, a pane declared as `{"argv": ["claude"]}` under zsh silently bypassed the wrapper and reported nothing: the coding-agent half was off for exactly the panes most likely to run an agent.

**Unread events survive a page reload** — kept in the tab's own `sessionStorage`, because a reload is exactly the moment you were not looking. What does *not* come back is the live "something is running" state: that is a claim about a process right now, and a reload is when it stops being knowable.

**What it cannot do.** The markers are read by the terminal in your window, so events exist only while a window is *open*: close the tab and its news is gone with it. **The *running* spinner follows the session's work, and for Claude and Pi work only ever begins with a prompt from you** — that is the only start signal either offers that doesn't fire once per tool call, and Codex offers none at all. So a turn that starts for some other reason, most visibly an agent picking work back up after a background command it left running, reads as *finished* until you next type something. It is the spinner that is missing, not an event: nothing false is filed, and the next real end-of-turn reports normally. **Pi never reports *waiting for you* at all** — it has no permission-prompt system to observe, so a Pi pane can sit at a genuine question with the badge still saying *finished* from its last run; there is no signal to promote instead, unlike Codex's deliberate `notify`-over-`hooks` trade above. *Failed* is command-only — none of the three agents exposes a clean per-turn failure event. Claude reads as *waiting for you* when it errors, which is what it usually is; Codex and Pi both read as *finished* regardless of outcome, because each one's end-of-run signal (`notify`, `agent_settled`) fires on completion without distinguishing success from failure, and neither has a waiting signal to fall back on either. Codex's payload also briefly travels as a command-line argument rather than on a pipe — Codex builds it that way, not Veld — so for the moment `veld agent-state` is running, it (and the short conversation preview Codex includes in it) is visible to anything else on the machine that can read a process list, the same as any other program's arguments; Claude's stays on a pipe the whole time. Pi's payload travels the same way Codex's does, but that one is Veld's own choice rather than Pi forcing it, and there is nothing in it more sensitive than an event name and a shutdown reason. And in the one setup where no command mark ever arrives to hand the pane back — shell integration off, or a shell its zsh/bash-4.4+ half does not cover, so nothing tells Veld a Codex *session* (as opposed to a turn) has ended — the pane keeps a hook's claim on it for good once Codex has spoken once, even after you quit `codex` and go back to typing plain commands in the same shell. It still reports each turn correctly while `codex` runs; what it loses afterwards is the working spinner (see above) and any later terminal-bell notification in that same pane, which a hook-claimed pane never shows. Quitting the shell, or closing and reopening the pane, both clear it. Config-declared panes, and any shell `terminal.shellIntegration` actually covers, are not affected — both have another way to learn the session ended (the pane's own process exit, or a command mark). **Pi does not have this problem**: its `session_shutdown` event fires with a real reason when you actually quit (`/new`, `/resume` and `/fork` — which continue in the same pane — are told apart from a real quit and correctly report nothing), so a Pi pane hands itself back the ordinary way and never needs the shell-exit fallback at all.

**Which shell a terminal opens** is *Settings → Terminal → Shell*, and it defaults to **Automatic** — your login shell, the one `$SHELL` names. Pick another when that is not the shell you actually work in: macOS has shipped zsh as the login shell since Catalina, so someone whose aliases, completions and tool integrations live in `~/.bashrc` was getting a terminal that loaded none of them. The list is what this machine has (`/etc/shells` plus what is on `PATH`), and *Custom path…* takes an absolute path for anything not on it. Veld never guesses from your rc files — a stale `~/.bashrc` sits in nearly every home directory, and switching a contented zsh user's terminals on that evidence would be worse than the problem. The choice applies to new terminals and to config-declared panes; a shell already open keeps the one it started with. It is also the shell veld consults to learn your `PATH` for commands it runs on your behalf, so a `PATH` you build in `.bashrc` is found too.

**Settings** (`⌘,`, or *Settings* in the ⋯ menu at the end of the top bar) covers which shell terminals open, terminal font, cursor, scrollback, bell volume, the Shift+Enter behaviour, **how a dropped connection auto-reconnects** (how many attempts, how soon the first is, and the backoff between the rest), what Veld notices about finished commands and waiting coding agents, what it shows in the rail, and which of those send you a system notification, whether terminal links open in a pane, whether `open`/`xdg-open` are caught as well, and which origins are exempt, how long detached shells are kept, whether the project column is shown, how worktrees are marked in the rail, where new worktree checkouts are stored, how long trashed worktrees are kept before they are deleted for good, how far back the run history views reach, which time zone log timestamps are shown in, which quick switches a browser pane's toolbar carries, and whether top-bar actions that currently can't fire (restart with nothing running, the machine-vars button for a project that asks for nothing, a URLs button with nothing to open) are hidden rather than shown greyed. They sit in five groups picked from a sidebar — **General** for the settings that belong to no larger surface (markers, the trash horizon, the history horizon, the log time zone, the hide-vs-greyed choice), then **Git** (where a new worktree's branch starts from, and where new checkouts are stored), **Terminal**, **Links** and **Browser panes** — so one group is on screen at a time rather than every setting on one scroll; on a narrow window the sidebar becomes a select. They are stored by the daemon rather than the browser, so Veld Desktop and a browser tab against the same daemon agree, and every window sees a change. There is no Save button — each control writes as you change it.

**Creating a worktree starts with its name.** *New worktree…* asks first what this checkout is *called* — `Checkout V2` — and derives the rest: the rail name (`checkout-v2`) and the branch, both shown in the dialog before anything is created, because both derivations drop characters a git ref or a hostname cannot carry. Umlauts and accents are transliterated rather than thrown away — `Größe ändern` becomes `groesse-aendern`, `café` becomes `cafe` — since `groesse` is what a German speaker writes when they have to reach ASCII, and `gr-e` is not. The branch follows the name until you type over it, and clearing that field hands it back to the derivation; unticking *Create the branch* switches it to checking out a branch git already has, named exactly. The rail marker is picked in the same dialog rather than on a second trip through the context menu, and opens on a **random free** colour and glyph — random rather than "the next unused one", which proposed the same marker for every new checkout in a repo and made a week's rail look dealt off the top of the deck. If the name you typed collides with a checkout this repo already has, the dialog says so before the button rather than quietly appending a `-2`.

**New worktrees are never born behind the remote.** By default a created branch is cut from **origin's latest default branch** (Settings → Git → *Create worktrees from*): veld fetches the remote first and bases the new branch on `origin/main`, so a checkout does not silently start from a `main` that has not been updated in weeks — missing the latest database migrations and guaranteed to conflict with open PRs. Set it to *Local main* to cut from the main checkout's current HEAD instead (offline, or deliberately basing on un-pushed local work); offline, veld falls back to local HEAD rather than failing the create. The dialog says which it is doing.

**Where new checkouts land is a setting, not a hardcode** (Settings → Git → *Worktree storage location*). *Next to repository* is the default and today's only past behaviour: a checkout lands in a `_worktrees` folder beside its repo. *Custom location* points every new checkout, for every repository, at one folder you choose — pick it with the same native folder picker *Import repository* uses, or open it straight from the setting once it is set. Either way each repository gets its own subfolder there, named from its directory and a short hash, so two repositories that happen to share a folder name can never collide on the same checkout path. Changing this only ever affects worktrees created **from now on** — nothing already on disk moves.

**One click keeps `main` current.** The top bar shows how far the repo's main checkout has drifted from the remote — a refresh icon with a count of commits behind `origin/main`, coloured by severity (green for few-and-recent through orange to red for many-and-old) — and clicking it fetches and **fast-forwards** the main checkout. How sensitively that pill colours is the project's call: `ide.git.stalenessSensitivity` (default `1`) scales the curve, so a fast-moving trunk sets `2` and a project whose worktrees naturally drift sets `0.5` (see [configuration.md](docs/configuration.md#idegit-per-project-git-knobs)). It is deliberately human-initiated (never scheduled), refuses a repo root that is not on the default branch, a dirty tree, or a root with a live run, and only ever fast-forwards — a diverged branch is refused, never rewritten. When the bar is set to hide inapplicable actions (Settings → General → *Hide top-bar actions that can't fire*), the button disappears once you are up to date; with that off, it stays, greyed with "Up to date".

**Organising the rail.** With a dozen checkouts, alphabetical stops helping. **Groups** are sections you name yourself — `review`, `spikes`, `client-x` — created from the rail's folder button or a worktree's context menu → *Move to group*, and a worktree is dragged between them — or created straight into one, since every group header carries its own **＋**; a checkout created that way lands at the **top** of the section you started it from, which is where the thing you just made belongs. Within a group you can **drag worktrees into any order** you like, and the **groups themselves are dragged by their headers** to reorder the rail — the same move the group's ⋮ menu offers as *Move group up* / *Move group down*, which is still there for the keyboard. Anything you have not placed by hand stays alphabetical underneath. The rail is also **resizable** — drag its right edge, or focus it and use `←`/`→` — and the width is remembered *per window*, because two windows on two monitors want different rails. Dragging cannot narrow the rail into its collapsed mode: collapsing hides the name and the branch, so it stays a deliberate click on the chevron rather than something a drag can do to you by accident.

Groups and order are **yours, not the repository's**. Nothing is written to `veld.json`, so your private rail layout never turns up as a review comment on someone else's pull request. (The project-declared counterpart already exists and is a different thing: `ide.quicklinks` is for links every checkout of the repo should see.)

**Removing a worktree puts it in the trash.** *Move to trash…* marks the checkout and hands you straight back the app — **nothing is deleted**, the directory stays on disk, and the row moves to a **Trash** section at the bottom of the rail. Restoring it from there is a real undo, not a race. That also makes the operation instant: awaiting `git worktree remove` on a large checkout is what froze the UI before, and now the request does no slow work at all.

A trashed worktree is deleted for good when you say so — *Delete permanently* on its row, or the trash-can button on the Trash header to empty the whole thing — or automatically once its **retention period** runs out (Settings → General). That setting is `0` by default, which means *keep until I empty it*: the only thing veld ever deletes without asking twice is something you put in the trash yourself and then left there for as long as you told it to.

Nothing else empties the bin. Restarting the daemon, quitting Veld Desktop, rebooting, running `veld update` — a trashed worktree survives all of them, for as long as the retention period says. The one thing a restart re-checks is the retention period itself, so trash that expired while veld was not running is collected when it comes back.

Deleting always goes through `git worktree remove`, un-forced. Any run in the worktree is stopped first. If git refuses — uncommitted changes, a locked worktree, a file open in another process — the worktree **comes back into the rail with the reason on its row**, and says so once in a toast; it is never a silent failure. Forcing (which discards uncommitted changes) is a second, explicit click, offered only after git has already told you why it said no. The branch itself is always kept.

**Worktree markers** are a colour by default, or the animal emoji if you prefer (Settings → General). Both are stored for every worktree, so switching between them never loses the one you picked, and either can be chosen per worktree from its context menu → *Change marker…*. The alias is always shown next to the marker, so the colour is a scanning aid rather than the only way to tell two checkouts apart.

It also has **browser panes**: a tab in the same dock that renders one of the run's URLs, with a back/forward/reload row and an address bar. Each pane runs in a **colour-coded session** — its own cookie jar, named after an animal (otter, wombat, gecko…) rather than numbered — so you can be logged in as two different users of your own app side by side and tell which is which from the tab's dot. Sessions are added and removed from the pane's session menu — up to eight alongside the default, remembered per worktree — and the same menu clears any session's data, signing out every pane on it. A pane that can't load tells you *which* problem it hit: nothing listening (start the run), a hostname that doesn't resolve or an untrusted certificate (`veld doctor`), a timeout, or a crash. A browser pane with no URL **is** the run's start page — "Where to?", then the places it can go, each row opening here on click with copy and open-externally beside it. **The list is the run's own URLs, and the project's `ide.quicklinks` are behind a *Bookmarks* button** in that heading, which opens all of them in a dialog: with four to eight services per run, inline bookmarks pushed the addresses veld is actually serving into the middle of a screen that scrolled. Typing in the address bar still matches them, because a filter that could not see a bookmark would be a filter lying about its scope. The two kinds still say where they came from: the run's URLs carry a live dot and the service's name, a bookmark carries a bookmark glyph and no live dot, because veld started the first kind and only read the second out of a config — the glyph and the group carry that, not dimmed text, since a row that is perfectly clickable must not read as disabled. The pane does **not** take your keyboard when it opens: it is as often opened to click a URL as to type one.

**The address bar is a search bar too.** Typing something that is not an address searches for it (Settings → Browser panes → *Search from the address bar*, `%s` for the query, Google by default, empty to turn it off), which is what makes a pane usable for reading documentation and not only for previewing your own app. Focusing the bar lists those same places, filtered as you type, with ↑/↓ and Enter to pick one and the literal thing you typed as the first row — *Go to* or *Search for*, ringed as soon as it exists, because that is what Enter already does. Over a page that is already loaded the list is a **dimmed overlay** on the page rather than something that pushes it down: an embedded Chromium view paints over anything the app draws on top of it, so the pane freezes the page to a still, hides the view, and dims that — which is what an address bar looks like everywhere else. It used to sit in flow between the toolbar and the page, which reflowed your app on every keystroke. Anything that is not http(s) is still refused rather than searched for: a typo'd `javascript:` URL is a refusal with a reason, not a web search — but outside that closed set a colon is just a colon, so `std::vec::Vec` and a pasted `error at line 12:5` are queries. A bare single label like `grafana` searches too, as it would in any browser; say `http://grafana` when you mean the host. A search is recorded in the pane like any other visited URL, so turn it off if you would rather a mistyped query left no trace.

A worktree opens on a **single undecided pane** — not a split. That chooser already lists the run's URLs, so opening split showed them twice and imposed two columns on every checkout the first time you looked at it, including the many that want one full-width terminal; splitting is a drag away, un-splitting something you never asked for is busywork. The top bar's globe opens a browser pane, and `+` in the tab strip opens an undecided pane (hover `+` for one-click shortcuts), or ⌘K ("Open &lt;service&gt; in a pane").

**The undecided pane is grouped by what you are trying to do**, in three groups whose order never changes between projects: *work in a terminal* (the project's own panes and a plain shell, together, because they are alternatives to each other — as equal-sized cards, each carrying its own description, since which agent you want is the repo's business and not veld's to recommend), *open a page* (the run's URLs as the list, with **Bookmarks** and **Blank browser** as two buttons pinned to the right of that heading — they are the two things that are *not* one of the run's URLs, and as extra rows they had pushed the answer into the middle of a scrolling list), and *check the run* (logs and node health, quieter, at the bottom). It got that shape from watching someone use it: they opened a Claude pane by pressing `Terminal`, because the declared pane was in a separate section further down, and they pressed `Browser` without ever connecting it to their app's URL five rows below. So the list of URLs *is* the browser affordance now, and there is no separate button competing with it. In **Veld Desktop** the pane is a real Chromium view, which means working history, page titles, isolated cookie jars, and pages that refuse to be framed still render. In a plain browser it falls back to an `<iframe>`: good enough for a preview, but a page sending `X-Frame-Options` shows blank, and history and separate sessions are unavailable — the pane says so, and offers to open the URL in your system browser instead.

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

A grant you make by hand always beats the file, in both directions, and anything from the file is labelled *set by veld.json* in the site panel where you can revoke it — so this is a default, not a decision taken out of your hands. Understand what you are declaring, though: an entry for a *remote* origin hands that server's JavaScript a standing capability on the machine of anyone who opens it in a pane, which is a step beyond what the rest of a config does. See [docs/configuration.md](docs/configuration.md#ide-the-projects-own-ide-surfaces) for the origin syntax, the full id list and the defaults.

One permission is answered without asking anyone: a pane may **capture its own contents** at an origin veld serves. That is what `veld feedback` needs to take a screenshot, and a pane that could not screenshot the page it is showing was the one place the feature should have worked best.

`ide.externalOrigins` is the third key in that block: origins a URL from a terminal must open in the *system* browser rather than in a pane — the sign-ins your app's login goes through, which need the browser the developer is already logged into. It is unioned with each user's own exempt list rather than replacing it.

`ide.quicklinks` is the other half of the same block, and the other half of a pane's start page: veld lists the URLs it made, and these are the ones it didn't — staging, a dashboard, an internal wiki — versioned with the repo so a teammate who clones it gets the same links. They sit behind that page's *Bookmarks* button, and the address bar matches them as you type.

**A project can tell its own team something changed** (`ide.news`). Somebody moves the test command or adds a required env var, and the people it affects normally find out when it breaks for them. A news entry is a short card — an eyebrow, a headline, one sentence and a date — merged alongside the change it describes; a teammate pulls, and the next time they open that project in the IDE they are told, once. It rides the same channel veld uses for its own announcements, so reading clears it, dismissing keeps it counted, and everything stays revisitable under *What's new…* in the project ⋯ menu. Cards carry the project's name where veld's carry the wordmark, so a teammate's sentence can never read as something veld said.

The limits are the point rather than an implementation detail: a headline of 44 characters, a body of 160, and **five live items at most** — with retiring one meaning deleting it. A surface that interrupts everybody on the team is worth having only while opening it is reliably worth their attention, which is also why the honest advice is to write the *outcome* ("stop guessing which test script works") and not the change ("test wrappers removed"). Anything malformed, or past the cap, is a `veld lint` warning and never a load failure; only news in your **main checkout** counts by default — the primary clone, at whatever it has checked out — so a card drafted in a worktree cannot prompt anybody until it lands. *Settings → General → Read project news from* (`news.source`) can switch that to every checked-out worktree instead, for previewing a card you're writing before it merges. A reader who wants none of it turns off *Settings → General → Show news from your projects*. See [docs/promotions.md](docs/promotions.md).

```jsonc
"ide": {
  "news": [
    {
      "id": "one-command-tests",
      "since": "2026-08-12",
      "eyebrow": "Heads up",
      "headline": "Stop guessing which test script works",
      "body": "The wrappers are gone — `just test` runs everything, and your old local alias is the one thing that will still fail today.",
      "glyph": "terminal"
    }
  ]
}
```
`ide.extensions` is what the project puts in the **top bar**: a status badge, a button, or a menu grouping several buttons. A badge is a command veld runs in the worktree whose stdout it renders — `{ "text": "PR #284 · checks green", "tone": "success", "href": "…" }` — and a command that knows nothing about veld already works, because output that is not the contract becomes the badge's text (`git rev-parse --short HEAD` is a one-line badge). Exiting 0 with nothing to say hides the badge; failing renders it red with the tool's own message on hover. A badge's output may also offer **actions**, and those name `action` entries you declared *by id* — veld resolves each against your config before offering it, so the running command chooses among your commands and can never introduce one. Links open in **your own browser** by default, not a pane, because a pull request is a page you are already signed in to. Which side of the bar an entry sits on is `align`; a missing tool is `hide`, `disable`, or (the default) a dashed `hint` with the install page one click away. Declarations come from your project's **main checkout by default** — *Settings → General → Read a project's extensions from* (`extensions.source`) — so a worktree cloned before your `veld.json` gained `ide.extensions` still sees them; commands still run in the worktree you're looking at. Switch it to *This worktree* to test a new badge in the branch that's writing it, before merging. See [docs/configuration.md](docs/configuration.md#ideextensions-the-projects-own-badges-buttons-and-menus) for the field reference and what bounds these commands.

**A project can add its own panes** (`ide.panes`), which is how the dock stops being a fixed set of four kinds. A declared pane is a terminal that runs *your* command inside your **login shell** — the same shell a plain terminal opens, so it inherits everything `.zprofile`/`.zshrc` export (model tokens, tool paths) — and it appears in the `+` menu, the pane chooser and ⌘K alongside veld's own. The chooser shows them as **equal cards in declaration order**, beside veld's plain Terminal, each with its `description` under its label — nothing is promoted, because a repo that declares Claude, Pi, Codex and a git log has four things a contributor might want and picking one for them would be a guess dressed as a default.

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

The dock also holds the run's **diagnostics**, so a worktree that is misbehaving can be diagnosed without leaving it. A **Logs** pane is the same viewer the dashboard has — search with ±N context lines, node filter, source filter (server/client/setup/internal), a run picker over history and auto-scroll, and **colour**: a line's ANSI colours are rendered rather than printed as escape codes, other escape sequences are dropped, a progress line that overwrites itself shows its last state instead of every frame at once, and search matches the text you can see rather than the bytes underneath it — and a **Nodes** pane is the per-node health table: status, failure/recovery counts and the last liveness error, URL with copy/open, variant, PID, live CPU and memory with a sparkline that expands into the scrubbable resource chart, and each node's configured actions. In IDE mode each node's URL also carries an **open-in-a-pane** button, so a service opens in an embedded browser beside the terminal instead of in another application (copy and open-in-your-browser sit next to it). The same node actions are also one click up in the **top bar** (a menu beside the run controls) and in the **new-pane chooser**, so a running project's actions are not behind a pane you have to open. They are literally the dashboard's two views, not lookalikes: runs mode is a run's controls plus a Nodes|Logs switcher over the same components, so the two surfaces cannot drift. Both read whichever run the *selected* worktree has, so switching worktrees re-points every open diagnostics pane; both reach past runs (the logs pane's run picker, a picker in the nodes pane's header); and both keep working after a run ends — a crashed run's logs and last node states are exactly what you want then. The nodes view is a card per node rather than a table — a table has columns to lose, and this view has to be readable in a 300px pane and in a 1080px dashboard card alike; here nothing is dropped as the width changes, only rewrapped. Open them from `+` in the tab strip, the pane chooser, or ⌘K.
The dock also holds the run's **diagnostics**, so a worktree that is misbehaving can be diagnosed without leaving it. A **Logs** pane is the same viewer the dashboard has — search with ±N context lines, node filter, source filter (server/client/setup/internal), a run picker over history and auto-scroll, and **colour**: a line's ANSI colours are rendered rather than printed as escape codes, other escape sequences are dropped, a progress line that overwrites itself shows its last state instead of every frame at once, and search matches the text you can see rather than the bytes underneath it — and a **Nodes** pane is the per-node health table: status, failure/recovery counts and the last liveness error, URL with copy/open, variant, PID, live CPU and memory with a sparkline that expands into the scrubbable resource chart, and each node's configured actions. In IDE mode each node's URL also carries an **open-in-a-pane** button, so a service opens in an embedded browser beside the terminal instead of in another application (copy and open-in-your-browser sit next to it). They are literally the dashboard's two views, not lookalikes: runs mode is a run's controls plus a Nodes|Logs switcher over the same components, so the two surfaces cannot drift. Both read whichever run the *selected* worktree has, so switching worktrees re-points every open diagnostics pane; both reach past runs (the logs pane's run picker, a picker in the nodes pane's header); and both keep working after a run ends — a crashed run's logs and last node states are exactly what you want then. The nodes view is a card per node rather than a table — a table has columns to lose, and this view has to be readable in a 300px pane and in a 1080px dashboard card alike; here nothing is dropped as the width changes, only rewrapped. Open them from `+` in the tab strip, the pane chooser, or ⌘K — or from the rail itself: a worktree whose run has **failed**, or one whose nodes veld is **recovering**, carries a warning on its row that says which, and clicking it brings you to that worktree with its Nodes pane in front. The rest of the row's run state is on its start/stop control, which spins while a run is coming up or going down — including a run you started from the terminal or from another window, not only one you clicked here.

**Sharing** is one surface in the top bar: it starts and stops the peer share and the public `--web` share for the selected worktree's run, offers the join link and the `veld join` command, toggles auto-accept, and shows each live connection's transport (`direct`, or `relayed via <relay>`, with RTT) — so a slow share is diagnosable here too. Join requests are not hidden behind it: while someone is waiting for approval, a prompt sits above the panes naming who wants to join which run, with Approve and Deny. Sharing is opt-in per port (`share.expose`) and needs a relay (`sharing.relays`), so a run that has neither is *refused* rather than shared — the daemon says exactly what to add to `veld.json`, and both UIs now show that text instead of a bare status code.

Because a terminal is a shell on your machine, `/api/pty/attach` is gated more tightly than the rest of the daemon's API: WebSocket handshakes cannot carry the `X-Veld-Request` CSRF header, so an attach needs a single-use ticket minted through a CSRF-gated `POST` **and** an `Origin` on the allowlist, failing closed when `Origin` is absent. Details and the reasoning are in `crates/veld-daemon/src/pty.rs`.

### Veld Desktop

The same `/ide` UI as a desktop app: a native window with a menu-bar icon, real Chromium browser panes (working history, page titles, isolated cookie jars, and pages that refuse to be framed), device emulation and per-pane DevTools. Everything else works identically in a browser — the app is a shell around the daemon, not a second implementation.

**Windows.** `⌘N` opens another full window — one worktree per monitor, rather than switching back and forth in one. Right-click a worktree in the rail for *Open in a new window* to send it straight there, or use the project selector's *Open in a new window* to give a whole **project** its own window — that one names no worktree, so the new window takes whichever of the project's checkouts is free rather than pulling one away from wherever it already is. ⌘K has the same entry per project, and so does a right-click in the project column. In a plain browser the same entries say *Open in a new tab* and open one, which the daemon arbitrates exactly like a second window.

**A worktree has one set of panes, and one client shows them.** Windows are for working on *different* worktrees side by side, not for opening the same one twice: pick a worktree another window already has and Veld brings you to that window instead of growing a second set of terminals. Those rows are marked in the rail before you click one, and the switch says so, so a window coming forward is never a surprise. Close that window and the worktree is free again — open it anywhere and its panes come back, still attached to the same shells, because the layout belongs to the worktree rather than to the window that happened to show it.

**And "anywhere" includes your own browser.** The panes live in the daemon, so `/ide` in Safari or Chrome is the same set as Veld Desktop's — the same tabs, the same splits, and the same running terminals, which it re-attaches to rather than starting a second copy of. Ownership is the daemon's too, so a browser tab takes part in it like any window: a worktree open in a tab is marked in the desktop app's rail, and clicking it says where it is. The one asymmetry is honest rather than hidden — a browser cannot raise its own tab, so Veld tells you which browser to switch to and marks that tab's title, where a desktop window it simply brings to the front.

A pane can also leave its window: right-click a tab → *Open in a new window*, or **drag it out** and drop it — on your second screen, and that is where the window appears. A dropped pane can go back the same way, or into any other Veld window showing that worktree: drag it over and the target shows its own drop indicators, so it lands at the tab position or pane edge you aimed at rather than at the end. That bare dock window can be split and hold tabs of its own, and closing it **returns** its tabs rather than ending them — closing a *pane* is still what ends a shell. Detaching a browser pane reloads its page (a Chromium view belongs to one window, so moving it is a rebuild); the URL, session, device and zoom all survive. Every window comes back on the next launch with its panes attached to the same shells. Up to eight windows.

**It needs the veld CLI**, which it does not ship. On a machine that has never had veld the app shows the two commands that get you there — the installer and `veld setup unprivileged` — and waits for the daemon to appear.

On macOS **the installer brings it with the CLI** — `curl -fsSL https://veld.oss.life.li/get | bash` installs both halves, and `veld update` moves both. `VELD_DESKTOP=0` opts out (a CI box or a server that wants no Dock icon), and `veld desktop install` gets it on a machine that skipped it.

Installing it this way is also what **skips the Gatekeeper detour.** A build downloaded in a browser carries `com.apple.quarantine`, and that flag is what makes macOS refuse the first launch of an app that is not notarized. curl does not set it, so an app installed by veld simply opens. `veld desktop status` says what is installed and whether it matches the CLI.

The app's own *Check for Updates…* hands the job to the CLI, and hands it the **whole release**: the dialog offers *veld 16.8.0*, not a new app, and *Quit and Update veld* runs `veld update` — CLI, daemon, helper and app, from one tag, in one restart. An app cannot replace its own bundle while running, so it quits, the CLI does the work, and the app reopens on the new version. This is why there is no second trip to a terminal afterwards, and no "your CLI is behind" notice a minute later: both halves move together or neither does.

**And it opens a terminal window to do it.** The app quits, so for the next one to four minutes there is no Veld window to draw a progress bar in — and, less obviously, no controlling terminal, which is what `sudo` needs to ask for a password on the one path that still requires one (a `/usr/local` install, below — the *helper restart* no longer does). Without one the update only ever gets `sudo -n` and gives up silently. So the CLI re-runs itself in *your* terminal: on macOS by opening the generated `~/.veld/update-console.command` through LaunchServices, which honours whatever you have registered for `.command` files (Terminal.app unless you chose otherwise); on Linux through `$TERMINAL`, `x-terminal-emulator`, or the first emulator it finds. You watch the whole install, and a password prompt appears where you are looking. If no terminal can be opened — a headless box, a machine with no emulator installed — the update runs exactly as it did before, in the background with its output in `~/.veld/desktop-update.log`, rather than failing. The app does not take "a launcher started" for an answer: it waits for the window's `veld update` to actually claim the update lock before it believes a window exists.

**Only one update ever runs at a time.** `veld update` takes a lock at `~/.veld/update.lock` and publishes what it is doing into it; a second one refuses with exit code 75 (`EX_TEMPFAIL`) and tells you who is updating, since when, and which phase they are on. So do the rest of veld's commands — anything that touches the daemon, the helper or the installed binaries, since those are being replaced; `veld doctor`, `veld version`, `veld config`, `veld lint`, `veld init` and `veld desktop status` keep working — and so does Veld Desktop, which quits with an explanation if you open it while its own bundle is being swapped. A lock never becomes permanent: it is written off the moment its holder's process is gone, or after 30 minutes without progress (the case where somebody walked away from a `sudo` prompt), and `veld update --force` skips the wait. `veld update --status` and `veld doctor` both read the same file, so all three agree.

**An update never asks for your password.** In privileged mode the helper is a
root service, so an unprivileged `veld update` cannot bounce it with `launchctl`
— but it does not have to. The helper restarts *itself*: the CLI asks it to over
the same Unix socket it already exposes, and it exits leaving Caddy running, so
launchd relaunches it on the new binary and no live URL blinks. A helper too old
to understand the request falls back to its own binary watcher, which notices the
file changed and does the same thing within about twelve seconds. Sudo is offered
only after both of those have had their full budget and the helper still has not
come back — a prompt that arrives with a reason on the line above it, rather than
one that interrupts a working update. `veld setup privileged`'s "sudo once, you
won't be asked again" is meant literally.

It needs a CLI that advertises it can do it — `veld desktop status --json` reports a `capabilities` list, and `full-update-handoff` is the one that matters. Two things withhold it: a CLI too old to have the flags, and **a CLI installed under `/usr/local`**. The second is not a version problem: `install.sh` refuses to relocate a system install (a privileged LaunchDaemon still points at `/usr/local` paths), so the install itself needs `sudo` — not just the helper restart. The terminal window above would give sudo somewhere to ask, but it is a best-effort thing: when no terminal can be opened the update falls back to running headless, and on these machines that fallback cannot finish rather than merely finishing slowly. A capability the binary can only *sometimes* deliver is worse than one it withholds, so it stays withheld: those machines get the app-only `veld desktop update` — exactly what they got before — and the dialog says *run `veld update` afterwards to move the rest of the release*. From a terminal, where sudo **can** prompt, `veld update` moves both halves for them normally. Against a CLI with no `veld desktop` at all the app points you at the release page rather than quitting into a command that does not exist. Because nothing is watching a terminal while the app is gone, everything the update says lands in `~/.veld/desktop-update.log`, and the app tells you on the way back if it did not work rather than reopening on the old version in silence. On Linux the AppImage still updates itself and a `.deb` still belongs to your package manager.

**From a terminal it works the other way round.** `veld update` with the app open used to print an error and skip it — the bundle cannot be swapped under a running app — so it now asks first:

```
ℹ Veld Desktop is running, and its bundle cannot be replaced while it is.
  Nothing is lost: terminal sessions belong to the daemon, keep running while the
  app is closed, and reattach with their scrollback when it reopens.
  Close Veld Desktop, update both halves, and reopen it? [Y/n]
```

Yes closes it politely — an Apple Event first, so `before-quit` persists your window layout exactly as ⌘Q would, falling back to `SIGTERM`, never `SIGKILL` — updates everything, and reopens it. Say no and only the CLI half moves. An app showing an unanswered dialog is left alone rather than forced. A **non-interactive** run (no TTY, or `VELD_NON_INTERACTIVE`) never closes your app without being asked: it skips the app half and says so, which is what keeps a scripted or agent-driven `veld update` from taking a window off your screen.

You can also download the `.dmg` (macOS) or `.AppImage` / `.deb` (Linux x64) from the [latest release](https://github.com/prosperity-solutions/veld/releases/latest) — `checksums.txt` on the same page has a SHA-256 for every artifact. The app ships with every veld release and carries the same version number as the CLI — one tag, one version, so the app and the daemon it talks to are halves of the same thing. When they drift apart the app says so and names the fix.

> **macOS, browser download: not code-signed yet.** Gatekeeper refuses the first launch of a `.dmg` you downloaded in a browser. Open it once, let the warning appear, then **System Settings → Privacy & Security** → *Open Anyway* — or `xattr -dr com.apple.quarantine /Applications/Veld.app`. (Right-click → *Open* used to be the shortcut; macOS 15 removed it for apps that aren't notarized.) `veld desktop install` avoids all of this; Developer ID signing and notarization are [tracked in #167](https://github.com/prosperity-solutions/veld/issues/167).

## Sharing

Share a running environment with a colleague so they open the **same** URLs on their own machine, over an encrypted peer-to-peer tunnel (iroh: QUIC with NAT hole-punching and an n0 relay fallback). No accounts, no Veld-hosted server.

**Ports must opt in, one at a time.** `share` is a field on a **port** — that is where exposure happens — and `veld share` refuses to expose anything that hasn't declared one. This makes what leaves your machine explicit and auditable:

```jsonc
{
  "sharing": { "relays": "public" },
  "nodes": {
    "api": {
      "variants": {
        "dev": {
          "type": "long_running",
          "shell": "api --port ${veld.port} --admin ${veld.ports.admin}",
          "ports": {
            "http":     { "port": "auto", "protocol": "http", "share": { "expose": ["peer"] } },
            "admin":    { "port": "auto", "protocol": "http" },                     // ops console: never shared
            "postgres": { "port": 5432,   "protocol": "tcp",  "share": { "expose": ["peer"] } }
          },
          "probes": { "readiness": { "type": "port" } }
        }
      }
    },
    "frontend": {
      "variants": {
        "local": { "type": "long_running", "argv": ["npm", "run", "dev"], "share": { "expose": ["peer"] } }
      }
    }
  }
}
```

`frontend` shows the shorthand: a `share` written on a node or a variant is **defined as the primary port's policy**. That keeps every config written before per-port consent meaning exactly what it meant — such a node had one exposed port — while making it impossible for the same three words to start covering an ops console or a database. It lands on the primary and stops there. A port's own `share` replaces the shorthand for that port rather than merging with it, an absent `share` is always "not shared", and nothing anywhere widens a port that declared none. `veld share` names every port it excluded and why, so a partial share is never silent.

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

### Sharing a raw TCP port

A `"protocol": "tcp"` port — a database, a debugger — can be shared to the `peer` audience. It rides the same encrypted tunnel and is reproduced on the joining machine as a **bare local TCP port**: a listener spliced to your service, with no Caddy route in front of it, because a raw connection carries no hostname for a proxy to match on.

**The port number your colleague uses is theirs, not yours.** Nothing is in front of the socket to preserve `5432`, so `veld join` prints raw endpoints apart from the URLs and never as links:

```
✓ Joined — 3 endpoint(s) now reachable on this machine:

    https://web.demo.acme.localhost
    https://api-admin.demo.acme.localhost
    api-postgres.demo.acme.localhost:49317  (tcp)
```

(`--json` puts them in `addresses`; `urls` stays URLs only.) Two things follow from "no route": `proxy` header rules are an HTTP concept and don't apply, and a colleague running an older Veld **refuses the whole join** rather than reproducing a raw endpoint as an HTTP route — the manifest entry has no `url` and their wire format required one. That is deliberate and fail-closed; upgrade both sides.

`tcp` is `peer`-only: **`"expose": ["web"]` requires `"protocol": "http"`**, and the combination is the `web-share-needs-http` lint error. The gateway speaks HTTP/1.1 over the tunnel and a browser cannot speak a raw protocol through it whatever the gateway does — that is what the `web` audience *is*, not a limitation waiting to be lifted. There is no `udp` protocol, and so no udp sharing.

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
        "local": { "type": "long_running", "argv": ["npm", "run", "dev"], "share": { "expose": ["peer", "web"] } }
      }
    }
  }
}
```

```sh
veld share --web            # prints https://<slug>.share.acme.internal per service + a password
```

The gateway `token` (and any relay token) is resolved in the **daemon's** environment, not your interactive shell — a bare `export …` won't reach a background daemon, so use a literal (quick start), a `file` secret mount (production), or set the variable in the daemon's service definition. Same rule as [relay auth tokens](docs/configuration.md#relay-auth-tokens).

`veld share --web` mints a **separate** share scoped to the `web`-opted `http` ports (its own capability — revoking the web audience never touches peer shares), registers it with the gateway, and prints the public URLs. The gateway joins over iroh like any peer and reverse-proxies the tunneled service onto `https://<slug>.<gateway-domain>`. URLs are **deterministic** (a hash bound to your machine, the service, and the share) and survive gateway restarts; a new share mints new URLs. The daemon keeps the registration alive with heartbeats; `veld unshare` (or the share's TTL) kills the public URLs.

**Web shares are password-protected by default.** `veld share --web` generates a share password (or takes yours via `--password`) and prints it next to the URLs; the first visit shows a password page, then a session cookie keeps the viewer in for up to 12 hours (never longer than the share). Send URL and password over different channels for real secrecy — or use the printed **one-link** (`https://…/#veld-key=…`), which carries the password in the URL *fragment*: it never appears in DNS, TLS, server logs, or `Referer`, so even the convenient form beats a bare link. To opt a service out (anyone with the link is served — the unguessable 128-bit slug is then the only gate), set `"share": { "expose": ["web"], "web": { "access": "link" } }` in config, or pass `--access link` for services whose config doesn't pin a mode — an explicit config value always wins over the flag. Viewer sessions are stateless (signed with a key derived from the share's capability), so a gateway restart doesn't log viewers out, and revoking the share invalidates every session instantly.

WebSockets (HMR) work through the gateway; redirects to shared sibling services are rewritten to their public URLs. Fidelity is best-effort by design: the app sees its own origin hostname (dev-server host allow-lists pass untouched), the public host arrives in `X-Forwarded-Host`, and response cookies scoped to origin hostnames are made host-only. Apps with hard-coded absolute URLs, strict CORS allow-lists, or OAuth redirect URIs need those configured for the public host — that's the operator's domain setup, not something Veld rewrites. One password caveat for multi-service shares: the session cookie is per public host, so a password-protected API called cross-origin from a shared frontend will get 401s — give API nodes `"web": { "access": "link" }` (their slugs stay unguessable and only the app's code ever uses them).

In the browser, the toolbar's arc menu has a top-level **Sharing** item (a green dot marks it when the current page is already on the public web) that opens a submenu: **Start sharing** / **Stop sharing** toggle a web share for the current page's run without touching the terminal, **Copy public URL** swaps the host of your *current* page for the public one — keeping path, query, and hash, so a deep link to the exact screen you're looking at lands on your recipient's screen too — and **Sharing status** reports whether the page is shared and its public URL. Transport detail (`direct` vs `relayed via <relay>`, RTT, throughput warnings) lives in `veld shares` and the management UI, not the in-page toolbar.

Deploying the gateway is one container (`ghcr.io/prosperity-solutions/veld-gateway`) plus a wildcard DNS record — see the [gateway operator guide](docs/gateway.md).

> **Upgrading:** opt-in is a behavior change. Before, `veld share` exposed every URL-bearing service in a run; now it shares only ports that declare `share.expose`, and errors (naming the candidates as `node:variant#port`) if none have opted in. Add `"share": { "expose": ["peer"] }` to the variants you previously relied on sharing — that spelling still works and now means their primary port, which on a single-port node is the only thing it ever meant. Password-by-default is a second behavior change: existing web shares gain a password on upgrade, and a freshly-upgraded daemon refuses `veld share --web` against a gateway too old to enforce it (clear error) — upgrade the gateway image, or share with `--access link`.

If the consumer already runs the same environment, the local URL wins — that node is skipped and reported as a warning. Shares live in the daemon's memory: if the daemon stops, shares stop (fail-closed). Stopping the run (`veld stop`) auto-unshares its shares, and so do `veld restart` and a `veld start` that replaces a live environment of the same name — each tears down the ports a share points at and mints a new run, so the old share is released rather than left pointing at nothing. A share whose run ended some other way (a crash) is listed as a "share without a run" in the dashboard, with an Unshare button, so it never becomes unreachable. The consumer's join self-tears-down when the tunnel closes. `veld unshare` and `veld leave` take the id optionally, resolving the sole active share/join when omitted. Run names are unique per project, not across your machine — two repos both on `main` each have an environment named `main` — so `veld share` resolves the run against the project directory you run it from. (Two *checkouts of one repo* are the exception worth knowing: they share a `veld.json`, so the same run name there means the same URL. `veld start` refuses the second one with an error naming the other project rather than hijacking its route — start it under a different `--name`.) A name that project doesn't run is an error naming where it *does* run, so `cd` there; run from outside any project, an ambiguous name is rejected naming the candidates rather than guessed at. Sharing also requires the run to be **running**, not merely started: a stopped environment's URLs point at ports that are gone, and one still coming up would share only the services that happen to be up already. Default TTL is 7200s (3600s for `--web` — the audience is the open internet, so idle web shares die sooner).

## Requirements

- macOS (arm64/x64) or Linux (x64/arm64)
- Optional: sudo access for `veld setup privileged` (clean URLs without port numbers, custom apex domains, and — on macOS — keeping the machine awake with the lid shut on battery)

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
