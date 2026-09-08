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
 * One fact, in the two registers the row needs it: a tooltip line and a short
 * clause for a screen reader.
 *
 * **The pairing is the point.** These two lists were maintained separately and
 * drifted three times in review — `ahead` was added to the description but not
 * `behind`, then `behind` and a deleted upstream but not "no upstream" — each time
 * leaving a screen-reader user with strictly less than a sighted user gets on
 * hover, and each time under a comment claiming the sets matched. Producing both
 * from one place makes that class of drift unrepresentable rather than merely
 * discouraged, which is why it is worth a type for.
 */
interface GitFact {
  /** A full sentence for the tooltip, where there is room to be explicit. */
  tooltip: string;
  /** A short clause for `aria-description`, where several are read in a row. */
  short: string;
}

/**
 * Every fact the daemon sent about this checkout, in reading order.
 *
 * The order is deliberate: what you are *holding* first (uncommitted work), then
 * how you stand against the remote. `gitTooltipLines` and `gitDescription` are both
 * thin projections of this, so they cannot disagree about which facts exist.
 */
function gitFacts(git: WorktreeGitSignals | undefined): GitFact[] {
  if (!git) return [];
  const facts: GitFact[] = [];
  if (git.dirty) {
    facts.push({ tooltip: "Uncommitted changes", short: "uncommitted changes" });
  }
  const upstream = git.upstream ?? "its upstream";
  if (git.upstream_gone) {
    // **Kept even though no glyph renders `[gone]`.** Reachable only alongside
    // another fact — a dirty tree, or an activity glyph holding the slot — which is
    // the same position the no-upstream fact below is in. Worth keeping: "the
    // branch you pushed to is gone" is the single most useful sentence about such a
    // checkout, and dropping it would mean a reader learns less than the daemon
    // knows. It still never asserts the merge outright.
    facts.push({
      tooltip: `${upstream} is gone — the remote branch was deleted, which usually means its pull request was merged`,
      short: "upstream branch deleted",
    });
    return facts;
  }
  if (git.ahead !== null && git.ahead > 0) {
    facts.push({
      tooltip: `${commits(git.ahead)} not pushed to ${upstream}`,
      short: `${commits(git.ahead)} not pushed`,
    });
  }
  if (git.behind !== null && git.behind > 0) {
    facts.push({
      tooltip: `${commits(git.behind)} behind ${upstream}`,
      short: `${commits(git.behind)} behind`,
    });
  }
  if (git.upstream === null && git.dirty !== null) {
    // Said only alongside another fact: on its own it is not a state worth a glyph
    // (see the type doc), and a tooltip with nothing but this would open on a row
    // that shows no mark.
    facts.push({
      tooltip: "This branch has no upstream — nothing has been pushed",
      short: "no upstream",
    });
  }
  return facts;
}

/**
 * Every fact the daemon sent, one line each — no label, no joining.
 *
 * **The glyph shows the winner; the tooltip shows all of them.** That is the whole
 * reason the wire carries facts rather than a state: a row can be dirty *and* three
 * commits ahead *and* two behind, and the reader deciding what to do next wants the
 * three of them.
 *
 * Lines rather than a finished string because these are the *second* half of the
 * row's tooltip — the activity lines come first (see `rowstate/rowState.ts`), and a
 * builder that had already prefixed a label could not be composed with them.
 */
export function gitTooltipLines(git: WorktreeGitSignals | undefined): string[] {
  return gitFacts(git).map((fact) => fact.tooltip);
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
  // **Gated on a glyph rendering, unlike the tooltip.** No glyph means no element
  // to hang a tooltip on, so there is nothing for this to annotate either — a row
  // that is merely `behind`, or merely has a deleted upstream, says nothing to
  // anybody. Past that gate it reports every fact, because this is the only
  // non-visual account of a row whose glyph is `aria-hidden`.
  if (git === undefined || rowGitState(git) === null) return undefined;
  const parts = gitFacts(git).map((fact) => fact.short);
  return parts.length === 0 ? undefined : parts.join(", ");
}
