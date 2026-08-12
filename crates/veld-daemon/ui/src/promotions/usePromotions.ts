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

import { api, type SettingsDoc } from "../api";
import { showProjectNews } from "../shared/settings";
import { PROMOTIONS } from "./content";
import {
  buildCards,
  type Card,
  mergeStates,
  type ProjectNewsItem,
  type PromotionState,
  toPrompt,
  unreadCount,
  unreadOf,
  utcDay,
} from "./model";

/** The selected project and the news its main checkout declares. */
export interface NewsProject {
  root: string;
  name: string;
  created_at: string;
  news: ProjectNewsItem[];
}

export interface PromotionsState {
  /**
   * The panel's contents, or `null` when it is closed.
   *
   * `project` is the root that was selected **when the panel opened**, carried here
   * rather than read at close time: the selection can move on its own (a claim hunt
   * retargets it, and `repos[0]` shifts when another window imports or removes a
   * repo), and settling the *current* selection would then mark a project settled
   * that never prompted — silencing its news for the rest of the page load, which is
   * the exact regression `settledFor` exists to prevent.
   */
  open: { cards: Card[]; automatic: boolean; project: string | null } | null;
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
   * branch **in a worktree** cannot prompt anybody until it lands. (The main
   * checkout is read as it stands, so a card drafted there is live.)
   */
  project: NewsProject | null;
  /**
   * The settings document, or `null` while it has not loaded.
   *
   * Taken raw rather than as a resolved boolean so that "not known yet" stays
   * distinguishable from "absent, take the default". `ui.showProjectNews` defaults
   * to **on**, so resolving it early would auto-open a project's cards in front of
   * a reader who had switched them off — and the *Got it!* that follows writes read
   * rows for cards they opted out of. Settings arriving later cannot re-close a
   * dialog that already latched.
   */
  settings: SettingsDoc | null;
}): PromotionsState {
  const [states, setStates] = useState<Record<string, PromotionState> | null>(null);
  const [firstUse, setFirstUse] = useState<string | null>(null);
  const [open, setOpen] = useState<PromotionsState["open"]>(null);
  /**
   * **Which project** the user has already settled the panel for — not *whether*
   * they have.
   *
   * It gates **the auto-open effect only**, not the mount fetch. The fetch can
   * resolve after the user reached the ⋯ menu and closed the panel, and applying
   * that older snapshot refills `promptable` — so the effect must not act on it.
   * Discarding the fetch outright instead was worse: a browse-and-close landing
   * mid-flight left `states`/`firstUse` null for the rest of the session, which
   * pins the badge at zero and makes every later settle a no-op.
   *
   * A project root rather than a boolean, because a session spans projects and a
   * single flag made the second one silent: close Veld's own prompt, switch to a
   * project with unread news, and that project's cards never interrupted for the
   * rest of the page load — only the badge moved, which is a worse version of the
   * promise ("a teammate pulls, and the next time they open the IDE they are
   * told"). Re-arming per project cannot re-prompt anything already acted on: a
   * read or dismissed card is suppressed by the stored state map, not by this.
   *
   * `undefined` is "nothing settled yet" and is deliberately distinct from `null`,
   * which is a real selection state (no project). A value rather than an effect
   * that resets a flag, because the ordering version of this is a race.
   */
  const [settledFor, setSettledFor] = useState<string | null | undefined>(undefined);
  const projectRoot = options.project?.root ?? null;
  const settled = settledFor === projectRoot;

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
   * Every card, from both sources — assembled by {@link buildCards}, which is pure
   * and tested. The gates it applies (no cards before `firstUse` loads, the
   * `showProject` switch, each channel's own arrival, the future-date drop) are
   * user-visible promises, so they live somewhere the no-jsdom suite can reach.
   *
   * Recomputed each render rather than memoised, deliberately. `project` arrives
   * from the `/api/repos` poll, so it is a fresh object every few seconds — a
   * `useMemo` keyed on it would rebuild anyway, and a `useMemo` keyed on a
   * hand-rolled signature string would be a second, subtler source of truth
   * about when news changed. The work is a hash of one path and a map over at
   * most a handful of items.
   *
   * `today` is read from the clock here, at the one edge that is allowed to have
   * one, and in **UTC** to match `utcDay`'s boundary.
   */
  const all: Card[] = buildCards({
    promotions: PROMOTIONS,
    firstUseIso: firstUse,
    project: options.project,
    showProject: options.settings === null ? null : showProjectNews(options.settings),
    today: utcDay(new Date().toISOString()),
  });

  const unread = states ? unreadCount(all, states) : 0;
  const promptable = states ? toPrompt(all, states) : [];

  useEffect(() => {
    // **Waits for the settings document too**, not only for cards to exist. The
    // prompt latches — `setSettledFor` runs when the reader closes it — so opening
    // before `ui.showProjectNews` is known means opening a panel that *cannot*
    // contain the project's cards, and then never auto-prompting them for the rest
    // of the page load. Badge only, which is the promise quietly downgraded. Same
    // reason `suppressAuto` waits for `repoList`.
    if (options.settings === null) return;
    if (settled || options.suppressAuto || promptable.length === 0 || open) return;
    setOpen({ cards: promptable, automatic: true, project: projectRoot });
  }, [settled, options.suppressAuto, options.settings, promptable, open, projectRoot]);

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
      setSettledFor(open.project);
      setOpen(null);
      // **Nothing is written until both halves of the arrival answer are known.**
      // A null `states` means the request carrying it failed. A null `firstUse` means
      // the cards in `open.cards` were built with `UNKNOWN_ARRIVAL`, so *none* of
      // them counts as predating this reader — and `browse()` is reachable before
      // that request lands, so a panel opened then and closed after `states` arrives
      // would write a row for every promotion in the build, including all the ones
      // that predate them. That is exactly what `unreadOf` exists to avoid. Closing
      // is still closing; the next page load asks again.
      if (!states || !firstUse) return;
      // Only what this user actually has outstanding. Browsing shows every card
      // there is, and marking the auto-read ones would write a row per card the
      // user never had.
      const ids = unreadOf(open.cards, states);
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
    // Everything there is, whatever its state — including auto-read items, which
    // is how somebody catches up on what changed before they arrived. Closing
    // this marks read: they came here on purpose.
    setOpen({ cards: all, automatic: false, project: projectRoot });
  }, [all, projectRoot]);

  return {
    open,
    all,
    // Only when its news is actually being shown: with `ui.showProjectNews` off,
    // a tab named after the project would offer to filter to cards this session
    // has deliberately not built.
    projectName:
      options.settings !== null && showProjectNews(options.settings)
        ? (options.project?.name ?? null)
        : null,
    any: all.length > 0,
    unread,
    browse,
    markRead,
    dismiss,
  };
}
