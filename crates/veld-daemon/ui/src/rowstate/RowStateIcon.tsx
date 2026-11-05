import { IconArrowUp, IconPencil, IconUnlink } from "@tabler/icons-react";

import type { WorktreeGitSignals } from "../api";
import { type GitRowState, rowGitState } from "../gitstate/gitState";
import { ICONS as ACTIVITY_ICONS } from "../inbox/InboxIcon";
import { RailGlyphTooltip } from "../shared/RailGlyphTooltip";
import type { RowBadge } from "./rowState";

/**
 * The git half of the glyph vocabulary, hand-picked by the maintainer against
 * this row.
 *
 * `pencil` for uncommitted changes — it reads as *authorship* rather than as a
 * warning, which is right: having edits is the normal state of a checkout you are
 * working in, not something to escalate. `arrow-up` for commits not pushed, the
 * convention every git UI already uses (`↑2`), so it needs no tooltip to be
 * understood. `unlink` for a detached HEAD — a broken link says "not attached to
 * a branch" in the same neutral register as the pencil, because a detached
 * checkout is a state to notice rather than an error to escalate.
 *
 * **Three glyphs, and every one of them means "this checkout is holding
 * something".** A detached HEAD qualifies on the same terms as the other two:
 * commits made on one belong to no branch and are lost the moment you check
 * something else out. It replaced the virtual "Detached" rail section, which said
 * the same thing by *moving the row out of its group* — so a rebase made rows
 * leave their group and come back, and a group you had just emptied still held
 * checkouts it would not show you. A branch glyph for everything-pushed was built and removed: it
 * does not communicate not-yet-saved work, and it sat permanently lit on the main
 * checkout, which never leaves that state. **There is deliberately no merged glyph
 * either.** A `git-merge` icon for a deleted
 * upstream was built and then removed on maintainer instruction, and the reason is
 * worth keeping: git cannot tell a merged pull request from one closed without
 * merging and then deleted, so the mark would be confidently wrong for a state
 * whose whole value is confidence. Real pull-request state holds a PR number and
 * belongs to an `ide.extensions` badge, which can say it properly.
 *
 * Three shapes this glyph has no room for, and why: no triangle (the run control
 * shows one for node health, and the badge's `failed` is its outline twin), no
 * filled circle (the worktree marker dot and its badge), and no media-control
 * shape (the run control is one slot away).
 */
const GIT_ICONS: Record<GitRowState, typeof IconPencil> = {
  dirty: IconPencil,
  detached: IconUnlink,
  unpushed: IconArrowUp,
};

/**
 * The badge on a rail row's marker: the worktree's terminals.
 *
 * Today's activity icon in today's colour (and with today's pulse on the two
 * states that ask for you), or a plain dot for a shell that is alive with
 * nothing to report. Drawn on the marker's corner rather than beside it: a
 * second circle beside a dot-face marker reads as one group with it, which is
 * why the rail's old run-status dot was removed.
 *
 * **Not a count**, for the reason the activity glyph never was one: nobody acts
 * differently on three unseen events than on five. The numbers are in the
 * tooltip the caller hangs on the marker.
 */
export function TerminalBadge(props: { badge: RowBadge | null }): React.JSX.Element | null {
  const { badge } = props;
  if (badge === null) return null;
  if (badge.kind === "idle") return <span className="wt-badge idle" aria-hidden="true" />;
  const state = badge.kind === "seen" ? "finished" : badge.state;
  const Icon = ACTIVITY_ICONS[state];
  return (
    <span
      className={`wt-badge act ${state}${badge.kind === "seen" ? " seen" : ""}`}
      aria-hidden="true"
    >
      <Icon size={11} stroke={2.3} />
    </span>
  );
}

/**
 * Whether the checkout is holding something: uncommitted changes, a detached
 * HEAD or unpushed commits. One glyph, worst first — see [`rowGitState`].
 *
 * **It does not swallow the click.** A `<span>`, not a `<button>`: a click
 * anywhere on a row selects the worktree, and none of the actions these states
 * imply (commit, push, trash the checkout) is a one-click decision.
 */
export function GitStateIcon(props: {
  git: WorktreeGitSignals | undefined;
  detached: boolean;
  /** Called only when there is a glyph to put it on. */
  tooltip: () => string;
}): React.JSX.Element | null {
  const state = rowGitState(props.git, props.detached);
  if (state === null) return null;
  const Icon = GIT_ICONS[state];
  return (
    <RailGlyphTooltip label={props.tooltip()}>
      <span
        className={`wt-git ${state}`}
        // Decorative for the screen reader: the row's `aria-description` carries
        // this in words, after the alias, so folding it into the accessible name
        // would announce it ahead of the worktree it belongs to — the same reason
        // the away icon is hidden.
        aria-hidden="true"
      >
        <Icon size={12} />
      </span>
    </RailGlyphTooltip>
  );
}
