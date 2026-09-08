import type { WorktreeGitSignals } from "../api";
import { type GitRowState, gitDescription, gitTooltipLines, rowGitState } from "../gitstate/gitState";
import type { RowState, RowSummary } from "../inbox/inbox";

/**
 * The one glyph a rail row shows, chosen from the two vocabularies that want it.
 *
 * # Why they share a slot instead of sitting side by side
 *
 * Not width — the row had room for both. **While an agent is running, the tree is
 * *expected* to be dirty**, so a pencil beside a spinner reports something the
 * spinner already implies, and the reader has to look at two marks to learn one
 * thing. The same holds for the rest of the activity vocabulary: those states are
 * all news that arrived and has not been read, and news outranks a standing
 * property of the checkout.
 *
 * So activity wins whenever there is any, and git state fills the slot the rest of
 * the time — which is most of the time, since activity is transient and `working`
 * is off by default.
 *
 * # The glyph collapses; the tooltip does not
 *
 * Hovering a spinning row still says it has three unpushed commits, because that
 * is the fact you would otherwise have to go and ask git for. Folding both halves
 * would have produced a quieter row that knows less, which was not the point — see
 * [`rowTooltip`] and [`rowDescription`].
 */
export type RowGlyph =
  | { kind: "activity"; state: RowState }
  | { kind: "git"; state: GitRowState };

/**
 * Which glyph the row renders, or `null` for none.
 *
 * `summary.state` has already applied the inbox's own precedence (attention →
 * failed → finished → working). This deliberately neither reorders nor reaches
 * inside it: that ordering is shipped and was tuned against this row. All this
 * decides is which *vocabulary* speaks.
 */
export function rowGlyph(
  summary: RowSummary,
  git: WorktreeGitSignals | undefined,
): RowGlyph | null {
  if (summary.state !== null) return { kind: "activity", state: summary.state };
  const state = rowGitState(git);
  return state === null ? null : { kind: "git", state };
}

/**
 * Everything the row has to say, headed by its label — activity, then git.
 *
 * Activity leads because it is what *changed*; the git lines are the standing
 * context underneath it. A row with only one kind of fact reads exactly as it did
 * before the two shared a slot.
 */
export function rowTooltip(
  label: string,
  activityLines: string[],
  git: WorktreeGitSignals | undefined,
): string {
  const lines = [...activityLines, ...gitTooltipLines(git)];
  return lines.length === 0 ? label : `${label} — ${lines.join("\n")}`;
}

/**
 * What a screen reader hears about a row, or `undefined`.
 *
 * **Both halves, always** — not just the one the glyph shows. The glyph is
 * `aria-hidden`, so this attribute is the whole non-visual account of the row's
 * state, and a reader told an agent is working but not that the checkout has
 * unpushed commits has been given strictly less than a sighted reader who hovers.
 *
 * Joined with `. ` because a screen reader reads the attribute as one string and
 * two clauses otherwise run together.
 */
export function rowDescription(
  activityDescription: string | undefined,
  git: WorktreeGitSignals | undefined,
): string | undefined {
  const parts = [activityDescription, gitDescription(git)].filter(
    (part): part is string => part !== undefined,
  );
  return parts.length === 0 ? undefined : parts.join(". ");
}
