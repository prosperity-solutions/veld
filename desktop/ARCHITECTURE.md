# Veld Desktop — Architecture

Veld Desktop is a desktop shell around veld's management UI. It lets a developer
import git repositories ("repos"), manage git worktrees per repo, and drive veld
runs per worktree — with the terminal and embedded-browser panes arriving in
later increments.

This document covers the foundation increment: what exists, why it's shaped
this way, and how to run it locally. The visual design source of truth is the
Claude Design handoff (kept outside the repo under `tmp/`, gitignored); the
stripped add-ons listed there (command palette, PR badges, extension system,
isolated browser sessions, pinned agent session, overview board) are
deliberately **not** part of this foundation.

## Decision log

| Decision | Choice | Why |
|---|---|---|
| Repo placement | Veld monorepo | Every feature crosses the daemon API boundary; separate repo means version skew and dual PRs. Release/CI/review machinery already exists here. |
| Name | **Veld Desktop** | The value prop *is* the veld integration (runs, URLs, share, SQLite state). A generic "agentic worktree manager" name promises veld-independence we chose not to build. Extraction later is cheap (see below). |
| UI delivery | Served by `veld-daemon` at `/v2` | The daemon already owns the management HTTP server (`127.0.0.1:19899`) and the SQLite state. The desktop app is a thin wrapper, and the same UI works in a plain browser. |
| Electron's role | Supplementary shell | Frameless window, tray icon, later: embedded webviews with isolated sessions, CLI install. The web UI must stay fully usable without it. |
| Run orchestration | Daemon shells out to the `veld` CLI | The daemon never runs the orchestrator in-process — stop/restart already work by spawning `cd <root> && veld …` in a login shell. Start follows the same pattern. |
| Theme | Handoff palette (Inter + JetBrains Mono, oklch greens) | Deviates from the classic product tokens in `docs/branding.md`; sanctioned there as the **desktop theme**. Structural branding rules (wordmark, self-contained assets, noindex) still apply. |
| UI library | None — hand-rolled components on the token CSS | The handoff design is the product identity; a styled kit (Mantine et al.) means overriding its design system forever, and the roadmap components (terminal panes, webviews, rail, palette) aren't kit components anyway. Threshold: if menu/overlay density grows, adopt **headless** primitives (Radix-style) for a11y/positioning — never a styled kit. |

### Extraction escape hatch

If the extension story matures into a standalone product: `desktop/` and
`crates/veld-daemon/ui/` are self-contained npm projects with no Rust code and
no reverse dependencies — extraction is `git filter-repo` plus a new API
client, not surgery.

## Components

```
┌─────────────────────────────────────────────────┐
│ desktop/            Electron wrapper            │
│  - frameless window (hiddenInset on macOS)      │
│  - macOS tray icon (run status)                 │
│  - loads http://127.0.0.1:19899/v2              │
└──────────────────────┬──────────────────────────┘
                       │ plain HTTP, same as a browser
┌──────────────────────▼──────────────────────────┐
│ veld-daemon         127.0.0.1:19899             │
│  GET  /v2                → embedded UI bundle   │
│  GET  /api/environments  → projects/runs/URLs   │
│  GET  /api/repos         → repos + worktrees    │
│  POST /api/repos/import  → register a git repo  │
│  POST /api/worktrees     → git worktree add     │
│  PATCH/DELETE /api/worktrees/{id}               │
│  POST /api/worktrees/{id}/start → `veld start`  │
│  POST /api/environments/{run}/stop|restart      │
└──────────────────────┬──────────────────────────┘
                       │ rusqlite (WAL)
┌──────────────────────▼──────────────────────────┐
│ veld.db   repos · worktrees · projects · runs…  │
└─────────────────────────────────────────────────┘
```

### `crates/veld-daemon/ui/` — the /v2 management UI

React + TypeScript + Vite. Built as a **single self-contained HTML file**
(`vite-plugin-singlefile`): JS, CSS, and fonts (Inter + JetBrains Mono
variable woff2, base64) are inlined so the daemon can embed it with
`include_str!` exactly like the existing feedback-overlay assets. No external
requests at runtime — branding rule.

- Served at `GET /v2` (one route; the app is a SPA with client-side state, no
  router needed yet).
- Talks to the same-origin `/api/*`. All mutating calls send the
  `X-Veld-Request: 1` CSRF header the daemon requires.
- Polls `/api/environments` + `/api/repos` (5s) — same model as the v1
  dashboard. Push/SSE is a later increment.
- Detects the Electron shell via a `?shell=electron` query param to render the
  native-title-bar layout (drag region, traffic-light inset padding) instead of
  the browser-build header row.
- The v1 dashboard at `/` is untouched; `/v2` replaces it only when it reaches
  parity.

Why not join `crates/veld-daemon/frontend/`? That package builds IIFE snippets
(feedback overlay, client-log) with esbuild and no framework; the management UI
is an application with a different toolchain (Vite, React, HMR). Two small
npm projects beat one franken-config.

### `desktop/` — the Electron wrapper

Minimal by design. Main process only does:

1. Create a frameless `BrowserWindow` (`titleBarStyle: 'hiddenInset'`) and load
   `${VELD_DESKTOP_URL ?? http://127.0.0.1:19899}/v2?shell=electron`.
2. If the daemon isn't reachable, show a local retry page (embedded data URL —
   install/start instructions) and poll until it appears.
3. macOS tray (template icon): shows running-run count, per-run stop/restart
   later; click focuses the window.
4. `contextIsolation: true`, `nodeIntegration: false`, tiny preload exposing
   `veldDesktop.shell` metadata only. No IPC surface beyond that yet — the
   webview/session APIs from the handoff arrive with the embedded-browser
   increment.

No packaging/signing in this increment — `npm start` (dev run) only.

## Data model

Desktop **repo** ≠ veld **project**. Veld keys its `projects` table by "any
directory containing veld.json" — so *every worktree with a veld.json is its
own veld project*. The desktop model sits one level above:

- `repos` — a git repository the user imported (its main checkout root).
- `worktrees` — checkouts of that repo (`git worktree`), each with a
  user-editable `alias`. The main checkout itself appears as a worktree row so
  the rail has one list.

Migration v5 (`crates/veld-core/src/db/mod.rs`, `user_version` 4 → 5):

```sql
CREATE TABLE repos (
  root       TEXT PRIMARY KEY,          -- absolute path, main checkout
  name       TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE worktrees (
  id         INTEGER PRIMARY KEY,
  repo_root  TEXT NOT NULL REFERENCES repos(root) ON DELETE CASCADE,
  path       TEXT NOT NULL UNIQUE,      -- absolute checkout path
  branch     TEXT NOT NULL,
  alias      TEXT NOT NULL,
  is_main    INTEGER NOT NULL DEFAULT 0,-- 1 = the repo's main checkout
  created_at TEXT NOT NULL
);
```

Run/health/URL state is **not** duplicated: the UI joins a worktree to veld
state by path (`worktrees.path` = veld `projects.root`, string equality) via
`/api/environments`. Both sides are physical (symlink-resolved) paths — git
porcelain emits them and `veld start` derives roots from `getcwd`; the daemon
additionally canonicalizes discovered paths at sync time to keep the key
stable. A worktree without a veld.json simply has no run controls.

Known limitation: a veld.json living in a *subdirectory* of the worktree is
not detected (`has_veld_config` checks the checkout root only), matching how
the desktop keys projects; such setups get no run controls.

UI selection state (project, worktree) lives in the URL (`?repo=…&wt=…`) with
localStorage as fallback — every view is addressable, which is the foundation
for later multi-window / split layouts (one URL per window).

## Daemon API additions

All under the existing management router (`crates/veld-daemon/src/management.rs`
delegating to a new `desktop` module), same conventions as today: CSRF header
on mutations, JSON errors, `202 Accepted` for fire-and-forget CLI spawns.

| Endpoint | Behavior |
|---|---|
| `GET /api/repos` | Pure DB read: repos with their worktrees, each worktree annotated with `has_veld_config` + `presets` (run state is NOT joined here — the UI joins `/api/environments` client-side by path). `available` is only the cheap directory-exists check; git reconciliation lives in `POST /api/repos/refresh` below. |
| `POST /api/repos/import` `{path}` | Accepts any directory inside the repo; resolves the main checkout via `git worktree list --porcelain`, derives the name, registers it, and syncs the worktree rows. Idempotent. |
| `DELETE /api/repos` `{root}` | Unregisters (never touches the filesystem). |
| `POST /api/worktrees` `{repo_root, branch, alias?, path?, create_branch?}` | `git worktree add`. Default path: `<repo_parent>/_worktrees/<alias>`. |
| `PATCH /api/worktrees/{id}` `{alias}` | Rename the alias (DB only). |
| `DELETE /api/worktrees/{id}?force=` | `git worktree remove` (`--force` discards a dirty tree); prunes git bookkeeping if the checkout was already removed by hand. Never deletes the main checkout. |
| `POST /api/worktrees/{id}/start` `{preset?, run_name?}` | Spawns `veld start --preset <p> --name <n>` with the worktree as cwd — the CLI resolves veld.json from cwd. Default run name: the alias. `202 Accepted`; progress observed via `/api/environments`. |
| `POST /api/repos/refresh` | The UI's poll target: reconciles every repo's worktree rows with `git worktree list`, then returns the same payload as `GET /api/repos`. POST (CSRF-gated) because it spawns git and writes; debounced daemon-side. The plain GET stays a pure read. |
| `POST /api/pick-directory` | Opens the native OS folder picker (the daemon runs in the user's GUI session — macOS `osascript`, Linux `zenity`/`kdialog`) and returns `{path}`; `204` on cancel, `409` while a picker is already open (single-flight), `408` after the 10-minute timeout, `500` on backend failure (no GUI session / permission denial), `501` without a picker backend. Works for the plain-browser build too — the web platform never exposes absolute paths. |

Git subprocesses follow the AGENTS.md daemon rule: resolved user login-shell
`PATH` via `veld_core::user_path::resolve_user_path()`.

Stop/restart reuse the existing `/api/environments/{run}` endpoints (runs are
keyed per project root, and each worktree is its own project root — no
collisions).

## Local dev setup

Prereqs: Rust stable, Node 22+, a working `veld` install (`veld doctor`).

```sh
# 0. once: npm deps for ui/ and desktop/
just setup-ui

# 1. dev daemon — a full parallel instance alongside the installed one:
#    own DB (.veld-dev/veld.db), own port (19898), https://veld-dev.localhost
just dev-daemon

# 2. UI with HMR — vite dev server on :5199, proxies /api → the dev daemon
just dev-ui

# 3. Electron shell pointed at the dev server
just dev-desktop
```

The dev-instance isolation (see CONTRIBUTING.md → Local development) is what
makes this safe: this branch adds a schema migration, and a schema-ahead
binary migrates whatever database it opens — on the real `veld.db` that would
lock out every released binary until `veld update`. The dev daemon runs on
its own database copy-free; to rehearse the migration against real data, use
`just dev-db-from-real` first. Runs started with `just dev` land in the same
dev instance, so the worktree rail picks them up.

> **Ran this branch before it was rebased onto the environments×runs split?**
> Your dev `veld.db` then has `user_version 3` holding the desktop tables —
> a numbering this branch now assigns to main's environments migration. No
> migration path can recover that database; wipe it once
> (`just dev-db-reset`) and re-import.

Without step 2/3: the dev daemon's embedded UI is at
`http://127.0.0.1:19898/v2` (or `https://veld-dev.localhost/v2`); once a
release ships these endpoints, the installed daemon serves the same at
`https://veld.localhost/v2`. Without step 3: everything works browser-only;
Electron adds the native shell (`just dev-desktop-embedded` points it at the
dev daemon without vite).

`just` recipes: `build-ui`, `test-ui`, `lint-ui`, `dev-desktop`,
`dev-desktop-embedded`, `desktop` mirror the existing frontend recipes. CI
runs typecheck + vitest + build for `ui/` and a syntax check for `desktop/`
(see `.github/workflows/ci.yml`); the Rust build jobs install `ui/` npm deps
because `veld-daemon`'s build.rs now builds both frontend packages.

## Later increments (explicitly out of scope here)

1. Embedded webviews + isolated sessions (Electron `WebContentsView`,
   `session.fromPartition`).
2. Terminal panes (PTY over WebSocket from the daemon, or node-pty in the
   Electron main process — decision deferred).
3. Start-run UX beyond preset picking; `veld share` from the UI.
4. Command palette / fuzzy search beyond the overlay shell.
5. Extension system (`veld-ui.json` badges), PR/CI badges, overview board.
6. Packaging, auto-update, CLI installation from the app.
7. `/v2` → `/` promotion once at parity with the v1 dashboard.
