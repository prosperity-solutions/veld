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
cargo build
cargo test
```

The workspace has four crates:

| Crate | Description |
|-------|-------------|
| `veld` | CLI binary |
| `veld-core` | Shared library (config, orchestrator, state, health checks) |
| `veld-helper` | Privileged daemon for DNS/Caddy management |
| `veld-daemon` | User-space daemon for health monitoring and GC |

## Guidelines

- Keep PRs focused — one feature or fix per PR.
- Don't break existing behavior without discussion.
- Add tests where it makes sense, but don't over-test trivial code.
- If CI fails, fix it before requesting review.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
