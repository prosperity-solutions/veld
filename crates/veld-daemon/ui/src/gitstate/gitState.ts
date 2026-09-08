import type { WorktreeGitSignals } from "../api";

/**
 * Whether a worktree is *used*, folded from what git measured into one glyph.
 *
 * The daemon sends independent facts ({@link WorktreeGitSignals}); this reduces
 * them to the one git state worth a mark. It is not the last word on what the row
 * shows — `rowstate/rowState.ts` decides whether the *activity* vocabulary takes
 * the slot instead, which it does whenever there is any activity at all.
 *
 * # The three states, and why there are only three
 *
 * - **`dirty`** — uncommitted work. It exists in this checkout and nowhere else,
 *   and it is what `git worktree remove` refuses on.
 * - **`unpushed`** — a clean tree with commits its upstream does not have. The work
 *   is committed but has not left the machine.
 * - **`synced`** — everything this checkout has is on the remote. The quietest
 *   state, and the only one that is *not* a call to action: it is here because it
 *   is the first step of a progression a reader tracks (pushed → reviewed →
 *   merged), not because "clean" is interesting on its own.
 *
 * **`synced` does not mean "no pull request exists".** Core cannot know that — a
 * pull request is a forge object, and nothing in the git CLI can be asked about
 * one. A branch that is pushed, in sync, and has an open pull request renders
 * exactly this. Saying otherwise in a tooltip would be veld asserting something it
 * never looked at.
 *
 * # Three things git reports that deliberately render nothing
 *
 * **A deleted upstream** — git's `[gone]`, what a merged-and-tidied pull request
 * leaves behind. There is no merged glyph, on maintainer instruction: git cannot
 * tell a merged pull request from one closed without merging and then deleted, and
 * a mark that is confidently wrong about which teaches people to distrust the rest
 * of the row. Real pull-request state belongs to an `ide.extensions` badge, which
 * holds a PR number and can say it properly.
 *
 * `upstream_gone` is still read here, and is load-bearing rather than vestigial:
 * it is what stops such a checkout falling through to `synced` and claiming
 * everything is pushed when the branch it was pushed to no longer exists.
 *
 * **A branch that was never pushed** — no upstream means nothing to be in sync
 * *with*. Rare in practice, because a worktree veld creates branches from
 * `origin/<default>` and gets an upstream automatically.
 *
 * **`behind`** — on the wire but unrendered: the top bar's staleness pill already
 * answers it for the main checkout, and a second amber mark per row was not asked
 * for.
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
export type GitRowState = "dirty" | "unpushed" | "synced";

/** The one state a row's glyph shows, or `null` for no glyph at all. */
export function rowGitState(git: WorktreeGitSignals | undefined): GitRowState | null {
  if (!git) return null;
  if (git.dirty) return "dirty";
  if (git.ahead !== null && git.ahead > 0) return "unpushed";
  // Last, because it is the absence of anything to do. Three conditions, and each
  // one is a claim this state would otherwise get wrong:
  //
  // - `dirty === false`, not merely falsy — `null` means the sweep has not looked,
  //   and a row must not claim everything is pushed off a reading never taken.
  // - an upstream exists — otherwise there is nothing to be in sync *with*.
  // - **the upstream has not been deleted.** Without this, a merged-and-tidied
  //   branch reads as "everything is pushed" to a remote branch that no longer
  //   exists. There is no glyph for `[gone]` (see the type doc), so this is the
  //   only thing keeping that row honest, which is why `upstream_gone` is still
  //   on the wire with nothing rendering it.
  if (git.upstream !== null && !git.upstream_gone && git.dirty === false) return "synced";
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
    if (git.upstream !== null && git.ahead === 0 && !git.dirty && git.behind === 0) {
      // The `synced` line. Deliberately says only what was measured: veld never
      // asked a forge anything, so it cannot add "and no pull request exists".
      lines.push(`Everything is pushed to ${upstream}`);
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
  const state = rowGitState(git);
  if (state === null) return undefined;
  if (state === "dirty") return "uncommitted changes";
  if (state === "unpushed") {
    return `${commits(git?.ahead ?? 0)} not pushed`;
  }
  return "everything pushed";
}
