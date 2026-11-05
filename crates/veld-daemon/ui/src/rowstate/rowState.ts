import type { WorktreeGitSignals } from "../api";
import { gitDescription, gitTooltipLines } from "../gitstate/gitState";
import type { RowState, RowSummary } from "../inbox/inbox";

/**
 * What the badge on a rail row's marker says about the worktree's terminals.
 *
 * The badge speaks for the terminals and only for them, which is what lets the
 * row's right side speak for the checkout (git) and the run without the two
 * competing for one slot. Activity wins over a merely running shell, because it
 * is news: `summary.state` has already applied the inbox's own precedence
 * (attention → failed → finished → opened → working), and this neither reorders
 * nor reaches inside it.
 *
 * `idle` is a shell that is alive with nothing to report — the grey dot.
 * Activity can outlive its shell (a command that finished in a pane since
 * closed is still unread), so it does not require a live terminal.
 */
export type RowBadge = { kind: "activity"; state: RowState } | { kind: "idle" };

export function rowBadge(summary: RowSummary, liveTerminals: number): RowBadge | null {
  if (summary.state !== null) return { kind: "activity", state: summary.state };
  return liveTerminals > 0 ? { kind: "idle" } : null;
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
  detached = false,
): string {
  const lines = [...activityLines, ...gitTooltipLines(git, detached)];
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
 *
 * `terminals` is the live-terminal badge's sentence, last because the badge is
 * a standing property like git state, and less likely to need acting on.
 */
export function rowDescription(
  activityDescription: string | undefined,
  git: WorktreeGitSignals | undefined,
  detached = false,
  terminals?: string,
): string | undefined {
  const parts = [activityDescription, gitDescription(git, detached), terminals].filter(
    (part): part is string => part !== undefined,
  );
  return parts.length === 0 ? undefined : parts.join(". ");
}
