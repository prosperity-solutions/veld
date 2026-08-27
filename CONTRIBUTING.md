# Contributing to Veld

Veld is 100% vibe coded with [Claude Code](https://claude.com/claude-code). The first well-working version was shipped in 3 days — entirely through agentic contributions. We want to keep it that way.

## We only accept agentic contributions

This means: your PR should be authored, reviewed, or substantially driven by an AI coding agent (Claude Code, Cursor, Copilot Workspace, Aider, etc.). We're not gatekeeping which tool you use — just that you're using one.

Why? Because that's how this project was built, and it's how we believe modern software should be maintained. If an agent can't understand the codebase well enough to make a change, that's a signal the codebase needs to be clearer — not that we need more manual labor.

## How to contribute

1. **Fork the repo** and create a branch from `main`.
2. **Use an AI coding agent** to implement your changes.
3. **Follow conventional commits** — we use [Conventional Commits](https://www.conventionalcommits.org/) for semantic versioning. Prefix your commit messages with `feat:`, `fix:`, `docs:`, `chore:`, etc.
4. **Run the checks locally first** — `just lint` and `just test` (`cargo fmt`, `cargo clippy`, `cargo test`) must all be green *before* you ask CI for an opinion. This is not a formality: **CI does not run while a PR is a draft.**
5. **Open a PR** with a clear description of what changed and why. Open it as a draft while you're still iterating — pushes to a draft are free — and mark it ready for review once your agent has finished reviewing the change and your local checks are green. That's the step that starts CI.

## Development setup

```sh
git clone https://github.com/prosperity-solutions/veld.git
cd veld
just setup-frontend   # install Node.js dependencies (once)
just setup-ui         # deps for the /ide management UI + desktop shell (also
                      # refreshes them after a bump; the ui/desktop recipes
                      # install what they need on their own)
just build            # build Rust + frontend
just test             # run all tests
```

For the /ide management UI and the Electron desktop shell, see
[desktop/ARCHITECTURE.md](desktop/ARCHITECTURE.md) (`just dev-ui`,
`just dev-desktop`, and `just desktop-package` to build the installers —
macOS builds are ad-hoc signed only, so one handed to someone else needs
`xattr -dr com.apple.quarantine` on the other end).

The workspace has four crates:

| Crate | Description |
|-------|-------------|
| `veld` | CLI binary |
| `veld-core` | Shared library (config, orchestrator, state, health checks) |
| `veld-helper` | Privileged daemon for DNS/Caddy management |
| `veld-daemon` | User-space daemon for health monitoring, feedback overlay, and GC |

## Local development

### First: install veld

Working on veld needs a working veld — the dev stack below is a veld
environment, started and supervised by your *installed* instance, and that
instance is also what owns Caddy, DNS and the helper:

```sh
curl -fsSL https://veld.oss.life.li/get | bash
veld setup unprivileged     # no sudo; `veld setup` alone only prints status
veld --version
```

`bash`, not `sh`: the installer uses `set -o pipefail` and `[[ ]]`, and on a
Linux box where `/bin/sh` is dash it dies on its first line.

Already have one? Make sure it is current — this repo's `veld.json` uses config
features from the latest release, and an older binary fails to parse the **whole
file** (see the note below). `veld update`.

### The dev stack is a veld environment

The usual way to run veld while working on it is to let veld run it. The root
`veld.json` declares the whole stack — dev daemon, `/ide` with HMR, and the
Electron shell:

```sh
veld start --preset dev            # daemon + /ide + Electron, empty database
veld start --preset dev-keep       # …reusing the database this run already has
veld start --preset dev-from-real  # …on a snapshot of the REAL database
veld start --preset dev-headless   # empty database, no Electron
veld status
veld logs dev-daemon --follow
veld stop
```

`dev` starts on an **empty** database, so it is reproducible and a migration
always runs against a known state. The cost is that the desktop app's own state
lives there — imported repos, worktrees, lanes, terminal layouts — so a fresh
start has none of it. `dev-keep` is the same stack over whatever the run's
database already holds; reach for it once you have things arranged.

> **`unknown variant 'long_running'`?** Your installed veld predates the release
> that added it, and the failure is the *whole file* — `veld start website:local`
> stops working too, because a config is parsed as one unit. Run `veld update`.
> Until you do, `just dev start --preset dev` runs the same thing through this
> branch's own binary, which is also the answer while you are working on a change
> to the config language itself.

**This is parallel-safe, which is the point.** Every worktree's stack gets its
own allocated ports, its own database under `.veld-dev/<run>/`, its own
hostnames, and its own `~/.local/bin/veld-dev-<run>` — so two branches can each
have a full stack up at the same time. The stack you start is monitored by the
*installed* veld, so `veld status` keeps working even when the dev daemon is
wedged, and a schema-ahead branch can never touch your real database.

The dev-stack nodes:

| Node | What it is |
|---|---|
| `dev-build` | `cargo build -p veld-daemon -p veld` plus the npm deps. Gates everything else, so a broken build fails the run instead of starting three things against a half-built tree |
| `dev-db` | Prepares `.veld-dev/<run>/veld.db`. Variants `ensure` (default — creates the directory and nothing else), `fresh`, `from-real` |
| `dev-daemon` | The daemon from source, on an allocated port, with its own socket and holder directory |
| `dev-ui` | vite for `/ide`, proxying `/api` to `dev-daemon` |
| `dev-electron` | Supervises the Electron shell against `dev-ui`. A `long_running` node with `"ports": null` — it binds nothing |
| `dev-link` | Writes `~/.local/bin/veld-dev-<run>`, removed again at stop |

`just dev-db-list` shows every per-run database in the worktree with its schema
version. They deliberately outlive a stopped run, so stopping the stack does not
throw away the state you were debugging.

### Quitting Electron does not stop the stack

You can close or Cmd+Q the desktop app and keep working; bring it back with:

```sh
veld action open --node dev-electron
```

That is not free behaviour — it needed the node to be a supervisor
(`scripts/dev/electron.sh`) rather than `electron .`. veld's health monitor
treats **any** node process dying as a crash of the whole run: it marks the run
`crashed` and SIGTERMs every surviving sibling. Run bare, one Cmd+Q took the dev
daemon and the vite server down with it. The node is therefore the supervisor,
which outlives a quit and relaunches Electron when the action asks it to.

The action handles both ways a window goes away: if Electron is still running
(on macOS, closing every window does not quit — the tray stays), it activates
the process so Electron's own `activate` handler opens one; if it has exited,
the supervisor relaunches it. The first activation may ask for macOS automation
permission; clicking the tray icon does the same job.

### The bootstrap tier

Everything below this line is the older, **single-worktree** path: fixed ports,
one dashboard hostname, one `veld-dev` wrapper. It has not gone anywhere,
because you need a way to run the daemon when the thing you broke is
`veld start` itself, and because a fresh clone has nothing to start a veld run
with. Use it for that; use the preset above the rest of the time.

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
instance (dev DB, daemon port 19898, dev socket). One file naming one worktree,
so whichever worktree ran `dev-link` last owns the name — the dev stack's
`dev-link` node writes a per-run `veld-dev-<run>` beside it instead, which is
the one to use when you have a stack up. It is the complete CLI —
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
| `veld start --preset dev` | The whole dev stack, per worktree (the usual way) | No |
| `just dev <args>` | Run CLI from source (safe, no install) | No |
| `just dev-link` | Create the bootstrap `veld-dev` wrapper for cross-project use | No |
| `just dev-db-list` | Every per-run dev database, with its schema version | No |
| `just dev-install-daemon` | Install daemon + restart service | No |
| `just dev-install-helper` | Install helper + restart Caddy | Yes |
| `just dev-install` | CLI + daemon | No |
| `just dev-install-all` | CLI + daemon + helper | Yes |
| `just dev-restore` | Restore to released version | No |
| `just build` | Build Rust + frontend | No |
| `just test` | Run all tests | No |
| `just lint` | Clippy + rustfmt + TypeScript type check + JS/TS Biome lint | No |
| `just workflow-gates` | Two workflow gates: no CI job can run on a draft PR, and `release.yml`'s publish script cannot ship an incomplete release or leave a complete one a draft. Run it whenever you touch `.github/workflows/`. Needs PyYAML: `python3 -m pip install --user pyyaml` | No |
| `just commit-subjects` | Check your commit subjects against the conventional-commits pattern CI enforces | No |

> **Git hooks (optional, recommended):** the repo ships `lefthook.yml`, a
> [`lefthook`](https://github.com/evilmartians/lefthook) `pre-push` hook that runs
> `just lint` so a red diff can't reach a draft PR. Install the `lefthook` binary
> (`brew install lefthook`, `cargo install lefthook`, or `npm i -g lefthook`) and run
> `lefthook install` once per clone to activate. It is intentionally skippable —
> `git push --no-verify` (or `LEFTHOOK=0`) bypasses it when you deliberately want a
> red draft.

## Guidelines

- Keep PRs focused — one feature or fix per PR.
- Don't break existing behavior without discussion.
- Add tests where it makes sense, but don't over-test trivial code.
- If CI fails after you've marked the PR ready, fix it before asking for a human review.
- **Draft PRs don't run CI.** Every job in `ci.yml` skips while a PR is a draft, and marking it ready for review is what starts them. A run occupies five macOS jobs alongside a four-target release build matrix, so a branch that gets 20 intermediate pushes while it's being written used to mean 20 full runs of that — runner time and macOS concurrency that queues ahead of everyone else's work. Keep the work in draft, run `just lint`/`just test` locally as you go, and flip to ready when it's genuinely ready.
- **A skipped check shows up as a passing one.** So a draft PR looks green even though nothing ran — don't read that tick as a pass. `gh pr checks --json name,bucket,workflow` puts skipped ones in the `skipping` bucket; that's the only thing that distinguishes them. Note that the `Release` workflow's jobs are *supposed* to be skipped on a PR, so check the `CI` workflow's jobs specifically. And don't rely on that bucket check alone: a skipped **matrix** job reports its name as the literal un-expanded template string, so it never lines up with the ready run's expanded names and the bucket never empties on a PR that was ever a draft. The unambiguous answer is the ready run's own jobs — `gh run view <ready-run-id> --json jobs` (see AGENTS.md's CI cost convention).

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
