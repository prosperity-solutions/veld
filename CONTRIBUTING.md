# Contributing to Veld

Veld is 100% vibe coded with [Claude Code](https://claude.com/claude-code). The first well-working version was shipped in 3 days — entirely through agentic contributions. We want to keep it that way.

## We only accept agentic contributions

This means: your PR should be authored, reviewed, or substantially driven by an AI coding agent (Claude Code, Cursor, Copilot Workspace, Aider, etc.). We're not gatekeeping which tool you use — just that you're using one.

Why? Because that's how this project was built, and it's how we believe modern software should be maintained. If an agent can't understand the codebase well enough to make a change, that's a signal the codebase needs to be clearer — not that we need more manual labor.

## How to contribute

1. **Fork the repo** and create a branch from `main`.
2. **Use an AI coding agent** to implement your changes.
3. **Follow conventional commits** — we use [Conventional Commits](https://www.conventionalcommits.org/) for semantic versioning. Prefix your commit messages with `feat:`, `fix:`, `docs:`, `chore:`, etc.
4. **Make sure CI passes** — `cargo fmt`, `cargo clippy`, and `cargo test` must all be green.
5. **Open a PR** with a clear description of what changed and why.

## Development setup

```sh
git clone https://github.com/prosperity-solutions/veld.git
cd veld
just setup-frontend   # install Node.js dependencies (once)
just setup-ui         # deps for the /ide management UI + desktop shell (once)
just build            # build Rust + frontend
just test             # run all tests
```

For the /ide management UI and the Electron desktop shell, see
[desktop/ARCHITECTURE.md](desktop/ARCHITECTURE.md) (`just dev-ui`,
`just dev-desktop`).

The workspace has four crates:

| Crate | Description |
|-------|-------------|
| `veld` | CLI binary |
| `veld-core` | Shared library (config, orchestrator, state, health checks) |
| `veld-helper` | Privileged daemon for DNS/Caddy management |
| `veld-daemon` | User-space daemon for health monitoring, feedback overlay, and GC |

## Local development

Veld has three tiers of binaries with different lifecycles:

| Tier | Binary | Runs as | How to test changes |
|------|--------|---------|---------------------|
| CLI | `veld` | Your user, exits immediately | `just dev <args>` — no install needed |
| Daemon | `veld-daemon` | User-level launchd service | `just dev-install-daemon` |
| Helper + Caddy | `veld-helper` | System launchd service (root) | `just dev-install-helper` (sudo) |

### Tier 1: CLI changes (most common)

`just dev` (and the `veld-dev` wrapper) run the source build against a
**dedicated dev database** at `.veld-dev/veld.db` (gitignored) — never the
installed veld's DB. Dev builds can carry newer schema migrations, and a
schema-ahead binary migrates whatever DB it opens; on the real DB that would
blind the installed daemon (`NewerSchema`) until `veld update`. Isolation
makes that impossible by default:

```sh
just dev start --name foo website:local   # own state, invisible to installed veld
just dev runs --name foo
just dev-db-reset                          # fresh dev state
just dev-db-from-real                      # snapshot the real DB → rehearse migrations on the copy
just dev-daemon                            # daemon from source, ALONGSIDE the installed one
```

`just dev-daemon` is a full parallel instance: own DB, own port (19898 vs the
installed 19899), own socket, and its own dashboard at
**https://veld-dev.localhost** (the route is self-registered with the shared
Caddy on startup and removed on Ctrl-C). Runs started with `just dev` mint
routes pointing at the dev daemon, so their feedback overlay/client logs land
in the dev instance too.

The helper/Caddy/DNS layer is *not* instanced — it's a singleton owning
443/18443 and system DNS; both instances share it. And the installed daemon
only watches the real DB: dev runs get no crash detection unless
`just dev-daemon` is running.

To point the source-built CLI at the **real** DB — e.g. a feedback loop
against a run the installed veld started — use `just dev-real <args>`. It
refuses to run when the branch's schema is ahead of the real DB (that's the
migration trap above); in that case test via `just dev` + `just dev-daemon`
or `just dev-install`.

**`veld-dev` — the dev instance from any project.** `just dev-link` (one-time)
installs `~/.local/bin/veld-dev`, a wrapper that carries the full dev
instance (dev DB, daemon port 19898, dev socket). It is the complete CLI —
`start`/`stop`/`restart` included — and shares the installed helper/Caddy,
so URLs work normally. The old "don't `veld-dev start`" caveat is gone: the
wrapper no longer overrides the lib directory (the CLI↔installed-services
version gate is skipped for dev instances instead).

```sh
just dev-link                       # one-time: creates ~/.local/bin/veld-dev
cd ~/some-test-project
veld-dev start website:local --name devtest   # dev instance, from anywhere
veld-dev runs --name devtest
veld-dev status
```

Rebuilds: `veld-dev` executes `target/debug/veld` directly, so a plain
`cargo build` in the repo refreshes it — no re-link needed.

### Tier 2: Daemon changes (feedback overlay, client-log, health monitoring)

```sh
just dev-install-daemon    # builds, installs to ~/.local/lib/veld/, restarts service
```

### Tier 3: Helper changes (Caddy config, route building, TLS, GODEBUG)

```sh
just dev-install-helper    # builds, installs, sudo restarts Caddy
```

### Going back to the released version

```sh
just dev-restore    # runs veld update
```

### All commands

| Command | What it does | Sudo? |
|---------|-------------|-------|
| `just dev <args>` | Run CLI from source (safe, no install) | No |
| `just dev-link` | Create `veld-dev` wrapper for cross-project use | No |
| `just dev-install-daemon` | Install daemon + restart service | No |
| `just dev-install-helper` | Install helper + restart Caddy | Yes |
| `just dev-install` | CLI + daemon | No |
| `just dev-install-all` | CLI + daemon + helper | Yes |
| `just dev-restore` | Restore to released version | No |
| `just build` | Build Rust + frontend | No |
| `just test` | Run all tests | No |
| `just lint` | Clippy + TypeScript type check | No |

## Guidelines

- Keep PRs focused — one feature or fix per PR.
- Don't break existing behavior without discussion.
- Add tests where it makes sense, but don't over-test trivial code.
- If CI fails, fix it before requesting review.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
