/**
 * How a client comes to be showing a worktree.
 *
 * **Extracted because this is where the bugs were.** Eight defects in this
 * change's review lived in these thirty lines while they sat inline in
 * `App.tsx`: a layout read that ran before its claim was granted, a hunt that
 * overwrote a click, a cancellation predicate that cancelled the acquire it was
 * guarding, a grant that was dropped without being given back. Every one was
 * found by reading, because a React component with a socket and five refs has no
 * test. As a function over an injected [`AcquireDeps`] it has one.
 *
 * The rules, all of which have a test below:
 *
 * - **Nothing is shown until it is granted.** A worktree's panes name live PTY
 *   sessions and attaching to one *takes it over*, so rendering before the
 *   daemon has answered steals the shells of whoever legitimately holds it.
 * - **Only `shown_elsewhere` starts a hunt.** `superseded` means a later request
 *   from this client owns the outcome; `offline`/`disconnected` mean nothing was
 *   decided at all. Hunting on either moves the window off a worktree for a
 *   transport failure — and the first candidate would be "granted" the same way.
 * - **A grant this client no longer wants is given back.** Cancellation can
 *   arrive after the daemon has already recorded the claim, and a claim is
 *   otherwise undone only by taking another one or by disconnecting: the
 *   worktree would sit greyed out in every other client's rail, shown by a
 *   window that is showing nothing.
 * - **Cancellation is checked before every request and every effect**, not once
 *   at the end, because each claim can block for the daemon's acknowledgement
 *   timeout.
 */

import type { ClaimResult } from "./channel";

/** The least a caller has to be able to identify. */
export interface AcquireTarget {
  id: number;
  repo_root: string;
}

/** Everything this needs from the app and the socket. */
export interface AcquireDeps {
  /** Ask the daemon. `focusHolder` is false throughout: this runs without a
   *  click behind it, and a refusal that yanked a window forward would be a
   *  window manager answering a question nobody asked. */
  claim(worktreeId: number, focusHolder: boolean): Promise<ClaimResult>;
  /** Give one back, having been granted it and then not wanted it. */
  release(worktreeId: number): void;
  /** What else this client could show, newest list available. */
  candidates(): AcquireTarget[];
  /** Render this worktree — selection *and* the granted marker, together. */
  show(target: AcquireTarget): void;
  /** Every worktree is on screen somewhere else. */
  blocked(): void;
  /** False once something newer has started. See `acquireGenRef` in `App.tsx`. */
  live(): boolean;
}

/**
 * Take `preferred`, or the next free worktree.
 *
 * `preferred` is passed as a target so a successful claim can render it without
 * a second lookup; the hunt reads `candidates()` fresh, where a five-second-old
 * list is harmless.
 */
export async function acquireWorktree(
  preferred: AcquireTarget,
  deps: AcquireDeps,
): Promise<void> {
  if (!deps.live()) return;
  const mine = await deps.claim(preferred.id, false);
  if (mine.ok) {
    // Granted, then cancelled: hand it straight back rather than sitting on a
    // worktree this client is not going to show.
    if (!deps.live()) {
      deps.release(preferred.id);
      return;
    }
    deps.show(preferred);
    return;
  }
  if (mine.reason !== "shown_elsewhere" || !deps.live()) return;

  // **Refused, so this client must show something else.** Ignoring the answer
  // was the hole that made the whole ownership model a suggestion: a new window
  // opens on the last-selected worktree by design, which is the one the window
  // you opened it from is showing — so the claim was always refused, always
  // ignored, and the new window rendered the same panes and took their shells.
  for (const candidate of deps.candidates()) {
    if (candidate.id === preferred.id) continue;
    if (!deps.live()) return;
    const free = await deps.claim(candidate.id, false);
    if (free.ok) {
      if (!deps.live()) {
        deps.release(candidate.id);
        return;
      }
      deps.show(candidate);
      return;
    }
    // Same rule inside the hunt: overtaken, or never asked, means stop — not
    // try the next one.
    if (free.reason !== "shown_elsewhere") return;
  }
  if (deps.live()) deps.blocked();
}
