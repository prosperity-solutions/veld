import type { WorktreeGitSignals } from "../api";

/**
 * Whether a worktree is *used*, folded from what git measured into one glyph.
 *
 * The daemon sends independent facts ({@link WorktreeGitSignals}); this is where
 * they become a single visual state, for the same reason the activity glyph does
 * it: a rail row has space for one small mark, and which *kind* of state it is
 * matters more than how much of it there is.
 *
 * # The three states, and why there are only three
 *
 * - **`dirty`** — uncommitted work. It exists in this checkout and nowhere else,
 *   and it is what `git worktree remove` refuses on.
 * - **`unpushed`** — a clean tree with commits its upstream does not have. The work
 *   is committed but has not left the machine.
 * - **`gone`** — the upstream branch has been deleted. Usually a merged pull
 *   request; see {@link WorktreeGitSignals.upstream_gone} for why the name is what
 *   was measured rather than "merged".
 *
 * Two states git can also report are deliberately absent. **`behind`** is on the
 * wire but unrendered — the top bar's staleness pill already answers it for the
 * main checkout, and a second amber mark per row was not asked for. **"no upstream
 * at all"** is not a state either: it is genuinely rare in practice, because a
 * worktree veld creates branches from `origin/<default>` and so gets an upstream
 * automatically, and the counts it would need are exactly what git cannot give for
 * a branch with nothing to compare against.
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
export type GitRowState = "dirty" | "unpushed" | "gone";

/** The one state a row's glyph shows, or `null` for no glyph at all. */
export function rowGitState(git: WorktreeGitSignals | undefined): GitRowState | null {
  if (!git) return null;
  if (git.dirty) return "dirty";
  if (git.ahead !== null && git.ahead > 0) return "unpushed";
  if (git.upstream_gone) return "gone";
  return null;
}

/** `n` with a unit, pluralised. Local because three sentences below need it. */
function commits(n: number): string {
  return n === 1 ? "1 commit" : `${n} commits`;
}

/**
 * Every fact the daemon sent, as one line each, headed by the worktree's label.
 *
 * **The glyph shows the top state; the tooltip shows all of them.** That is the
 * whole reason the wire carries facts rather than a state — a row can be dirty
 * *and* three commits ahead *and* two behind, and the reader deciding what to do
 * next wants the three of them, not the winner.
 *
 * Returns the bare label when there is nothing to add, so a caller can hand it
 * straight to a tooltip without checking.
 */
export function gitTooltip(git: WorktreeGitSignals | undefined, label: string): string {
  if (!git) return label;
  const lines: string[] = [];
  if (git.dirty) lines.push("Uncommitted changes");
  const upstream = git.upstream ?? "its upstream";
  if (git.upstream_gone) {
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
  if (lines.length === 0) return label;
  return `${label} — ${lines.join("\n")}`;
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
  return "upstream branch deleted";
}
