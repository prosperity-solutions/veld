/**
 * The keep-awake switch's state, shared by every client of this daemon.
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
 */
import { useCallback, useEffect, useRef, useState } from "react";

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

export interface UseCaffeinate {
  /** `null` until the first answer arrives (or while the daemon is unreachable). */
  state: CaffeinateState | null;
  /** `null` = until turned off. */
  start: (durationSecs: number | null) => Promise<void>;
  stop: () => Promise<void>;
}

export function useCaffeinate(): UseCaffeinate {
  const [state, setState] = useState<CaffeinateState | null>(null);
  // What the last poll saw, so an expiry can be told from a plain "off": only
  // the transition from a *timed* session to idle is worth a toast, and only
  // when this window was the one watching it happen.
  const previous = useRef<CaffeinateState | null>(null);

  const apply = useCallback((next: CaffeinateState) => {
    const prev = previous.current;
    previous.current = next;
    setState(next);
    if (prev?.active && !next.active && prev.expires_at) {
      notifyDone("Keep-awake ended — this machine can sleep again");
    }
  }, []);

  const load = useCallback(async () => {
    try {
      apply(await api.caffeinate());
    } catch {
      // Silent: the daemon being unreachable is already surfaced by the app's
      // offline banner, and a failed poll every 30s would be a stream of toasts.
    }
  }, [apply]);

  useEffect(() => {
    void load();
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [load]);

  const active = state?.active ?? false;
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => void load(), TICK_MS);
    return () => clearInterval(timer);
  }, [active, load]);

  const start = useCallback(
    async (durationSecs: number | null) => {
      try {
        // The daemon's answer, not an optimistic guess: it owns the deadline,
        // and a locally-invented one would show a countdown that never matches.
        apply(await api.startCaffeinate(durationSecs));
      } catch (e) {
        notifyError("Could not keep this machine awake", e);
        await load();
      }
    },
    [apply, load],
  );

  const stop = useCallback(async () => {
    try {
      // Skips the expiry toast by design — the user just clicked "off", so
      // telling them it is off is noise. `apply` only fires on a *timed*
      // session's transition, and the daemon reports `expires_at` as absent
      // once idle, so the previous state is what would trigger it; suppress it
      // by writing the result straight through.
      const next = await api.stopCaffeinate();
      previous.current = next;
      setState(next);
    } catch (e) {
      notifyError("Could not turn keep-awake off", e);
      await load();
    }
  }, [load]);

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
