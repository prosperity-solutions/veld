import type { WorktreeGitSignals } from "../api";

/**
 * Whether a worktree is *used*, folded from what git measured into one glyph.
 *
 * The daemon sends independent facts ({@link WorktreeGitSignals}); this reduces
 * them to the one git state worth a mark. It is not the last word on what the row
 * shows — `rowstate/rowState.ts` decides whether the *activity* vocabulary takes
 * the slot instead, which it does whenever there is any activity at all.
 *
 * # The two states, and why there are only two
 *
 * - **`dirty`** — uncommitted work. It exists in this checkout and nowhere else,
 *   and it is what `git worktree remove` refuses on.
 * - **`unpushed`** — a clean tree with commits its upstream does not have. The work
 *   is committed but has not left the machine.
 *
 * # Every state is work that is not safe yet, and nothing else is a state
 *
 * That is the rule the vocabulary settled on, and it is what keeps the column
 * scannable: a mark here always means *this checkout is holding something*. So
 * three things git reports render nothing at all.
 *
 * **Everything pushed.** A branch glyph for it was built and removed — it "does
 * not help in communicating not yet saved work", and it was permanently lit on the
 * main checkout, which never leaves that state and so was decorated forever with a
 * mark nobody could act on.
 *
 * **A deleted upstream** — git's `[gone]`, what a merged-and-tidied pull request
 * leaves behind. No merged glyph either: git cannot tell a merged pull request from
 * one closed without merging and then deleted, and a mark that is confidently
 * wrong about which teaches people to distrust the rest of the row. Real
 * pull-request state belongs to an `ide.extensions` badge, which holds a PR number
 * and can say it properly. `upstream_gone` stays on the wire for the tooltip,
 * which may be probabilistic where a glyph may not.
 *
 * **A branch that was never pushed** — no upstream, so no count, so nothing to
 * report. Rare in practice, because a worktree veld creates branches from
 * `origin/<default>` and gets an upstream automatically.
 *
 * **`behind`** gets no glyph for the same reason — being behind is not work you are
 * holding, and the top bar's staleness pill already answers it for the main
 * checkout — but, like `upstream_gone`, it does reach the tooltip. "No glyph" and
 * "not on the wire" are different things throughout this module.
 *
 * # Worst-state-wins, and `dirty` is the worst
 *
 * `dirty` outranks the others because it is the only one with a *consequence*:
 * every other state describes work that is safely somewhere else, and this one
 * describes work that is not. So a merged-and-deleted worktree with a stray edit in
 * it reads as dirty, which is the reading that stops someone throwing it away.
 *
 * `unpushed` and `gone` cannot co-occur: git reports no ahead/behind counts for a
 * branch whose upstream is `[gone]`, so `ahead` is `null` in exactly that case.
 * The ordering between them is therefore only a tie-break on paper.
 */
export type GitRowState = "dirty" | "unpushed";

/** The one state a row's glyph shows, or `null` for no glyph at all. */
export function rowGitState(git: WorktreeGitSignals | undefined): GitRowState | null {
  if (!git) return null;
  if (git.dirty) return "dirty";
  if (git.ahead !== null && git.ahead > 0) return "unpushed";
  // Nothing else is a state. In particular there is no "everything is pushed" —
  // see the type doc — so `dirty === null` (not measured) and `dirty === false`
  // (measured, clean) both correctly reach here and render the same blank space.
  return null;
}

/** `n` with a unit, pluralised. Local because three sentences below need it. */
function commits(n: number): string {
  return n === 1 ? "1 commit" : `${n} commits`;
}

/**
 * Every fact the daemon sent, one line each — no label, no joining.
 *
 * **The glyph shows the winner; the tooltip shows all of them.** That is the whole
 * reason the wire carries facts rather than a state: a row can be dirty *and*
 * three commits ahead *and* two behind, and the reader deciding what to do next
 * wants the three of them.
 *
 * Lines rather than a finished string because these are the *second* half of the
 * row's tooltip — the activity lines come first (see `rowstate/rowState.ts`), and
 * a builder that had already prefixed a label could not be composed with them.
 */
export function gitTooltipLines(git: WorktreeGitSignals | undefined): string[] {
  if (!git) return [];
  const lines: string[] = [];
  if (git.dirty) lines.push("Uncommitted changes");
  const upstream = git.upstream ?? "its upstream";
  if (git.upstream_gone) {
    // **Kept even though no glyph renders `[gone]`.** This line is now reachable
    // only alongside another fact — a dirty tree, or an activity glyph holding the
    // slot — which is exactly the position the no-upstream line below is in. Worth
    // keeping for those cases: "the branch you pushed to is gone" is the single
    // most useful sentence about such a checkout, and dropping it would mean a
    // hovering reader learns less than the daemon knows.
    lines.push(
      `${upstream} is gone — the remote branch was deleted, which usually means its pull request was merged`,
    );
  } else {
    if (git.ahead !== null && git.ahead > 0) {
      lines.push(`${commits(git.ahead)} not pushed to ${upstream}`);
    }
    if (git.behind !== null && git.behind > 0) {
      lines.push(`${commits(git.behind)} behind ${upstream}`);
    }
    if (git.upstream === null && git.dirty !== null) {
      // Said only alongside another fact: on its own it is not a state worth a
      // glyph (see the type doc), and a tooltip with nothing but this would open
      // on a row that shows no mark.
      lines.push("This branch has no upstream — nothing has been pushed");
    }
  }
  return lines;
}

/**
 * What a screen reader should hear about a row's git state, or `undefined`.
 *
 * Kept out of the accessible *name* for the same reason the activity glyph's is:
 * the row is a `role=button` whose name is built from its content, and a status
 * clause folded in there would be read before the worktree it belongs to. The row
 * puts this in `aria-description` instead, after the alias.
 */
export function gitDescription(git: WorktreeGitSignals | undefined): string | undefined {
  if (git === undefined || rowGitState(git) === null) return undefined;
  // **Every fact the tooltip has, not just the winning glyph's.** The glyph shows
  // one state and the tooltip shows all of them; this is the tooltip's equivalent
  // for a reader who cannot see either, so any fact it omits is one a screen-reader
  // user does not get and a hoverer does. Two review rounds landed here: the first
  // caught it emitting only "uncommitted changes" for a row also commits ahead, and
  // the second caught the fix closing it for `ahead` alone while `behind` and a
  // deleted upstream were still dropped — both of which the daemon emits alongside
  // `dirty` routinely.
  const parts: string[] = [];
  if (git.dirty) parts.push("uncommitted changes");
  if (git.upstream_gone) {
    parts.push("upstream branch deleted");
  } else {
    if (git.ahead !== null && git.ahead > 0) parts.push(`${commits(git.ahead)} not pushed`);
    if (git.behind !== null && git.behind > 0) parts.push(`${commits(git.behind)} behind`);
  }
  return parts.join(", ");
}
