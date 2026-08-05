/**
 * The `logs` and `nodes` panes.
 *
 * Thin on purpose: the views themselves are `shared/RunViews.tsx`, which runs mode
 * renders too. All these add is the pane's contract — resolve the *selected*
 * worktree's run, or say honestly why there isn't one.
 *
 * Neither pane holds a run identity: they render the run the window is *bound* to
 * — the top bar's run selector, resolved once in `App` — so switching worktrees or
 * picking another run re-points every open diagnostics pane at the same time.
 * Capturing the run into the tab instead would leave a pane showing a run whose
 * worktree is off screen, with nothing in the tab strip to say so.
 *
 * That the selector moves *all* of them together is the deliberate limit of this
 * design: a directory with two live runs is watched one at a time, not
 * side-by-side. Per-tab pinning would be the next increment, and it belongs in the
 * tab (a value the layout carries) rather than in a second selector.
 *
 * History *is* reachable from a pane: the logs view has its own run picker and the
 * nodes view grows one when no host owns the choice (`selected` omitted below).
 * The two are deliberately independent here — they are separate tabs, and a card's
 * single head picker has no equivalent in a dock.
 */

import type { NodeStats, RunInfo, RunRef } from "../api";
import { LogsView, NodesView, NoRunView, type RunViewTarget } from "../shared/RunViews";

/** What the diagnostics panes need about the selected worktree's run. */
export interface RunPaneContext {
  /** Project-scoped run address; null when the worktree has no run. */
  ref: RunRef | null;
  run: RunInfo | null;
  /** Stats for this run, keyed `node:variant`. */
  stats?: Record<string, NodeStats>;
  /** Why there is no run — only the app knows (no veld.json, or nothing started). */
  emptyHint: string;
  /** Re-poll after an action landed. */
  onChanged: () => void;
  /** Open a URL in a browser pane in this worktree's layout. */
  onOpenPane: (name: string, url: string) => void;
}

/** The context as a view target, or null when there is nothing to show. */
function target(ctx: RunPaneContext): RunViewTarget | null {
  if (!ctx.ref || !ctx.run) return null;
  return {
    ref: ctx.ref,
    run: ctx.run,
    stats: ctx.stats,
    onChanged: ctx.onChanged,
    onOpenPane: ctx.onOpenPane,
  };
}

export function LogsPane(props: { ctx: RunPaneContext }) {
  const t = target(props.ctx);
  if (!t) return <NoRunView kind="logs" hint={props.ctx.emptyHint} />;
  return <LogsView target={t} fill />;
}

export function NodesPane(props: { ctx: RunPaneContext }) {
  const t = target(props.ctx);
  if (!t) return <NoRunView kind="nodes" hint={props.ctx.emptyHint} />;
  return <NodesView target={t} fill />;
}
