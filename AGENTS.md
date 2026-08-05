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

For **contributors** working on this repo with Claude Code, `.claude/skills/ship/` provides a `/ship` workflow skill that wraps the PR Workflow below (kickoff questionnaire → autonomous implement → adversarial review rounds → draft PR → mark ready for review → wait for green CI → bypass-merge when authorized). It's a dev tool, not a published consumer skill.

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
4. **Push to draft PR** — Push the branch and open a draft PR on GitHub. **CI does not run while a PR is a draft** (see the CI cost convention below), so do not push intermediate commits expecting a signal from GitHub — the local pre-pass is your only signal at this stage, which is what makes it load-bearing rather than advisory.
5. **Mark ready for review — only after the review loop is done.** `gh pr ready` is what actually spends runner minutes, so it is a deliberate step, not a formality. Do not flip a PR ready until step 3's exit criteria hold *and* the local pre-pass (clippy, fmt, tests, plus the UI checks when the diff touches them) is green. A draft that has not been locally reviewed has not earned a CI run.
6. **Wait for CI** — All checks must be green *and none of them skipped*. A skipped job reports as a passing check, so a draft's checks look successful; `gh pr checks` will exit 0 and say "All checks were successful" on a PR where nothing ran. Confirm `isDraft: false` and zero `skipping` buckets (commands in the CI cost convention below) before believing a green summary. Never assume checks are missing just because they haven't started yet — but if none appear after marking ready, the PR is probably `CONFLICTING`, which stops `pull_request` events firing entirely.
7. **Ask before merging** — Ask the maintainer for explicit approval before merging. Only merge with admin bypass if the maintainer explicitly says so upfront at task start.

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
- **CI runs only on a PR that is ready for review, so marking one ready is a
  spending decision.** Every job in `ci.yml` and `release.yml` carries
  `if: github.event_name != 'pull_request' || github.event.pull_request.draft == false`,
  and both workflows list `ready_for_review` in `on.pull_request.types` — both
  halves are required, because with the guard and without the type a PR flipped
  from draft to ready would fire nothing at all and sit with no checks forever.
  Pushes to a draft still create a workflow run whose jobs all skip: queue time,
  not runner minutes. The reason is what this workflow costs — a `pull_request`
  run reaches **five macOS legs** (`integration`, `injection-test`, the mac leg of
  `desktop-package`, and both darwin legs of `release-build`), each billed at 10×
  a Linux minute, on top of a four-target release build matrix — all of which an
  agent was previously re-running on every intermediate commit of a branch nobody
  had reviewed yet.

  **A skipped job reports as a *passing* check, so a draft PR looks green rather
  than looking unrun.** This is the trap the convention creates, and it is
  Actions' behaviour, not ours: `gh pr checks` sorts a skipped check into the
  `skipping` bucket, exits 0, and prints "All checks were successful". Nothing
  distinguishes that from a real pass except the bucket. So never conclude "CI
  is green" from an exit code or a summary line — check both of these:

  ```sh
  gh pr view --json isDraft,mergeable          # isDraft must be false
  gh pr checks --json name,bucket,event        # no bucket may be "skipping"
  ```

  The other consequence: **the local pre-pass is the only correctness signal a
  draft gets**, so never push-and-mark-ready on a red pre-pass hoping CI will
  tell you what broke, and never treat "I'll let CI catch it" as a substitute for
  running clippy/fmt/tests locally.

  Mechanics worth not rediscovering: the guard is repeated per job because
  Actions has no workflow-level `if`, and although YAML anchors *are* supported
  (GA 2025-09-18), the documented pattern defines the anchor on first use — inside
  one job — so every other job's guard would depend on that job's position in the
  file. A dependent job may instead inherit the skip through `needs:`, but must
  not carry `always()`/`!cancelled()`, which runs it even when its dependencies
  skipped. Do not OR the guard with anything either: `true || <guard>` reads as
  guarded and gates nothing. `github.event_name != 'pull_request'` is also the
  *wrong* half for a `pull_request_target` workflow, where it evaluates true — use
  the bare `github.event.pull_request.draft == false` there (on a push the left
  side is null, and Actions coerces null and false alike to `0`, so the job still
  runs). `tests/validate-workflow-gates.py` (run by the `schema` job, or
  `just workflow-gates` locally) asserts all of this and self-tests that it still
  detects each hole, because nothing in Actions does and the symptom of a missing
  guard is a bill rather than a failing job.

  Both workflows also carry `concurrency` with `cancel-in-progress` scoped to
  `pull_request` only — a superseded PR run is waste, but a half-cancelled push
  to `main` is a release that tagged without publishing.
- **Any user-supplied command executed by a daemon must inherit the user's login-shell `PATH`.** The daemon (launchd), gateway (systemd), and helper run with a bare service `PATH`, so a raw `sh -c` cannot find user-installed CLIs (`op`, `vault`, `pg_isready`, version-manager shims) even though the same command works in the user's terminal. Pass it via `.env("PATH", …)`, resolved by one of the two helpers in `veld_core::user_path`. Anything inside a daemon uses `cached_user_path()` — every request handler (`spawn_veld`, the desktop directory picker and git plumbing, and `SecretSource::Command` token resolution in `veld-share/src/endpoint.rs`, which `POST /api/shares` reaches while the caller waits) and the health monitor's liveness probes; a dedicated `warm_user_path_cache` task keeps that entry warm, deliberately **not** the monitor's scan, whose own cadence stretches to 300s during a recovery restart. `resolve_user_path()` is the uncached primitive, for a one-shot context that resolves once and exits — today only a CLI run's lazy var sources. Resolution spawns a login shell: sub-second normally, up to 10s on a stalled rc file, and the slow rc files belong to exactly the users who need the resolved PATH — which is why a handler must not resolve inline. The cache is one writer (the warm task) publishing into a cell that readers only read, with a single rule doing the work: **a resolution that learned nothing never displaces one that did.** A stall, or a shell that exits zero while answering with nothing better than the process's own `PATH`, leaves the previous value alone — because that value is the bare service PATH, i.e. the bug itself. Resist adding a TTL or a which-resolution-wins rule back: the first version of this cache had both, and each grew its own bug. Never spawn a config-declared command on a daemon without this. **A login shell is not a substitute for the injection.** `$SHELL -l -c` sources `.zprofile` but not `.zshrc`, which is where version managers (nvm, fnm, rbenv) and `brew shellenv` live on most machines; on Debian `/etc/profile` overwrites `PATH` outright, so a login shell wrapped around an injected PATH *discards* it. That mistake shipped in `spawn_veld` and made every UI- or Desktop-initiated `veld start` resolve node commands against the bare launchd PATH (`sh: npx: command not found`). Spawn the binary directly with the injected PATH; don't reach for a shell to get it. Scope: the rule covers daemon/gateway/helper spawns only — commands the `veld` CLI itself spawns (orchestrator `command`/`start_server` steps, setup checks, actions) already inherit the terminal's `PATH` and are exempt. Only `PATH` is inherited, never the rest of the shell environment — and note that this genuinely differs from the CLI path, where a macOS terminal's zsh *is* a login shell, so `.zprofile` exports (`JAVA_HOME`, `LANG`, tool tokens) do reach a terminal-run `veld` and its node commands but not a daemon-spawned one. A node that needs such a variable must declare it in its `env`; "works in my terminal, fails from the UI" for a non-PATH variable is this asymmetry, not a bug.
- **`rg` needs `-a` on `crates/veld-daemon/ui/src/App.tsx`.** That file contains a
  NUL byte (`trustedOrigins.join("\0")`), so ripgrep and grep classify the largest
  file in the UI as binary and **silently skip it** — no match, no warning. A sweep
  for a symbol across the UI will report zero hits while the only production consumer
  sits in that file. Found when a review agent concluded a function had no callers.
  Use `rg -a` (or `grep -a`) for anything that greps the UI tree.
- **Never let a dev build touch the production database — and use the dev-DB
  recipes.** The installed veld's state lives in one file per user
  (`<data_dir>/veld/veld.db`). A binary that opens it and applies a migration makes
  it unreadable to the installed release, because a binary refuses a `user_version`
  newer than it supports (`DbError::NewerSchema`) — so one careless `cargo test`
  can take a developer's working veld offline until the schema is hand-rolled back.

  The workflow, which is already built:

  | Command | What it does |
  |---|---|
  | `just dev <cmd>` | Run the dev CLI against the dev DB (`.veld-dev/veld.db`, gitignored) on its own daemon port |
  | `just dev-db-from-real` | Snapshot the **real** DB into the dev DB — the way to exercise a migration against real-shaped data. The real file is never written. **macOS-only path today** |
  | `just dev-db-reset` | Wipe the dev DB for a fresh-install path |

  Two files, both in `.veld-dev/`: `veld.db` belongs to the `just dev` instance,
  and `veld-cargo.db` is what a plain `cargo run`/`cargo test` gets. They are split
  on purpose — sharing one meant `cargo test --workspace` wrote the database a
  running dev daemon owned, and a `cargo test` between `dev-db-from-real` and
  `just dev` silently migrated the snapshot to head so the rehearsal verified
  nothing. So run the rehearsal through `just dev <cmd>`, not through `cargo run`.

  **Test a new migration with `just dev-db-from-real`, not only with fixtures.** A
  synthetic row cannot tell you that 16 worktrees across 2 repos survive, that
  `PRAGMA foreign_key_check` stays quiet, or that a backfill sentinel lands on
  every pre-existing row. Verify counts before and after, plus `integrity_check`.

  As a backstop, `Db::default_path()` resolves *any* cargo-built binary to
  `.veld-dev/veld-cargo.db` automatically — detected by walking up from
  `current_exe()` to the directory cargo marks with `CACHEDIR.TAG`
  (`Db::cargo_target_db`), bounded at `$HOME` so a stray marker cannot divert an
  installed binary, then requiring the worktree's `justfile`. `VELD_DB_PATH` still
  overrides, and `Db::open_at` with a tempdir remains right for an isolated test.

  **Do not "simplify" the backstop away**, and do not replace it with a
  `#[cfg(test)]` guard: `veld-core` is compiled *without* `cfg(test)` when
  `veld-daemon`'s tests link it, so a test-only panic never fires for the callers
  that matter. It exists because a `Db::open()` reached the PTY session-spawn path
  (reading the terminal detach grace, once that became a setting) and twelve
  *existing* tests migrated a real database as a side effect — no test in the diff,
  no test that looked like a database test.
  `a_test_binary_never_resolves_to_the_real_user_database` pins it.

  The corollary when adding a setting the daemon itself reads: putting a
  `Db::open()` on a hot request path is a design decision, not a detail.
- **Anything that must outlive the daemon leaves its process group.** A child
  spawned with `process_group(0)` survives launchd's `bootout` and systemd's
  `KillMode=process`; a plain child does not. Both halves are required and neither
  is optional: the helper's unit has carried `KillMode=process` since Caddy was
  first spawned from it, and the *daemon's* unit gained it when terminal holders
  arrived (`veld-daemon --pty-holder`, one process per terminal session, which
  owns the PTY master precisely so `veld update` stops ending every shell — see
  `crates/veld-daemon/src/pty/holder.rs`). Under the default control-group kill
  mode, `systemctl restart veld-daemon` SIGKILLs every descendant, which silently
  reintroduces exactly the failure the out-of-process design removes. The
  corollary is the ownership rule: **whoever holds the unreaped child is the only
  one that may signal it.** The daemon asks a holder to hang up over the socket
  and never signals the shell's process group itself — a `killpg` racing
  `child.wait()` can land on a recycled pid.
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
