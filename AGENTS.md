# Agents Guide — veld

veld is a Rust-based local development environment orchestrator for monorepos. This repo contains the CLI tool, the helper daemon, the user-space daemon, the public web gateway, and the marketing website.

## Workspace Structure

```
veld/
├── crates/
│   ├── veld/              # CLI binary
│   ├── veld-core/         # Shared types, feedback protocol
│   ├── veld-daemon/       # User-space daemon (health, GC, state)
│   │   ├── frontend/      # npm: feedback overlay + client-log bundles (esbuild)
│   │   └── ui/            # npm: management UI (React+Mantine), served at /ide
│   ├── veld-helper/       # Privileged daemon (DNS, Caddy routes)
│   ├── veld-share/        # Shared P2P transport (iroh) — used by daemon + gateway
│   └── veld-gateway/      # Public web gateway server (veld share --web)
├── desktop/               # Veld Desktop: Electron shell around /ide (ARCHITECTURE.md)
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

**Every skill under `.claude/skills/` must carry `metadata.internal: true` in its
SKILL.md frontmatter.** The `npx skills` CLI scans `.claude/skills/` alongside
`skills/` — it is a built-in discovery prefix, not something the repo opts into —
so a contributor-only skill without that flag gets installed into unrelated
projects by `npx skills add prosperity-solutions/veld`. `internal: true` is the
CLI's supported opt-out and is honoured by both its discovery paths (local clone
and GitHub tree). Claude Code ignores the field, so `/ship` still loads here.
Verify with `npx skills add . --list` from the repo root: only the `skills/`
entries may appear.

## PR Workflow

Follow this workflow for every feature or fix:

1. **Implement** — Make the code changes.
2. **Docs audit** — Before considering the work done, check the [documentation checklist](#documentation-checklist) below.
3. **Review loop (autonomous, multi-angle, staged)** — Run the loop in [docs/agentic-review.md](docs/agentic-review.md) on the diff: pre-pass (clippy/fmt/tests) → context pack → staged angles as parallel background subagents with explicit per-angle model tiering → verify each critical/major yourself → fix → re-review the fix delta. Loop until the doc's exit criteria hold or a cap/hard stop fires. Do not run separate single-reviewer warm-up rounds — the multi-angle pass replaces them. Diffs under ~50 lines with no stakes flag take the doc's trivia clause (§11); the stakes override (§3.3) is never downgradable.
4. **Push to draft PR** — Push the branch and open a draft PR on GitHub.
5. **Wait for CI** — All checks must be green. Never assume checks are missing just because they haven't started yet.
6. **Ask before merging** — Ask the maintainer for explicit approval before merging. Only merge with admin bypass if the maintainer explicitly says so upfront at task start.

**Hand the maintainer the running software before the PR.** For any change with a UI or a new CLI surface, the house style is *checkpointed autonomy*: stop after implementing so they can drive it themselves, run the review loop unattended, then stop once more after the review fixes — because fixes that touch rendering, wire shapes, or CLI output regress exactly what was hand-verified the first time. A review subagent cannot see that a graph renders wrong. `/ship` captures this in its kickoff questionnaire; the two stops are planned, not escalations.

## Documentation Checklist

When a change introduces new config fields, CLI flags, subcommands, or user-visible behavior, update **all** of the following:

| File | What to update |
|------|----------------|
| `README.md` | Features list, CLI reference table, Configuration section |
| `docs/configuration.md` | Config field reference (top-level table, field section, variant table) |
| `skills/veld/SKILL.md` | Agent-facing skill (quick reference, gotchas) |
| `skills/veld/reference/config.md` | Agent-facing config reference |
| `schema/v2/veld.schema.json` | JSON Schema for v2 configs (probes, recovery, skip_if) |
| `schema/v3/veld.schema.json` | JSON Schema for v3 configs. **Hand-maintained — there is no compiler check tying it to the Rust types.** Any config field you add or change must be reflected here AND covered by `schema/v3/examples/`, which `tests/validate-schema.sh` validates against the schema and `schema_v3_examples_round_trip` deserializes with serde. That pair is the drift gate; skipping it ships a schema that confidently reports the wrong thing in the editor |
| `docs/migrating-to-v3.md` | Migration guide. Update whenever v3 gains a field, or whenever something changes for v1/v2 configs too |
| `website/index.html` | **Marketing site.** If the change adds or renames a user-visible capability, decide whether it belongs on the site and, if so, update the relevant part — the features grid, CLI reference, sharing section, or the architecture diagram (`for the nerds`). Keep the brand tokens per `website/AGENTS.md` / `docs/branding.md`. |
| `website/llms-full.txt` | LLM-facing docs — sync with any `index.html` content change (see `website/AGENTS.md`) |

**Always ask "does the website need to change?"** For every user-visible feature, weigh whether it's worth surfacing on the marketing site — the site should stay an accurate, current picture of what veld can do, not drift behind the CLI. If it fits, update `website/index.html` (and `llms-full.txt`); if it deliberately doesn't, say so.

If the change is purely internal (refactor, bugfix with no new surface area), this checklist does not apply.

## Config Authoring Principles

These govern the `veld.json` surface. They are not style preferences — each one
exists because the obvious alternative makes a large monorepo config unreadable,
and several were paid for in this codebase already.

- **Deduplicate values, never structure.** Which keys a node has stays written in
  that node; only *values* get a single definition point (`vars`, node-level
  defaults). A reader — or a coding agent — must be able to open a node file and
  see what that node runs, and `rg <ENV_VAR_NAME>` must still find the line that
  sets it.
- **Keys stay at the use site.** A var holds a value, not a config fragment. The
  moment a probe block or an `env` map can live in a var, this is a template
  system.
- **No inheritance, mixins, `extends`, or node templates.** No loops, `matrix`,
  `for_each`, or ranges. No conditionals, operators, or arithmetic in
  interpolation. No second config language that compiles to JSON. If five nodes
  are similar, write five nodes and hoist the shared *values*.
- **`argv` is preferred over `shell`, and `shell` is permanently supported.**
  `argv` is spawned directly, so an interpolated value can never change the
  argument count; `shell` is the escape hatch that makes `argv` a safe default,
  because any node that misbehaves under it can be reverted with no veld change.
- **Any new field that runs something is called `argv`/`shell`** — never
  `command`, `cmd`, `exec`, or `run`. One vocabulary, everywhere.
- **A secret is a pointer plus a sensitivity flag, never custody.** veld carries a
  value source and `secret: true`, resolves it at run start, and delivers it to a
  child's environment or a file. Never into a command line (the process table is
  world-readable), a log, `--json` output, or a share payload. No vendor name ever
  becomes a first-class concept in the schema.
- **New diagnostics never go in the loader.** `config::parse_config` runs on every
  subcommand — `stop`, `status`, `logs` — and inside the daemon monitor. `on_stop`
  is read from the on-disk config *at stop time*, so a config that fails to load
  means teardown never runs and containers leak with no way to clean them up. Put
  semantic checks in `config::validate`, which returns `Finding`s and is called
  only from `veld start`, `veld lint`, and the share flow. `validate` returns a type
  `ConfigError` does not contain, so this is enforced by the compiler — keep it that
  way.
- **`veld.*` is a closed set.** Node outputs live in `${output.*}` and
  `${nodes.<node>.*}`. Merging them into the builtins let an output shadow a
  builtin on some paths and not others, so the same string resolved to two
  different values.
- **One owner for resolution.** A node+variant's effective config comes from
  `config::resolve_variant`. Do not read `variant_cfg.<field>` directly in the
  orchestrator, the graph, the monitor, or the share flow — a second resolution
  path is invisible until it produces a wrong value at runtime, or (for `share`) a
  silent consent bypass.
- **Remote execution is never added.** No `host`, `ssh`, or `target` field, ever.
  Repo-declared hooks only — a hook may not originate from a fetched extension,
  because that is what preserves the guarantee.

## Key Conventions

- **RFCs and working documents are never tracked in git.** Drafts, RFCs, PRDs,
  plans, and any other working document live in `notes/` (gitignored) — never
  commit them. The repo's tracked Markdown is user/contributor documentation
  only (`README.md`, `docs/`, `skills/`, `AGENTS.md`, `CONTRIBUTING.md`).
  Design context that must outlive a working document belongs in the PR
  description, commit messages, or `docs/` — don't cite `notes/` files from
  code comments, since readers of the repo can't see them.
- **Any user-supplied command executed by a daemon must inherit the user's login-shell `PATH`.** The daemon (launchd), gateway (systemd), and helper run with a bare service `PATH`, so a raw `sh -c` cannot find user-installed CLIs (`op`, `vault`, `pg_isready`, version-manager shims) even though the same command works in the user's terminal. Pass it via `.env("PATH", …)`, resolved by one of the two helpers in `veld_core::user_path`. Anything inside a daemon uses `cached_user_path()` — every request handler (`spawn_veld`, the desktop directory picker and git plumbing, and `SecretSource::Command` token resolution in `veld-share/src/endpoint.rs`, which `POST /api/shares` reaches while the caller waits) and the health monitor's liveness probes; a dedicated `warm_user_path_cache` task keeps that entry warm, deliberately **not** the monitor's scan, whose own cadence stretches to 300s during a recovery restart. `resolve_user_path()` is the uncached primitive, for a one-shot context that resolves once and exits — today only a CLI run's lazy var sources. Resolution spawns a login shell: sub-second normally, up to 10s on a stalled rc file, and the slow rc files belong to exactly the users who need the resolved PATH — which is why a handler must not resolve inline. The cache is one writer (the warm task) publishing into a cell that readers only read, with a single rule doing the work: **a resolution that learned nothing never displaces one that did.** A stall, or a shell that exits zero while answering with nothing better than the process's own `PATH`, leaves the previous value alone — because that value is the bare service PATH, i.e. the bug itself. Resist adding a TTL or a which-resolution-wins rule back: the first version of this cache had both, and each grew its own bug. Never spawn a config-declared command on a daemon without this. **A login shell is not a substitute for the injection.** `$SHELL -l -c` sources `.zprofile` but not `.zshrc`, which is where version managers (nvm, fnm, rbenv) and `brew shellenv` live on most machines; on Debian `/etc/profile` overwrites `PATH` outright, so a login shell wrapped around an injected PATH *discards* it. That mistake shipped in `spawn_veld` and made every UI- or Desktop-initiated `veld start` resolve node commands against the bare launchd PATH (`sh: npx: command not found`). Spawn the binary directly with the injected PATH; don't reach for a shell to get it. Scope: the rule covers daemon/gateway/helper spawns only — commands the `veld` CLI itself spawns (orchestrator `command`/`start_server` steps, setup checks, actions) already inherit the terminal's `PATH` and are exempt. Only `PATH` is inherited, never the rest of the shell environment — and note that this genuinely differs from the CLI path, where a macOS terminal's zsh *is* a login shell, so `.zprofile` exports (`JAVA_HOME`, `LANG`, tool tokens) do reach a terminal-run `veld` and its node commands but not a daemon-spawned one. A node that needs such a variable must declare it in its `env`; "works in my terminal, fails from the UI" for a non-PATH variable is this asymmetry, not a bug.
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
- **A veld config is JSONC.** Comments and trailing commas are legal in every
  config file, at every `schemaVersion`. Anything in this repo that reads a
  `veld.json` as strict JSON — a CI script, a `jq` pipeline, a test helper — must
  strip comments first (`tests/validate-schema.sh` shows the pattern, mirroring
  `veld_core::jsonc::strip`).
- **The root config has two legal names, so never hardcode one.** `veld.json` and
  `veld.jsonc` are both read (`config::ROOT_CONFIG_NAMES`; `veld init` writes the
  first). Code that walks upward uses `config::discover_config`; code that already
  knows the project root uses `config::root_config_in(dir)` — never
  `project_root.join("veld.json")`. The daemon shipped five of those, and each was
  a `veld.jsonc` project it could not see at all: no liveness probes, no actions
  in the dashboard, `veld share` refusing outright, while the CLI worked fine.
  `tests/validate-schema.sh` greps for the pattern, because nothing in the type
  system does.
- **veld does not rewrite a user's config.** `veld init` writes one when none
  exists; nothing else edits one. A serde round-trip deletes every comment, and
  the byte-level alternative cannot see structure — which is exactly how the
  removed `veld config --migrate --write` ended up editing the `hooks`/`ui` blobs
  veld promises not to interpret. If a change seems to need a config rewriter,
  emit a precise diagnostic instead and let the author (or their agent) apply it;
  `veld lint` is the verification step.
- Domain: `veld.oss.life.li` (not `veld.dev`)
- Install URL: `https://veld.oss.life.li/get`
- URL templates use `{variable}` (single braces); commands/env use `${variable}`
- `command` type steps do NOT get `${veld.port}` — only `start_server` does
- `start_server` outputs are objects; `command` outputs are arrays
- Website content changes must be synced to `llms-full.txt` (see `website/AGENTS.md`)
