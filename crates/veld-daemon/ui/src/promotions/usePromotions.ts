/**
 * The client half of the promotion channel.
 *
 * Reads the state map and the arrival stamp once per page load, and owns the
 * open/close state of the panel. Deliberately thin: every decision — what counts
 * as unread, what may prompt, whether a dated item predates the user — lives in
 * `model.ts`, where the node-environment test suite can reach it.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import { api } from "../api";
import { PROMOTIONS } from "./content";
import { manifestIds, type Promotion, type PromotionState, toPrompt, unreadCount } from "./model";

export interface PromotionsState {
  /** The panel's contents, or `null` when it is closed. */
  open: { promotions: Promotion[]; automatic: boolean } | null;
  /** What the indicator shows: unread *and* dismissed, never auto-read. */
  unread: number;
  /** Open everything this build ships, on demand. */
  browse: () => void;
  /** "Got it" — actually read. Clears the indicator. */
  markRead: () => void;
  /** Esc, the close button, the overlay: stop prompting, stay unread. */
  dismiss: () => void;
}

export function usePromotions(options: {
  /**
   * Whether the app is showing the first-run screen (or has not yet learned
   * whether it will).
   *
   * Suppresses the *automatic* prompt only — the ⋯ menu still works. A panel
   * thrown over the screen that is trying to get somebody started is the wrong
   * moment, and it is a real collision now that onboarding-kind promotions are
   * shown to brand-new users by design.
   */
  suppressAuto: boolean;
}): PromotionsState {
  const [states, setStates] = useState<Record<string, PromotionState> | null>(null);
  const [firstUse, setFirstUse] = useState<string | null>(null);
  const [open, setOpen] = useState<PromotionsState["open"]>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await api.promotionState();
        if (cancelled) return;
        setStates(res.states);
        setFirstUse(res.first_use);
      } catch {
        // A promotion is never worth an error toast. An older daemon has no such
        // endpoint and a newer one may be mid-restart; either way the right
        // outcome is silence, and the next page load asks again. Leaving both
        // `null` is what keeps the indicator and the prompt quiet rather than
        // guessing the user has seen nothing.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const unread = useMemo(
    () => (states && firstUse ? unreadCount(PROMOTIONS, states, firstUse) : 0),
    [states, firstUse],
  );

  const promptable = useMemo(
    () => (states && firstUse ? toPrompt(PROMOTIONS, states, firstUse) : []),
    [states, firstUse],
  );

  useEffect(() => {
    if (options.suppressAuto || promptable.length === 0 || open) return;
    setOpen({ promotions: promptable, automatic: true });
  }, [options.suppressAuto, promptable, open]);

  /**
   * Record a state for whatever the panel is showing and close it.
   *
   * The optimistic local merge matters: the server is the record, but the panel
   * has to stop re-prompting *now*, and `promptable` is recomputed from `states`
   * — so without it the auto-open effect would fire again on the next render.
   * The merge mirrors the daemon's, where read wins and neither is undone.
   */
  const settle = useCallback(
    (state: PromotionState) => {
      if (!open) return;
      const ids = manifestIds(open.promotions);
      setStates((current) => {
        const next = { ...(current ?? {}) };
        for (const id of ids) {
          if (next[id] !== "read") next[id] = state;
        }
        return next;
      });
      // Never block the close on the write. The merge is idempotent, so a failed
      // request costs the user seeing the card once more rather than a dialog
      // that will not shut.
      void api.markPromotions(ids, state).catch(() => {});
      setOpen(null);
    },
    [open],
  );

  const markRead = useCallback(() => settle("read"), [settle]);
  const dismiss = useCallback(() => settle("dismissed"), [settle]);

  const browse = useCallback(() => {
    // Everything this build ships, whatever its state — including auto-read
    // items, which is how somebody catches up on what changed before they
    // arrived. Closing this marks read: they came here on purpose.
    setOpen({ promotions: PROMOTIONS, automatic: false });
  }, []);

  return { open, unread, browse, markRead, dismiss };
}
