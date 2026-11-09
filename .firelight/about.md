# Veld

Veld is a local development environment orchestrator. You declare your services in one
`veld.json`; veld resolves the dependency graph, allocates ports, starts every process,
waits for its readiness probe, and gives each service a stable HTTPS URL such as
`https://frontend.dev.myproject.localhost`. No port numbers, no manual wiring.

Around that core:

- a browser dashboard at `https://veld.localhost` and an IDE (`/ide`, also the Veld Desktop app)
  for worktrees, terminals, coding agents and browser panes;
- sharing a running environment with a colleague, peer-to-peer or as a public URL through a
  self-hosted gateway;
- agent documentation carried by the binary (`veld agent skills`).

Every command is `veld <part> <verb>`: `orchestrator` (alias `orc`), `ide`, `agent`, `network`,
`self`. Every command takes `--json`. Runs on macOS (arm64/x64) and Linux (x64/arm64). MIT licensed.

The in-browser feedback threads (the `feedback` command) were removed in #424 and have no replacement.
