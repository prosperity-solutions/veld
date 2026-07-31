// The Electron shell loads /ide?shell=electron: the top bar then doubles as
// the frameless window's native title bar (drag region, traffic-light inset).
export const isElectron =
  new URLSearchParams(window.location.search).get("shell") === "electron";

export const topbarClass = `topbar${isElectron ? " electron" : ""}`;

/**
 * Which persisted pane layout this window owns, or `null` in a plain browser.
 *
 * Assigned by the Electron shell (one slot per window) and passed in the URL, so
 * the renderer never has to guess whether it is the app's only window. A browser
 * tab has no slot and keeps its layout in `sessionStorage` alone — which is the
 * right semantics there: a tab *is* a session, and two tabs must never restore
 * the same terminal session ids and fight over one shell.
 *
 * An older shell that does not send a slot gets `null` too, i.e. exactly the
 * behaviour it had before slots existed. That is deliberate: guessing "main" for
 * it would let two such windows share one slot, which is worse than not
 * restoring.
 */
export const layoutSlot: string | null = (() => {
  const raw = new URLSearchParams(window.location.search).get("slot");
  // The slot becomes part of a storage key; keep it to a charset that cannot
  // collide with the key structure around it.
  return raw && /^[A-Za-z0-9_-]{1,64}$/.test(raw) ? raw : null;
})();
