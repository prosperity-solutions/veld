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
