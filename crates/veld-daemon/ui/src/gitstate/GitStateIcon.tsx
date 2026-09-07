import { IconArrowUp, IconGitMerge, IconPencil } from "@tabler/icons-react";

import type { WorktreeGitSignals } from "../api";
import { RailGlyphTooltip } from "../shared/RailGlyphTooltip";
import { type GitRowState, gitTooltip, rowGitState } from "./gitState";

/**
 * The glyph vocabulary for "is this worktree used", hand-picked by the maintainer
 * against this row.
 *
 * `pencil` for uncommitted changes — it reads as *authorship* rather than as a
 * warning, which is right: having edits is the normal state of a worktree you are
 * working in, not a problem to escalate. `arrow-up` for commits not pushed, the
 * convention every git UI already uses (`↑2`), so it needs no tooltip to be
 * understood. `git-merge` for an upstream that is gone, because that is the
 * conclusion a reader draws and is right about most of the time; the tooltip
 * carries the honest wording.
 *
 * Three shapes this row has no room for, and why each was ruled out: no triangle
 * (`wt-alert` is node health, and the activity glyph's failed state is the outline
 * twin of it), no filled circle (the worktree marker dot sits at the row's other
 * end), and no media-control shape (the run control is one slot away).
 */
const ICONS: Record<GitRowState, typeof IconPencil> = {
  dirty: IconPencil,
  unpushed: IconArrowUp,
  gone: IconGitMerge,
};

/**
 * A worktree's git state in the rail. One glyph, worst-state-wins.
 *
 * **Not a count.** `↑3` versus `↑7` changes nothing about what you do — you go and
 * push either way — so the row's scarce width goes on *which* state it is, and the
 * numbers live in the tooltip. The activity glyph beside it made the same call for
 * the same reason, and the two have to agree or the row teaches two languages.
 *
 * # It does not swallow the click
 *
 * A `<span>`, not a `<button>`: a click anywhere on a row selects the worktree, and
 * this is part of a row. There is deliberately nothing to press here — the actions
 * this state implies (commit, push, trash the worktree) are not one-click
 * decisions, and they already live in the row's ⋮ menu and the terminal.
 */
export function GitStateIcon(props: {
  git: WorktreeGitSignals | undefined;
  label: string;
}): React.JSX.Element | null {
  const state = rowGitState(props.git);
  if (state === null) return null;
  const Icon = ICONS[state];
  return (
    <RailGlyphTooltip label={gitTooltip(props.git, props.label)}>
      <span
        className={`wt-git ${state}`}
        // Decorative for the screen reader: the row's `aria-description` carries
        // this state in words, after the alias, so folding it into the accessible
        // name would announce it ahead of the worktree it describes.
        aria-hidden="true"
      >
        <Icon size={12} />
      </span>
    </RailGlyphTooltip>
  );
}
