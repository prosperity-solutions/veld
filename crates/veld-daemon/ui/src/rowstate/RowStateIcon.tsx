import { IconArrowUp, IconGitBranch, IconPencil } from "@tabler/icons-react";

import type { WorktreeGitSignals } from "../api";
import type { GitRowState } from "../gitstate/gitState";
import { ICONS as ACTIVITY_ICONS, activityLines } from "../inbox/InboxIcon";
import type { RowSummary } from "../inbox/inbox";
import { RailGlyphTooltip } from "../shared/RailGlyphTooltip";
import { rowGlyph, rowTooltip } from "./rowState";

/**
 * The git half of the glyph vocabulary, hand-picked by the maintainer against
 * this row.
 *
 * `pencil` for uncommitted changes — it reads as *authorship* rather than as a
 * warning, which is right: having edits is the normal state of a checkout you are
 * working in, not something to escalate. `arrow-up` for commits not pushed, the
 * convention every git UI already uses (`↑2`), so it needs no tooltip to be
 * understood. `git-branch` for everything-pushed, the quiet resting state.
 *
 * **There is deliberately no merged glyph.** A `git-merge` icon for a deleted
 * upstream was built and then removed on maintainer instruction, and the reason is
 * worth keeping: git cannot tell a merged pull request from one closed without
 * merging and then deleted, so the mark would be confidently wrong for a state
 * whose whole value is confidence. Real pull-request state holds a PR number and
 * belongs to an `ide.extensions` badge, which can say it properly.
 *
 * Three shapes this row has no room for, and why: no triangle (`wt-alert` is node
 * health, and the activity vocabulary's `failed` is the outline twin of it), no
 * filled circle (the worktree marker dot is at the row's other end), and no
 * media-control shape (the run control is one slot away).
 */
const GIT_ICONS: Record<GitRowState, typeof IconPencil> = {
  dirty: IconPencil,
  unpushed: IconArrowUp,
  synced: IconGitBranch,
};

/**
 * A worktree's state in the rail: one glyph for activity *or* git, never both.
 *
 * See [`rowGlyph`] for which wins and why. Two properties this shares with the
 * activity-only glyph it replaced, both load-bearing:
 *
 * **Not a count.** `↑3` versus `↑7` changes nothing about what you do — you push
 * either way — so the row's scarce width goes on *which* state it is and the
 * numbers live in the tooltip.
 *
 * **It does not swallow the click.** A `<span>`, not a `<button>`: a click
 * anywhere on a row selects the worktree, and this is part of a row. Nothing here
 * is pressable, because none of the actions these states imply (read the pane,
 * commit, push, trash the checkout) is a one-click decision — they live in the
 * row's ⋯ menu and in the pane itself.
 *
 * The class name still comes from the vocabulary that won, so `.wt-inbox` keeps
 * its four colours and pulses and `.wt-git` keeps its three. One element, two
 * style families, because the *meaning* is what the colour tracks.
 */
export function RowStateIcon(props: {
  summary: RowSummary;
  git: WorktreeGitSignals | undefined;
  label: string;
}): React.JSX.Element | null {
  const glyph = rowGlyph(props.summary, props.git);
  if (glyph === null) return null;
  const Icon = glyph.kind === "activity" ? ACTIVITY_ICONS[glyph.state] : GIT_ICONS[glyph.state];
  const className = glyph.kind === "activity" ? "wt-inbox" : "wt-git";
  return (
    <RailGlyphTooltip
      label={rowTooltip(props.label, activityLines(props.summary), props.git)}
    >
      <span
        className={`${className} ${glyph.state}`}
        // Decorative for the screen reader: the row's `aria-description` carries
        // both halves of this in words, after the alias, so folding either into
        // the accessible name would announce it ahead of the worktree it belongs
        // to — the same reason the away icon is hidden.
        aria-hidden="true"
      >
        <Icon size={12} />
      </span>
    </RailGlyphTooltip>
  );
}
