# Agents Guide — veld

veld is a Rust-based local development environment orchestrator for monorepos. This repo contains the CLI tool, the helper daemon, the user-space daemon, the public web gateway, and the marketing website.

## Workspace Structure

```
veld/
├── crates/
│   ├── veld/              # CLI binary
│   ├── veld-core/         # Shared types, feedback protocol
│   ├── veld-daemon/       # User-space daemon (health, GC, state)
│   ├── veld-helper/       # Privileged daemon (DNS, Caddy routes)
│   ├── veld-share/        # Shared P2P transport (iroh) — used by daemon + gateway
│   └── veld-gateway/      # Public web gateway server (veld share --web)
├── website/               # Marketing website (one static HTML page)
│   ├── index.html         # The whole site (/, single boring page)
│   ├── llms.txt           # LLM index
│   ├── llms-full.txt      # LLM full docs
│   └── AGENTS.md          # Website-specific agent guide
├── schema/                # JSON Schema for veld.json
├── testproject/           # Example project for manual testing
├── veld.json              # Veld config to serve the website locally
└── AGENTS.md              # This file
```

## Building

```sh
cargo build --release
# Binaries: target/release/{veld, veld-helper, veld-daemon, veld-gateway}
```

## Serving the Website Locally

The root `veld.json` is configured to serve the website for local development and feedback:

```sh
veld start website:local --name dev
```

This starts a local HTTP server for the `website/` directory with an HTTPS URL like `https://website.dev.veld.localhost`. You can use `veld feedback` to leave feedback on the website via the in-browser overlay, enabling human-agent collaboration on design and content changes.

### Feedback workflow

1. Start the website: `veld start website:local --name dev`
2. Open the URL in your browser
3. Use the feedback overlay to leave comments on the website
4. The agent pulls the next item: `veld feedback next --wait --name dev --json`
5. The agent fixes it, then `veld feedback reply <thread-id> "..."` and loops
6. Repeat until the reviewer clicks "Done" (`result: "ended"`)

## Agent Skills

Veld ships consumer-facing skills in `skills/` for the [npx skills](https://github.com/vercel-labs/skills) ecosystem. Users install with `npx skills add prosperity-solutions/veld`. Skills are auto-discovered from `skills/*/SKILL.md`.

For **contributors** working on this repo with Claude Code, `.claude/skills/ship/` provides a `/ship` workflow skill that wraps the PR Workflow below (kickoff questionnaire → autonomous implement → adversarial review rounds → draft PR → wait for green CI → bypass-merge when authorized). It's a dev tool, not a published consumer skill.

## PR Workflow

Follow this workflow for every feature or fix:

1. **Implement** — Make the code changes.
2. **Docs audit** — Before considering the work done, check the [documentation checklist](#documentation-checklist) below.
3. **Review rounds (repeated multi-angle)** — Run the five-angle adversarial review in [docs/agentic-review.md](docs/agentic-review.md) on the diff, fix the findings, then repeat the review on the post-fix diff. Iterate until a round surfaces no critical/major findings (see the doc's stop condition + round cap). Do not run separate single-reviewer warm-up rounds — the multi-angle pass replaces them. (For trivial changes, three angles — counterfactual + what-isn't-here + self-consistency — suffice, per the tuning notes in the review doc.)
4. **Push to draft PR** — Push the branch and open a draft PR on GitHub.
5. **Wait for CI** — All checks must be green. Never assume checks are missing just because they haven't started yet.
6. **Ask before merging** — Ask the maintainer for explicit approval before merging. Only merge with admin bypass if the maintainer explicitly says so upfront at task start.

## Documentation Checklist

When a change introduces new config fields, CLI flags, subcommands, or user-visible behavior, update **all** of the following:

| File | What to update |
|------|----------------|
| `README.md` | Features list, CLI reference table, Configuration section |
| `docs/configuration.md` | Config field reference (top-level table, field section, variant table) |
| `skills/veld/SKILL.md` | Agent-facing skill (quick reference, gotchas) |
| `skills/veld/reference/config.md` | Agent-facing config reference |
| `schema/v2/veld.schema.json` | JSON Schema for v2 configs (probes, recovery, skip_if) |
| `website/index.html` | **Marketing site.** If the change adds or renames a user-visible capability, decide whether it belongs on the site and, if so, update the relevant part — the features grid, CLI reference, sharing section, or the architecture diagram (`for the nerds`). Keep the brand tokens per `website/AGENTS.md` / `docs/branding.md`. |
| `website/llms-full.txt` | LLM-facing docs — sync with any `index.html` content change (see `website/AGENTS.md`) |

**Always ask "does the website need to change?"** For every user-visible feature, weigh whether it's worth surfacing on the marketing site — the site should stay an accurate, current picture of what veld can do, not drift behind the CLI. If it fits, update `website/index.html` (and `llms-full.txt`); if it deliberately doesn't, say so.

If the change is purely internal (refactor, bugfix with no new surface area), this checklist does not apply.

## Key Conventions

- **Any user-supplied command executed by a daemon must inherit the user's login-shell `PATH`.** The daemon (launchd), gateway (systemd), and helper run with a bare service `PATH`, so a raw `sh -c` cannot find user-installed CLIs (`op`, `vault`, `pg_isready`, version-manager shims) even though the same command works in the user's terminal. Resolve the PATH with `veld_core::user_path::resolve_user_path()` and pass it via `.env("PATH", …)` — as liveness probes (`veld-daemon/src/monitor.rs`) and `SecretSource::Command` token resolution (`veld-share/src/endpoint.rs`) already do. Never spawn a config-declared command on a daemon without this. Scope: the rule covers daemon/gateway/helper spawns only — commands the `veld` CLI itself spawns (orchestrator `command`/`start_server` steps, setup checks, actions) already inherit the terminal's `PATH` and are exempt. Only `PATH` is inherited, never the rest of the shell environment.
- **Every user-facing HTML surface carries the Veld brand.** Any HTML a Veld
  binary serves to a browser — management UI, gateway pages (index, login,
  404), overlays, error pages, and every future surface — must follow
  [docs/branding.md](docs/branding.md): embedded `veld.` wordmark (accent-green
  dot), the dark product token palette, self-contained assets (inline CSS,
  data-URI favicon, no external requests), and no enumerable share/run
  metadata on anonymous pages. Never ship an unbranded, system-default-styled
  page; when adding one to an existing binary, reuse its page shell (e.g.
  `veld-gateway`'s `pages::shell`) instead of writing bespoke HTML.
- **Diagnostics go to stderr; machine-readable output goes to stdout.** Tracing
  logs, progress, and human status/receipt lines are stderr; `--json` payloads
  and the terminal node's own output under `veld start --oneshot` are the only
  things on stdout. A stray `println!`/`tracing::*!`-to-stdout in a command
  silently corrupts an agent's or CI's stdout capture — keep chrome on stderr.
- Domain: `veld.oss.life.li` (not `veld.dev`)
- Install URL: `https://veld.oss.life.li/get`
- URL templates use `{variable}` (single braces); commands/env use `${variable}`
- `command` type steps do NOT get `${veld.port}` — only `start_server` does
- `start_server` outputs are objects; `command` outputs are arrays
- Website content changes must be synced to `llms-full.txt` (see `website/AGENTS.md`)
