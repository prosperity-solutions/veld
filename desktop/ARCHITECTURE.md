# Veld Desktop — Architecture

Veld Desktop is a desktop shell around veld's management UI. It lets a developer
import git repositories ("repos"), manage git worktrees per repo, and drive veld
runs per worktree — with the terminal and embedded-browser panes arriving in
later increments.

This document covers the foundation increment: what exists, why it's shaped
this way, and how to run it locally. The visual design source of truth is the
Claude Design handoff (kept outside the repo under `tmp/`, gitignored); of the
stripped add-ons listed there, PR badges, the extension system, isolated
browser sessions, the pinned agent session and the overview board are
deliberately **not** part of this foundation. (The command palette was also
stripped from the foundation, but has since shipped — see below.)

## Decision log

| Decision | Choice | Why |
|---|---|---|
| Repo placement | Veld monorepo | Every feature crosses the daemon API boundary; separate repo means version skew and dual PRs. Release/CI/review machinery already exists here. |
| Name | **Veld Desktop** | The value prop *is* the veld integration (runs, URLs, share, SQLite state). A generic "agentic worktree manager" name promises veld-independence we chose not to build. Extraction later is cheap (see below). |
| UI delivery | Served by `veld-daemon` at `/ide` | The daemon already owns the management HTTP server (`127.0.0.1:19899`) and the SQLite state. The desktop app is a thin wrapper, and the same UI works in a plain browser. |
| Electron's role | Supplementary shell | Frameless window, tray icon, later: embedded webviews with isolated sessions, CLI install. The web UI must stay fully usable without it. |
| Run orchestration | Daemon shells out to the `veld` CLI | The daemon never runs the orchestrator in-process — stop/restart already work by spawning `cd <root> && veld …` in a login shell. Start follows the same pattern. |
| Theme | Handoff palette (Inter + JetBrains Mono, oklch greens) | Deviates from the classic product tokens in `docs/branding.md`; sanctioned there as the **desktop theme**. Structural branding rules (wordmark, self-contained assets, noindex) still apply. |
| Terminal transport | **PTY spawned daemon-side (`portable-pty`) over a WebSocket** | Not `node-pty` in the Electron main process: the browser build needs a daemon route regardless, so Electron would mean two implementations of one feature and would break the "usable without Electron" invariant above. Also avoids node-pty's native-rebuild churn across Electron versions. |
| Terminal auth | Single-use ticket from a CSRF-gated `POST`, plus an `Origin` allowlist | The daemon's CSRF gate is custom-header-only, and a WebSocket handshake can carry neither a custom header nor a CORS preflight — so the usual gate is structurally unreachable on an endpoint that hands out a shell. Loopback is not the mitigation either: the helper publishes the daemon at `veld.localhost`. Both gates are kept deliberately; see the module docs in `crates/veld-daemon/src/pty.rs`. |
| Terminal session lifetime | Sessions outlive their socket; explicit `DELETE` ends one | A terminal is not re-creatable state — dropping it kills the shell and everything in it — and a reload drops the socket. So a socket closing means "come back later" (scrollback is buffered and replayed on reattach) while closing a *pane* means "done". Two consequences are accepted rather than solved: selecting a worktree opens a terminal in its default layout and that shell lives as long as the page, so browsing the rail spends the session budget (hence a cap of 48 and a readable error on hitting it); and closing the browser window discards the `sessionStorage` holding the session ids, leaving shells that can never be reattached — the detach grace is what collects those. Disconnecting a hidden pane and reattaching on return would fix both, and is cheap *because* reattach is lossless, but it is a behaviour change worth its own increment. |
| Terminal renderer | **xterm.js**, with `ghostty-web` a fast-follow candidate | `ghostty-web` is the better emulator but pre-1.0, with an unverified addon story (fit-addon is required for resize) and a ~400 KB WASM blob that `vite-plugin-singlefile` would base64-inline into the daemon binary. It is API-compatible with xterm.js, so swapping stays cheap and this does not gate the terminal work. |
| Pane layout | Two docks of tabs with a draggable split | The terminal, the embedded browser and run diagnostics all want the same region; each one added as another fixed column makes the window unusable. A tab kind per content type means the next increment is a renderer, not a layout rewrite. Layouts persist per worktree in `sessionStorage` — per browser tab, since a layout names live shells and two tabs must not claim the same one. |
| UI library | **Mantine** (v9), theme mapped to the handoff tokens | Maintainer call, reversing an earlier hand-roll decision: a desktop-scale app accumulates overlay/chrome density (menus, dialogs, palette, notifications, settings) where hand-rolling re-derives focus traps, aria, and keyboard nav forever. Mantine v7+ is CSS-variable-themable, so the handoff palette maps onto it (`src/theme.ts`); custom layout surfaces (rail, panes, top bar) stay hand-built on the token CSS. Specialized libs still win for their niches (xterm.js, resizable panes). |

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
│  - loads http://127.0.0.1:19899/ide              │
└──────────────────────┬──────────────────────────┘
                       │ plain HTTP, same as a browser
┌──────────────────────▼──────────────────────────┐
│ veld-daemon         127.0.0.1:19899             │
│  GET  /ide                → embedded UI bundle   │
│  GET  /api/environments  → projects/runs/URLs   │
│  GET  /api/repos         → repos + worktrees    │
│  POST /api/repos/import  → register a git repo  │
│  POST /api/worktrees     → git worktree add     │
│  PATCH/DELETE /api/worktrees/{id}               │
│  POST /api/worktrees/{id}/start → `veld start`  │
│  POST /api/environments/{run}/stop|restart      │
│       ?project_root=… (run names repeat!)       │
│  POST /api/pty/tickets   → attach ticket        │
│  GET  /api/pty/attach    → terminal WebSocket   │
│  DEL  /api/pty/sessions/{id} → end a shell      │
└──────────────────────┬──────────────────────────┘
                       │ rusqlite (WAL)
┌──────────────────────▼──────────────────────────┐
│ veld.db   repos · worktrees · projects · runs…  │
└─────────────────────────────────────────────────┘
```

### `crates/veld-daemon/ui/` — the /ide management UI

React + TypeScript + Vite. Built as a **single self-contained HTML file**
(`vite-plugin-singlefile`): JS, CSS, and fonts (Inter + JetBrains Mono
variable woff2, base64) are inlined so the daemon can embed it with
`include_str!` exactly like the existing feedback-overlay assets. No external
requests at runtime — branding rule.

- Served at `GET /ide` (one route; the app is a SPA with client-side state, no
  router needed yet).
- Talks to the same-origin `/api/*`. All mutating calls send the
  `X-Veld-Request: 1` CSRF header the daemon requires.
- Polls `/api/environments` + `/api/repos` (5s) — same model as the v1
  dashboard. Push/SSE is a later increment.
- Detects the Electron shell via a `?shell=electron` query param to render the
  native-title-bar layout (drag region, traffic-light inset padding) instead of
  the browser-build header row.
- The v1 dashboard at `/` is untouched until the runs-mode rebuild reaches
  parity and takes it over.

#### Worktree rail and command palette

- **Rail rows** carry inline start/stop controls, but only while the rail is
  expanded — a 64px collapsed row has no space, so right-click is its
  affordance in both states. Rows are `div[role=button]`, not `<button>`,
  because they contain real nested buttons and a button inside a button is
  invalid HTML that browsers resolve by dropping the inner one. The row's
  `onKeyDown` must therefore ignore events bubbling from those children, or it
  cancels their activation. Known cost: `role="button"` takes presentational
  children, so the nested controls aren't exposed to assistive tech — the
  honest shape is `role="listbox"` on the rail with `role="option"` rows,
  deferred to a later increment.
- **One start predicate.** `canStartWorktree` gates all four surfaces that can
  fire a run action — top bar, rail row, context menu and palette. They
  disagreed before: some checked "is anything already in flight", others "is
  there anything to start", so one surface offered an enabled control whose
  click was a silent no-op while another allowed a double-spawned
  `veld start`.
- **Pending markers** (`prunePending`, `crates/veld-daemon/ui/src/model.ts`)
  are optimistic per-worktree flags cleared when the worktree's *run signature*
  (`status:run_id`) moves — status alone is not enough, because `veld restart`
  returns to `running` and would never register. A 60s TTL bounds an action
  that 202s and then never lands.
- **⌘K** fuzzy-searches worktrees *and* commands. With no query the items are
  grouped in `PALETTE_GROUPS` order; once the user types, grouping gives way to
  a single score-ordered list. The matcher runs two scans — plain leftmost and
  one anchoring the query's first character to a word start — and keeps the
  better: plain greedy alone ranks "Switch to Runs" above "New worktree…" for
  `wt`, while anchoring alone can strand the rest of the query and drop the
  item from the list entirely.

#### Panes and terminals (`ui/src/panes/`)

- **Layout is a pure module** (`panes/model.ts`): two docks, each a tab list
  with an active tab, plus a split ratio. Every mutation is a function on that
  value, so the layout rules are unit-testable without a DOM and the React side
  holds only the state cell.
- **Terminals live outside React** (`panes/terminalHost.ts`). Unmounting a
  terminal would close its socket and kill the shell, and React unmounts freely
  — on a tab switch, and on every worktree switch, since each worktree has its
  own layout. So the xterm instance and its container element sit in a
  module-level registry keyed by tab id, and the component only *reparents* that
  element into itself. This is also why a tab id must be unique for longer than
  the page: it doubles as the daemon session id.
- **Never write into a live shell.** A `[veld] …` notice injected into a running
  terminal lands in the middle of whatever full-screen program is redrawing
  (Claude Code, vim, top) and corrupts it. Notices are for shells that have
  *ended*; anything about a live one goes on the pane's status chip.
- **Replayed scrollback is bracketed, and input is gated inside the bracket.**
  Recorded output can contain queries the shell once made (device attributes,
  cursor position, colour). Parsing them again makes the emulator answer them
  again, and that answer reaches a shell that asked nothing — arriving as
  keystrokes. The symptom was a `1;2c` fragment at the prompt after every
  reload. The gate lifts on xterm's `write` completion callback rather than on
  `replay_end`, because the answers are emitted while parsing, not on receipt.
- **`fit-addon` sizes the grid from the computed height of the terminal's
  parent element**, which with border-box sizing includes that parent's own
  padding. Padding there therefore buys a row that doesn't fit and the bottom
  line renders off-pane; the padding belongs on the wrapper *outside* the
  measured element (`.term-slot`, not `.term-host`).
- **Focus** is shown with `:focus-within` on the dock rather than the layout's
  `focused` field — that field only says where a new tab would open, while the
  CSS tracks real DOM focus and picks up xterm's hidden textarea for free. Only
  the focused dock's terminal claims the keyboard on mount; both docks mount on
  load, so focusing unconditionally handed it to whichever mounted last.

Why not join `crates/veld-daemon/frontend/`? That package builds IIFE snippets
(feedback overlay, client-log) with esbuild and no framework; the management UI
is an application with a different toolchain (Vite, React, HMR). Two small
npm projects beat one franken-config.

### `desktop/` — the Electron wrapper

Minimal by design. Main process only does:

1. Create a frameless `BrowserWindow` (`titleBarStyle: 'hiddenInset'`) and load
   `${VELD_DESKTOP_URL ?? http://127.0.0.1:19899}/ide?shell=electron`.
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

Migrations v5 + v6 (`crates/veld-core/src/db/mod.rs`, `user_version` 4 → 6):

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
  emoji      TEXT NOT NULL DEFAULT '',  -- v6: stable visual identifier
  is_main    INTEGER NOT NULL DEFAULT 0,-- 1 = the repo's main checkout
  created_at TEXT NOT NULL
);
```

The `emoji` (v6) is one glyph from a curated animal set — hash-seeded by
alias, probed to stay unique across ALL repos' worktrees, assigned at
insert, backfilled for pre-v6 rows on sync, preserved across renames. It is
the collapsed rail's identifier.

Uniqueness is a property of *assignment*, not an invariant: "Change emoji…"
lets the user pick a glyph another worktree already holds (the picker marks
which, so the ambiguity is visible before it's created) — an explicit choice
outranks the heuristic. Sync only backfills an *empty* emoji, so a chosen one
survives every later reconciliation **of an unmoved worktree** — `git worktree
move` changes the path, which is the sync key, so the row is pruned and
re-inserted with a fresh id, a re-assigned emoji and a default alias (the
path-keyed `veld.start.<path>` entry is orphaned by the same move).

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
| `GET /api/repos` | Pure DB read: repos with their worktrees, each worktree annotated with `has_veld_config`, `presets`, and `nodes` (startable nodes with variants + default variant — the custom-selection source). `presets` are full records (`name`, `key`, `pinned`, `label`, `when_to_use`, `group`, `selections`, `is_default`) in display order, produced by `veld_core::presets::resolve` — the same resolver the CLI picker uses, so a preset's key means the same thing in both surfaces. Ordering is the resolver's, never a sort here. (run state is NOT joined here — the UI joins `/api/environments` client-side by path). `available` is only the cheap directory-exists check; git reconciliation lives in `POST /api/repos/refresh` below. |
| `POST /api/repos/import` `{path}` | Accepts any directory inside the repo; resolves the main checkout via `git worktree list --porcelain`, derives the name, registers it, and syncs the worktree rows. Idempotent. |
| `DELETE /api/repos` `{root}` | Unregisters (never touches the filesystem). |
| `POST /api/worktrees` `{repo_root, branch, alias?, path?, create_branch?}` | `git worktree add`. Default path: `<repo_parent>/_worktrees/<alias>`. An explicit `alias` a sibling already holds is a `409`. The check runs *before* `git worktree add`, so the common case creates nothing; it is a plain read though, so a create that races another one (or a sibling on disk not yet synced) still 409s on the authoritative check after the checkout exists — what survives then is a registered worktree under its branch-derived alias, not an orphan. |
| `PATCH /api/worktrees/{id}` `{alias?, emoji?}` | Partial update, DB only. Both fields optional (alias-only callers stay wire-compatible); an empty patch is a `400` and an unknown field a `422` (`deny_unknown_fields` — with everything optional, a client typo would otherwise be a silent `200`). Both columns are written in one `UPDATE … COALESCE`, so the pair can't half-apply. An alias a sibling checkout of the same repo already holds is a `409`: `unique_alias` establishes that invariant at insert and the rename path must not be a hole in it, since the alias becomes the default run name. The check and the write share one transaction, so two concurrent renames can't both win. Cross-repo duplicate aliases stay legal — forbidding them would break importing two repos that are both on `main`. `emoji` is checked against the curated set — an allowlist rather than a "one grapheme?" test, which keeps the rail uniform and leaves no room for a multi-codepoint or zero-width payload; the rule lives in `veld_core::db::is_worktree_emoji`, beside the constant, so no caller can bypass it. |
| `GET /api/worktree-emoji` | The curated glyph list, for the picker. Served rather than duplicated in TypeScript, because the same constant is the server-side allowlist; the picker fetches it once on open instead of riding the 5s poll. |
| `DELETE /api/worktrees/{id}?force=` | `git worktree remove` (`--force` discards a dirty tree); prunes git bookkeeping if the checkout was already removed by hand. Never deletes the main checkout. |
| `POST /api/worktrees/{id}/start` `{preset?, selections?, run_name?}` | Spawns `veld start` with the worktree as cwd (the CLI resolves veld.json from there). Two mutually-exclusive start modes: `preset` (`--preset <p>`) or explicit `selections` (`node:variant` positionals, validated per half) — a non-TTY bare start fails "No selections provided", so the UI always sends one. Default run name: the alias. `202 Accepted`; progress observed via `/api/environments`. |
| `POST /api/repos/refresh` | The UI's poll target: reconciles every repo's worktree rows with `git worktree list`, then returns the same payload as `GET /api/repos`. POST (CSRF-gated) because it spawns git and writes; debounced daemon-side. The plain GET stays a pure read. |
| `POST /api/pick-directory` | Opens the native OS folder picker (the daemon runs in the user's GUI session — macOS `osascript`, Linux `zenity`/`kdialog`) and returns `{path}`; `204` on cancel, `409` while a picker is already open (single-flight), `408` after the 10-minute timeout, `500` on backend failure (no GUI session / permission denial), `501` without a picker backend. Works for the plain-browser build too — the web platform never exposes absolute paths. |

Terminals live in their own module (`crates/veld-daemon/src/pty.rs`) rather than
the `desktop` one, because they cannot use that router's CSRF layer — a
WebSocket upgrade is a GET, which a method-keyed layer waves through, and a
handshake can carry neither a custom header nor a CORS preflight.

| Endpoint | Behavior |
|---|---|
| `POST /api/pty/tickets` `{worktree_id, session_id}` | CSRF-gated, and the only place a worktree id is resolved to a directory — so the socket below never accepts a path from the client. Returns a single-use ticket (122 bits from the OS CSPRNG, 30s TTL) plus `resumed`, true when a live session already answers to `session_id`. The client names the session (`crypto.randomUUID()`), which is what lets a reload ask for the same shell; the name is an identifier, not a credential. `409` if that session belongs to a different worktree. |
| `GET /api/pty/attach?ticket=&cols=&rows=` | The WebSocket. Requires an allowlisted `Origin`, failing closed when absent, **and** an unredeemed ticket; a rejected origin does not burn the ticket. Binary frames are terminal bytes in both directions, text frames are JSON control (`resize` up; `replay_begin`/`replay_end`/`ready`/`exit`/`taken_over`/`lagged` down). A second attach takes the session over and tells the displaced socket why. Note that a browser can read neither the status nor the body of a *failed* handshake, so anything a legitimate client can trip over (capacity, a missing directory) is pre-checked at ticket time instead; the client distinguishes a refused upgrade from a dropped connection by whether `ready` ever arrived. |
| `DELETE /api/pty/sessions/{id}` | CSRF-gated. Ends the shell now — the distinction the detach model rests on, since a socket closing means "come back later". `204` even when the session is already gone. |

The shell is the user's `$SHELL -l`, which is this module's stated exception to
the AGENTS.md `resolve_user_path()` rule: that helper *is* a login shell
spawned to scrape `PATH`, so calling it here would add a second shell's startup
cost to every terminal to compute what this one computes anyway.

Git subprocesses follow the AGENTS.md daemon rule: resolved user login-shell
`PATH` via `veld_core::user_path::resolve_user_path()`.

Stop/restart reuse the existing `/api/environments/{run}` endpoints, which
take the target project as a **required** `?project_root=` query parameter.

Runs are keyed per project root in the database (`UNIQUE(project_root, name)`),
but the *name* is not globally unique, and the endpoints originally took the
name alone: the daemon resolved it by scanning the registry and taking the
first project that held it. Two repos both checked out on `main` each default
to an environment called `main` (the start endpoint derives the run name from
the alias, and `unique_alias` de-duplicates only within one repo), so a stop on
one could tear down the other — see issue #168. The UI sends the worktree path,
which is already its join key into `/api/environments`; a project that does not
run the named environment is a `404`. `/api/logs/{run}` and
`/api/environments/{run}/action` take the same parameter.

`POST /api/shares` takes `project_root` too, but **optional** rather than
required, because its callers are separately-invoked binaries (the CLI) and a
browser overlay rather than JS compiled into this daemon: the CLI sends its own
project root, the overlay rides Caddy's `X-Veld-Project` header, and a request
with neither still resolves by name — rejecting an ambiguous one with a `409`
instead of publishing an unrelated project's URLs. When a project *is* named it
is authoritative: a run that project doesn't hold is a `404` naming where it does
run, never a silent fallback to another project (that fallback was tried and
reverted — with `--web` it published a project the caller never named). Sharing
additionally requires a live run, since `Db::registry()` carries each
environment's latest run whatever its status.

The corollary for anything else that needs global identity: use `worktrees.id`,
the checkout path, or a `run_id` — never an alias or a run name.

The name-addressed endpoints above are the deliberate exception, not a
counter-example: `veld stop --name` / `veld restart --name` is the CLI's own
contract, so the daemon spawns the CLI *in the project directory* and lets the
cwd disambiguate rather than inventing a second addressing scheme. That also
makes the address instance-agnostic — a stop re-resolves the name inside the
project, so it acts on whatever run is current, not necessarily the instance the
user was looking at. Harmless while an environment has at most one live run
(`idx_runs_one_live` enforces that), but it is the reason, and it is why the
share *responses* report each share's `run_id`, so a UI attaches a share to the
exact instance it was minted from rather than to whatever run currently answers
to the name. (The share *request* is name+project addressed like the others —
`POST /api/shares` resolves the project's latest run.)

The layer below — the proxy store — is keyed by **hostname**, not by run name:
`veld_core::url::run_route_id(hostname)` is the one place the id is built, so a
route id collides exactly when the URL does, and the helper re-keys any route a
pre-#170 build persisted when it starts. Two checkouts that share a project name
*and* a run name do still mint one hostname; `veld start` now refuses that up
front rather than overwriting the other project's route. Shares are released when
a live run is replaced, and both dashboards list a hosted share whose run they no
longer know about, so it stays stoppable. The tray marks a run with its worktree
emoji + alias (falling back to the checkout path) only when another shown run
carries the same project name — which is exactly what two clones of one repo
produce, the name coming from `veld.json`; unambiguous rows keep the plain label
and cost no extra request. The emoji picker
compares worktree ids for the same reason, and `/api/shares` reports each share's
`run_id` so the dashboards attach a share to its own run card instead of to a
same-named run in another repo.

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
`http://127.0.0.1:19898/ide` (or `https://veld-dev.localhost/ide`); once a
release ships these endpoints, the installed daemon serves the same at
`https://veld.localhost/ide`. Without step 3: everything works browser-only;
Electron adds the native shell (`just dev-desktop-embedded` points it at the
dev daemon without vite).

`just` recipes: `build-ui`, `test-ui`, `lint-ui`, `dev-desktop`,
`dev-desktop-embedded`, `desktop` mirror the existing frontend recipes. CI
runs typecheck + vitest + build for `ui/` and a syntax check for `desktop/`
(see `.github/workflows/ci.yml`); the Rust build jobs install `ui/` npm deps
because `veld-daemon`'s build.rs now builds both frontend packages.

## Target shape: one app, two modes

The end state is a single React/Mantine app replacing the hand-written v1
dashboard, with two modes and a view switcher in the top bar:

- **Runs mode** (served at `/`) — what the v1 management dashboard does
  today: environments/runs across all projects, health, logs, stats,
  sharing, feedback.
- **Worktree mode** (served at `/ide`) — this increment's cockpit: rail,
  run controls, terminals, previews, scoped to one worktree.

Worktree mode already lives at its final route (`/ide`); once runs mode
reaches v1 parity the app also takes over `/` and `assets/management-ui.html`
is retired. Selection
already lives in the URL, so modes are just routes.

## Later increments (explicitly out of scope here)

1. **Runs mode** (v1-dashboard parity in React/Mantine) + view switcher +
   `/` / `/ide` routing as above.
2. Embedded webviews + isolated sessions (Electron `WebContentsView`,
   `session.fromPartition`). These are meant to arrive as a new `PaneKind` in
   the dock (`crates/veld-daemon/ui/src/panes/`), not as another column.
3. ~~Terminal panes~~ — shipped; see the decision log above.
4. Start-run UX beyond preset picking; `veld share` from the UI.
5. Extension system (`veld-ui.json` badges), PR/CI badges, overview board.
6. Packaging, auto-update, CLI installation from the app.

The sequencing and the transport/renderer decisions for these live in
[issue #167](https://github.com/prosperity-solutions/veld/issues/167).
