---
name: veld-usage
description: Use the Veld CLI to manage local development environments. Use when the user asks to start, stop, restart, or check the status of environments, view logs, list URLs, debug environment issues, or run any veld command.
metadata:
  author: prosperity-solutions
  version: "1.0.0"
---

# Veld CLI Usage

Veld is a local development environment orchestrator. It starts services defined in `veld.json`, wires them together with dependency resolution, and gives each service a clean HTTPS URL like `https://frontend.my-feature.myproject.localhost`. No port numbers, no manual wiring.

## Core Workflow

```bash
# Start an environment (resolves dependencies, starts services, configures HTTPS)
veld start frontend:local --name my-feature

# Check what's running
veld status --name my-feature
veld urls --name my-feature

# View logs
veld logs --name my-feature
veld logs --name my-feature --node backend --follow

# Stop
veld stop --name my-feature
```

## Command Reference

### `veld start [NODE:VARIANT...] [OPTIONS]`

Start an environment. Resolves the dependency graph, allocates ports, starts processes in parallel where possible, runs health checks, and configures Caddy HTTPS routes.

```bash
# Explicit node selections
veld start frontend:local backend:local --name dev

# Using a preset
veld start --preset fullstack --name dev

# Interactive mode (no args in a TTY — prompts for selections)
veld start

# Attach to foreground (stream logs, Ctrl+C to stop)
veld start frontend:local --name dev --attach
```

**Options:**
- `--name <NAME>` — Name the run (auto-generated if omitted)
- `--preset <NAME>` — Use a named preset from veld.json
- `-a, --attach` — Stay in foreground, stream logs
- `--debug` — Enable debug logging

### `veld stop [OPTIONS]`

Stop a running environment. Runs `on_stop` teardown commands in reverse dependency order.

```bash
veld stop --name dev
veld stop --all
```

### `veld restart [OPTIONS]`

Restart an environment (stop + start with same selections).

```bash
veld restart --name dev
```

### `veld status [OPTIONS]`

Show the status of a running environment — node states, health, URLs.

```bash
veld status --name dev
veld status --name dev --outputs   # Include node outputs (env vars, ports)
veld status --name dev --json      # Machine-readable
```

### `veld urls [OPTIONS]`

Show just the URLs for a running environment.

```bash
veld urls --name dev
veld urls --name dev --json
```

### `veld logs [OPTIONS]`

View logs from running or recently-stopped services.

```bash
veld logs --name dev                          # Last 50 lines, all nodes
veld logs --name dev --node backend           # Single node
veld logs --name dev --lines 200              # More lines
veld logs --name dev --since 5m               # Last 5 minutes
veld logs --name dev --follow                 # Stream (like tail -f)
veld logs --name dev --node backend --follow  # Stream single node
veld logs --name dev --json                   # NDJSON format
```

### `veld runs [OPTIONS]`

List all environment runs across all projects.

```bash
veld runs
veld runs --json
```

### `veld list [OPTIONS]`

List all Veld projects on this machine with their runs.

```bash
veld list
veld list --urls   # Include URLs
veld list --json   # Machine-readable
```

### `veld feedback [OPTIONS]`

Read or wait for in-browser feedback. See the **veld-feedback** skill for the full human-in-the-loop workflow.

```bash
veld feedback --name dev              # Show latest feedback
veld feedback --wait --name dev       # Block until feedback arrives
veld feedback --history --name dev    # Show all feedback batches
veld feedback --json --name dev       # Machine-readable
```

### `veld graph [NODE:VARIANT...]`

Print the dependency graph for given selections. Useful for understanding execution order.

```bash
veld graph frontend:local
```

### `veld nodes`

List all nodes and their variants defined in veld.json.

```bash
veld nodes
veld nodes --json
```

### `veld presets`

List all presets defined in veld.json.

```bash
veld presets
veld presets --json
```

### `veld init`

Create a new veld.json interactively. Auto-detects workspaces (pnpm, npm, Cargo), services, and databases.

```bash
veld init
```

### `veld gc`

Garbage-collect stale state, orphaned processes, and old feedback directories.

```bash
veld gc
```

### `veld setup`

One-time system setup. Installs Caddy, daemon/helper services, trusts TLS certificates. Requires sudo.

```bash
veld setup
```

### `veld update`

Update Veld to the latest version.

```bash
veld update
```

## Common Patterns

### Checking if an environment is healthy

```bash
veld status --name dev --json
```

Look at the node status fields. All nodes should be `"running"` with health checks passed.

### Getting the URL for a specific service

```bash
veld urls --name dev --json
```

Parse the JSON to find the URL for the service you need. URLs follow the template in veld.json (default: `https://{service}.{run}.{project}.localhost`).

### Restarting after code changes

Most dev servers have hot reload, so you usually don't need to restart. But if you changed configuration, dependencies, or the veld.json itself:

```bash
veld restart --name dev
```

### Debugging a service that won't start

```bash
# Check status for error details
veld status --name dev

# Check recent logs
veld logs --name dev --node <failing-node> --lines 100

# Check if the port is actually in use
veld status --name dev --outputs
```

### Running multiple environments

Each run is isolated with its own ports and URLs:

```bash
veld start frontend:local --name feature-a
veld start frontend:local --name feature-b
# Both run simultaneously with different URLs
```

### Name resolution

If `--name` is omitted:
- One active run → uses that run automatically
- Multiple runs → prompts you to pick
- No runs → error

## Environment Details

- **Ports**: Veld allocates from range 19000–29999. Never hardcode ports.
- **Working directory**: All commands run from the veld.json directory (`${veld.root}`), not your CWD.
- **TLS**: Caddy handles HTTPS automatically. Certificates are trusted system-wide after `veld setup`.
- **State**: Stored in `~/.local/share/veld/`. Runs persist across CLI invocations.
- **Feedback**: Stored in `.veld/feedback/{run_name}/` within the project directory.
