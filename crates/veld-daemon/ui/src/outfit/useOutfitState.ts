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
  /** True while the first answer for `path` is in flight; false once it landed or failed. */
  loading: boolean;
  set: (state: OutfitState) => void;
  /** Read again, keeping the current answer on screen until the new one lands. */
  refresh: () => void;
} {
  const [state, setState] = useState<OutfitState | null>(null);
  const [loading, setLoading] = useState(path !== null);
  useEffect(() => {
    setLoading(path !== null);
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
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [path]);
  const set = useCallback((s: OutfitState) => setState(s), []);
  // Something other than this window can switch the profile — an agent's `veld ide
  // worktree outfit use`, `outfit use` in a terminal — so a caller re-reads when the
  // answer is about to matter. A failed re-read keeps what was there.
  const refresh = useCallback(() => {
    if (path === null) return;
    api
      .outfitState(path)
      .then(setState)
      .catch(() => {});
  }, [path]);
  return { state, loading, set, refresh };
}
