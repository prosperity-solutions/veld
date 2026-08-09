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
| Theme | Handoff palette (Inter + JetBrains Mono, oklch greens) | Deviates from the classic product tokens in `docs/branding.md`; sanctioned there as the **desktop theme**. Structural branding rules (an embedded mark — wordmark or icon mark, self-contained assets, noindex) still apply. |
| Terminal transport | **PTY spawned daemon-side (`portable-pty`) over a WebSocket** | Not `node-pty` in the Electron main process: the browser build needs a daemon route regardless, so Electron would mean two implementations of one feature and would break the "usable without Electron" invariant above. Also avoids node-pty's native-rebuild churn across Electron versions. |
| Terminal auth | Single-use ticket from a CSRF-gated `POST`, plus an `Origin` allowlist | The daemon's CSRF gate is custom-header-only, and a WebSocket handshake can carry neither a custom header nor a CORS preflight — so the usual gate is structurally unreachable on an endpoint that hands out a shell. Loopback is not the mitigation either: the helper publishes the daemon at `veld.localhost`. Both gates are kept deliberately; see the module docs in `crates/veld-daemon/src/pty.rs`. |
| Terminal session lifetime | Sessions outlive their socket; explicit `DELETE` ends one | A terminal is not re-creatable state — dropping it kills the shell and everything in it — and a reload drops the socket. So a socket closing means "come back later" (scrollback is buffered and replayed on reattach) while closing a *pane* means "done". The session budget is defended at the *creation* end rather than by disconnecting hidden panes: a worktree's default layout seeds a `new` pane instead of a terminal, so browsing the rail starts no shells at all (cap of 48, readable error on hitting it). Disconnecting a hidden pane's socket was the other candidate and is **rejected**: a detached session is reapable, so a build left running in a worktree you switched away from would be hung up after the detach grace — the exact loss the row below exists to prevent. |
| Terminal PTY ownership | **One holder process per session** (`veld-daemon --pty-holder`) — not the daemon, and not one shared supervisor | A PTY's master descriptor dies with the process holding it, and the kernel then hangs up the shell's foreground process group. So while the daemon held it, `veld update` ended every terminal and no care on the shutdown path could change that: `shutdown_sessions` hung the shells up *deliberately*, because the alternative was the same death half a second later with orphaned grandchildren. Moving the master out is the only fix, and an `exec`-handoff is not available — `install.sh` restarts the daemon with `launchctl bootout` + `bootstrap`, so the daemon never controls its own restart. **Per session** rather than one supervisor: nothing global is handed over, so there is no "the supervisor is itself being updated" problem (a new daemon spawns new holders from the new binary while old holders keep serving old sessions), and one holder lost is one terminal lost. It is the daemon's own binary in a different argv mode, so there is no new artifact, no `electron-builder` entry and no `release.yml` change; the daemon proxies its WebSocket to a `0600` unix socket under `<daemon socket dir>/pty-<port>/` and rebuilds its registry at boot by scanning that directory, which `veld doctor` also reports (holders are invisible to every other check, so a terminal that did not come back had nowhere to look). **The client-visible protocol does not change**: the ticket, the `Origin` gate, the takeover epoch, the one-writer rule, the replay bracket and the detach grace all stay where they were, and the daemon's scrollback is now a *mirror* fed by the holder — which is what keeps `serve_socket`'s subscribe-and-snapshot-under-one-lock argument intact. Two things had to be designed rather than discovered: after an update the holders run the *old* inode while the daemon is the new one, so the handshake carries a protocol version, and an unrecognised one reports the session as gone **and** tells the holder to hang up (`HANGUP` is pinned to one byte and stable across every version — otherwise a refused session leaks a shell that no future daemon could reach either, since every daemon would refuse it identically); and the holder must leave the daemon's process group (`process_group(0)`, plus `KillMode=process` on the daemon's systemd unit) or launchd's `bootout` and systemd's default control-group kill take it down with the daemon — the very failure it exists to prevent. |
| Terminal renderer | **xterm.js**, with `ghostty-web` a fast-follow candidate | `ghostty-web` is the better emulator but pre-1.0, with an unverified addon story (fit-addon is required for resize) and a ~400 KB WASM blob that `vite-plugin-singlefile` would base64-inline into the daemon binary. It is API-compatible with xterm.js, so swapping stays cheap and this does not gate the terminal work. |
| Pane layout | Two docks of tabs with a draggable split | The terminal, the embedded browser and run diagnostics all want the same region; each one added as another fixed column makes the window unusable. A tab kind per content type means the next increment is a renderer, not a layout rewrite. Layouts persist **in the daemon's database**, one row per worktree (`pane_layouts`, migration v15, read and written through `ui/src/ide/layoutStore.ts`). They were browser storage, and that was the wrong place for a reason no amount of key-juggling fixed: the same `/ide` also runs in the user's own browser, which had no access to the app's `localStorage` at all — so a tab opened on a worktree the app was running rendered a *different*, empty set of panes and, not knowing the app's terminal session ids, spawned a second set of shells beside the live ones. A row a worktree owns is the same answer in every client. Writes carry the version they read and are refused rather than merged on a mismatch; contention is prevented upstream (one client shows a worktree at a time), so the version is a hand-off guard — the client that yielded a worktree can still have a debounced save in flight when the one that took it starts editing. A **detached** window is the exception and keeps browser storage keyed by a window **slot** the shell assigns and hands to the preload script (`webPreferences.additionalArguments`, read back as `window.veldDesktop.layoutSlot`), because its tabs were transferred *out of* a worktree and belong to the window rather than to the checkout: an app update replaces the window, and a new window is a new `sessionStorage`, so the shells survived while the ids naming them did not. **Not a query parameter**, which is where it started: anything a link can forge is not a safe key for state that names live PTY sessions — a browser tab opening `/ide?slot=main` would have restored the desktop app's terminals and then fought it for them, since an attach *takes over*. A packaged app owns `main`; a dev instance owns `dev`, with a pid lockfile (`claimSlot`) so a second concurrent one takes a slot of its own; and a browser tab has no slot at all, and is never a detached window, so it never reaches the slot store. Deriving the slot from `requestSingleInstanceLock` was tried and is wrong: the lock is per appId and first-caller-wins, so a dev instance holding it made the packaged app quit on launch. A slot now has **two parts**, because two independent things can collide: `claimSlot` separates *processes* (the base) and a suffix separates *windows* within one (`slotFor`/`nextSuffix` in `windowState.js`) — see the Windows row below. |
| Windows | **One window type with two chromes** — `main` (a full `/ide`) and `detached` (a bare dock), both owning a layout slot and a place in a persisted window set (`desktop/src/windows.js`) | Two things were wanted and they are not two features: a second full window for a second repository on a second monitor (`⌘N`), and a single pane pulled out to sit beside the editor. Everything hard is common to both — a slot of their own, a row in the window set, the ownership rules below — so the difference is reduced to a query parameter (`chrome=none`) and what the window is seeded with. A window that renders one pane and cannot be split was the alternative and is a dead end: "logs on the side" becomes "logs *and* a terminal on the side" almost immediately, and a browser pane needs its device toolbar out there anyway, so the pane chrome ships regardless. **A detach transfers, it never copies.** A layout names live PTY session ids and a second attach *takes a session over* rather than mirroring it, so a copied tab id is two windows trading one shell forever; the tab therefore leaves the origin layout in the same step the new window is seeded. The seed is read **synchronously, in the preload** (`ipcRenderer.sendSync` on `veld:window:seed`) because it has to be available in the first render — `pruneTerminals` ends every session the layouts do not name, so a layout arriving one tick late reads as "these are orphans" and hangs up the shells that were just moved. It rode `additionalArguments` first, and that was wrong twice over: a process's argv is world-readable on Linux while a seed carries browser panes' URLs (fragments included), and the size ceiling counted JavaScript string length when what had to fit was the base64 of the UTF-8 bytes — up to 4× larger, so a long page title produced a window whose renderer never launched, after the origin had already let its tabs go. `releaseTerminal` exists beside `disposeTerminal` for the related reason: removing a tab from a layout is what kills a shell, intent has nothing to do with it.

**The seed is retired when the renderer first reports a snapshot** — not when it is read, and not when the page loads. Both of those were tried and both dropped it before anything else held the tabs: a preload runs on *every* load including the `data:` waiting page, and a page that has finished loading still does not know its layout until `/api/repos` resolves (a failed first request retries five seconds later). A snapshot arriving is the only event that proves something else holds these tabs, and until one does, closing the window hands the *seed* back.

**Only a window the shell *reopened* reads the durable **slot** store** (`restored` on the bridge, checked in `readLayouts`). Slots are recycled lowest-first and a slot's key is never cleared, so to a `⌘N` window the layout sitting there is a dead one that happens to share a number — restoring it meant attaching to terminal ids another window was using, and an attach takes a shell over. This governs *detached* windows, which are the only ones still keyed by slot; a main window reads its worktree's row from the daemon instead, where picking a worktree up is the point rather than a collision (see the Worktree ownership row).

The seed also **beats the durable slot store**, which is the opposite of the obvious ordering. A seed exists only for a window created this instant, and slots are *reused* — so detach, close, detach again lands on the same slot, where the first window's dead layout was still sitting; reading it discarded the seed, killed the shell being moved (it was in no layout at all by then) and re-attached to ids another window had just adopted.

Closing a detached window **hands its tabs back** to the window they came from — matched on a per-process record id, falling back to the persisted suffix only for a window restored from disk (where every id is new but no suffix has been recycled yet), because a freed suffix is otherwise a plausible-looking wrong answer. The hand-back is **queued in the main process rather than pushed**, because `webContents.send` is fire-and-forget and the receiving listener does not exist until the `/ide` bundle has mounted: a hand-back to a window still on the waiting page (the daemon is restarting — exactly the case terminals are built to survive) would otherwise be dropped on the floor. A detached window also persists **which worktree it is a dock for**, since it has no rail to re-resolve one and would otherwise reopen against whatever the main window last selected. The renderer drains the queue at mount as well as on the nudge. Closing a *pane* still ends a shell, and those two have to keep meaning different things. Ceiling of eight windows, which also bounds the durable layout keys: suffixes are reused lowest-first, so a closed window's abandoned layout is overwritten rather than accumulating, and nothing prunes those keys because a pruner in one window would be deleting another's live layout. |
| Worktree ownership | **One set of panes per worktree, one client showing it — arbitrated by the daemon** (`crates/veld-daemon/src/ide.rs`), over a control WebSocket every `/ide` opens (`ui/src/ide/channel.ts`) | It lived in this process, in a `claims` map, and the flaw was structural rather than a bug: Electron main can see its own `BrowserWindow`s and nothing else, while the same page also runs in the user's browser. A tab was therefore invisible to the entire arrangement — it opened a worktree the app already had, rendered its own separate panes for it, and fought the app for every shell, since a second PTY attach *takes a session over* rather than mirroring it. Moving the registry to the daemon is what makes a browser tab a client like any other, and it is the only process both kinds of client share. **The lease is the socket.** A claim lives exactly as long as the connection that made it — no heartbeat, no TTL, no reaper — because every way a client can stop existing closes the socket, and a close releases what it held. The one exception is a page *reload*, which the old design could not tell from a close at all: the daemon holds a disconnected client's claims for a short grace, and the same `client_id` (per tab, in `sessionStorage`) reconnecting takes them back, while anyone else claiming one gets it immediately — nothing is attached behind a closed socket, so an orphan must never make a worktree unopenable. The rest of the protocol is carried over intact and for its original reasons: the claim is recorded synchronously, so a third client asking during the wait is refused rather than granted alongside; the *answer* is held until every other holder has acknowledged the yield, because the claimer attaches to the worktree's terminals on the strength of it; a holder that cannot answer is proceeded past after a timeout with a warning, since a claim blocked forever is worse than the race; the acknowledgement is sent from a passive effect rather than the message handler, because the release runs inside a `setLayouts` updater and acknowledging on receipt would promise a release React had not performed yet; and `releaseTerminal`, never `disposeTerminal` — the shells keep running and the layout stays in the database, which is what makes "close that window and it comes back over here" need no hand-off protocol. **Focus is where the two client kinds genuinely differ, and the difference is stated rather than papered over.** The daemon pushes `focus` to the holder; an Electron window raises itself (`veld:window:focus-self`, the one thing only the shell can do), and a browser tab **cannot** — `window.focus()` outside a user gesture is ignored by every browser. So the refusal carries the holder's *kind*, and the client that asked says "open in Safari — switch to that tab" instead of claiming a raise that visibly did not happen, while the holding tab marks its own title. Faking it was the alternative and is worse than the gap: a promise the user watches fail. Detached windows are exempt from all of it — their tabs were transferred out of a worktree a main window owns, so they keep the per-slot store and never claim. **Ownership is visible before it is enforced**: the daemon pushes the whole claims table to every client on connect and after every change, and the rail marks the rows another client has. Dimmed with an icon rather than disabled, because the row still does something (it takes you there). What this process kept is one much smaller thing: `showing`, a `worktreeId → window id` map the renderer reports into (`veld:window:shows`), used only to route a cross-window tab drop at a window those tabs belong in — a question about its own windows, which is the one class of question it can still answer best. A stale entry there costs a drop that opens a new window instead of moving tabs; a stale *claim* used to grey out a rail row with no window behind it. |
| Cross-window drags | **The shell carries the pointer**: it broadcasts the drag to every window and forwards the cursor to whichever one it is over, in that window's own coordinates | A drag never leaves the document it started in, so the window being dropped *onto* is not told one exists. Three symptoms, one cause: its native views kept painting over any drop overlay (a `WebContentsView` is above all DOM), it showed no insertion indicator, and a dropped tab could only be appended at the end — appending was not a policy, it was the only thing a window that cannot see the pointer can do. So every window freezes its views for the duration, and the target resolves the forwarded position with `elementFromPoint` and renders its *ordinary* drop UI: same edge zones, same caret, same code. A drop from another window is therefore not a second behaviour to learn. Polling the cursor rather than forwarding events, because the source stops receiving them the moment the pointer leaves it — which is the whole problem; `browserViews.js` forwards pointers window-wide during a pane resize for the same reason. **The release commits what the drag resolved and recomputes nothing.** Four attempts failed the other way: reading `dragend`'s coordinates (which hold the drag's *start*, or `0,0`), testing window bounds (blind to two windows overlapping), and twice clearing the remembered answer before reading it. Both halves — which window, and where inside it — outlive the drag that computed them and are invalidated only by the next one. **And the source never lets go until the destination acknowledges**: it releases terminals and closes tabs on the shell's answer, so answering on *send* meant any failure on the far side destroyed the pane. A tab that stays put is a visible non-event; a vanished one with a live shell behind it is not recoverable. **A claim outlives the `PaneArea` that can answer for it**, though, so acknowledging is not the same as being able to: a window holds its claim from the moment it *asks* for a worktree, while the listener exists only once `/ide` has mounted — and again not at all through a reload, while the first `/api/repos` is in flight (a failed one retries five seconds later), or on the waiting page during a daemon restart. Pushed there, the send went nowhere and the drop reported `refused` two seconds later, which the source turns into "The desktop shell refused the request" for a gesture that looked like it worked. So a drop is routed only at a window that can answer, which is two questions with two owners: the shell asks itself whether the app is even on screen and still loading (`getURL()` plus `isLoading()` — `isLoading()` alone is false both before the first `loadURL` and while the `data:` waiting page sits there, which is the longest gap of the lot), and the renderer reports whether its listener is registered. A drop at a window failing either is **queued** — the hand-back queue, appended rather than placed, since a window that previewed nothing has no caret to honour. A window that has *never* reported is sent to rather than queued for, which is deliberate: that is what an older `/ide` bundle against a newer shell leaves behind, and there send-and-time-out is the behaviour that build already had. The ack timing out on a window that *does* claim a listener falls into the same queue, and only a handler that explicitly places nothing is still a refusal — re-delivering that would insert tabs its own validation had just rejected. The queue is therefore the one place the main process is custodian of tabs the source has let go of, which is why closing *any* window carries an un-drained queue on to another rather than dropping it: `handBack` was detached-only, and a main window closing before it drained already lost them. Two exceptions, both stated rather than implied — a **quit** hands nothing on (every window is closing, and the persisted window set must be left exactly as it stood), and a window with no other window to hand to keeps nothing alive either; in both the shells outlive the app under the detach grace, which is the same fallback a detached window's tabs have always had. The gap the queue does *not* close is the one it inherits: a target that never drains and is then closed hands on again, and the chain ends at the last window. |
| Detached browser panes | **Destroy and recreate**, with the reload named in the menu | A `WebContentsView` belongs to a `BrowserWindow`, and re-parenting one is not an operation Electron offers. The tab record already carries everything needed to rebuild — URL, session, emulation, zoom — because a *session switch* recreates a view for its own reasons, so this path existed already. What is lost is scroll position and anything typed into the page, which is why the context menu says "(reloads the page)" rather than letting it be discovered. |
| Drop model | **One model for both gestures**: edge zones on the pane *area*, and out-of-window to detach | The tab strips accepted drops and the pane body did not, so dropping a tab where its content will be — the thing people try first — silently did nothing (`dragover` without `preventDefault` is a refusal). Zones are read off the whole dock area rather than per dock, which is what makes edge-split and plain-move one rule: with one pane the outer edges are its own, with two they are the pair's, and the gesture means the same thing in both. Detach is the same drag with a third destination, and "did the pointer leave this window" is answered by **`dragleave` with a null `relatedTarget`** — the OS routes drag events by stacking order, so that is the one signal here that knows two Veld windows overlap. `dropEffect === "none"` plus a screen-coordinate test was the first attempt and is *gone*: the effect also describes a fumbled drop inside the window, and `dragend`'s coordinates hold the drag's start (or `0,0`) rather than the release point, so it detached on drags that had not left and ignored ones that had. The whole gesture holds a `pushBrowserSuspend`, for the reason the splitter does: a native view owns every event in its own rect, so the new edge zones are exactly the region a browser pane would have swallowed. The suspend is module state, not component state, because a drop *moves* tabs between docks and can unmount the element the drag started on before its `dragend` arrives — every path that ends a drag releases it, idempotently. |
| Embedded browser | **Electron `WebContentsView`**, with an `<iframe>` fallback in a plain browser | The pane has to render the user's own dev server, which frequently sends `X-Frame-Options`/`frame-ancestors` — an iframe shows those as a blank rectangle with nothing observable to report. A native view also has real back/forward, a readable URL and title, and per-pane cookie jars. The iframe stays because "usable without Electron" is the invariant above; it is honest about what it cannot do (no history, no separate sessions, no way to detect a refused frame) rather than pretending. |
| Browser sessions | Global, colour-coded, animal-named `persist:` partitions — the default plus up to 8 — with the set that *exists* tracked per worktree in `localStorage` | The point of the feature is being two logged-in users of your own app at once, which is a cookie jar per pane. The *allowed* slot names are a closed list because the name becomes an Electron partition — an identifier the main process has to validate anyway — but which of them **exist** is a set the user builds up, stored in `localStorage` and keyed by worktree. Deriving that set from which slots panes occupy was the first attempt and it inverted the feature: moving a pane onto a new session vacated its old slot, so adding one appeared to delete the previous. `localStorage` rather than the daemon because a session only means anything under Electron (the browser build's iframe backend has no cookie jars of its own), so there is no second client for the list to disagree with — this is one client's preference about its own capability, not the shared settings store batch 5 needs. Eight above the default is the colour ceiling: more dots stop being tellable apart, and the colour is what makes "which session is this pane?" answerable without opening a menu. The slots are **named** (otter, wombat, gecko…) rather than numbered, because a number implies a sequence: removing "Session 2" and being left with "Default, Session 3" reads as breakage, while a name has no successor to be missing. The name is also the partition, so the identifier says what it is. Partitions stay global, so two worktrees whose sets both hold the same slot share that jar. Stated rather than hand-waved: only the *run* differs between two worktrees' hostnames (`{service}.{run}.{project}.localhost`), so a project-scoped cookie is shared, and a third-party login domain is shared unconditionally. Keying the partition by worktree is the fix if it bites. Clearing data is addressed by partition, not by pane, so a slot no pane holds can still be emptied. |
| Native-view z-order | The renderer hides views while a DOM overlay is open | A `WebContentsView` is a native sibling of the page: it paints over every menu, dialog and dropdown regardless of z-index, and there is no CSS answer. `panes/overlayGuard.ts` suspends the views while one is open. It watches the subtree of Mantine's *shared* portal node — `Portal` reuses one container that is appended to `body` once and never removed, so watching `body`'s children sees a single mutation and then goes deaf — and it requires a match to be actually painted, because `Combobox` keeps its dropdown mounted at `display: none` and `mantine-Modal-root` stays in the DOM when the modal is closed. Both of those shipped as bugs first: the deaf observer, then a permanent false positive that hid every pane. Hidden is not blank, though: each visible view is **captured first, decoded, and painted onto the container before the view goes**, so a pane freezes rather than disappearing every time a menu opens — hiding first and painting when the capture landed was itself a visible flicker — and so was routing the still through React state, which costs a render plus an async image decode. Bounded by a timeout, so a slow capture cannot leave the overlay stuck behind a view. App-owned surfaces that aren't portalled call `pushBrowserSuspend` from their own state. It is a heuristic, and deliberately the kind that fails *visibly* (a dropdown behind a pane) rather than the kind that blanks a pane at random. |
| Run diagnostics | Two pane kinds (`logs`, `nodes`) rendering **the same views runs mode renders** (`ui/src/shared/RunViews.tsx`) | Every endpoint already existed (`/api/logs/{run}`, `/api/stats`, per-node health on `/api/environments`), so this is a UI question only, and the UI question is where they live. Panes rather than a third fixed column, for the reason the dock exists at all. Extraction rather than a second implementation, because the pair would have drifted on the first change to what a node row says — the health sub-line (failures / recoveries / last liveness error) is exactly what a fork forgets. The panes hold **no run identity**: they read the run the *window* is bound to — the top bar's run selector, resolved once in `App` by `pickRun` — so a worktree switch or a run switch re-points every open one at the same time, and a pane can never show a run whose worktree is off screen. The binding is deliberately *wider* than "something to stop": an ended run stays selectable, because after a crash there is nothing to restart but the logs and the last node states are the whole reason you opened the pane. One selector moving every pane together is the accepted limit — watching two runs of one directory side by side needs a per-tab run pin, which belongs in the tab (a value the layout carries), not in a second selector. |
| Sharing in IDE mode | One top-level surface in the top bar (a popover), with join requests as a **banner above the panes** | Follows #152's rule — a Sharing surface, not a relay-details dump — and a popover rather than a `Menu` because the content is interactive (the auto-accept checkbox and the copy buttons must not close it). Sharing is *refused* far more often than it fails — a service has to opt in (`share.expose`) and a relay has to be configured (`sharing.relays`) — so the daemon's refusal text is the feature, and it reaches the user as a toast. The join requests are deliberately *not* in it: someone is sitting on the other end waiting for an answer, so the prompt has to be visible without opening anything, and it lists **every** share's requests rather than the selected worktree's — a request against a worktree you are not looking at must not be invisible. It names the run each one is for, since a request carries only its `share_id`. |
| The run's URLs are not a pane kind | A launcher component (`panes/VeldLinks.tsx`) shown inside whatever pane is about to need it | They are how you *get* a page, not a peer of a terminal and a page. A kind for them meant a singleton tab id, a "does one already exist" check at every call site that could open one, and a second implementation of the same rows. Now a `new` pane and a browser pane **with no URL** both show them — the second being the useful one, since the list sits in the thing that is one click from becoming the page. The top bar's globe just opens an empty browser pane, and a worktree's default layout is a single undecided pane, which shows the same list. Links that are *not* veld's belong in the project's config, not hardcoded: `ide.quicklinks`, the *Per-project quicklinks* item in issue #167 (referenced by name, not by its number, which moves as the roadmap is reordered). |
| Pane creation | `+` opens an undecided `new` pane; the choice happens inside it | A menu off a `+` button is the size of a cursor and vanishes when you look away, while the thing being chosen is a whole pane. So `+` opens the pane and the pane asks what it should be, at content size — and picking a kind *replaces* the tab (`replaceTab`) rather than adding one, so the flow costs a single tab. The same screen serves an empty dock and a closed-everything region, which is why they now look identical. Hovering `+` still offers the one-click shortcuts for people who know what they want. |
| Pane screens | Loading, error and chooser are DOM screens *and* they hide the view | A native view paints over DOM, so "the pane has something to say" and "the view is off screen" are one decision, not two — `covered()` in `browserHost.ts` owns it and the pane's render mirrors it. Error copy is keyed off Chromium's net error, not its message: "nothing is listening" (start the run) and "that hostname doesn't resolve" (`veld doctor`) are different problems, and the codes are stable where the prose is not. A *re*-load keeps the old page up rather than covering it with a spinner, which is what `loaded` is for. |
| Orphan views are dropped by the *page*, not by a navigation event | `reset()` at module load, before any `create` | A reload replaces the page's registry of views, so the old ones are orphans painting over the new document. Disposing them from the shell's own `did-navigate` is a race against the renderer's first `create` — and losing it destroyed the view the new page had just asked for, which is why the first browser pane after a hard reload came up blank with reload as the only escape. Driving it from the renderer makes the ordering a queue. |
| Views start visible | `create` no longer hides the view and then shows it | Chromium background-throttles a hidden `WebContents`, and a view created hidden and loaded in the same tick sometimes never rendered its first page — blank until you pressed Reload. The renderer sends its own visibility immediately, so starting visible costs nothing and removes the race. The spinner's 8-second "taking a while" reload is the backstop, since a genuinely slow dev server must not be called an error. |
| Device emulation | **Native Electron `enableDeviceEmulation` for the metrics and the UA, CDP only for touch**, per pane, with detached DevTools beside it — presets as **size classes**, plus a draggable screen | The case that justifies doing this inside the dock rather than saying "use Chrome" is not the phone — it is the *desktop*: emulating a 1440-wide viewport **scaled down to fit a 600px pane** is what no real browser window can give you without a second monitor, and it is one API call. The metrics and `setUserAgent` are native, so the useful 90% costs no CDP plumbing. Touch is the exception and it is a deliberate one: `Emulation.setEmitTouchEventsForMouse` needs `webContents.debugger.attach()`, which Electron's docs say the built-in DevTools takes over — measured on Electron 43, it does not, and the two sessions coexist. Both outcomes are handled rather than either being trusted, and the pane reports what it *achieved* (`touchActive`, separate from the `touch` it asked for) instead of asserting a mode it may not have. Touch is worth the state machine because metrics alone test the layout and never the interaction: a swipe carousel, a `@media (hover: none)` rule and any library that branches on `ontouchstart` are all invisible to a narrow viewport that still receives mouse events. Emulation is **per pane**, not per window — a phone beside a desktop is the comparison people actually want — which follows from it being per-`WebContents` anyway. Page zoom rides along in the same control and the same layout field, because it has the same shape of problem (per-`WebContents`, lost when the view is recreated) and is useful well before any preset is: a 1440-wide layout is readable in a 600px pane at 60%. Zoom carries one honest caveat — Chromium's zoom policy is per *origin*, so two panes on one session and origin cannot hold different zooms across a navigation; the alternative is a partition per pane, which is a heavier feature than the problem. |
| Browser pane permissions | **A policy with three sources — the user's stored answer, the project's `ide.permissions`, then veld's defaults — and a prompt rendered *in the pane*** | Panes used to refuse every permission outright, and the reason was sound: a prompt raised by an embedded pane has no chrome to attribute it to, and "example.com wants your camera" is a lie when the window says Veld. Blanket denial had a cost that had become a functional break, though — veld's own feedback overlay screenshots through `getDisplayMedia({ preferCurrentTab: true })`, so `veld feedback` could not take a screenshot inside a browser pane, the one place it should work best. The prompt is therefore a strip in the pane's own chrome rather than a native dialog: there it can name the origin, the pane and its session colour, which is exactly the attribution a dialog cannot give. It sits *above* the slot, never over it, because a native view paints over DOM whatever the z-index says; shrinking the slot is free since its `ResizeObserver` republishes the view's box. **Handlers are registered per session, not per view** — panes sharing a profile share one `Session`, and the handler is asked about any `WebContents` on it including a pane's *detached DevTools frontend*, so dispatch starts by resolving the `WebContents` back to a pane and denies anything unresolvable. Two constraints are Electron's, not choices: the *check* handler is synchronous so it cannot prompt, which means `navigator.permissions.query()` reports `denied` until a real request has been answered (and is the strongest argument for the config layer, whose answer *is* available synchronously); and `getDisplayMedia` is rejected outright without a `setDisplayMediaRequestHandler`, since there is no built-in picker to fall back on. A prompt's answer is **sticky** per (session, origin, permission) — as Chrome's camera and microphone answers are — which is also what lets the display-media handler re-resolve the verdict for the same `getDisplayMedia` call instead of raising a second prompt. Screen capture is granted **only at an origin veld serves**, and hands over only the requesting frame. One Electron mapping detail is load-bearing and cost three test rounds to find: `getDisplayMedia` arrives as a **`media` request with an empty `mediaTypes`**, not under the `display-capture` name that also exists in the request union — Electron fills that array only for *device* capture. Reading the empty case as a device enumeration (which is what it means on the *check* handler, where `enumerateDevices` asks whether labels may be shown) refused every in-pane screenshot before the display-media handler was consulted, and resolved to no permission id at all, so it could not even raise a prompt — a permission the user could have granted, denied with no recourse in the UI. The two handlers now say which they are. |
| Project-declared permissions | **`ide.permissions` in `veld.json`, with the user's own answer outranking it** | Giving the key reserved in schemaVersion 3 its first real meaning rather than adding a config surface (`ide.quicklinks` lands with it). It was spelled `ui` while it was wholly reserved and was **renamed to `ide`** in the same release that interprets it — a top-level key rename is breaking, so there was no later chance at it, and `/ide` is the route while "UI" could equally have meant the dashboard or the CLI's own output. A config still using `ui` gets an unknown-top-level-key error naming the rename. A repo that can already run arbitrary commands through `veld start` is not meaningfully constrained by withholding a camera from its own dev server, so a *loopback* grant costs nothing that was not already spent; a **remote-origin** grant is a genuine step further — a standing capability for third-party JavaScript — and the mitigation chosen is visibility rather than a blocking prompt: every config grant appears in the pane's per-site panel labelled *set by veld.json*, and a user's answer wins in both directions. Origins carry no `${...}` interpolation — the panel and the permission check both run with no live run to resolve against — but they **do** take a leading `*.` on the host, and that is not a convenience: veld's own URLs are `{service}.{run}.{project}.localhost`, so the run name is *in the hostname* and a pinned host is a rule that survives exactly one run. The wildcard is confined rather than trusted: leading label only, matched label-wise (so `evilveld.localhost` misses `*.veld.localhost`), not matching the apex itself, and refused over a single label — `*.com` is the accident worth blocking outright, with `*.localhost` allowed because RFC 6761 pins it to loopback. Malformed entries are dropped and reported by `veld lint` as warnings, never load errors — F0.1 says a config that will not load takes `veld stop` with it, and a typo in a desktop-only convenience field has no business doing that. Ids are veld's own rather than Electron's in one place on purpose: Electron reports camera and microphone as a single `media` permission, and a per-site panel has to show the two switches every browser shows. |
| Emulated media features | **`Emulation.setEmulatedMedia` over the same CDP session touch uses**, and *both* now driven by one applier | `prefers-color-scheme`, `prefers-reduced-motion` and `forced-colors` are the same question a device width asks, put to a media feature — and the one part of emulation Electron exposes no API for. Doing all three together costs the same call. The load-bearing part is not the CDP command but the ownership: touch previously *detached* the debugger when it was turned off, which would have silently dropped a media override that was still meant to be in force. So the attach is driven by whether anything wants the session and the detach only happens when nothing does, and the pane reports `mediaActive` beside `touchActive` — the same honesty about a session something else can take. No reload, unlike the user agent: a media feature is a live media query and Chromium re-evaluates it, which is why this reads as the page's theme flipping rather than the page being reloaded into another one. The control is worded as being about *the page*, since the app themes itself light/dark too. |
| Browser pane lifetime | Re-created on reload, unlike a terminal | A page is re-creatable state: the URL is persisted in the layout and re-navigated to, so a reload is allowed to drop the views and rebuild them. The page asks for that itself (`reset()`, see the row above) rather than the shell inferring it from a navigation event. A shell is the opposite — see the terminal row above. |
| Icons | One mark, two assets, drawn by a stdlib-only rasteriser (`desktop/scripts/make-icons.py`) | The app icon is the *favicon's* shape (rounded dark tile, white `V`, accent dot) because that is already what veld shows in a browser tab, and the menu-bar icon is `logo.svg`'s mark. The tray asset is a macOS **template** image (`*Template.png`, black + alpha): the OS tints it per menu bar, which is the only way one file stays legible in light *and* dark mode. Shipping the coloured mark instead is a white glyph on a light menu bar. Cost: the accent dot is a shape there, not a colour; the app icon carries the colour. The generator draws the mark analytically — it is a polygon and a circle, since every segment of `logo.svg`'s V is a straight line — so there is nothing to install and the bytes are identical on every machine. Both tools tried first were wrong in the same direction: `qlmanage` (QuickLook) composites thumbnails on an **opaque white background** and pads below its minimum size, which shipped a menu-bar icon that was a white tile with a dark V (a template image is alpha, so an opaque render is a solid blob — and invisible as a bug in any light-background preview), and ImageMagick's SVG renderer is blobby at icon sizes *and* its resize dropped the alpha channel to grayscale. The app icon is inset in a transparent margin, because macOS draws its own shadow into one and a full-bleed tile reads as a bigger, blockier icon than everything beside it in the dock. |
| Packaging | **electron-builder**, macOS + Linux, riding the CLI's release pipeline | The app is packaged in the same workflow run that builds the CLI binaries, before the tag is created, and its artifacts are attached to the same GitHub release. A parallel pipeline would have been less plumbing exactly once and a version-skew source forever. No Windows target: the UI is served by the daemon, so a plain browser already covers that platform and a Windows build would be an installer around a page you can already open. |
| Versioning | **One version, one tag** — the app's `package.json` version is the CLI's `Cargo.toml` version | The shell renders the daemon's UI, so the two halves are one product with one compatibility surface (the preload IPC a newer UI expects from an older shell). Release CI writes the version into `package.json` before packaging and semantic-release commits the same bump, which is exactly what it already does to `Cargo.toml`. The app reports skew (`updatePolicy.js` → `versionSkew`) rather than blocking on it: the fix is `veld update` in one direction and an app update in the other, and neither is something to refuse to start over. |
| Auto-update | electron-updater against the same GitHub releases, **install-in-place only where it can actually work** | The check is uniform; applying it is not. macOS is download-only because Squirrel.Mac verifies that the replacement carries the running app's code signature and there is no Developer ID yet (see the row below) — a downloaded-then-rejected update is worse than no button. A Linux `.deb` is download-only because its files belong to dpkg. The AppImage self-installs, being a single file the running process may swap; `APPIMAGE` in the environment is how it knows it is one. `updateMode()` is one pure function, and the macOS half flips with the `MACOS_SIGNED` constant beside it — a constant rather than a branch to delete, because deleting the darwin case would fall through to a catch-all that returns `"download"` anyway, i.e. a no-op the existing test still passes. Both sides of the constant are tested. **A fourth mode, `"cli"`, outranks all of it on macOS when the veld CLI is present**: the app spawns `veld desktop update --wait-pid <pid> --relaunch` detached and quits, and the CLI — which outlives it — waits for the process to be gone, swaps the bundle and reopens it. That inversion (something outside the app replacing it) is what makes an *unsigned* build updatable at all, since Squirrel.Mac only accepts a replacement carrying the running app's signature; it also delivers the app through curl, which never sets `com.apple.quarantine`, so the install is not subject to Gatekeeper's first-launch check either. It stays the preferred route after signing lands, because it is the one that keeps the app and the CLI on a single version — which is what "one tag, one version" already promises. Three things make that handoff survivable rather than merely clever, and each exists because its absence is silent: the app passes **`--app-path process.execPath`**, so the bundle replaced is the one the user launched rather than whatever is in `/Applications`; the CLI runs the installer with **`VELD_DESKTOP_ONLY=1`**, so an app update cannot reach the code that swaps binaries, restarts the daemon or asks for sudo; and because the app is *gone* while this happens, the installer **relaunches it on every exit path, not only success**, restores the bundle it moved aside if it is interrupted mid-swap, and writes its output to `~/.veld/desktop-update.log` with the outcome in `~/.veld/desktop-update.json` — which the app reads on the way back up (`reportPreviousCliUpdate`). Without that last part every failure mode of the script, from a 404 to a checksum mismatch, arrived as an app that vanished and came back older with nothing said. Every download is offered in a dialog first, including where it could be silent: applying an update restarts the app, and a dock full of terminal panes is not something to interrupt unannounced. **The `"cli"` route now spawns `veld update`, not `veld desktop update`, and the `VELD_DESKTOP_ONLY=1` sentence above is what it deliberately gives up**: an app that quits to be replaced is already paying the restart, so paying it for the app alone and then having the skew notice send the user to a terminal for the other half was two interruptions where one would do. `veld update` orchestrates the wait and the relaunch itself in Rust — `install.sh` needed no change for this — and the app's stdio is redirected into `~/.veld/desktop-update.log` at spawn, so the CLI's own progress and both installer runs land in one file in order. Which of the two commands is spawned is decided by **capability, not version**: `veld desktop status --json` carries a `capabilities` array, `full-update-handoff` selects the new route, and anything unparseable, absent or malformed selects the old one — the failure being avoided is spawning a flag at a CLI that exits 2 with the app already gone. `handoffCommand()` is that decision as one pure function, and the fallback's `--version` is load-bearing where the new route must *not* have it (an older CLI told to install "its own" version relaunches into being offered the newer one forever; `veld update` resolves the release itself). **The capability is also withheld by a new CLI living under `/usr/local`**, which is what makes it a capability rather than a version floor: install.sh refuses to relocate a system install, so it needs `sudo -n`, and a detached child has no terminal to prompt on — so that machine keeps the app-only route it already had instead of quitting into an update that cannot finish. Two things this route had to be taught that the app-only one never faced: `run_install_script` **appends the running binary's own directory to PATH**, because install.sh picks its install directory with `command -v veld` and the app's deliberate `SAFE_PATH` cannot find one — under which a `/opt/homebrew/bin` machine silently grew a second CLI in `~/.local/bin`; and the app hands its log descriptors to the child **only on the full route**, since `veld desktop update` opens the same file itself and two open descriptions on one path interleave into nonsense. |
| Linux artifact naming | `executableName: veld-desktop`, set explicitly | electron-builder derives the Linux executable name from the npm package name, and `@veld/desktop` becomes `@velddesktop`, which its own path validation then rejects — *every* Linux build fails, which (because `release` needs the desktop job) takes the CLI's release with it. The scoped name stays: it is what keeps this npm project unpublishable by accident. Caught by CI's Linux packaging job, not by a local macOS build — which is the argument for that job existing. |
| macOS signing | **Ad-hoc signed** (`scripts/adhoc-sign.js`), not unsigned, until a Developer ID exists | An ad-hoc signature is not a trust signal and Gatekeeper still quarantines the download. What it buys is launchability: on Apple Silicon every executable needs *some* valid signature, repacking Electron invalidates the one the prebuilt binaries shipped with, and the failure mode is "Veld is damaged and can't be opened" — which reads as a corrupt download and has no in-UI way out, unlike the "unidentified developer" prompt an ad-hoc build gets. electron-builder's own signing is explicitly disabled (`identity: null`) rather than left to auto-discovery, so the artifacts do not depend on which certificates happen to be in the building machine's keychain. |
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
  the browser-build header row. In native full screen macOS moves the traffic
  lights out of the content area, so the shell mirrors that state onto
  `<body data-fullscreen>` (preload `window.fullScreen` + `onFullScreen`,
  applied by `watchFullScreen` in `shell.ts`) and the bar drops the inset. No
  CSS can detect it: `:fullscreen` is the element API and
  `display-mode: fullscreen` never matches an Electron window.
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
- **Creating a worktree happens in a lane, not beside the rail.** There is one
  "＋" per rail section that can hold a checkout (the ungrouped section and each
  user lane), and the toolbar's global "+" is gone in the expanded rail. The old
  single button could only ever mean *ungrouped*, so filing a new checkout was
  always create-then-drag; a per-section button makes the destination the thing
  you clicked, and the lane rides on the create request rather than a follow-up
  PATCH so the row cannot appear in the wrong section first.
  - The ungrouped section therefore **gained a header** ("Worktrees"), since a
    headerless section has nowhere to put a button and the default destination
    would otherwise have been the only unreachable one. "Worktrees" and not
    "Ungrouped" because most repos define no lane at all and that header would
    name a state they never left.
  - The **collapsed** rail keeps the toolbar "+", mirroring how "New lane" is
    expanded-only: collapsed there are no headers, so it is the only place a
    create can live. It files into the ungrouped section.
  - `RailGroup` carries `addable` and `editable` as explicit flags rather than
    the view deriving them from `pinned`. They are three different claims: the
    trash takes drops but cannot be created into, and the ungrouped section is
    unpinned and creatable but has no lane record to rename or delete.
  - The per-lane **row count was removed** from the header. It restated what the
    rows immediately below it already show, in a surface whose entire job is
    showing those rows, and its slot is where the "＋" now sits.
  - A checkout created from a header lands at the **top** of that section, not
    the end. Unplaced rows sort last (`WT_ORDER`), so the new one appeared
    furthest from both the button that made it and the work about to happen in
    it. The client writes the order after the create with `POST
    /api/worktree-order`, filtered to the rows that were **already hand-placed**
    plus the new one. Sending the full order — what a drag sends — would give a
    `sort_position` to every unplaced row in the repo, so one click of any "＋"
    would silently freeze the label sort everywhere and break the promise that
    what you have not placed stays alphabetical. The daemon clears the positions
    it is not given, so the omitted rows keep exactly the state they had. A
    create that succeeds and a placement that fails is reported, never thrown:
    the checkout exists and only its position is wrong.
- **A whole lane is dragged by its header, and the drop is resolved from the
  pointer.** The header is the handle (its ＋ and ⋮ drag it too, harmlessly —
  they act on click), and the lane's ⋮ menu keeps *Move lane up* / *Move lane
  down* as the keyboard path. Dropping is **displacement**: anywhere on a lane
  means "take that lane's place", and `moveLane` says that with two lane
  **names** — never an index. Which coordinate system an index was in (a final
  position or an insertion point) is the thing that took three attempts to get
  right, with the row drag next door handing out the other kind as the obvious
  template; naming the target deletes the category, and both gestures share the
  one write.
  - **The scrollable list is the drop zone, not the sections.** Per-element
    hit testing made the gesture one-directional and shipped broken twice: the
    9px gutters, the list's padding and everything below the last lane belonged
    to no section at all, so "pull it to the bottom and let go" landed on
    nothing. `laneDropTarget` (model.ts, tested) maps the pointer's Y onto the
    sections' bottom edges instead: above the first lane is the first, below the
    last is the last, a gutter belongs to the lane under it. The DOM read stays
    in the component; the choice is a pure function because it is the part that
    kept being wrong. What this does *not* remove is the travel a downward move
    costs — the dragged lane keeps its place and height while carried, so the
    pointer must clear its own section before the lane below is the answer. That
    is the price of not reflowing the rail under a pointer aiming at it.
  - **The bottom dock is a second drop zone, and it hard-codes the last lane.**
    It is the natural overshoot for "pull this lane to the bottom", so refusing
    there made the last position the one place the gesture could miss — but it
    must not reuse the list's geometry: `getBoundingClientRect` is layout and is
    not clipped by the scroller, so with the rail scrolled up a section below the
    fold has a bottom below the dock's own Y and the drop would land mid-rail.
    The dock means "the bottom" and says so directly. Its whole area answers
    that way, the Trash header included.
  - Lane positions are keyed on `RailGroup.lane` behind `editable`, **never on
    `key`**: the main checkout's key is the literal `"main"` and `"main"` is a
    legal lane name, so keying on it handed that pinned section a real lane's
    position. Same collision the ungrouped header's `aria-label` already guards
    against for `"Worktrees"`.
  - The lane drag and the worktree row drag are separate states with mutually
    exclusive handlers — each drop zone answers only to its own drag — rather than
    one drag model with a discriminant, because they resolve differently: a row
    has meaningful halves, a lane is a block.
- **One start predicate.** `canStartWorktree` gates all four surfaces that can
  fire a run action — top bar, rail row, context menu and palette. They
  disagreed before: some checked "is anything already in flight", others "is
  there anything to start", so one surface offered an enabled control whose
  click was a silent no-op while another allowed a double-spawned
  `veld start`.
  - **Starting a *second* environment is a different question, with its own
    predicate.** ▶/■ is a toggle, so `canStartWorktree` can only ever answer for
    an idle worktree — which left "two runs in one directory", a state the daemon
    has always supported and a coding agent produces routinely, reachable from the
    CLI alone. `canStartAnother` gates the explicit *Start another run* entries
    (run selector, palette, rail context menu). It differs in two ways that are
    the whole point: it requires something to be **live** (idle, ▶ *is* the start
    affordance and re-runs the environment on screen), and it gates on
    `pendingForRun` rather than `pendingFor`, because starting one environment
    while another is mid-transition is exactly the case it exists for. The two
    also compute **different names** — `startRunName` re-runs a bound ended
    environment under its own name; `freshRunName` avoids every name the worktree
    has ever used, since "another run named `dev`" is false while a stopped `dev`
    is in the list.
- **One state channel per row, and the run control is it.** A rail row carries
  two dots' worth of meaning — *which worktree is this* and *what is its run
  doing* — and it used to draw them as two adjacent circles. Colour markers
  (#204) made that unreadable rather than causing it: with an emoji the two were
  distinguishable by shape, and with a colour swatch they were not. So run state
  lives on the **run control** (▶ / ■ / spinner) and the row has no status dot.
  The rule this follows is worth keeping: **an identity channel never carries
  state.** Tinting the marker on failure was the obvious alternative and is
  rejected for that reason — the marker's colour *is* the identifier.
  - Failure gets an affordance rather than a colour: `.wt-alert`, an icon whose
    click selects the worktree and reveals its Nodes pane (`revealDiagPane`).
    It renders in the **collapsed** rail too, where the run control cannot go, so
    it is sized to its glyph rather than to a 17px control box.
  - `recovering` routes there as well, and is the reason `worktreeStatus` no
    longer folds it into `partial`: the health monitor restarting a node that
    keeps failing its probe has no expected end, so a spinner read as
    "perpetually starting". Note that issue #214 is **wrong** about the prior
    behaviour, and so was the first draft of this section — folded into `partial`
    it rendered `.dot.partial`, a static amber dot *identical to an ordinary
    starting/stopping row*. The defect was therefore not an absent signal but an
    unbounded restart loop that was indistinguishable from progress, which is the
    worse of the two: a wrong signal gets acted on.
  - **Known and deliberate:** the collapsed rail therefore has no *running*
    signal, only an attention one. It carries the run's status in the row's
    `title` instead. The collapsed mode already drops the alias and the branch;
    what it must not drop is anything asking to be acted on.
  - The ⌘K worktree rows had the identical two-dot collision and have no run
    control to move state onto, so they spell the state out in the hint
    (`PALETTE_STATUS`) — and only for `failed`/`recovering`, since ⌘K is how you
    *go* somewhere and the rail is on screen while it is open.
- **Pending markers** (`prunePending`, `crates/veld-daemon/ui/src/model.ts`)
  are optimistic flags keyed by **worktree *and* environment name**
  (`pendingKey`), cleared when *that environment's* run signature
  (`status:run_id`, via `runSignatureFor`) moves — status alone is not enough,
  because `veld restart` returns to `running` and would never register, and one
  slot per worktree was not enough either: a directory can hold two live runs, so
  stopping one while the other started overwrote a marker and stranded its
  spinner. A 60s TTL bounds an action
  that 202s and then never lands. They are a **latency optimisation, not the
  source of truth**: every run control spins on `spinnerAction(pending, run)` =
  `pending ?? transitionAction(run)`, so a run started from the CLI, from another
  window, or already coming up when the window opened spins too. It did not
  before, and that was survivable only while the dot covered those cases —
  deleting the dot without this would have shown ▶ on a run that was starting.
  - **One function, because two surfaces derived it separately and disagreed.**
    The rail row got the observed-transition fallback first and the top bar's
    play/stop did not, so the same worktree spun in the rail and showed a static
    glyph in the bar for the whole of an externally-started transition. Any new
    run control uses `spinnerAction`.
  - `pending` stays a *separate* prop from `spinner`, and only it may disable:
    a spinner is a state display, and a run some other surface started is still
    legitimately stoppable while it comes up. Only an action this window fired
    and has not seen land locks the control. It also keys the restart button,
    since `transitionAction` cannot know a stop-then-start was one action.
  - The two halves must keep agreeing about which statuses are in transition,
    which `model.test.ts` pins as `partial` ⇔ `transitionAction() !== null`.
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
- **A shell outliving the daemon is only useful if the tabs naming it do too.**
  The PTY belongs to a holder process (see the decision log), so a daemon restart
  leaves the shells running — but reachable only if the layout holding their
  session ids survived as well. A main window's layout therefore lives in the
  daemon's database (`pane_layouts`, migration v15), which survives the app, the
  browser, and the daemon alike; a *detached* window's lives in a slot-keyed
  durable store beside `sessionStorage` (`layoutSlotKey` in `panes/model.ts`,
  `layoutSlot` in `shell.ts`). Two clients must never restore one layout: an
  attach *takes over*, so they would ping-pong every shell between them — which
  is what the daemon's claim registry prevents, and why the slot (for the windows
  that still use one) comes from the shell instead of being assumed.
- **Which shells were *expected* to still be running is a client-side fact, and
  it now travels with the layout.** A terminal that cannot reattach has to say
  so rather than quietly opening a fresh prompt, which needs the answer to "did I
  think this one was alive?" — `noteExpectedResumes` in `panes/terminalHost.ts`,
  fed by every layout the daemon hands over. It used to be read once at module
  load from this page's own storage, which is precisely why a browser tab said
  nothing: it had no storage to read, so every one of the app's live terminals
  looked brand new to it.
- **A worktree's default layout starts no shell.** `defaultLayout` seeds a `new`
  pane rather than a terminal: a terminal tab's id *is* a daemon session id, so
  seeding one made merely selecting a worktree start a real shell — and now a
  holder process with it — against a cap of 48.
- **Removing a tab from a layout is what ends a shell** — not closing it, and not
  intending to. `pruneTerminals` runs off the `[layouts]` effect and disposes any
  session the layouts no longer name, which is the whole collection mechanism.
  Anything that moves a tab *somewhere else* must therefore call
  `releaseTerminal` first: it tears down this page's half of the session (socket,
  xterm, element) without the `DELETE` that hangs up the shell, so by the time
  the prune runs there is nothing left for it to collect. That is the difference
  between detaching a pane and closing one, and it is one function call.
- **`splitWithTab` is the operation `moveTab` cannot express.** With both docks
  on screen, "put this on the right" is a dock index. With one, making a tab the
  *left* pane means everything else becomes the right one, and no index says
  that. Dropping on a pane edge means the former when there are two panes and the
  latter when there is one, so it is one function rather than a branch at the
  call site.
- **Shift+Enter is a key handler, not a preference** (`panes/terminalKeys.ts`).
  A terminal cannot distinguish it from Enter — there is one carriage return on
  the wire and no modifier byte for it — so nothing on the daemon side could
  implement it. The handler sends `ESC CR`, which is what Claude Code's
  `/terminal-setup` configures iTerm2 and VS Code to send, so a coding agent's
  composer reads it as a newline with no setup inside Veld. `preventDefault` is
  load-bearing: without it the browser still delivers the key to xterm's hidden
  textarea and the shell gets a bare `CR` as well — a newline *and* a submit.
  Alt+Enter is left alone, and other modifier combinations pass through, because
  swallowing them would be eating someone else's keybinding.
- **A URL a terminal produces is routed by the daemon, not by the renderer**
  (`veld_core::ide::route_url`, `POST /api/pty/sessions/{id}/open-url`). Two
  entry points reach it — a click on a link in the output
  (`@xterm/addon-web-links`, which is worth the dependency because it stitches a
  URL back together across the rows a terminal wrapped it onto) and `$BROWSER` in
  the session's environment, which a process in the shell invokes and which lands
  in `veld open-url`. Both ask the same question, and half the answer is
  `ide.externalOrigins` in the project's `veld.json`, which the renderer does not
  read: a policy copy on this side would be a second implementation of it.
  The URL is parsed **once**, in the daemon, with `url::Url` — the same standard the
  renderer's `new URL()` and Chromium implement — and the **canonical serialisation**
  is what travels onward. Routing on the caller's spelling and opening something else
  was a real hole rather than untidiness: a hand-rolled parser ended the authority at
  `/?#` only, so `https://accounts.google.com\@evil.com` routed on `evil.com` (not
  exempt → a pane) while a browser loads `accounts.google.com`, and a tab character in
  the host did the same — either one silently sidesteps an `externalOrigins` entry,
  which is the one control the feature offers for SSO hosts.
  Delivery is a new `open_url` control frame on **that session's socket**, so the
  socket *is* the routing decision — the page attached to a session is the window
  whose dock holds that terminal, and no window id has to be invented, stored, or
  kept correct across a detach. Placement then follows `onBrowserOpenRequest`'s
  shape exactly: find the dock holding the tab whose id is the session id, and add
  a browser tab to it.
- **`$BROWSER` is not enough, because the case that matters does not read it.**
  An agent's shell tool runs `open <url>` directly (`Bash(open "https://…")`), and
  Claude Code sets `BROWSER=true` for its children on top of that. So the shim
  directory has to be on `PATH` — and `PATH` set in the spawn environment does not
  survive a login shell, which is measured rather than assumed: macOS
  `/etc/zprofile` runs `path_helper`, which rebuilds `PATH` with the system
  directories first, so a prepended entry lands behind `/usr/bin` and `open` still
  resolves to `/usr/bin/open`; Debian's `/etc/profile` overwrites `PATH` outright.
- **So veld owns exactly one file in a shell's startup, and hands control back
  immediately** (`pty/shims.rs`, `zshenv`). `ZDOTDIR` points at a veld directory
  holding a single `.zshenv`, which (1) restores `ZDOTDIR` to the user's value —
  unsetting it when they had none, since `ZDOTDIR` is conventionally set *in*
  `~/.zshenv` — (2) sources the user's `.zshenv`, whose place in the order it took,
  and (3) registers a `precmd` hook that prepends the shim directory. Everything
  after that stage is the user's own file, read in the normal order, with their own
  `$ZDOTDIR` visible to it. Nothing of the user's is edited or wrapped.
  **A hook, not an assignment**, and that is the whole point: an assignment in
  `.zshenv` is what `path_helper` undoes two steps later, while `precmd` runs
  before the first prompt — after every rc file. It stays registered and is
  idempotent, so a later `PATH` rebuild (a venv, a version manager) cannot silently
  drop the shim. Pinned by a test that drives a real `zsh -l -i` whose `.zshrc`
  rebuilds `PATH` from scratch; note that the test has to feed **stdin** rather than
  use `-c`, because `zsh -i -c` prints no prompt and therefore runs no `precmd` —
  a `-c` version of that test passes while the mechanism does nothing.
- **zsh only, and the reason is structural**: it is the one shell with a startup
  file that runs *before* `$ZDOTDIR` matters and a hook array that runs *after*
  every rc file. bash has no env-only interactive hook (`BASH_ENV` is
  non-interactive shells only) and reaching one through `--rcfile` means veld
  reimplementing the user's login-startup order. bash, fish and the rest get
  `$BROWSER` plus the documented `$VELD_SHIM_DIR` line.
  `terminal.interceptSystemOpen` turns the whole thing off, because anything that
  runs inside a shell's startup gets a switch; it is read at **ticket** time, where
  the database is already open, so nothing puts a `Db::open()` on the
  session-spawn path.
- **A shim may shadow a real tool; it may never invent one.** `xdg-open` does not
  exist on macOS, and generating a shim for it put a command on `PATH` whose only
  answer is "no system opener" — so the portable idiom
  `command -v xdg-open >/dev/null && xdg-open "$f"` stopped finding nothing and
  started finding something broken, for every file type rather than just URLs. A shim
  is written only where `real_opener` finds something behind it, and a stale one is
  removed when the tool disappears. `veld-open` is the exception: `$BROWSER` is a
  variable veld sets itself, so it shadows no name.
- **The `$BROWSER` half is re-asserted once, after the rc files.** A user whose
  `.zshrc` exports its own `BROWSER` would otherwise switch that half off silently;
  the `veld_browser` hook takes it back on the first prompt and keeps their value in
  `VELD_BROWSER_ORIGINAL` so a fall-through opens the browser they chose. **Once**,
  not every prompt: an rc file is startup, but `export BROWSER=lynx` typed at a prompt
  is a deliberate act and veld does not argue with it.
- **`veld doctor` reports it**, because the failure mode is silence: the shims are
  written once at daemon start and a `OnceLock` means a failure is never retried, so a
  daemon with no sibling `veld` (a moved install, an interrupted update) leaves the
  feature off with only one line in a log. The row checks the script exists *and* that
  the CLI path baked into it is still there.
- **Every branch that is not "one http(s) URL, in a Veld terminal, that the daemon
  routed" ends in `exec`ing the real tool with the original argv** — `open .`,
  `open report.pdf` and `open -a Safari …` must behave exactly as they did before
  veld was in the picture, and a wrapper around a command people use dozens of
  times a day has no other acceptable failure mode.
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
- **The logs viewer decodes ANSI, and the terminal's palette is the one it uses.**
  A log line is written for a terminal — the CLI colours its own output and dev
  servers colour far more — so `shared/ansi.ts` turns SGR into styled spans, drops
  every other escape sequence (there is no cursor to move in a line), and treats a
  carriage return as the line starting over, because otherwise a 40-step progress
  bar renders as all forty frames at once. Three consequences worth keeping: the
  16-colour palette **moved out of `terminalHost.ts`** into that module and is now
  shared, since the same output coloured one way in a shell and another in the logs
  reads as a bug in whichever you saw second; `stripAnsi` is *defined* as the spans
  joined, so search can never match text no span contains; and search runs over the
  **joined** text rather than per span, because `ERROR` bold followed by a plain
  message is the commonest colouring there is and a per-span search cannot see a
  word that straddles the boundary. Searching the raw line was the previous
  behaviour and silently missed any word with a colour change inside it.
- **A tab strip is one keyboard stop, and its arrows do not select.** Tabs carry
  `role="tab"` inside a `role="tablist"` scroller with a roving `tabIndex`, so Tab
  reaches the strip once and `←`/`→`/`Home`/`End` move within it; `Delete` closes,
  which is why the close button is deliberately *not* a tab stop (with both docks
  full, tabbing through every close button on the way to the content is worse).
  **Manual activation** is the load-bearing half: the ARIA pattern's other variant
  selects as focus moves, which here would mount a `WebContentsView` for every
  browser pane walked past and replace what you were looking at on the way. Only
  the selected tab carries `aria-controls`, because a dock has one panel and it
  shows the active tab — pointing an unselected tab at it would send a screen
  reader to another tab's content. The key-to-index arithmetic is
  `panes/tabKeys.ts`, pure and tested; the DOM half reads the tablist's own
  children, which already hold what a threaded index would have to reproduce.
- **A browser pane refuses Veld's own UI.** `/ide` inside `/ide` is the first thing
  anyone tries, and the reason to catch it is not the joke: a nested instance is a
  second complete copy of this app against the *same* daemon — its own pane
  registry, its own PTY session ids spending the 48-session cap, and the shared
  worktree layout store and the shell's claim map written from a place no window
  knows about. `isVeldOwnUi` matches `/` or `/ide` on **this document's origin or
  `veld.localhost`**, deliberately not on any loopback host: previewing a dev server
  on `localhost:3000` is the pane's whole job, and a project with its own `/ide`
  route is not far-fetched. The refusal is `nested` on the pane's state rather than
  an `error` (nothing failed) and it is read by `paneCovers`, which stays the single
  decision about whether a view is hidden. No `WebContentsView` is created while refused —
  creating the thing being refused in order to keep it hidden would mint the very
  sessions this prevents. (The browser build does create its `<iframe>` element,
  because a forced navigation assigns `src` to it, but leaves that `src` unset, so
  it loads nothing either way.) and the screen offers both ways out: the system browser,
  and loading it here anyway, which addresses the *refused* URL rather than whatever
  the address bar currently holds.
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
- **The renderer computes `scale`; the shell only clamps it.** Fitting a 1920-wide
  viewport into a 600px pane is the entire argument for doing this inside the dock,
  and the arithmetic lives in `deviceLayout` (`panes/devices.ts`) with the rest of
  the placement — where the screen sits, how far it is scaled and what radius that
  leaves are one calculation, and they are pushed together with the bounds. The shell
  applies what it is handed (`safeScale`, `safeRadius`) and deliberately re-derives
  nothing: an earlier version had the shell compute the factor from the box it was
  given, which is one number with two owners and a half-off-screen device waiting to
  happen. Both dimensions bind: the emulated screen *is* the view, so a viewport
  scaled to the width but taller than the box is clipped with nothing to scroll it
  into sight. Note the one thing the renderer cannot see — a native view's bounds are
  device-independent pixels while the renderer measures CSS pixels — which is why the
  *bounds* are converted in the shell (`rect * getZoomFactor()`) even though the
  factor is not.
- **The mobile user agent is the string only.** `setUserAgent` takes a string and
  Electron exposes no metadata argument, so `navigator.userAgentData` and the
  `Sec-CH-UA*` request headers keep reporting the host desktop while
  `navigator.userAgent` claims a phone — a stack that branches on client hints still
  serves its desktop bundle. The pane's device menu says so, in the same spirit as the
  iframe backend's gaps. Closing it properly means `Emulation.setUserAgentOverride`
  with `userAgentMetadata` over CDP, which would put the user agent behind a debugger
  attach that DevTools can take away; that is a trade worth its own increment rather
  than a quiet half-fix.
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
- **The quick switches are reach, not capability — so they are a preference.**
  The responsive viewport and the page's colour scheme are the two things people
  change dozens of times an hour while working on a layout, and both were three
  levels deep in the device menu. They are now one click each in the pane's
  chrome, beside the device button, and the menu keeps every control it had: a
  switch turned off costs a shortcut and nothing else. Which of them appears is
  `browser.quickSwitch.responsive` / `browser.quickSwitch.colorScheme` in the
  settings store, and both default **on**, since a control defaulted off is a
  control nobody finds and reach was the whole point. The keys say whether the
  *switch is shown*; the emulation itself stays per pane in the layout. They live
  in the settings store rather than the layout for the same reason the font size
  does: this is a preference about the app, not state belonging to one pane.
  **Which also means it is not the answer to the 300px pane**, and should not be
  read as one: the document is global, so a switch turned off is off in every pane,
  every window and every browser tab, while bar width is per pane and changes on
  every split. Hiding the switches below a *measured* bar width would answer that
  case directly and would need no key at all — it is the better answer to that
  specific problem, and deliberately not what this is. What the preference actually
  answers is "do I want this shortcut on my toolbar at all".
- **The colour scheme is a three-state cycle, not a dark toggle.**
  System → Dark → Light → System (`nextColorScheme` in `panes/devices.ts`, pure
  and tested). The first version toggled dark against System, which is the shape
  the roadmap described, and it was wrong for one reason: a light-only layout bug
  is as ordinary as a dark one, so half the feature's audience was still going
  three levels into the menu. **System stays the absence of an override** rather
  than becoming a third stored value — it is what `withMediaFeature(…, null)`
  produces, and therefore what lets the CDP session be released, so it has to be
  reachable *by cycling* and not only from the menu. An unrecognised scheme (one a
  newer build wrote) returns System, which is the same answer `light` gets — that is
  the function being total, **not** a guard against a reachable state:
  `sanitizeMedia` already drops any scheme outside `MEDIA_FEATURES` on every layout
  load, so the real defence is upstream and this is not licence to weaken it.
- **`data-disabled`, not `disabled`, on the inert colour-scheme switch.** A real
  `<button disabled>` dispatches no pointer events, and Mantine puts a Tooltip's
  hover handlers on the child element itself while adding no `pointer-events` rule of
  its own — so a disabled button's tooltip never opens, and the browser build would
  get a grey button with its one explanation unreachable. That is precisely the
  "control that silently does nothing" this feature was told to avoid. `data-disabled`
  drives the styling through `mod` and leaves the element hoverable, which moves the
  refusal into the click handler and the semantics into `aria-disabled`. The device
  menu never hit this because it states its gaps in a `Menu.Label`, which renders
  regardless of any item's state. **The same defect still exists on the chrome's
  Back/Forward buttons** ("History needs the desktop app" is unreachable in the
  iframe backend) — pre-existing, out of that diff's scope, worth fixing next.
- **Sun and moon, but not the top bar's System glyph.** The lit states reuse the
  app's own theme icons because they answer the same question, and the tooltip says
  *the page's* colour scheme because Veld themes itself too. System deliberately
  does **not** reuse `IconDeviceDesktop`: one button away sits the device picker,
  and two monitor shapes side by side read as two device controls.
- **A switch's off is one definite state, never menu history.** Responsive's off is
  **no emulation at all**, deliberately not the device that was selected before: the
  switch then answers exactly one question ("am I in the resizable viewport"), and a
  lit switch means the same thing every time. Restoring a previous device would have
  made the control's meaning depend on menu history that nothing on screen shows.
  The cost, stated because it is not obvious: a *dragged* preset or a hand-entered
  size is `custom`, not `responsive` (`resizeEmulation`), so the switch reads off
  over a viewport that already has draggable edges, and clicking it replaces that
  size with a pane-measured one that the layout cannot undo. An off-looking toggle
  reads as costless, so the tooltip names what the click will replace rather than
  leaving it to be discovered.
- **They report `mediaActive`, not what was asked for.** The colour-scheme switch
  shows as *paused* rather than as set while Chromium's debugger is held elsewhere,
  for the same reason `touchActive` exists: a switch claiming Dark over a page that
  is still light is the exact lie those flags were added to prevent. Responsive has
  no equivalent — nothing can take a viewport size away — so it reads straight off
  `emulation.device`, the same source the menu's own checkmark uses. Entering the
  state is `enterResponsive`, shared with the menu item, so the "measured from the
  pane's own box, skipped when there is no box" rule has one owner.
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

1. Own the app's **windows** (`src/windows.js`, with the pure half — slot naming,
   the persisted window set, suffix allocation — in `src/windowState.js` so
   `node --test` can reach it). A `main` window is frameless
   (`titleBarStyle: 'hiddenInset'`) and loads
   `${VELD_DESKTOP_URL ?? http://127.0.0.1:19899}/ide?shell=electron`; it is
   titled by the app and `page-title-updated` is cancelled — the UI arrives over
   HTTP, so otherwise the window takes whatever `<title>` that bundle carries, and
   a reload could rename it. `app.setName("Veld")` for the same reason: an
   unpackaged run would call itself "Electron" in the macOS application menu.
   A `detached` window is the opposite on both counts: it keeps a **normal frame**
   (a bare dock has no title-bar row of its own to drag by) and the page owns its
   title, because with one dock in it the active tab is the most useful thing a
   title bar could say. Both carry `--veld-layout-slot`, `--veld-window-kind` and
   `--veld-window-restored` in argv (short, fixed-charset, not secret); the seed
   is deliberately **not** there. The window set is persisted to
   `userData/windows.json` (written to a temp file and renamed, since a torn file
   reopens as one window and the next persist makes that permanent) and reopened
   on launch, because a detached window holds live shells and reopening only the
   main one abandons them to the detach grace.
   `before-quit` stops both the hand-back and the persist: a quit is not a series
   of window closes, and treating it as one would record the app as having one
   window and reopen exactly that. That latch is **one-way**, cleared only by
   callers that know the app survived — opening a window, `activate`, and the
   updater's `onQuitCancelled`. Two versions tried to infer it from
   `browser-window-focus` and were wrong in opposite directions, the second
   only by assuming an Electron event ordering nobody here can check.
2. If the daemon isn't reachable, show a local retry page (embedded data URL —
   install/start instructions) and poll until it appears.
3. macOS tray (template icon): shows running-run count, per-run stop/restart
   later; click focuses the window.
4. `contextIsolation: true`, `nodeIntegration: false`, preload exposing
   `veldDesktop.shell` metadata, `veldDesktop.window` (open, detach, snapshot,
   title, close, adopt) and `veldDesktop.browser` — the embedded browser panes
   (`src/browserViews.js`) — which are the two things a page cannot do for
   itself. Every method is a fixed channel with a fixed shape: the page
   never names a channel, so it cannot reach a handler the preload doesn't list.
   Both handler sets resolve their target window from `event.sender` and require
   the main frame, so an iframe inside a pane can reach none of them and a
   renderer acts on its own window — with **two deliberate exceptions, both
   because the state behind them is process-wide, not per window**:
   `veld:browser:clear-session` clears a `persist:` partition, which every
   window's panes share, so it reloads all of them (scoping only the *repair* to
   the sender left another window rendering a signed-in page whose jar was
   already gone), and `veld:window:detach` opens a new window by definition.
   `veld:window:seed` is resolved from `event.sender` without the frame check,
   because it is answered during preload where `senderFrame` is not yet
   populated; nothing else can reach it, since Electron runs a preload in the
   main frame only and the embedded panes have no preload at all.
5. Own the browser panes' lifetime: views are keyed by (window, view id), so a
   renderer can only address its own. They are disposed when the window closes,
   and otherwise only when the page asks — it calls `reset()` as it boots, before
   creating any, because a view outliving its renderer's registry paints over the
   new page with nothing able to close it. Disposing from a navigation event in
   this process instead is a race against the renderer's first `create`, and
   losing it destroys the view the new page just asked for.
   Views run sandboxed with no preload, in a `persist:veld-browser-<profile>`
   partition, with only `http(s)` accepted and every permission answered by the
   policy in `src/permissions.js` (see the *Browser pane permissions* row in the
   decision log).
6. Own the update story (`src/updater.js`, `src/updatePolicy.js`) — see
   *Packaging and updates* below.
7. An application menu and a single-instance lock, both of which only matter once
   there is a bundle: Electron's default menu has nowhere to put *Check for
   Updates…* (and on Linux there is no tray to put it in instead), and a second
   launch of an installed app would otherwise open a window that fights the first
   one over the same daemon, tray and browser partitions. The lock is taken only
   when packaged — two *dev* instances are a normal thing to want, and they would
   share one lock because they share one `appId`.

## Packaging and updates

Config: `desktop/electron-builder.yml`. Local build: `just desktop-package`
(output in `desktop/dist/`, and `desktop/dist-deb/` on Linux — both gitignored).

Linux takes **two electron-builder invocations**, into separate output
directories. `FpmTarget` writes a `package-type` file naming its format into
`<out>/linux-unpacked/resources`, which is the same directory the AppImage packs
from, and the two targets run concurrently — so a single invocation can produce
an AppImage that claims to be a .deb. electron-updater dispatches on exactly that
file, so the one self-updating Linux build would download a .deb and run
`dpkg -i` while never replacing itself. CI asserts the marker is absent.

The .deb is then **moved into `dist/`**, so everything published comes from one
directory (`upload-artifact` roots an artifact at the least common ancestor of
its patterns, and a second directory silently nests every file a level deeper).
What stays behind in `dist-deb/` is that invocation's own `latest-linux.yml`,
describing the .deb — publishing it would overwrite the AppImage's feed and point
the one self-updating Linux build at a package it cannot install. Only `dist/`'s
feed is ever uploaded, and both workflows assert it exists.

### Artifacts

Named `veld-desktop-<version>-<os>-<arch>.<ext>` (`artifactName` in
`electron-builder.yml`). A release is one flat list of assets, and the CLI's are
`veld-<version>-<os>-<arch>.tar.gz` — the previous `Veld-` prefix sorted itself
away from them while still reading as "the veld download", so the two were told
apart by file extension alone. Nothing else has to agree with this string: the
release workflow's globs match on extension, `checksums.txt` is generated from
whatever is there, and the update feed beside the file is written from this same
pattern.

| Platform | Targets | Self-updates? |
|---|---|---|
| macOS arm64 + x64 | `.dmg` (install) and `.zip` (the update payload Squirrel.Mac reads) | No — unsigned; the app opens the release page |
| Linux x64 | `.AppImage` | Yes, in place |
| Linux x64 | `.deb` | No — the files belong to dpkg |

Alongside them, `latest-mac.yml` / `latest-linux.yml` (electron-updater's feed)
and the `.blockmap` files it uses to download a delta rather than 120 MB. The
feed is only written when a publish provider is configured, which is why
`electron-builder.yml` has a `publish:` block even though CI always packages with
`--publish never` and attaches the files through the release workflow's own
`publish` job. That same block is baked into the bundle as `app-update.yml`,
which is how an installed app knows where to look.

### Version flow

The app and the CLI are one release. `.github/workflows/release.yml`:

1. `plan` — semantic-release dry run computes the next version.
2. `build` (CLI binaries) and `desktop` (the app) both write that version into
   their manifest and build. This is *before* the tag exists, so the checkout is
   still on the previous version — the same reason the CLI job seds `Cargo.toml`.
3. `release` — needs **both**, then tags. A broken app build blocks the tag
   rather than producing a release the app is missing from.
4. `publish` — attaches everything to the GitHub release and checksums it.

`.releaserc.json` then commits the bump to `desktop/package.json` (and its lock)
beside `Cargo.toml`, so `main` reflects what shipped.

CI verifies packaging on every PR (`desktop-package` in `ci.yml`) rather than
finding out at release time: Linux builds the real installers, macOS builds
`--dir` and asserts the ad-hoc signature verifies. macOS stops short of the
dmg/zip because most PRs here never touch `desktop/` and that runner is billed at
10×; the full macOS build runs at release.

### Installing an unsigned macOS build

Gatekeeper quarantines it. First launch: let the warning appear, then **System
Settings → Privacy & Security → *Open Anyway***; `xattr -dr
com.apple.quarantine /Applications/Veld.app` is the scriptable equivalent.
Right-click → *Open* is the instruction everyone remembers and it is no longer
true — macOS 15 removed that bypass for apps that aren't notarized. This
goes away with a Developer ID + notarization (issue #167 §10), which is also what
turns macOS self-updates on.

### What the app tells the user

Two different mismatches, deliberately reported differently:

- **A newer release exists.** Checked 15 s after launch and every 6 h, silent
  unless it finds something, never re-prompting for a version already declined in
  this session. A user-initiated *Check for Updates…* (tray on macOS, application
  menu everywhere) reports every outcome including "up to date".

  On the `"cli"` route the prompt is about the **release**, not the app —
  *"veld 16.8.0 is available"*, *"Quit and Update veld"* — because that is what
  the button does: `veld update`, both halves, one restart. The wording is
  derived, not written twice: `primaryAction()` in `updatePolicy.js` maps the
  mode to the label, so a route that can only move the app can only say so. That
  matters more than it sounds, because the two-step shape it replaced (update the
  app, then be told by the skew notice to go and run `veld update`) is exactly the
  thing users read as the app nagging about someone else's release.
- **The app and the daemon disagree.** A notification once per session, plus a
  row in the application menu (and in the tray, which is macOS-only — Linux is
  the platform whose app can update itself and so the likelier one to drift, so
  the row cannot live only there). `/api/health` carries the daemon's version and the
  shell already polls it, so this costs one field; it is polled on a minute so a
  `veld update` performed while the app is open both raises and clears the
  notice.

The app is a shell around a daemon it does not ship, so its waiting screen spells
out both commands — the installer *and* `veld setup unprivileged` — rather than
saying "install veld". For a packaged download on a machine that has never had
it, that screen is the whole first impression, and the installer deliberately
does not run setup, which is the step that actually installs the daemon agent
the screen is waiting for. `veld doctor` only diagnoses, so it is offered to
someone who is already set up.

## Data model

Desktop **repo** ≠ veld **project**. Veld keys its `projects` table by "any
directory containing veld.json" — so *every worktree with a veld.json is its
own veld project*. The desktop model sits one level above:

- `repos` — a git repository the user imported (its main checkout root).
- `worktrees` — checkouts of that repo (`git worktree`), each with a
  user-editable `alias` and, since v13, a `display_name`. The main checkout
  itself appears as a worktree row so the rail has one list.
- `lanes` — user-named groups the rail renders as sections (v10). Keyed
  `(repo_root, name)` with **no surrogate id**; `worktrees.lane` stores the name.

**A worktree has two names and they are not interchangeable.** `alias` is the
*identifier*: bounded to `[A-Za-z0-9._-]`, unique among the repo's checkouts, and
the default run name — which is what puts it inside the hostname
`{service}.{run}.{project}.localhost`. `display_name` (v13) is the *label*: free
text, not unique, and what every surface renders — the rail row, the window title,
menu items, the tray. `worktreeLabel(w)` in `ui/src/shared/worktreeName.ts` is the
one place the fallback (`display_name || alias`) is spelled, and the Electron main
process repeats the rule by hand in `worktreeMarks()` because it shares no code
with the React app. Anything that is a *key* — the run name, the collision check,
an API argument — keeps using the alias directly.

The two exist separately rather than the alias simply being relaxed, because
their rules genuinely differ: one has to survive DNS, the other only has to be
readable in a 236px column. Deriving the label back out of the slug was the
alternative and is not possible — the derivation drops capitals, punctuation and
non-ASCII, so `Hello test` and `hello-test` are indistinguishable by then. `''` is
the "no separate name" sentinel (a NULL would make every reader handle an absence
meaning the same thing), which is the state every pre-v13 row is in and the state
clearing the field returns a row to.

`WT_ORDER`'s final tie-break sorts on the **label**, not the alias, since the
label is the string on screen; the alias follows it so the order stays total when
two rows render the same label.

v10 also puts four columns on `worktrees`: `lane` and `sort_position` (rail
organisation — stored *on the row* so nothing points at a worktree, which is what
makes reused rowids harmless), plus `trashed_at` and `trash_error` (the trash).
`trashed_at` is a timestamp rather than a flag because it is also the **retention
clock**: the checkout stays on disk until `worktree.trashRetentionDays` elapses from
it, or until the user deletes it explicitly. The row records the bin and nothing more
— it is deliberately *not* also the work queue, because at boot "in the bin" and
"queued for removal" are indistinguishable on the row, and a daemon that resumed every
trashed row deleted the whole trash on every restart. What a restart *does* re-run is
the retention sweep, so nothing depends on the daemon having been up when a period
expired. A removal interrupted by the daemon going away is not resumed: git deletes a
checkout in readdir order and leaves no reliable trace of a half-finished removal, so
there is nothing honest to detect, and the worktree simply stays in the bin.

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
  -- v13 adds: display_name TEXT NOT NULL DEFAULT ''  ('' = render the alias)
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
| `GET /api/repos` | Pure DB read: repos with their worktrees, each worktree annotated with `has_veld_config`, `presets`, and `nodes` (startable nodes with variants + default variant — the custom-selection source). Each repo also carries its `lanes` (name + position). `presets` are full records (`name`, `key`, `pinned`, `label`, `when_to_use`, `group`, `selections`, `is_default`) in display order, produced by `veld_core::presets::resolve` — the same resolver the CLI picker uses, so a preset's key means the same thing in both surfaces. Ordering is the resolver's, never a sort here. **`presets` is `null` when the config exists but did not parse**, which is not the same as `[]` (declares none): a client comparing a run's recorded preset against an empty list concludes the preset was deleted, so a mid-edit `veld.json` made every healthy run read "preset dev (no longer defined)". Each entry also carries `expansion`, a three-state answer to what the preset expands to *now* — `{state:"ok", tokens}` (comparable to `RunInfo.started_from.selections`; empty tokens is a real answer), `{state:"failed"}` (exists, does not expand — what `veld status` calls "cannot be expanded — see `veld lint`"), or `{state:"skipped"}` (past this listing's per-poll expansion cap, so nothing is known and nothing is wrong). The three are separate because folding any two makes a surface state something false, and expansion is recursion over a config that arrives with a checked-out branch — hence the cap on an ungated, polled GET. (run state is NOT joined here — the UI joins `/api/environments` client-side by path). `available` is only the cheap directory-exists check; git reconciliation lives in `POST /api/repos/refresh` below. |
| `POST /api/repos/import` `{path}` | Accepts any directory inside the repo; resolves the main checkout via `git worktree list --porcelain`, derives the name, registers it, and syncs the worktree rows. Idempotent. |
| `DELETE /api/repos` `{root}` | Unregisters (never touches the filesystem). |
| `POST /api/worktrees` `{repo_root, branch, alias?, display_name?, lane?, path?, create_branch?, emoji?, marker_color?}` | `git worktree add`. Default path: `<repo_parent>/_worktrees/<alias>`. An explicit `alias` a sibling already holds is a `409`. The check runs *before* `git worktree add`, so the common case creates nothing; it is a plain read though, so a create that races another one (or a sibling on disk not yet synced) still 409s on the authoritative check after the checkout exists — what survives then is a registered worktree under its branch-derived alias, not an orphan. `lane` is pre-checked the same way and for the same reason (a `400` that created nothing beats a checkout filed in the wrong section), with `Db::patch_worktree`'s transaction still the authority. `display_name` is trimmed server-side and bounded by `validate_display_name`: ≤80 **characters** (not bytes), nothing that misrenders its neighbours (`is_forbidden` — control characters, U+2028/U+2029, bidi overrides and isolates, BOM), and at least one **visible** character. That last clause is the one a per-character blocklist cannot express and the one that matters: `""` means "render the alias", so a non-empty name of nothing but zero-width characters would render blank everywhere while defeating the sentinel. Zero-width joiners *beside* a visible character stay legal — they are the glue in emoji like `👩‍💻` and are orthographically required in Persian and Hindi. `emoji`/`marker_color` are the create dialog's marker pick, validated up front. The name and the marker are applied in one `patch_worktree` after the sync assigns its own, and *before* the alias rename, so a checkout that loses the alias race still wears them. **The lane is a deliberate second write**: it is the only one of the four validated inside `patch_worktree`'s transaction, so folding it in made that write fallible and a lane deleted mid-flight discarded the name and marker with it. Omit the marker and the daemon assigns. |
| `PATCH /api/worktrees/{id}` `{alias?, display_name?, emoji?, marker_color?, lane?}` | Partial update, DB only. Every field optional (alias-only callers stay wire-compatible); an empty patch is a `400` and an unknown field a `422` (`deny_unknown_fields` — with everything optional, a client typo would otherwise be a silent `200`). Every column is written in one `UPDATE … COALESCE`, so a multi-field patch can't half-apply. An alias a sibling checkout of the same repo already holds is a `409`: `unique_alias` establishes that invariant at insert and the rename path must not be a hole in it, since the alias becomes the default run name. The check and the write share one transaction, so two concurrent renames can't both win. Cross-repo duplicate aliases stay legal — forbidding them would break importing two repos that are both on `main`. **`display_name` carries no such rule** — it is a label, two checkouts sharing one collide in nothing, and `""` is a *clear* rather than a no-op (the only way back to rendering the alias). `emoji` is checked against the curated set — an allowlist rather than a "one grapheme?" test, which keeps the rail uniform and leaves no room for a multi-codepoint or zero-width payload; the rule lives in `veld_core::db::is_worktree_emoji`, beside the constant, so no caller can bypass it. The Rust side takes a `WorktreePatch` struct rather than five positional `Option<&str>`s, because four of them are the same type and a transposed pair would write the lane into the alias and still compile. |
| `GET /api/worktree-emoji` | The curated glyph list, for the picker. Served rather than duplicated in TypeScript, because the same constant is the server-side allowlist; the picker fetches it once on open instead of riding the 5s poll. |
| `DELETE /api/worktrees/{id}?force=` | **Moves the worktree to the trash and returns `202`.** Nothing is deleted: `worktrees.trashed_at` is set, the checkout stays on disk, and no work is queued — which is why the request is fast. With `?force=true` it instead deletes **inline** (`204`/`422`), because the user is answering a refusal they have already been shown, and refuses with `409` if a run is still live since the forced path does not stop runs. Never touches the main checkout (`Db::trash_worktree` enforces that). Prunes git bookkeeping if the checkout was already gone. |
| `POST /api/worktrees/{id}/restore` | Takes it out of the trash. A real undo for the whole retention period; `409` once a deletion has actually started (past that point the directory is going and no write brings it back, so refusing is the honest answer), `404` if the row is already gone. The check and the write share one lock, so a deletion cannot start between them. |
| `POST /api/worktrees/{id}/delete` | Delete a trashed worktree now instead of waiting for its retention (`202`). `409` if it is not in the trash — not a shortcut past the confirmation. Queues the same worker the retention sweep uses, so exactly one code path runs `git worktree remove`. |
| `DELETE /api/trash?repo_root=` | Empty the trash: queue every trashed worktree of a repo. Returns `{queued}`. |
| `DELETE /api/worktrees/{id}/trash-error` | Clears a recorded deletion failure (the user has read it). |
| `POST /api/worktree-order` `{repo_root, order}` | Rewrites the manual rail order from the **full** list of worktree paths being displayed; omitted paths go back to unplaced (`sort_position = NULL`), except trashed ones. Paths, not ids — rowids are reused. At most `MAX_ORDER_LEN` entries. Not `/api/worktrees/order`, so it cannot shadow `/api/worktrees/{id}`. |
| `GET /api/lanes?repo_root=` / `POST /api/lanes` `{repo_root, name}` | Rail lanes: user-named groups of worktrees, per repo. Names are trimmed, ≤32 chars, reject control characters, and are unique per repo **case-insensitively** (`409`). Max 32 per repo. Lanes also ride along on `GET /api/repos` so the rail never renders a worktree whose lane it has not heard of. |
| `PATCH /api/lanes/{name}` `{repo_root, name}` / `DELETE /api/lanes/{name}?repo_root=` | Rename carries the members (two statements, one transaction — `worktrees.lane` stores the *name*, not an id). Delete **ungroups** its members and removes no checkout. |
| `POST /api/lane-order` `{repo_root, order}` | Lane display order, by name. Deliberately **not** `/api/lanes/order`: a static segment beats a dynamic one, so that path would shadow `/api/lanes/{name}` and make a lane called "order" impossible to rename or delete. |
| `POST /api/worktrees/{id}/start` `{preset?, selections?, run_name?}` | Spawns `veld start` with the worktree as cwd (the CLI resolves veld.json from there). Two mutually-exclusive start modes: `preset` (`--preset <p>`) or explicit `selections` (`node:variant` positionals, validated per half); the UI always sends one. Sending neither is **not** a safe probe: a bare `veld start` uses the project's `default_preset` when one is declared, and only fails "No selections provided" when there isn't one — so an empty body may spawn a run. `run_name` names the environment and defaults to the worktree alias; the UI always sends it explicitly, because a name nobody typed is how "two runs in one directory" became confusing in the first place. **`409` when that environment is already live** — `veld start`'s takeover of a live same-named run is right for the CLI and never what a UI meant, and the client cannot close that race itself (it computes names from a run list up to one poll stale, so ▶ on an "ended" run an agent has restarted, or two windows proposing the same next-free name, would kill the loser silently). Otherwise `202 Accepted`; progress observed via `/api/environments`. |
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
| `POST /api/pty/sessions/{id}/open-url` `{url}` | CSRF-gated. Parses the URL once and routes on — and sends — its **canonical** form, so what was checked against the exempt list is what gets loaded. Answers `{target: "pane" \| "system", reason?}` for a URL that terminal produced, and for `pane` has already pushed the `open_url` frame down that session's socket by the time it returns. `system` means the *caller* opens it (the CLI `exec`s the real opener; the renderer `window.open`s), which is also the answer when no socket is attached — a frame is deliberately not queued for a future attach, because a login page that arrives ten minutes late is worse than one that opened in the wrong browser. A non-`http(s)` URL is a `400` rather than a `system`: the one caller for which that is expected (`veld open-url` standing in for `open report.pdf`) never asks. |
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
veld start --preset dev
```

That is the whole thing: the root `veld.json` declares the dev daemon, the vite
dev server for `/ide`, and the Electron shell as three nodes, so veld builds
them, starts them in order, watches them, and tears them down together. Every
port, database, hostname and wrapper is keyed to the run, so **a second worktree
can have its own stack up at the same time** — which the three-step flow below
could not do, because it hardcoded one of each.

`veld start --preset dev-headless` leaves Electron out; `veld logs dev-ui
--follow` and `veld stop` do what you expect. `dev-electron` is the config's
worked example of `"ports": null`: a supervised process that binds nothing.

**Quit the app freely — the stack stays up, and this comes back:**

```sh
veld action open --node dev-electron
```

The node is a supervisor (`scripts/dev/electron.sh`), not `electron .`, because
veld's health monitor treats any node process dying as a crash of the whole run
and SIGTERMs the survivors — so a bare Electron node made Cmd+Q take the dev
daemon and vite with it. The action covers both cases: it activates the process
when Electron is running but windowless (macOS keeps the app alive on
`window-all-closed`, so `app.on("activate")` is what reopens a window), and asks
the supervisor to relaunch when it has exited.

### The three-step flow, for bootstrapping

Still here, still a singleton, and the right answer in exactly two cases: the
thing you broke is `veld start` itself, or this is a first clone with nothing to
start a run with.

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
`dev-desktop-embedded`, `desktop`, `desktop-package` mirror the existing frontend recipes; each
depends on a guarded deps step, so a checkout with no `node_modules` installs them
instead of failing on a missing binary. For `desktop/` that step also fetches the
Electron binary explicitly: npm defers install scripts it has not been told to
allow, which otherwise leaves a complete `node_modules` whose `electron` reports
`command not found`. CI
runs typecheck + vitest + build for `ui/`, and for `desktop/` a syntax check, the
`node --test` suite, the icon drift gate and a packaging build
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
7. ~~Packaging, auto-update~~ — shipped; see *Packaging and updates* above.
   Still open: macOS Developer ID signing + notarization (which is also what
   turns macOS self-updates on), and installing the veld CLI *from* the app
   rather than pointing at the install command.

The sequencing and the transport/renderer decisions for these live in
[issue #167](https://github.com/prosperity-solutions/veld/issues/167).
