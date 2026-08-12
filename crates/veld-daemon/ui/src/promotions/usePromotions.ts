/**
 * The client half of the promotion channel.
 *
 * Reads the state map and the arrival stamp once per page load, and owns the
 * open/close state of the panel. Deliberately thin: every decision — what counts
 * as unread, what may prompt, whether a dated item predates the user — lives in
 * `model.ts`, where the node-environment test suite can reach it.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api } from "../api";
import { PROMOTIONS } from "./content";
import {
  mergeStates,
  type Promotion,
  type PromotionState,
  toPrompt,
  unreadCount,
  unreadOf,
} from "./model";

export interface PromotionsState {
  /** The panel's contents, or `null` when it is closed. */
  open: { promotions: Promotion[]; automatic: boolean } | null;
  /** Whether there is anything to reopen at all. */
  any: boolean;
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
  /**
   * Whether the user has already settled the panel this session.
   *
   * It gates **the auto-open effect only**, not the mount fetch. The fetch can
   * resolve after the user reached the ⋯ menu and closed the panel, and applying
   * that older snapshot refills `promptable` — so the effect must not act on it.
   * Discarding the fetch outright instead was worse: a browse-and-close landing
   * mid-flight left `states`/`firstUse` null for the rest of the session, which
   * pins the badge at zero and makes every later settle a no-op.
   */
  const settled = useRef(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await api.promotionState();
        if (cancelled) return;
        // Applied even after a settle. It may briefly undo the optimistic merge,
        // which the mark's own response then corrects; the auto-open effect is
        // what must not fire, and that is gated separately.
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
    if (settled.current || options.suppressAuto || promptable.length === 0 || open) return;
    setOpen({ promotions: promptable, automatic: true });
  }, [options.suppressAuto, promptable, open]);

  /**
   * Record a state for whatever the panel is showing, and close it.
   *
   * The optimistic local merge matters: the server is the record, but the panel
   * has to stop re-prompting *now*, and `promptable` is recomputed from `states`
   * — so without it the auto-open effect fires again on the next render. The
   * server's own merged map is then applied when it answers, which is what makes
   * a second window's concurrent change visible without a reload.
   */
  const settle = useCallback(
    (state: PromotionState) => {
      if (!open) return;
      // Close first, unconditionally. The marking below needs `firstUse` to know
      // what is outstanding, but a dialog that will not shut because a fetch
      // failed is far worse than a card shown again — and `browse()` can open
      // this panel while that fetch is still in flight or has already failed.
      settled.current = true;
      setOpen(null);
      if (!firstUse) return;
      // Only what this user actually has outstanding. Browsing shows every card
      // the build ships, and marking the auto-read ones would write a row per
      // promotion the user never had.
      const ids = unreadOf(open.promotions, states ?? {}, firstUse);
      if (ids.length === 0) return;
      setStates((current) => mergeStates(current ?? {}, ids, state));
      // Never block the close on the write. The merge is idempotent, so a failed
      // request costs the user seeing the card once more rather than a dialog
      // that will not shut.
      void api
        .markPromotions(ids, state)
        .then((res) => setStates(res.states))
        .catch(() => {});
    },
    [open, states, firstUse],
  );

  const markRead = useCallback(() => settle("read"), [settle]);
  const dismiss = useCallback(() => settle("dismissed"), [settle]);

  const browse = useCallback(() => {
    // Everything this build ships, whatever its state — including auto-read
    // items, which is how somebody catches up on what changed before they
    // arrived. Closing this marks read: they came here on purpose.
    setOpen({ promotions: PROMOTIONS, automatic: false });
  }, []);

  return { open, any: PROMOTIONS.length > 0, unread, browse, markRead, dismiss };
}
