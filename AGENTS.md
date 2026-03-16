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

## Key Conventions

- Domain: `veld.oss.life.li` (not `veld.dev`)
- Install URL: `https://veld.oss.life.li/get`
- URL templates use `{variable}` (single braces); commands/env use `${variable}`
- `command` type steps do NOT get `${veld.port}` — only `start_server` does
- `start_server` outputs are objects; `command` outputs are arrays
- Website content changes must be synced to `llms-full.txt` (see `website/AGENTS.md`)
