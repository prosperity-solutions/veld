import { useCallback, useEffect, useState } from "react";
import { type OutfitState, api } from "../api";

/**
 * The last answer for each checkout path, for this window's lifetime.
 *
 * Switching worktrees remounts the top-bar control, and starting from nothing each
 * time made the shirt vanish and come back on every switch. A remembered answer is
 * shown at once and replaced when the fresh one lands — outfit state changes
 * rarely, and a stale label is corrected within the round trip.
 */
const answers = new Map<string, OutfitState>();

/**
 * Bumped per path by every switch. A read that started before the latest switch is
 * dropped when it lands, so a slow read cannot put the old profile back. Reads do
 * not bump: two readers of one path (the top bar and the create dialog) must not
 * cancel each other.
 */
const generations = new Map<string, number>();
const generation = (path: string) => generations.get(path) ?? 0;
const bump = (path: string) => generations.set(path, generation(path) + 1);

/**
 * outfit's state for the checkout at `path`: the remembered answer at once, then
 * a fresh read when `path` changes, and never again on its own.
 *
 * The daemon spawns a process per ask, so this is on demand by design: no poll,
 * and `null` (not known yet, failed, or no path) is rendered as nothing. `set`
 * takes the answer a switch already returned, so a switch costs no second round
 * trip.
 */
export function useOutfitState(
  path: string | null,
  /** `false` to ignore the remembered answer and wait for a fresh one — for a
   *  caller that acts on `required`/`default` rather than only labelling. */
  remember = true,
): {
  state: OutfitState | null;
  /** True while there is no answer for `path` yet — none remembered, and the read
   *  in flight. False once one is on screen, or the read failed. */
  loading: boolean;
  set: (state: OutfitState) => void;
  /** Read again, keeping the current answer on screen until the new one lands. */
  refresh: () => void;
} {
  const remembered = path === null || !remember ? null : (answers.get(path) ?? null);
  const [state, setState] = useState<OutfitState | null>(remembered);
  const [loading, setLoading] = useState(path !== null && remembered === null);
  useEffect(() => {
    const known = path === null || !remember ? null : (answers.get(path) ?? null);
    // Never another checkout's answer: what shows is this path's own, or nothing.
    setState(known);
    setLoading(path !== null && known === null);
    if (path === null) return;
    let cancelled = false;
    const started = generation(path);
    void api
      .outfitState(path)
      .then((s) => {
        if (generation(path) !== started) return;
        answers.set(path, s);
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
  }, [path, remember]);
  const set = useCallback(
    (s: OutfitState) => {
      if (path !== null) {
        bump(path);
        answers.set(path, s);
      }
      setState(s);
    },
    [path],
  );
  // Something other than this window can switch the profile — `outfit use` run by an
  // agent or in a terminal — so a caller re-reads when the answer is about to matter.
  // A failed re-read keeps what was there.
  const refresh = useCallback(() => {
    if (path === null) return;
    const started = generation(path);
    api
      .outfitState(path)
      .then((s) => {
        if (generation(path) !== started) return;
        answers.set(path, s);
        setState(s);
      })
      .catch(() => {});
  }, [path]);
  return { state, loading, set, refresh };
}
