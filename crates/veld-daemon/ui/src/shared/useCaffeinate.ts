/**
 * The keep-awake switch's state, shared by every client of this daemon **and by
 * every component in this one**.
 *
 * Deliberately **not** on the app's 5s poll. The state changes when a human
 * changes it, which is rare, and the only thing that moves on its own is a
 * countdown nobody reads to the second — so this refetches on mount, on window
 * focus (the same reasoning as `useSettings`), after every write, and on a slow
 * tick while a session is running. That last one is what makes an expiry, or
 * another window's click, show up here without a page reload.
 *
 * The remaining time is the *daemon's* number, re-read rather than counted down
 * locally: a client-side clock drifts across a suspend, and a suspend is exactly
 * the event this feature is about.
 *
 * **One store behind the hook, not one per caller.** This started with a single
 * mount site and now has three — the top bar's cup, the sharing panel's note and
 * the settings dialog's has-a-battery check. Per-hook state would mean three
 * independent pollers of one machine-wide fact (each of which now costs the
 * daemon a settings read), three `previous` refs, and therefore three identical
 * "Keep-awake ended" toasts for one ending. The store keeps the transition
 * detection in one place, where it can only fire once.
 */
import { useCallback, useEffect, useState } from "react";

import { api, type CaffeinateState } from "../api";
import { notifyDone, notifyError } from "./notify";

/**
 * How often a running session is re-read.
 *
 * The label is minutes, so a stale half-minute is invisible; the cost of being
 * wrong the other way is a request per client per tick for a document that
 * changes a handful of times a day.
 */
const TICK_MS = 30_000;

let current: CaffeinateState | null = null;
const listeners = new Set<(next: CaffeinateState) => void>();
/** Refcount of mounted hooks, so the timer and the focus listener exist exactly
 *  while something is rendering this. */
let mounted = 0;
let timer: ReturnType<typeof setInterval> | null = null;
let onFocus: (() => void) | null = null;

/**
 * Publish a new state, announcing an ending nobody asked for.
 *
 * `stop()` writes through `publish` with `requested` set, which is what
 * suppresses the toast for a deliberate switch-off.
 */
function publish(next: CaffeinateState, requested = false) {
  const prev = current;
  current = next;
  // **Any** unrequested `active → inactive`, not only a timed expiry. Gating on
  // `expires_at` excluded the "until I turn it off" session — the overnight one,
  // whose ending (a daemon restart from `veld update`, a crash, an inhibitor
  // that died) is both the least expected and the most expensive to miss. The
  // timed case, whose deadline the user already knows, was the only one being
  // announced.
  //
  // The one exception is a hold **nobody asked for**: an automatic one ends
  // every time a share stops, which is many times a day and is not news. Left
  // in, this would pop a toast in every open window for something the user never
  // turned on — the busiest and least informative notification in the app. The
  // state it would be reporting is not dropped: the cup's menu says the
  // allowance is used up, and says it persistently rather than for four seconds
  // while somebody is looking elsewhere.
  if (!requested && prev?.active && !next.active && prev.reason !== "sharing") {
    notifyDone("Keep-awake ended — this machine can sleep again");
  }
  for (const listener of listeners) listener(next);
}

async function load() {
  try {
    publish(await api.caffeinate());
  } catch {
    // Silent: the daemon being unreachable is already surfaced by the app's
    // offline banner, and a failed poll every 30s would be a stream of toasts.
  }
}

export interface UseCaffeinate {
  /** `null` until the first answer arrives (or while the daemon is unreachable). */
  state: CaffeinateState | null;
  /** `null` = until turned off. */
  start: (durationSecs: number | null) => Promise<void>;
  stop: () => Promise<void>;
}

export function useCaffeinate(): UseCaffeinate {
  const [state, setState] = useState<CaffeinateState | null>(current);

  useEffect(() => {
    listeners.add(setState);
    mounted += 1;
    if (mounted === 1) {
      onFocus = () => void load();
      window.addEventListener("focus", onFocus);
    }
    void load();
    return () => {
      listeners.delete(setState);
      mounted -= 1;
      if (mounted === 0) {
        if (onFocus) window.removeEventListener("focus", onFocus);
        onFocus = null;
        if (timer) clearInterval(timer);
        timer = null;
      }
    };
  }, []);

  // The tick exists only while something is being held — an idle machine has no
  // countdown to follow. Driven off the shared state, so three mounted hooks
  // still produce one interval.
  const active = state?.active ?? false;
  useEffect(() => {
    if (!active || timer) return;
    timer = setInterval(() => void load(), TICK_MS);
    return () => {
      if (!active && timer) {
        clearInterval(timer);
        timer = null;
      }
    };
  }, [active]);

  const start = useCallback(async (durationSecs: number | null) => {
    try {
      // The daemon's answer, not an optimistic guess: it owns the deadline,
      // and a locally-invented one would show a countdown that never matches.
      publish(await api.startCaffeinate(durationSecs));
    } catch (e) {
      notifyError("Could not keep this machine awake", e);
      await load();
    }
  }, []);

  const stop = useCallback(async () => {
    try {
      // `requested`: the user just clicked "off", so telling them it is off is
      // noise. This is the only path that suppresses the toast, which is why
      // `publish` can fire on every other ending.
      publish(await api.stopCaffeinate(), true);
    } catch (e) {
      notifyError("Could not turn keep-awake off", e);
      await load();
    }
  }, []);

  return { state, start, stop };
}

/** `"3h 12m"` / `"12m"` / `"under a minute"` — the countdown as a human reads it. */
export function formatRemaining(seconds: number): string {
  if (seconds < 60) return "under a minute";
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (hours === 0) return `${minutes}m`;
  return minutes === 0 ? `${hours}h` : `${hours}h ${minutes}m`;
}
