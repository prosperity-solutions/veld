---
name: veld
description: >
  Orchestrate local dev environments with veld. Use this skill when the user wants to
  start, stop, or restart services; check run status or logs; configure veld.json
  (nodes, services, dependencies, presets, health checks, ports, URL templates); or
  debug environment issues like port conflicts or health-check failures. Also use when
  the user wants to show their UI to a human for review, get visual feedback on
  changes, listen for comments, run a feedback loop, or coordinate multiple agents
  working on feedback threads — even if they say "let me check," "show the user,"
  "wait for feedback," or "let them review it." Covers any `veld` CLI command.
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
compatibility: Requires veld v6.6.0+
allowed-tools: Read, Edit, Bash(veld *)
metadata:
  author: prosperity-solutions
  version: "6.6.0"
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

## Feedback Loop

For the full feedback workflow, events, thread fields, interactive controls, and framework binding templates, see [reference/feedback.md](reference/feedback.md).

Core pattern: listen (returns all pending feedback at once) → fix → release with status comment → listen again with `--after <last_seq>` → repeat until `session_ended`. Threads are auto-claimed so multiple agents can work in parallel without conflicts.

## Reading Outputs

After starting an environment, read node outputs (database URLs, ports, credentials, etc.):

```sh
veld status --outputs --name my-feature        # human-readable
veld status --outputs --json --name my-feature  # machine-readable
```

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
- **`setup`/`teardown` are not nodes** — they have no variants, no health checks, no outputs. Only project-level variables (`${veld.name}`, `${veld.root}`, `${veld.run}`) are available, not `${veld.port}` or `${nodes.*}`
- **Ports are dynamic** (19000–29999) — never hardcode a port in veld.json or dependent config
- **Commands run from veld.json directory**, not your CWD — use `cwd` field if a node needs a different working directory
- **Name resolution** — if `--name` omitted: one run → auto-selects, multiple → prompts, none → errors
- **`--json`** — most commands accept it for machine-readable output, prefer it when parsing results

## Troubleshooting

If something isn't working (WebSocket failures, CSP errors, overlay disappearing, port conflicts, cert warnings), see [reference/troubleshooting.md](reference/troubleshooting.md).
