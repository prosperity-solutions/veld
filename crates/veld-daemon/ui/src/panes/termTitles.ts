/**
 * The title each terminal's process gave itself (OSC 0/2), keyed by session id.
 *
 * **Why this is not part of the layout.** Claude Code animates a spinner glyph
 * in its title, roughly ten times a second per working session. While the title
 * lived on `PaneTab.termTitle`, each frame was a `setLayouts`: a new `layouts`
 * object, a re-render of the whole app, and a re-run of every effect that
 * depends on `layouts` — the persistence write, the channel `keep`/`holds`
 * frames, the terminal and browser prunes. With a dozen working sessions that
 * was the app's single largest idle cost.
 *
 * A title is display-only: it is never persisted and never transferred
 * (`parseTab` does not read it), so nothing about the layout's *meaning* ever
 * depended on it. Here it is an external store with a subscription per session,
 * and the only components that re-render on a frame are the ones drawing that
 * session's name (`useTermTitle`, in the tab strip).
 *
 * **The spinner glyph is deliberately not stripped before comparing.** The
 * working → idle transition is, for Claude Code, exactly a glyph change
 * (`✳`), and a store that collapsed "same text, different glyph" would freeze
 * it. A frame is now one `Map.set` and one listener call, which is cheap enough
 * not to need the trick.
 *
 * The session outlives any one layout (a shell in a worktree the user switched
 * away from still renames its tab), and the store is keyed by session rather
 * than by tab for the same reason, so it needs no knowledge of which layout a
 * tab is in. An entry is dropped when the session is released
 * (`releaseTerminal`) or restarted fresh (`setTermTitle(id, "")`).
 */

import { useCallback, useSyncExternalStore } from "react";
import { type AdoptedTermTitle, adoptedTermTitle } from "./model";

const titles = new Map<string, AdoptedTermTitle>();
const listeners = new Map<string, Set<() => void>>();

function notify(sessionId: string): void {
  const set = listeners.get(sessionId);
  if (!set) return;
  // Copied: a listener may unsubscribe (a re-render unmounting a tab) while we
  // are still walking the set.
  for (const fn of [...set]) fn();
}

/**
 * Record a session's title, as the process wrote it.
 *
 * Runs the raw string through [`adoptedTermTitle`] — the one place the clamp
 * lives — *before* storing, so nothing downstream ever sees an uncleaned title.
 * An empty (or all-invisible) result clears the entry, which is how a fresh
 * relaunch gets the tab back to its `label`. Listeners fire only when the
 * stored value actually changed.
 */
export function setTermTitle(sessionId: string, raw: string): void {
  const next = adoptedTermTitle(raw);
  if (next === undefined) {
    clearTermTitle(sessionId);
    return;
  }
  if (titles.get(sessionId) === next) return;
  titles.set(sessionId, next);
  notify(sessionId);
}

/** Forget a session's title — on release, and on a fresh restart. */
export function clearTermTitle(sessionId: string): void {
  if (!titles.delete(sessionId)) return;
  notify(sessionId);
}

export function getTermTitle(sessionId: string): AdoptedTermTitle | undefined {
  return titles.get(sessionId);
}

/** Subscribe to one session's title. Returns the unsubscribe. */
export function subscribeTermTitle(sessionId: string, fn: () => void): () => void {
  let set = listeners.get(sessionId);
  if (!set) {
    set = new Set();
    listeners.set(sessionId, set);
  }
  set.add(fn);
  return () => {
    const current = listeners.get(sessionId);
    if (!current) return;
    current.delete(fn);
    if (current.size === 0) listeners.delete(sessionId);
  };
}

/**
 * A session's title, re-rendering the caller — and only the caller — when it
 * changes. `null` means "no session" (a non-terminal tab) and subscribes to
 * nothing.
 */
export function useTermTitle(sessionId: string | null): AdoptedTermTitle | undefined {
  const subscribe = useCallback(
    (fn: () => void) => (sessionId === null ? () => {} : subscribeTermTitle(sessionId, fn)),
    [sessionId],
  );
  return useSyncExternalStore(subscribe, () =>
    sessionId === null ? undefined : titles.get(sessionId),
  );
}

/** Test seam: drop every entry and listener. */
export function resetTermTitlesForTest(): void {
  titles.clear();
  listeners.clear();
}
