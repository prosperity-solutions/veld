import { useCallback, useEffect, useState } from "react";
import { type OutfitState, api } from "../api";

/**
 * outfit's state for the checkout at `path`, fetched when `path` changes and
 * never again on its own.
 *
 * The daemon spawns a process per ask, so this is on demand by design: no poll,
 * and `null` (loading, failed, or no path) is rendered as nothing. `set` takes
 * the answer a switch already returned, so a switch costs no second round trip.
 */
export function useOutfitState(path: string | null): {
  state: OutfitState | null;
  set: (state: OutfitState) => void;
} {
  const [state, setState] = useState<OutfitState | null>(null);
  useEffect(() => {
    // Dropped at once: the previous checkout's profile must not label this one
    // while its answer is in flight.
    setState(null);
    if (path === null) return;
    let cancelled = false;
    void api
      .outfitState(path)
      .then((s) => {
        if (!cancelled) setState(s);
      })
      // Nothing to say: no outfit is the common case, and a failed read is
      // indistinguishable from it to the person looking at the bar.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [path]);
  const set = useCallback((s: OutfitState) => setState(s), []);
  return { state, set };
}
