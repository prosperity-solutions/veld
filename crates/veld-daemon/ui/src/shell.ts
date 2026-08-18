// The Electron shell loads /ide?shell=electron: the top bar then doubles as
// the frameless window's native title bar (drag region, traffic-light inset).
export const isElectron =
  new URLSearchParams(window.location.search).get("shell") === "electron";

export const topbarClass = `topbar${isElectron ? " electron" : ""}`;

/**
 * Which persisted pane layout this window owns, or `null` in a plain browser.
 *
 * Assigned by the Electron shell (one slot per window), so the renderer never has
 * to guess whether it is the app's only window. A browser tab has no slot and
 * keeps its layout in `sessionStorage` alone — which is the right semantics
 * there: a tab *is* a session, and two tabs must never restore the same terminal
 * session ids and fight over one shell.
 *
 * Read from the **preload bridge, not the URL.** A query parameter is forgeable:
 * anyone could link `https://veld.localhost/ide?slot=main`, and that tab would
 * then share the desktop app's durable layout and restore its live PTY session
 * ids — two clients claiming one shell, which an attach resolves by *taking it
 * over*, so they would trade it back and forth indefinitely. Gating on
 * `isElectron` would not have helped, since that is a URL parameter too.
 * `window.veldDesktop` only exists where a preload script put it.
 *
 * An older shell that exposes no slot gets `null`, i.e. exactly the behaviour it
 * had before slots existed. Deliberate: assuming "main" for it would let two such
 * windows share one slot, which is worse than not restoring.
 */
export const layoutSlot: string | null = (() => {
  const raw = (window as { veldDesktop?: { layoutSlot?: unknown } }).veldDesktop?.layoutSlot;
  // The slot becomes part of a storage key; keep it to a charset that cannot
  // collide with the key structure around it.
  return typeof raw === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(raw) ? raw : null;
})();

/**
 * Tabs that were pulled out of another window into this one — the layout this
 * window boots with, before it has a slot store of its own.
 *
 * On the bridge for the same reason the slot is: it names live PTY session ids.
 * Read once, at boot, and only when neither store has anything (see
 * `readLayouts`), so it cannot resurrect a layout the user has since changed.
 */
export const windowSeed: string | null = (() => {
  const raw = (window as { veldDesktop?: { window?: { seed?: unknown } } }).veldDesktop?.window
    ?.seed;
  return typeof raw === "string" && raw !== "" ? raw : null;
})();

/**
 * Whether the shell **reopened** this window on a slot it owned before, rather
 * than opening a new one that happened to be handed the number.
 *
 * Only a reopened window may restore the durable per-slot layout. Suffixes are
 * recycled and a slot's key is never cleared, so to a genuinely new window that
 * layout is a dead one naming terminal ids another window may be using — and
 * attaching to those *takes the shells over*. See `readLayouts`.
 *
 * From the bridge, not the URL, for the same reason the slot is: it decides
 * whether durable state naming live PTY sessions is adopted.
 */
export const windowRestored: boolean =
  (window as { veldDesktop?: { window?: { restored?: unknown } } }).veldDesktop?.window
    ?.restored === true;

/** A payload of tabs moving between windows. Deliberately `unknown[]`: the
 *  receiving side runs them through `parseTransferTabs`, which is the same gate
 *  a restored layout goes through. */
export interface TabTransfer {
  worktreeId: number;
  tabs: unknown[];
}

/** What the Electron shell offers a page for managing windows. `null` in a
 *  plain browser, which has no window manager and must stay fully usable. */
export interface DesktopWindowApi {
  kind: "main" | "detached";
  seed: string | null;
  /**
   * Another full window. Omit the payload for "another one like this"; pass a
   * worktree to open pointed at it.
   *
   * `worktreeId` is optional *within* a payload that names a repo: that is "open
   * this project", and the new window runs its own acquire hunt to land on
   * whichever of its worktrees is free. The shell already builds the URL that way
   * (`appUrl` in `desktop/src/windows.js` sets `?repo=` and `?wt=` independently);
   * this type was the only thing insisting on both.
   */
  newWindow(payload?: {
    repoRoot: string;
    worktreeId?: number;
  }): Promise<{ opened: boolean; reason?: string | null }>;
  /**
   * Which worktree this window is displaying.
   *
   * **Reporting, not asking.** Whether a client *may* show a worktree, who has
   * to let go of it, and what a click on a taken one does are the daemon's —
   * see `ide/channel.ts`. They moved there because this shell can only see its
   * own windows, and the same page also runs in a plain browser tab, which was
   * therefore invisible to the whole arrangement.
   *
   * What is left is the one question only the shell can answer: when tabs are
   * dropped onto a window, is that a window this worktree's panes belong in?
   *
   * Optional: an older shell has no such channel, and routes drops by the
   * detached window's own worktree alone.
   */
  showsWorktree?(worktreeId: number | null): Promise<boolean>;
  detach(payload: {
    worktreeId: number;
    repoRoot: string;
    ratio: number;
    tabs: unknown[];
  }): Promise<{ opened: boolean; reason?: string | null; accepted?: string[] }>;
  /** A tab drag started in this window. */
  dragBegin(): Promise<boolean>;
  dragEnd(): Promise<boolean>;
  /** A tab drag began in *some* window — freeze this one's browser views, or a
   *  drop overlay under a native view would be invisible. */
  onDragBegin(fn: () => void): () => void;
  onDragEnd(fn: () => void): () => void;
  /** The cursor, in this window's content coordinates, while a drag started
   *  elsewhere is over it. Drag events never cross a window, so this is the
   *  only way a target can render its own drop preview. */
  onDragOver(fn: (p: { x: number; y: number }) => void): () => void;
  onDragOut(fn: () => void): () => void;
  /** Tabs dropped here — place them where the preview said, then acknowledge.
   *  The source window does not release them until you do. */
  onDropHere(fn: (p: TabTransfer & { dropId: number }) => void): () => void;
  /** Whether that listener is registered right now. The shell's claim map says
   *  which worktree this window shows, not whether the page showing it has
   *  mounted — so without this a drop is pushed at a window mid-reload and goes
   *  nowhere. Optional: an older shell has no such channel. */
  dropsReady?(ready: boolean): Promise<boolean>;
  /** Which of a `drop-here`'s tabs were placed. Omitting one leaves it in the
   *  window it came from, which is the safe direction: a tab that stayed put is
   *  a visible non-event, a vanished one is unrecoverable. */
  dropApplied(dropId: number, accepted: string[]): Promise<boolean>;
  /** A tab released outside this window. The shell resolves the screen point
   *  against every window it owns: onto one showing this worktree, the tabs
   *  move there; onto nothing, a new window opens. */
  dropOut(payload: {
    worktreeId: number;
    repoRoot: string;
    ratio: number;
    tabs: unknown[];
  }): Promise<{ moved: boolean; opened: boolean; reason?: string | null; accepted?: string[] }>;
  /** Native full screen when the page started. Optional: an older shell has no
   *  such field, and `undefined` reads as the windowed state it always had. */
  fullScreen?: boolean;
  /** …and every change to it. Optional for the same reason. */
  onFullScreen?(fn: (p: { fullScreen: boolean }) => void): () => void;
  /** The page's zoom factor when it started. Optional: an older shell has no
   *  such field, and `undefined` reads as 100%. */
  zoom?: number;
  /** …and every change to it. Optional for the same reason. */
  onZoom?(fn: (p: { zoom: number }) => void): () => void;
  /**
   * Bring this window to the front, because somebody asked to be taken to the
   * worktree it is showing.
   *
   * The daemon decides *who* is wanted — it owns the claim registry — and this
   * is the one thing only the shell can do about it. A plain browser tab has no
   * such capability, which is not a gap to fill: `window.focus()` outside a user
   * gesture is ignored by every browser, so the refusal the asking client got
   * names the holder's kind and says where the worktree is instead of promising
   * a raise that will not happen.
   *
   * Optional: an older shell has no such channel, and a page that finds it
   * absent simply marks itself the way a browser tab does.
   */
  focusSelf?(): Promise<boolean>;
  snapshot(payload: TabTransfer): Promise<boolean>;
  /**
   * This window's tab strip in drawn order — dock 0 left-to-right, then dock 1
   * — pushed on every layout change.
   *
   * The shell's copy of this, across every window, *is* the cross-window cycle
   * order; nothing else assembles one. Sent by a main window as well as a
   * detached one, unlike [`snapshot`], because a main window's own tabs are
   * most of the order rather than a special case of it.
   *
   * Ids only. A title or a URL here would put a second copy of page content in
   * the privileged process for a feature that never reads it — and *which* tab
   * is active is not here either, because that is only ever asked about the
   * window that pressed the key, which sends it with [`cycleTab`].
   *
   * Optional: an older shell has no such channel, and cycling then falls back to
   * this window's own tabs — which is exactly what it does in a browser tab.
   *
   * `worktreeId` is the **granted** worktree (`shownId`), the same id
   * `showsWorktree` registers — the shell checks the two against each other, so
   * reporting one this window has asked for but not been given makes cycling
   * refuse. `null`, or an empty `tabIds`, means "this window is drawing no tab
   * strip" and clears what the shell holds: a window in the Runs view still has
   * a layout, and advertising it there let another window cycle into a pane area
   * that is not mounted.
   */
  tabs?(payload: { worktreeId: number | null; tabIds: string[] }): Promise<boolean>;
  /**
   * Step to the next (`delta: 1`) or previous (`delta: -1`) tab across every
   * window showing this worktree, wrapping at both ends.
   *
   * `activeId` is sent **with the call** rather than left to the last [`tabs`]
   * push: the two are separate IPCs, so a chord pressed in the same frame as a
   * tab change would otherwise be answered from the previous position.
   *
   * Resolves `{tabId}` when the answer is a tab in this window — activate it
   * here, the same way a click would — `{focused: true}` when another window was
   * raised and told to activate its own, and `null` when there is nothing to do.
   *
   * Optional: an older shell has no such channel; see [`tabs`] for what happens
   * then.
   */
  cycleTab?(payload: {
    worktreeId: number;
    delta: 1 | -1;
    activeId: string | null;
  }): Promise<{ tabId?: string; focused?: boolean } | null>;
  /**
   * Cycling landed on this window: activate `tabId`.
   *
   * The shell can raise a window but cannot reach into its renderer's DOM, so
   * which tab is active — and where real focus sits inside it — is work only
   * this side can do. Doing it *here*, through the same path a click takes, is
   * what makes a window cycled to indistinguishable from one clicked into;
   * #315 tried to cover the same ground from the calling window and could not.
   *
   * Optional: an older shell never sends it.
   */
  onActivateTab?(fn: (p: { tabId: string }) => void): () => void;
  setTitle(title: string): Promise<boolean>;
  close(): Promise<boolean>;
  /** Drain the hand-back queue. Call at mount as well as on the nudge — the
   *  nudge can arrive before this page's listener exists. */
  takeAdopted(): Promise<TabTransfer[]>;
  /** A nudge with no payload: there is something in the queue. */
  onAdopt(fn: () => void): () => void;
}

export const desktopWindow: DesktopWindowApi | null =
  (window as { veldDesktop?: { window?: DesktopWindowApi } }).veldDesktop?.window ?? null;

/**
 * Mirror the window's native full-screen state onto `<body data-fullscreen>`.
 *
 * On the body rather than in React state because the thing that reads it is one
 * CSS rule (`.topbar.electron`'s traffic-light inset) and there are two top bars
 * in two modes, neither of which owns the window. `data-fullscreen` sits beside
 * `data-theme`, which is set the same way for the same reason.
 *
 * Called once at boot from `main.tsx`, and nothing unsubscribes: the state is
 * the window's, so the subscription is meant to live as long as the page. The
 * unsubscribe is returned anyway rather than swallowed — a caller that ever does
 * need to detach should not have to change this function to do it.
 */
export function watchFullScreen(): () => void {
  const apply = (fullScreen: boolean) => {
    document.body.dataset.fullscreen = String(fullScreen);
  };
  apply(desktopWindow?.fullScreen === true);
  return desktopWindow?.onFullScreen?.((p) => apply(p?.fullScreen === true)) ?? (() => {});
}

/**
 * Mirror the page's zoom factor onto the `--topbar-zoom` custom property.
 *
 * On the root rather than in React state for the same reason full screen is on
 * the body: the thing that reads it is one CSS rule — `.topbar.electron`'s
 * traffic-light inset is `calc(100px / var(--topbar-zoom, 1))` — and the top
 * bar has no React owner of the window. The zoom is the shell's to know (the
 * main process polls it; the page cannot see its own factor), so this is the
 * daemon-side half of the `veld:window:zoom` channel the shell pushes on
 * change. Dividing the inset by the factor keeps the gap before the view
 * switcher fixed in DIP as the page zooms: the lights are OS-drawn at a fixed
 * size, so an inset that scales with the page makes the gap grow on zoom-in and
 * collapse into the lights on zoom-out.
 *
 * Called once at boot from `main.tsx`, beside `watchFullScreen`, and nothing
 * unsubscribes for the same reason.
 */
export function watchZoom(): () => void {
  const apply = (zoom: number) => {
    const value = Number.isFinite(zoom) && zoom > 0 ? zoom : 1;
    document.documentElement.style.setProperty("--topbar-zoom", String(value));
  };
  apply(desktopWindow?.zoom ?? 1);
  return desktopWindow?.onZoom?.((p) => apply(p?.zoom ?? 1)) ?? (() => {});
}

/** App-level surfaces the Electron main process drives. */
export interface DesktopAppApi {
  /**
   * The `⌘,` menu accelerator fired and this window was chosen to answer it.
   *
   * A menu accelerator rather than a page key handler because a focused
   * `WebContentsView` swallows every keystroke — the page's own binding works
   * everywhere except with a browser pane focused, which is where it is most
   * likely to be pressed. Returns its own unsubscribe.
   */
  onOpenSettings(fn: () => void): () => void;
  /**
   * A menu accelerator fired and this is the focused window: the File menu's
   * "New Tab" (`⌘T`) or "Close Tab" (`⌘W`).
   *
   * Menu accelerators for the same reason `⌘,` is one, and the argument is
   * stronger here: a browser pane swallows every keystroke, and these two are
   * the chords most likely to be pressed while sitting in one. Unlike `⌘,`
   * there is no fallback to another window — a tab command means the strip in
   * front of you, so an unfocused window is never asked.
   *
   * Optional: an older shell has no such channel, and the two chords then do
   * whatever the platform does with them.
   */
  onTabCommand?(fn: (p: { command: "new" | "close" }) => void): () => void;
  /**
   * Show a native OS notification (a terminal's OSC 9 request). `silent` — the
   * terminal bell is the sound; this is the banner. The click focuses this
   * window and reports back through [`onNotifyClick`](`onNotifyClick`).
   */
  notify(payload: { title: string; body: string; worktreeId: number; sessionId: string }): Promise<boolean>;
  /**
   * A native notification this window showed was clicked — focus the pane.
   *
   * `sessionId` *is* the terminal tab id; the payload was echoed back by the
   * shell so the handler needs no other state. Returns its own unsubscribe.
   */
  onNotifyClick(fn: (p: { worktreeId: number; sessionId: string }) => void): () => void;
}

export const desktopApp: DesktopAppApi | null =
  (window as { veldDesktop?: { app?: DesktopAppApi } }).veldDesktop?.app ?? null;

/**
 * Whether the page was opened with settings already requested.
 *
 * Set by the shell only when `⌘,` had no window to send to and opened one for it —
 * an IPC `send` would race the page load. Read from the URL because it grants
 * nothing: settings are a daemon-side document either way, so a forged
 * `?settings=1` opens a dialog the ⋯ menu already opens.
 */
export const openSettingsOnBoot: boolean =
  new URLSearchParams(window.location.search).get("settings") === "1";

/**
 * Whether this window is a bare dock: no worktree rail, no top bar, just the
 * panes that were detached into it.
 *
 * Read from the URL *as well as* the bridge, and unlike everything else on this
 * page that is deliberate. `chrome=none` grants nothing — it hides UI — so a
 * forged one in a browser tab is a page with no top bar, not access to anything,
 * and honouring it means the chrome-less layout can be opened and styled in a
 * plain browser. The two halves that would matter if forged (the slot and the
 * seed) stay on the bridge.
 */
export const chromeless: boolean =
  new URLSearchParams(window.location.search).get("chrome") === "none" ||
  desktopWindow?.kind === "detached";
