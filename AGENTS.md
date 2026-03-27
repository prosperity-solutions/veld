# Agents Guide — veld

veld is a Rust-based local development environment orchestrator for monorepos. This repo contains the CLI tool, the helper daemon, the user-space daemon, and the marketing website.

## Workspace Structure

```
veld/
├── crates/
│   ├── veld/              # CLI binary
│   ├── veld-core/         # Shared types, feedback protocol
│   ├── veld-daemon/       # User-space daemon (health, GC, state)
│   └── veld-helper/       # Privileged daemon (DNS, Caddy routes)
├── website/               # Marketing website (3 static HTML pages)
│   ├── index.html         # Agents view (/, structured for LLMs)
│   ├── humans.html        # Humans view (/humans, docs + demos)
│   ├── experience.html    # Experience view (/experience, cinematic)
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
# Binaries: target/release/veld, target/release/veld-helper, target/release/veld-daemon
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
4. The agent reads feedback: `veld feedback --wait --name dev`
5. The agent makes changes based on feedback
6. Repeat

## Agent Skills

Veld ships consumer-facing skills in `skills/` for the [npx skills](https://github.com/vercel-labs/skills) ecosystem. Users install with `npx skills add prosperity-solutions/veld`. Skills are auto-discovered from `skills/*/SKILL.md`.

## PR Workflow

Follow this workflow for every feature or fix:

1. **Implement** — Make the code changes.
2. **Docs audit** — Before considering the work done, check the [documentation checklist](#documentation-checklist) below.
3. **Self-review rounds** — Use sub-agents to review the diff. Iterate (review → fix → review → fix) until no issues remain.
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
| `website/llms-full.txt` | LLM-facing docs (if applicable, see `website/AGENTS.md`) |

If the change is purely internal (refactor, bugfix with no new surface area), this checklist does not apply.

## Key Conventions

- Domain: `veld.oss.life.li` (not `veld.dev`)
- Install URL: `https://veld.oss.life.li/get`
- URL templates use `{variable}` (single braces); commands/env use `${variable}`
- `command` type steps do NOT get `${veld.port}` — only `start_server` does
- `start_server` outputs are objects; `command` outputs are arrays
- Website content changes must be synced to `llms-full.txt` (see `website/AGENTS.md`)
