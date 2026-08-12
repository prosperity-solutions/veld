/**
 * The client half of the promotion channel — Veld's cards and the selected
 * project's, as one list.
 *
 * Reads the state map and the arrival stamp once per page load, and owns the
 * open/close state of the panel. Deliberately thin: every decision — what counts
 * as unread, what may prompt, whether a dated item predates the reader, how an
 * id is namespaced — lives in `model.ts`, where the node-environment test suite
 * can reach it.
 *
 * **The two channels differ in exactly two places, and both are handled by
 * `model.ts` before anything here sees them.** A project's ids are namespaced,
 * so a repo shipping `new-build` can never collide with a Veld card or with
 * another repo's; and a project's cards gate on when this user imported *that
 * project* rather than on when they arrived at Veld. Past that point a card is a
 * card, which is what keeps one unread count, one panel and one merge.
 */

import { useCallback, useEffect, useState } from "react";

import { api } from "../api";
import { PROMOTIONS } from "./content";
import {
  type Card,
  mergeStates,
  type ProjectNewsItem,
  type PromotionState,
  projectCards,
  toPrompt,
  unreadCount,
  unreadOf,
  veldCards,
} from "./model";

/** The selected project and the news its main checkout declares. */
export interface NewsProject {
  root: string;
  name: string;
  created_at: string;
  news: ProjectNewsItem[];
}

export interface PromotionsState {
  /** The panel's contents, or `null` when it is closed. */
  open: { cards: Card[]; automatic: boolean } | null;
  /** Everything this build and this project ship, for the history view. */
  all: Card[];
  /**
   * The selected project's name, or `null` when there is none.
   *
   * Passed through so the history view can offer a **disabled** tab for a project
   * with no news, rather than omitting it — "this project has told you nothing"
   * is an answer, and a missing tab reads as a missing feature. It is not derived
   * from `all` for exactly that reason: a project with no cards contributes none.
   */
  projectName: string | null;
  /** Whether there is anything to reopen at all. */
  any: boolean;
  /** What the indicator shows: unread *and* dismissed, never auto-read. */
  unread: number;
  /** Open everything, on demand. */
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
   * moment. Rarer than it looks, since every card is date-gated and a genuinely
   * new user therefore has none — but an existing user who removed their last
   * project lands on that screen with a real backlog, and that is the collision.
   */
  suppressAuto: boolean;
  /**
   * The selected project, or `null` when there is none.
   *
   * One project at a time, not every imported repo: the stored state row grows
   * monotonically and the daemon cannot prune an id it does not understand, so
   * "mark everything the user has ever had a repo for" is a row that only ever
   * gets bigger. The selected project is also the only one whose news the reader
   * has any context for.
   *
   * Its `news` comes from the repo's **main** checkout — the daemon takes it from
   * there and discards every other worktree's copy, so a card being drafted on a
   * feature branch cannot prompt anybody until it lands.
   */
  project: NewsProject | null;
  /** `ui.showProjectNews`. When off, a project's cards are not built at all. */
  showProject: boolean;
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
   *
   * A `useState` rather than a `useRef` because it is read by the effect below
   * and an effect must not depend on a value React does not track — but it is
   * only ever set to `true`, so the extra render it costs happens once.
   */
  const [settled, setSettled] = useState(false);

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

  /**
   * Every card, from both sources.
   *
   * Recomputed each render rather than memoised, deliberately. `project` arrives
   * from the `/api/repos` poll, so it is a fresh object every few seconds — a
   * `useMemo` keyed on it would rebuild anyway, and a `useMemo` keyed on a
   * hand-rolled signature string would be a second, subtler source of truth
   * about when news changed. The work is a hash of one path and a map over at
   * most a handful of items.
   *
   * Nothing is built while `firstUse` is null: with no arrival there is no date
   * gate, and guessing one is how a fresh install gets a modal about last spring.
   * That deliberately gates the project's cards on Veld's own stamp being loaded
   * too — the request that carries it is the same one that carries `states`, so
   * without it there is nothing to compare a read against either.
   */
  const all: Card[] = firstUse
    ? [
        ...veldCards(PROMOTIONS, firstUse),
        ...(options.showProject && options.project
          ? projectCards(options.project, options.project.news)
          : []),
      ]
    : [];

  const unread = states ? unreadCount(all, states) : 0;
  const promptable = states ? toPrompt(all, states) : [];

  useEffect(() => {
    if (settled || options.suppressAuto || promptable.length === 0 || open) return;
    setOpen({ cards: promptable, automatic: true });
  }, [settled, options.suppressAuto, promptable, open]);

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
      // Close first, unconditionally. The marking below needs the cards' stored
      // state to know what is outstanding, but a dialog that will not shut
      // because a fetch failed is far worse than a card shown again — and
      // `browse()` can open this panel while that fetch is still in flight or has
      // already failed.
      setSettled(true);
      setOpen(null);
      // Only what this user actually has outstanding. Browsing shows every card
      // there is, and marking the auto-read ones would write a row per card the
      // user never had.
      const ids = unreadOf(open.cards, states ?? {});
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
    [open, states],
  );

  const markRead = useCallback(() => settle("read"), [settle]);
  const dismiss = useCallback(() => settle("dismissed"), [settle]);

  const browse = useCallback(() => {
    // Everything there is, whatever its state — including auto-read items, which
    // is how somebody catches up on what changed before they arrived. Closing
    // this marks read: they came here on purpose.
    setOpen({ cards: all, automatic: false });
  }, [all]);

  return {
    open,
    all,
    // Only when its news is actually being shown: with `ui.showProjectNews` off,
    // a tab named after the project would offer to filter to cards this session
    // has deliberately not built.
    projectName: options.showProject ? (options.project?.name ?? null) : null,
    any: all.length > 0,
    unread,
    browse,
    markRead,
    dismiss,
  };
}
