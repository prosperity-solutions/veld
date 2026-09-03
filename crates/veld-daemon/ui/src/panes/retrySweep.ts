/**
 * When to try a stalled terminal again, and how fast.
 *
 * The policy behind `retryStalledTerminals` in `terminalHost.ts`, extracted for
 * the reason `terminalKeys.ts` and `terminalPaste.ts` are: that file reaches
 * xterm and the DOM, so it cannot be imported by this package's
 * `environment: "node"` test runner at all, and everything in it is therefore
 * untestable by construction. What is left there is the loop and the timers;
 * the two decisions live here.
 *
 * The problem being solved: `maybeAutoReconnect`'s budget is spent in about
 * eleven seconds, and the two events that most often break a terminal's socket
 * outlast that by a long way — a laptop that slept, and a daemon restarted by
 * `veld update`. So the budget ran out while the cause was still present, and the
 * pane settled into `error` with a **live shell behind it**, waiting for a click
 * nobody knew to make.
 */

import type { TerminalState } from "./terminalHost";

/**
 * Don't sweep more often than this. A sweep is one attempt per stalled terminal,
 * and the events that trigger it arrive in bursts — tabbing back and forth, or a
 * daemon that comes up and goes down again.
 */
export const RETRY_SWEEP_MS = 3000;

/**
 * Gap between one stalled terminal's attempt and the next one's.
 *
 * **A sweep is a fan-out, and the throttle only bounds how often it happens.**
 * Every attempt is a `POST /api/pty/tickets`, which opens the database in the
 * daemon, and the trigger that matters most is the control socket coming up —
 * i.e. every daemon start. Firing all of them at once aims a burst of up to
 * `MAX_SESSIONS` requests per open window at a daemon in the one second it is
 * least able to serve them. Spread out, the whole sweep still finishes well
 * inside the first attempt's own retry ladder.
 */
export const RETRY_STAGGER_MS = 200;

/**
 * Whether this terminal is one a sweep should touch.
 *
 * `error` and nothing else: `ended` is a shell that exited and has an exit code
 * on the screen, and reconnecting to one would replay its ending as though it had
 * just happened. `live`, `connecting` and `idle` each already have something in
 * flight or an answer of their own.
 *
 * `hasPendingRetry` is the session's own auto-reconnect timer. A cycle still
 * running will make the next attempt by itself, and jumping in would cancel it
 * and re-arm the budget from zero — turning a backoff into a busy loop against a
 * daemon that is, by assumption, not answering.
 */
export function isStalled(state: TerminalState, hasPendingRetry: boolean): boolean {
  return state === "error" && !hasPendingRetry;
}

/** What to do with a sweep request that has just arrived. */
export interface SweepPlan {
  /** Sweep now. */
  run: boolean;
  /** Come back in this many ms instead. `null` when `run` is true. */
  deferMs: number | null;
}

/**
 * Decide whether a sweep request runs now or waits out the throttle window.
 *
 * **A throttled sweep is deferred, never dropped**, and the wake sequence is why:
 * the page becoming visible and the control socket coming up land within a second
 * or two of each other, and the *first* of them is the one made while the daemon
 * may still be down. Dropping the second would throw away the only trigger that
 * is actually evidence of anything.
 *
 * `lastSweep` of `0` is the sentinel for "never swept" and always runs. With a
 * real clock that falls out of the arithmetic — `Date.now()` is far past any
 * window — but a fake-timer test starts near zero, which is precisely where it
 * would not, and a first sweep silently delayed by the whole window is the kind
 * of thing that only shows up as "the wake-up retry sometimes doesn't".
 */
export function planSweep(now: number, lastSweep: number, windowMs = RETRY_SWEEP_MS): SweepPlan {
  if (lastSweep === 0) return { run: true, deferMs: null };
  const since = now - lastSweep;
  // A clock that appears to have gone backwards (a system time change between
  // the two reads) must not defer for a negative — or enormous — interval.
  if (since < 0 || since >= windowMs) return { run: true, deferMs: null };
  return { run: false, deferMs: windowMs - since };
}
