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
just build            # build Rust + frontend
just test             # run all tests
```

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

Use `just dev` or `veld-dev` for read-only CLI commands:

```sh
just dev feedback listen --name myrun --json
just dev status
veld-dev doctor
```

**Do not use `veld-dev` for `start`/`restart`** — it overrides the lib directory which breaks Caddy path resolution. Use the installed `veld` for starting environments:

```sh
veld start --name myrun website:local    # uses installed veld
veld-dev feedback listen --name myrun    # uses source build for CLI
```

For cross-project CLI use:

```sh
just dev-link    # one-time: creates ~/.local/bin/veld-dev
cd ~/other-project
veld-dev feedback listen --name myrun
```

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
