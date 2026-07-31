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
  newWindow(): Promise<{ opened: boolean }>;
  detach(payload: {
    worktreeId: number;
    repoRoot: string;
    ratio: number;
    tabs: unknown[];
  }): Promise<{ opened: boolean; reason?: string | null }>;
  snapshot(payload: TabTransfer): Promise<boolean>;
  setTitle(title: string): Promise<boolean>;
  close(): Promise<boolean>;
  onAdopt(fn: (payload: TabTransfer) => void): () => void;
}

export const desktopWindow: DesktopWindowApi | null =
  (window as { veldDesktop?: { window?: DesktopWindowApi } }).veldDesktop?.window ?? null;

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
