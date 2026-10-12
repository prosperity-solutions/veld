/**
 * The main poll's cadence: how often the window asks the daemon for repos,
 * environments and the rest, and how that changes while nobody can see it.
 *
 * Its own module because the decision is the part worth testing, and the interval
 * effect in `App.tsx` that applies it is all DOM and timers.
 */

/** How often a visible window polls. Drives run health, so a service going down shows promptly. */
export const POLL_MS = 5000;

/**
 * How often a hidden window polls — minimised, in a background tab, or covered.
 *
 * Not zero, because one thing on the poll is for a window nobody is looking at: the
 * OS banner for a fault in veld's own database (`dbNotice` in `App.tsx`), which
 * exists so that a fault starting in the background is not discovered tomorrow. A
 * minute late is fine for that; five seconds was buying nothing else.
 *
 * And not five seconds, because this poll is what kicks the daemon's dirty sweep — a
 * `git status` in every checkout — and a reconcile of every repo. On a machine with
 * a few large monorepo checkouts that kept a core busy for as long as any window
 * was open, minimised or not (#400).
 */
export const HIDDEN_POLL_MS = 60_000;

/**
 * Whether an interval tick should actually poll, given whether the document is
 * hidden and how long ago the last poll was issued.
 *
 * The interval itself keeps running at {@link POLL_MS} either way: a tick that
 * returns here costs nothing, and it means the window picks the visible cadence
 * back up on its own without anyone restarting a timer.
 */
export function pollDue(hidden: boolean, sinceLastMs: number): boolean {
  return !hidden || sinceLastMs >= HIDDEN_POLL_MS;
}

/**
 * Whether becoming visible should poll at once rather than wait for the next tick.
 *
 * Only when the last answer is already older than a visible window would have let it
 * get: coming back after a minute should not show a minute-old rail for another five
 * seconds, but a window covered for a moment and uncovered again has nothing to
 * catch up on, and polling then would just double the next tick.
 */
export function pollOnShow(sinceLastMs: number): boolean {
  return sinceLastMs >= POLL_MS;
}
