# Veld Desktop — Architecture

Veld Desktop is a desktop shell around veld's management UI. It lets a developer
import git repositories ("repos"), manage git worktrees per repo, and drive veld
runs per worktree, with terminal, embedded-browser and run-diagnostics panes in a
dock, and sharing from the top bar.

This document covers the foundation increment plus the increments that have
landed on top of it: what exists, why it's shaped this way, and how to run it
locally. The visual design source of truth is the Claude Design handoff (kept
outside the repo under `tmp/`, gitignored); of the stripped add-ons listed there,
PR badges, the extension system, the pinned agent session and the overview board
are deliberately **not** part of this foundation. (The command palette, terminal
panes and isolated browser sessions were also stripped from the foundation, and
have since shipped — see below.)

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
| Embedded browser | **Electron `WebContentsView`**, with an `<iframe>` fallback in a plain browser | The pane has to render the user's own dev server, which frequently sends `X-Frame-Options`/`frame-ancestors` — an iframe shows those as a blank rectangle with nothing observable to report. A native view also has real back/forward, a readable URL and title, and per-pane cookie jars. The iframe stays because "usable without Electron" is the invariant above; it is honest about what it cannot do (no history, no separate sessions, no way to detect a refused frame) rather than pretending. |
| Browser sessions | Global, colour-coded, animal-named `persist:` partitions — the default plus up to 8 — with the set that *exists* tracked per worktree in `localStorage` | The point of the feature is being two logged-in users of your own app at once, which is a cookie jar per pane. The *allowed* slot names are a closed list because the name becomes an Electron partition — an identifier the main process has to validate anyway — but which of them **exist** is a set the user builds up, stored in `localStorage` and keyed by worktree. Deriving that set from which slots panes occupy was the first attempt and it inverted the feature: moving a pane onto a new session vacated its old slot, so adding one appeared to delete the previous. `localStorage` rather than the daemon because a session only means anything under Electron (the browser build's iframe backend has no cookie jars of its own), so there is no second client for the list to disagree with — this is one client's preference about its own capability, not the shared settings store batch 5 needs. Eight above the default is the colour ceiling: more dots stop being tellable apart, and the colour is what makes "which session is this pane?" answerable without opening a menu. The slots are **named** (otter, wombat, gecko…) rather than numbered, because a number implies a sequence: removing "Session 2" and being left with "Default, Session 3" reads as breakage, while a name has no successor to be missing. The name is also the partition, so the identifier says what it is. Partitions stay global, so two worktrees whose sets both hold the same slot share that jar. Stated rather than hand-waved: only the *run* differs between two worktrees' hostnames (`{service}.{run}.{project}.localhost`), so a project-scoped cookie is shared, and a third-party login domain is shared unconditionally. Keying the partition by worktree is the fix if it bites. Clearing data is addressed by partition, not by pane, so a slot no pane holds can still be emptied. |
| Native-view z-order | The renderer hides views while a DOM overlay is open | A `WebContentsView` is a native sibling of the page: it paints over every menu, dialog and dropdown regardless of z-index, and there is no CSS answer. `panes/overlayGuard.ts` suspends the views while one is open. It watches the subtree of Mantine's *shared* portal node — `Portal` reuses one container that is appended to `body` once and never removed, so watching `body`'s children sees a single mutation and then goes deaf — and it requires a match to be actually painted, because `Combobox` keeps its dropdown mounted at `display: none` and `mantine-Modal-root` stays in the DOM when the modal is closed. Both of those shipped as bugs first: the deaf observer, then a permanent false positive that hid every pane. Hidden is not blank, though: each visible view is **captured first, decoded, and painted onto the container before the view goes**, so a pane freezes rather than disappearing every time a menu opens — hiding first and painting when the capture landed was itself a visible flicker — and so was routing the still through React state, which costs a render plus an async image decode. Bounded by a timeout, so a slow capture cannot leave the overlay stuck behind a view. App-owned surfaces that aren't portalled call `pushBrowserSuspend` from their own state. It is a heuristic, and deliberately the kind that fails *visibly* (a dropdown behind a pane) rather than the kind that blanks a pane at random. |
| Run diagnostics | Two pane kinds (`logs`, `nodes`) rendering **the same views runs mode renders** (`ui/src/shared/RunViews.tsx`) | Every endpoint already existed (`/api/logs/{run}`, `/api/stats`, per-node health on `/api/environments`), so this is a UI question only, and the UI question is where they live. Panes rather than a third fixed column, for the reason the dock exists at all. Extraction rather than a second implementation, because the pair would have drifted on the first change to what a node row says — the health sub-line (failures / recoveries / last liveness error) is exactly what a fork forgets. The panes hold **no run identity**: they read whichever run the selected worktree has, so a worktree switch re-points every open one, and a pane can never show a run whose worktree is off screen. They also read a *wider* run than the top bar's controls do (`diagnosticsRun` vs `activeRun`): after a crash there is nothing to stop or restart, but the logs and the last node states are the whole reason you opened the pane. |
| Sharing in IDE mode | One top-level surface in the top bar (a popover), with join requests as a **banner above the panes** | Follows #152's rule — a Sharing surface, not a relay-details dump — and a popover rather than a `Menu` because the content is interactive (the auto-accept checkbox and the copy buttons must not close it). Sharing is *refused* far more often than it fails — a service has to opt in (`share.expose`) and a relay has to be configured (`sharing.relays`) — so the daemon's refusal text is the feature, and it reaches the user as a toast. The join requests are deliberately *not* in it: someone is sitting on the other end waiting for an answer, so the prompt has to be visible without opening anything, and it lists **every** share's requests rather than the selected worktree's — a request against a worktree you are not looking at must not be invisible. It names the run each one is for, since a request carries only its `share_id`. |
| The run's URLs are not a pane kind | A launcher component (`panes/VeldLinks.tsx`) shown inside whatever pane is about to need it | They are how you *get* a page, not a peer of a terminal and a page. A kind for them meant a singleton tab id, a "does one already exist" check at every call site that could open one, and a second implementation of the same rows. Now a `new` pane and a browser pane **with no URL** both show them — the second being the useful one, since the list sits in the thing that is one click from becoming the page. The top bar's globe just opens an empty browser pane, and a worktree's default layout is a terminal beside one. Links that are *not* veld's belong in the project's config, not hardcoded: `ui.quicklinks`, the *Per-project quicklinks* item in issue #167 (referenced by name, not by its number, which moves as the roadmap is reordered). |
| Pane creation | `+` opens an undecided `new` pane; the choice happens inside it | A menu off a `+` button is the size of a cursor and vanishes when you look away, while the thing being chosen is a whole pane. So `+` opens the pane and the pane asks what it should be, at content size — and picking a kind *replaces* the tab (`replaceTab`) rather than adding one, so the flow costs a single tab. The same screen serves an empty dock and a closed-everything region, which is why they now look identical. Hovering `+` still offers the one-click shortcuts for people who know what they want. |
| Pane screens | Loading, error and chooser are DOM screens *and* they hide the view | A native view paints over DOM, so "the pane has something to say" and "the view is off screen" are one decision, not two — `covered()` in `browserHost.ts` owns it and the pane's render mirrors it. Error copy is keyed off Chromium's net error, not its message: "nothing is listening" (start the run) and "that hostname doesn't resolve" (`veld doctor`) are different problems, and the codes are stable where the prose is not. A *re*-load keeps the old page up rather than covering it with a spinner, which is what `loaded` is for. |
| Orphan views are dropped by the *page*, not by a navigation event | `reset()` at module load, before any `create` | A reload replaces the page's registry of views, so the old ones are orphans painting over the new document. Disposing them from the shell's own `did-navigate` is a race against the renderer's first `create` — and losing it destroyed the view the new page had just asked for, which is why the first browser pane after a hard reload came up blank with reload as the only escape. Driving it from the renderer makes the ordering a queue. |
| Views start visible | `create` no longer hides the view and then shows it | Chromium background-throttles a hidden `WebContents`, and a view created hidden and loaded in the same tick sometimes never rendered its first page — blank until you pressed Reload. The renderer sends its own visibility immediately, so starting visible costs nothing and removes the race. The spinner's 8-second "taking a while" reload is the backstop, since a genuinely slow dev server must not be called an error. |
| Device emulation | **Native Electron `enableDeviceEmulation` for the metrics and the UA, CDP only for touch**, per pane, with detached DevTools beside it — presets as **size classes**, plus a draggable screen | The case that justifies doing this inside the dock rather than saying "use Chrome" is not the phone — it is the *desktop*: emulating a 1440-wide viewport **scaled down to fit a 600px pane** is what no real browser window can give you without a second monitor, and it is one API call. The metrics and `setUserAgent` are native, so the useful 90% costs no CDP plumbing. Touch is the exception and it is a deliberate one: `Emulation.setEmitTouchEventsForMouse` needs `webContents.debugger.attach()`, which Electron's docs say the built-in DevTools takes over — measured on Electron 43, it does not, and the two sessions coexist. Both outcomes are handled rather than either being trusted, and the pane reports what it *achieved* (`touchActive`, separate from the `touch` it asked for) instead of asserting a mode it may not have. Touch is worth the state machine because metrics alone test the layout and never the interaction: a swipe carousel, a `@media (hover: none)` rule and any library that branches on `ontouchstart` are all invisible to a narrow viewport that still receives mouse events. Emulation is **per pane**, not per window — a phone beside a desktop is the comparison people actually want — which follows from it being per-`WebContents` anyway. Page zoom rides along in the same control and the same layout field, because it has the same shape of problem (per-`WebContents`, lost when the view is recreated) and is useful well before any preset is: a 1440-wide layout is readable in a 600px pane at 60%. Zoom carries one honest caveat — Chromium's zoom policy is per *origin*, so two panes on one session and origin cannot hold different zooms across a navigation; the alternative is a partition per pane, which is a heavier feature than the problem. |
| Browser pane lifetime | Re-created on reload, unlike a terminal | A page is re-creatable state: the URL is persisted in the layout and re-navigated to, so a reload is allowed to drop the views and rebuild them. The page asks for that itself (`reset()`, see the row above) rather than the shell inferring it from a navigation event. A shell is the opposite — see the terminal row above. |
| Icons | One mark, two assets, drawn by a stdlib-only rasteriser (`desktop/scripts/make-icons.py`) | The app icon is the *favicon's* shape (rounded dark tile, white `V`, accent dot) because that is already what veld shows in a browser tab, and the menu-bar icon is `logo.svg`'s mark — the same one the Hammerspoon widget uses, so the two menu-bar presences are one identity rather than two lookalikes. The tray asset is a macOS **template** image (`*Template.png`, black + alpha): the OS tints it per menu bar, which is the only way one file stays legible in light *and* dark mode. Shipping the coloured mark instead is a white glyph on a light menu bar — the bug the Hammerspoon widget has, since it sets its icon non-template. Cost: the accent dot is a shape there, not a colour; the app icon carries the colour. The generator draws the mark analytically — it is a polygon and a circle, since every segment of `logo.svg`'s V is a straight line — so there is nothing to install and the bytes are identical on every machine. Both tools tried first were wrong in the same direction: `qlmanage` (QuickLook) composites thumbnails on an **opaque white background** and pads below its minimum size, which shipped a menu-bar icon that was a white tile with a dark V (a template image is alpha, so an opaque render is a solid blob — and invisible as a bug in any light-background preview), and ImageMagick's SVG renderer is blobby at icon sizes *and* its resize dropped the alpha channel to grayscale. The app icon is inset in a transparent margin, because macOS draws its own shadow into one and a full-bleed tile reads as a bigger, blockier icon than everything beside it in the dock. |
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
│  POST /api/environments/{run}/action → node act │
│  GET  /api/logs/{run}    → run + node logs      │
│  GET  /api/stats         → per-node cpu/memory  │
│  GET  /api/shares        → shares/joins/pending │
│  POST /api/shares…       → share, mode, approve │
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
  dashboard — plus `/api/shares` and `/api/stats` on the same tick while IDE mode
  is on screen (runs mode does its own). Push/SSE is a later increment.
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
- **One visible dock is always the left one.** Every mutation ends in
  `normalizeDocks`, so a layout is never left-empty-and-right-full. With a single
  pane on screen "left" and "right" name nothing, and the distinction surfaced as
  a tab menu offering *Move to the left pane* with nothing to the left of
  anything. With one dock that action is a split, and it is labelled as one.
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

#### Run diagnostics and sharing (`ui/src/shared/`, `panes/RunPanes.tsx`)

- **`ui/src/shared/` is what both modes render.** `RunViews.tsx` is the unit of
  reuse — `NodesView` and `LogsView`, over `NodeList`/`nodeRows` and `LogsPanel` —
  with the sharing pieces (`ShareControls`, `PeerShareStrip`, `WebShareStrip`,
  `JoinRequestRow`, `RunSharePanel`) and the formatting helpers beside them. Runs
  mode is now *only* a head, run controls and a Nodes|Logs switcher over those
  views; IDE mode is the same two behind pane tabs. Anything host-specific is a
  prop: `fill` (own the parent's height vs. sit in a card), `visible` (a card keeps
  a hidden view mounted so its filters and scroll survive; a pane unmounts), and
  `selected` (whether the *host* owns the history choice — a card's head picker
  does, a pane has none, so the view grows its own). When the nodes view gets
  scrubbable resource timelines, it gets them once.
- **Errors are toasts, everywhere** (`shared/notify.ts`), which is why no shared
  component takes an error-reporting prop: there is one behaviour, not one per
  host. It replaced `window.alert` in runs mode (blocks the page, steals the
  keyboard) and a banner in IDE mode (reflows the panes on every failure). Each
  toast carries `data-veld-overlay`, because a native browser pane paints over DOM
  — an unmarked toast is an error nobody sees. The attribute goes on the
  notification, never on the always-mounted container.
- **The daemon omits empty arrays.** `veld_core::share::ShareInfo` marks
  `public_urls` and `connections` `skip_serializing_if = "Vec::is_empty"`, so a
  peer share with no joiners arrives with neither key while the TS type claims
  both — and `s.public_urls.length` is a TypeError that takes the view down.
  `normalizeShare` in `api.ts` fills them once, at the boundary; the declared type
  describes what consumers get, and the comment says why it is not what the wire
  carries.
- **Two error shapes on one API.** The management and desktop routers answer
  `{"error": …}`; the share router returns a bare `text/plain` body. The client
  read only the first, so sharing's refusals — the ones that tell you exactly what
  to add to `veld.json` — surfaced as `400 Bad Request`, which reads as a bug in
  Veld rather than as a config that has not opted in. `errorMessage` handles both.
- **The nodes view is a card per node, not a table.** The same view renders in a
  300px pane and in a 1080px card, and columns cannot survive that range: they
  either squeeze every cell to two characters or get dropped as the width shrinks,
  which loses a fact (the pid, the URL) exactly when someone needed it. Container
  queries doing the dropping was the first attempt and it is the wrong shape. A card
  has nothing to lose — the long values get their own line, and width only decides
  where things wrap. Ordered by how often each part is read: identity and state,
  then the URL with its actions, then what is wrong, then what you can do about it;
  resources sit on the opposite edge of the first line, because that is the column
  people scan *down*. With no header row to carry meaning, units travel with the
  values (`pid 21672`, an `aria-label` of "Memory 212 MB").
- **Opening a node's URL in a pane is a prop, not a capability check.** The card
  shows that button when the host passes `onOpenPane`, which only IDE mode does —
  runs mode has no panes, and a control saying "open here" with no *here* is worse
  than no control. It is deliberately not gated on Electron: a pane is a pane in
  the browser build too (an iframe there), and all three entry points — this
  button, the URL launcher and ⌘K — go through the same `addTabToFocused`, so none
  of them invents its own placement.
- **Tabs shrink before the strip scrolls.** Only the tabs are in the scroll box, so
  the `+` follows the last tab while there is room and pins to the end of the strip
  once there is not — one layout, not a second mode. Labels are clamped (a browser
  pane's label is the *page title*, which can be a sentence, and one wrapped to two
  lines deformed the whole strip), tabs shrink to a floor that still shows the kind
  glyph and the close button, and past that the strip scrolls. The active tab
  scrolls itself into view, because a tab can become active without being clicked —
  ⌘K, a drop, or closing its neighbour. There is no scrollbar (30px has no room for
  one), so each edge carries a **fade that appears only while something is past
  it** — otherwise a scrolled strip has a hidden state with nothing to announce it,
  and a permanent gradient would dim the first and last tab of a strip that fits,
  saying the opposite of what it means. The edges are measured (`ResizeObserver` for
  the tab count and the dock's width, the scroll event for the position) rather than
  inferred. The fade is three layers, because one colour gradient is nearly
  invisible in both themes for opposite reasons — in the dark theme an unselected
  tab *is* the strip's colour, in the light one the tones are close: the strip
  colour hides the tab's edge, a black wash dims what is under it (the text, which
  is what the eye reads as cut off), and a 1px line marks where the cut is. One trap when wrapping the tabs in a scroll box: `.pane-tabs` centres
  its items, so the scroller needs `align-self: stretch` or it stops filling the
  strip and every tab in it renders as a floating chip — which is exactly how it
  shipped for one round.
- **`LogsPanel` has two shapes, one implementation.** In a card it is a
  fixed-height area that stays mounted while its tab is hidden (filters and scroll
  survive); in a pane (`fill`) it is the whole dock body, with the toolbar fixed
  and the log area taking the rest. The pane variant is keyed by run instance, so
  a restart or a worktree switch does not carry another run's node filter in.
- **The panes poll through the app, not themselves.** `/api/shares` and
  `/api/stats` ride IDE mode's existing 5s tick, `allSettled` beside the two calls
  that decide the offline banner — a stats hiccup must keep the last values rather
  than blank the view, and runs mode already polls its own on its own cadence, so
  the extra two reads are skipped while it is the mode on screen.
- **A share action is not a `PendingAction`.** Those markers clear when the
  *run signature* moves, which a share never touches; one taken out for a share
  would sit spinning until its 60s TTL. The poll is what confirms a share, and a
  failure surfaces as a toast, like every other action's.

#### Browser panes (`ui/src/panes/browserHost.ts`, `BrowserPane.tsx`)

- **Two backends behind one registry**, chosen once at module load by whether
  `window.veldDesktop.browser` exists. Views live outside React for the same
  reason terminals do, one notch weaker: a remount would reload the page and
  discard scroll position, form state and anything the dev server had
  hot-reloaded in.
- **Nothing may render on top of the slot.** Under Electron the content is a
  native view at the slot's screen rect — chrome goes above it, status below it,
  and the empty-pane placeholder only exists while there is no page. A `position:
  absolute` decoration over the slot would be invisible in the browser build and
  cover the page in the desktop one.
- **The renderer owns the geometry.** A `ResizeObserver` on the container covers
  resizes; a 400 ms tick catches a pane that *moved* without resizing (a banner
  appears above the dock, a transition settles after the observer fired), and only
  sends IPC when the box actually differs. A native view left behind does not
  glitch subtly — it covers the wrong part of the window.
- **Only `http(s)` reaches a view**, checked in `normalizeBrowserUrl` *and* again
  in the shell (`safeUrl`), because a renderer is not a trust boundary. The
  renderer copy is what turns a bad address into an error instead of a silently
  ignored Enter; restored layouts are re-validated on the way in, since storage
  is where a hand-edited `javascript:` URL would sit waiting.
- **A focused native view swallows every keystroke**, so the app's accelerators
  are dead while a pane has focus. The shell intercepts `Ctrl/⌘+Shift+P` only,
  moves focus back to the page, and forwards it — `⌘K` is left to the previewed
  page, which is likelier to want it.
- **A URL row's primary action is opening it here**, with copy and
  open-externally as siblings rather than the row's only affordances. Siblings,
  not nested: a `<button>` inside a `<button>` is invalid HTML and browsers
  resolve it by dropping the inner one.
- **The session set is per worktree**, so two worktrees can each hold the same
  slot (their runs are on different hostnames, so the shared jar never shows).
  Removing a session returns its panes to Default rather than being refused —
  refusing meant the session you were looking at was the one you could never
  remove. Only this worktree's panes move, because the sets are per worktree.
- **A pane's own slot is unioned into its menu** (`sessionSetFor`). A restored
  layout can name a session the stored set has lost, and a pane missing from its
  own menu is worse than an extra row.
- **A session is a colour, in three places** — the tab's own kind glyph (a globe,
  tinted; one marker rather than an icon *and* a dot at tab size), the pane's
  session control, and the chrome's bottom edge. The edge rather than a strip above the
  view: a strip would move the native view's box every time the session changed.
  The colours are literal hexes in the model, not theme tokens, because an
  identity marker that changes with the theme identifies nothing.
- **`target=_blank` becomes a tab in the same dock**, carrying the pane's
  profile, because the shell denies the native popup and defers placement to the
  layout. A popup that relies on `window.opener` therefore breaks; the OAuth
  flows that need one are the known cost.

#### Device emulation, zoom and DevTools (`ui/src/panes/devices.ts`)

- **The state lives in the layout, and is re-asserted on create.** Emulation, page
  zoom and the user agent are per-`WebContents`, and a pane switching session
  *destroys and recreates its view* — so `PaneTab.emulation` and `PaneTab.zoom` are
  the record, `browserHost` holds the live copy, and both go in with the shell's
  `create` call rather than in a follow-up. A device that arrived one round trip
  late would be visible as the page laying out at pane size and then jumping.
- **The presets are size classes, not model names.** Small/medium/large phone,
  three tablet sizes, a 14″ laptop, a 24″ monitor, a 27″ widescreen — each carrying
  the metrics a current device of that class reports. A list of named handsets is
  the disliked part of every browser's version of this feature: it is long, it is
  out of date within a year of shipping, and the name never answers the question
  being asked, which is *how wide*. It also keeps the table short enough that
  nobody has to maintain a device database.
- **Metrics are stored, not a preset id.** A tab could store `device: "phone"` and
  look the numbers up, but then a layout written by a build whose preset table has
  since moved restores as a different device — silently, and only for the presets
  that changed. The id is kept for the *label* only, and an emulation whose id no
  longer exists reads as "Custom", which is what it now is. Two ids are not in the
  table and still mean something: `custom` (a hand-entered size) and `responsive`,
  which `sanitizeEmulation` therefore has to know about or a dragged pane would
  silently demote itself on reload.
- **The screen is draggable, and the page reflows live while it is.** A fixed list
  cannot contain the width a layout actually breaks at, so any emulated screen can
  be resized from handles on its right edge, bottom edge and corner — the
  `Responsive` entry is that with nothing else claimed, starting at the pane's own
  size. Dragging a preset lands on `custom` and keeps its flags; dragging the
  responsive viewport stays responsive. The screen grows about its **centre**
  (`deviceLayout` again — one placement rule, not a second one for drags), so the
  edge under the cursor moves half of what the size does, and the pointer delta is
  doubled to keep the edge glued to the cursor.
- **The pointer is the hard part of that, in both backends, for one reason: the
  thing being resized owns the events that land on it.** A `WebContentsView` is an
  OS-level sibling, so a mouse event inside its rect belongs to the guest and the
  /ide document never sees it; a cross-origin iframe consumes the ones on it too.
  Either way a drag whose pointer crossed the page would lose its moves *and* never
  see `pointerup` — a gesture that cannot end. The handles being *outside* the
  screen (in the inset) is what makes the common case work; the rest is:
  - under Electron, the shell forwards the view's own mouse events while a drag is
    live (`webContents.on('input-event')` → `veld:browser:pointer`), which is what
    lets the view stay **visible**. The coordinates come from
    `screen.getCursorScreenPoint()` minus the window's content origin, divided by the
    zoom factor — deliberately *not* from the event's own `x`/`y`, whose coordinate
    space the docs never state. That makes them the exact inverse of the CSS→DIP
    conversion the bounds handler does, so the page receives what its own
    `pointermove` would have carried;
  - the iframe backend has no such channel, so its frame goes `pointer-events: none`
    for the gesture. It cannot be hidden — there the frame *is* the thing being
    resized.
  An earlier version hid the native view for the whole drag instead. That was
  reliable and it was worse: you were resizing a rectangle rather than watching a
  layout reflow, which is the entire point of doing this in a pane.
- **Applied sizes are coalesced to one animation frame.** Each one is an
  `enableDeviceEmulation`, which relayouts *the user's page* and fires its `resize`
  handlers; a mouse emits several moves per painted frame, and there is no sense
  relayouting someone's app twice for one frame. The layout write happens once, on
  release. `syncGeometry` also stands down while a drag is live — it draws from the
  applied emulation on a 400 ms tick, so it was a second writer of the one element
  the drag owns, and it won whenever the pointer went still.
- **No DOM readout over the screen.** The size being dragged to goes in the chrome's
  chip, where the emulated size lives anyway. A label over the screen would be
  painted over by the native view in the desktop app and visible in a browser tab,
  which is the worst of both.
- **`scale` is computed in the main process.** Fitting a 1440-wide viewport into a
  600px pane is the entire argument for doing this inside the dock, and the number
  it needs is the view's box in **device-independent pixels** — which only the shell
  knows, since page zoom scales the CSS pixels the renderer measures and not the
  bounds a native view is given. So the renderer sends `fit: true` and the shell
  re-derives the scale, including on every bounds change. Both dimensions bind: the
  emulated screen *is* the view, so a viewport scaled to the width but taller than
  the box is clipped with nothing to scroll it into sight.
- **Zoom is re-asserted after every navigation.** Chromium's zoom policy is
  per *origin*, not per view: navigating adopts whatever that origin was last
  viewed at — including a level set by a different pane on the same session — so a
  pane that does not re-assert changes zoom on its own as you browse. That also
  means two panes on the same origin and session cannot hold different zooms
  indefinitely; the last one set wins on the next navigation. Documented rather
  than worked around, because the workaround is a partition per pane.
- **The emulation calls need a committed frame — or the app dies.**
  `enableDeviceEmulation` *and* `disableDeviceEmulation` on a `WebContentsView`
  that has not navigated yet **SIGSEGV the whole process**: not an exception, so
  there is nothing to catch, and the window is simply gone. That includes the
  `disable` call on a view where emulation was never enabled, which is what the
  no-device path did on every pane — the first version of this feature killed
  Electron on startup for exactly that reason. So the metrics are applied on the
  first `did-navigate` (or a main-frame `did-fail-load`, since an error page is a
  normal place to set a device from) and directly thereafter, tracked by
  `frameReady`; a `render-process-gone` clears it again. `setUserAgent` and
  `setZoomFactor` *are* safe before a load, which is the half that matters —
  a document reads `navigator.userAgent` once, while it loads. All of this was
  measured against the installed Electron rather than read, because the docs say
  nothing about it.
- **Touch is the one thing that can be taken away.** There is no Electron API for
  it: `Emulation.setTouchEmulationEnabled` (which makes `ontouchstart` exist) and
  `setEmitTouchEventsForMouse` (which turns a drag into `touchstart`) are CDP, so
  it needs `webContents.debugger.attach()`. Electron's docs say the built-in
  DevTools takes that session — on Electron 43 it does not, and the two coexist
  (measured; `isAttached()` is still true with the inspector open). Both worlds are
  handled rather than either being trusted: nothing is detached pre-emptively (an
  earlier version did, and threw touch away for a conflict this Electron does not
  have), a `detach` from any cause flips `touchActive` to false, and
  `devtools-closed` retries the attach. The pane therefore reports what it
  *achieved* — `touchActive`, separate from the `touch` it asked for — and its menu
  says touch is paused without claiming to know why. Both CDP calls are sent,
  because a page asks both questions: feature detection and behaviour.
- **A user-agent change reloads the pane; a size change does not.** The emulated
  viewport is live — Chromium relays out and the media queries follow — but a
  document read `navigator.userAgent` and tested for `ontouchstart` once, while it
  was loading. Picking "iPhone" and being handed the desktop bundle is the
  confusing half of that, so the UA and touch flags reload and nothing else does.
  On a *fresh* view the order avoids it entirely: emulation is applied before the
  first `loadURL`, so the page's first request already carries the emulated UA.
- **DevTools is always detached** (`openDevTools({ mode: "detach" })`). A docked
  inspector resizes the view from the inside while the renderer mirrors the pane's
  box from the outside, and the two fight — every resize the inspector makes is
  undone by the next `setBounds`, which arrives on a 400 ms tick.
- **The user agent is validated in the shell**, not only in the renderer
  (`safeUserAgent` in `desktop/src/validate.js`). It is the one field of an
  emulation that leaves the process as *protocol* rather than as geometry:
  `setUserAgent` takes a header value, so a CR or LF in one is header injection
  against every origin the pane visits. Printable ASCII, bounded, and rejected
  rather than repaired.
- **The browser build gets the layout half and says so.** An iframe's own width
  *is* a real viewport, so a page in a 393px frame sees 393px in its media queries,
  and a CSS `transform` scales the rendered result to fit the pane. What a frame has
  no API for is the rest — no user agent, no touch, no device pixel ratio, no page
  zoom — so the device menu states the gap instead of implying parity. (A CSS
  transform is not zoom: it scales the *rendered result* rather than the viewport,
  which is exactly why it is the right tool for `fit` and the wrong one for zoom.)

Why not join `crates/veld-daemon/frontend/`? That package builds IIFE snippets
(feedback overlay, client-log) with esbuild and no framework; the management UI
is an application with a different toolchain (Vite, React, HMR). Two small
npm projects beat one franken-config.

### `desktop/` — the Electron wrapper

Minimal by design. Main process only does:

1. Create a frameless `BrowserWindow` (`titleBarStyle: 'hiddenInset'`) and load
   `${VELD_DESKTOP_URL ?? http://127.0.0.1:19899}/ide?shell=electron`. The window
   is titled by the app and `page-title-updated` is cancelled — the UI arrives over
   HTTP, so otherwise the window takes whatever `<title>` that bundle carries, and
   a reload could rename it. `app.setName("Veld")` for the same reason: an
   unpackaged run would call itself "Electron" in the macOS application menu.
2. If the daemon isn't reachable, show a local retry page (embedded data URL —
   install/start instructions) and poll until it appears.
3. macOS tray (template icon): shows running-run count, per-run stop/restart
   later; click focuses the window.
4. `contextIsolation: true`, `nodeIntegration: false`, preload exposing
   `veldDesktop.shell` metadata plus `veldDesktop.browser` — the embedded
   browser panes (`src/browserViews.js`), which is the one thing a page cannot
   do for itself. Every method is a fixed channel with a fixed shape: the page
   never names a channel, so it cannot reach a handler the preload doesn't list.
5. Own the browser panes' lifetime: views are keyed by (window, view id), so a
   renderer can only address its own. They are disposed when the window closes,
   and otherwise only when the page asks — it calls `reset()` as it boots, before
   creating any, because a view outliving its renderer's registry paints over the
   new page with nothing able to close it. Disposing from a navigation event in
   this process instead is a race against the renderer's first `create`, and
   losing it destroys the view the new page just asked for.
   Views run sandboxed with no preload, in a `persist:veld-browser-<profile>`
   partition, with all permission requests denied and only `http(s)` accepted.

No packaging/signing in this increment — `npm start` (dev run) only. The app icon
`electron-builder` will want already exists (`assets/icon.png`); an unpackaged run
sets it on the dock itself, since otherwise a dev window is indistinguishable from
any other Electron app.

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
| `POST /api/worktrees/{id}/start` `{preset?, selections?, run_name?}` | Spawns `veld start` with the worktree as cwd (the CLI resolves veld.json from there). Two mutually-exclusive start modes: `preset` (`--preset <p>`) or explicit `selections` (`node:variant` positionals, validated per half); the UI always sends one. Sending neither is **not** a safe probe: a bare `veld start` uses the project's `default_preset` when one is declared, and only fails "No selections provided" when there isn't one — so an empty body may spawn a run. Default run name: the alias. `202 Accepted`; progress observed via `/api/environments`. |
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
# 0. optional: npm deps for ui/ and desktop/ up front (also how you refresh them
#    after a dependency bump). Every recipe below installs what it needs first,
#    so a fresh worktree can skip straight to step 1.
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
`dev-desktop-embedded`, `desktop` mirror the existing frontend recipes; each
depends on a guarded deps step, so a checkout with no `node_modules` installs them
instead of failing on a missing binary. For `desktop/` that step also fetches the
Electron binary explicitly: npm defers install scripts it has not been told to
allow, which otherwise leaves a complete `node_modules` whose `electron` reports
`command not found`. CI
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
2. ~~Embedded webviews + isolated sessions~~ — shipped as the `browser`
   `PaneKind`; see the decision log and the browser-panes notes above.
3. ~~Terminal panes~~ — shipped; see the decision log above.
4. ~~`veld share` from the UI~~ — shipped as IDE mode's Sharing surface; see the
   decision log. Start-run UX beyond preset picking is still open.
5. ~~Device emulation + DevTools for browser panes~~ — shipped; see the decision
   log and the emulation notes above.
6. Extension system (`veld-ui.json` badges), PR/CI badges, overview board.
7. Packaging, auto-update, CLI installation from the app.

The sequencing and the transport/renderer decisions for these live in
[issue #167](https://github.com/prosperity-solutions/veld/issues/167).
