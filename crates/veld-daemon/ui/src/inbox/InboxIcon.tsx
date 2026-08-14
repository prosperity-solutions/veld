import { Tooltip } from "@mantine/core";
import {
  IconAlertTriangle,
  IconCircleCheck,
  IconLoader,
  IconMessageQuestion,
} from "@tabler/icons-react";

import type { RowState, RowSummary } from "./inbox";

/**
 * The activity vocabulary, shared by the rail row and the pane tab.
 *
 * One map, two surfaces, on purpose: the rail says a worktree has news and the tab says
 * which pane, so they have to mean the same thing by the same glyph or the user learns
 * two languages.
 *
 * # The icons are the maintainer's, hand-picked against this row
 *
 * `message-question` (pulsing) for an agent waiting on you — it is *asking* you something,
 * which is what every blocked state actually is: a permission prompt, a question, a plan
 * to approve. `alert-triangle` (pulsing) for a failure, `circle-check` in the brand green
 * for something that ended well, `loader` spinning for activity.
 *
 * The one hard constraint was **no media-control shapes**:
 * `IconPlayerPlayFilled`/`IconPlayerStopFilled` are the run control, one slot away.
 *
 * **Two deliberate overlaps with the row's existing glyphs**, accepted with eyes open:
 *
 * - `IconLoader` spinning sits beside the run control's own Mantine `<Loader>`, which
 *   marks a *pending run*. Different meanings, similar shape. Tolerable because that one
 *   is transient (a run starting or stopping) and this one is off by default.
 * - `IconAlertTriangle` pulsing is a near-twin of `wt-alert`
 *   (`IconAlertTriangleFilled`, also pulsing) which means *a node failed its liveness
 *   probe*. These **can** co-occur, and then the row shows two pulsing triangles, only
 *   one of which is clickable. Kept apart by weight — outline here, filled there. If it
 *   reads badly in practice, the cheap fix is dropping the pulse from this one: the
 *   shared animation is most of what makes them look like one thing.
 *
 * No filled circles for the small states: at this size a filled circle competes with the
 * worktree marker dot at the other end of the row.
 */
export const ICONS: Record<RowState, typeof IconCircleCheck> = {
  attention: IconMessageQuestion,
  failed: IconAlertTriangle,
  finished: IconCircleCheck,
  working: IconLoader,
};

export const HEADLINE: Record<RowState, string> = {
  attention: "waiting for you",
  failed: "a command failed",
  finished: "finished",
  working: "working",
};

/**
 * A tooltip for an activity glyph.
 *
 * Mantine's, not the native `title`: it is what the run control beside it uses, it
 * honours the theme's 400ms `openDelay` instead of the browser's second-and-a-bit, and
 * it can be styled. `pre-line` because the body is one line per pane — the whole point
 * of the tooltip is that a single glyph cannot say *which* pane, so enumerating them is
 * the job.
 */
function ActivityTooltip(props: {
  label: string;
  children: React.ReactNode;
}): React.JSX.Element {
  return (
    <Tooltip
      label={props.label}
      multiline
      w={260}
      withArrow
      // The row is draggable and scrolls; a tooltip that followed the pointer would
      // fight both. Anchored, like the run control's.
      position="left"
      style={{ whiteSpace: "pre-line" }}
    >
      {props.children}
    </Tooltip>
  );
}

/**
 * The whole tooltip for a worktree's glyph.
 *
 * **One event says what happened; several say how many and then list them.** The first
 * version always printed the state headline *and* the per-pane details, so a single
 * waiting agent read:
 *
 * ```
 * main — waiting for you
 * Waiting for you
 * ```
 *
 * — the same sentence twice, because with one entry the headline and its detail are
 * necessarily the same fact. So a single entry uses its own detail and nothing else.
 */
function tooltipFor(summary: RowSummary, label: string): string {
  const { state, entries, running } = summary;
  if (state === null) return label;
  if (entries.length === 1) return `${label} — ${entries[0].unseen.detail}`;
  if (entries.length === 0) {
    // `working` is the only state with no entries behind it.
    return `${label} — ${
      running === 1 ? "1 pane is running something" : `${running} panes are running something`
    }`;
  }
  const lines = entries.slice(0, 4).map((e) => e.unseen.detail);
  if (entries.length > lines.length) {
    lines.push(`…and ${entries.length - lines.length} more`);
  }
  return `${label} — ${entries.length} unseen\n${lines.join("\n")}`;
}

/**
 * A worktree's activity glyph in the rail.
 *
 * One icon, worst-state-wins. **Not a count**: nobody acts differently on three unseen
 * events than on five — you go and look either way — so the row's scarce space goes on
 * *which kind* of news rather than on how much.
 *
 * # It does not swallow the click
 *
 * A `<span>`, not a `<button>`. The first version was a button that marked the worktree
 * read, which made a click on the row mean two different things depending on which pixel
 * it landed on. Selecting the worktree is what a click on any part of a row does, and
 * this is part of a row. **Mark-all-read lives in the row's ⋯ menu**, where a
 * deliberate gesture belongs.
 */
export function InboxIcon(props: {
  summary: RowSummary;
  label: string;
}): React.JSX.Element | null {
  const { state } = props.summary;
  if (state === null) return null;
  const Icon = ICONS[state];
  return (
    <ActivityTooltip label={tooltipFor(props.summary, props.label)}>
      <span
        className={`wt-inbox ${state}`}
        // Decorative for the screen reader: the row is a `role=button` whose accessible
        // name is built from its content, and folding a status sentence into the name
        // would have it read the state before the worktree it belongs to. The state is
        // announced by the row's own `aria-description` instead.
        aria-hidden="true"
      >
        <Icon size={12} />
      </span>
    </ActivityTooltip>
  );
}

/**
 * A pane's activity glyph, for its tab.
 *
 * The same icon as the rail's, deliberately — the rail says *that* something happened in
 * the worktree, this says *which pane*. It lives outside the tab's label button and left
 * of the close button, because inside it the icon fell under the label's
 * `text-overflow: ellipsis` and a long tab title clipped the one thing saying this pane
 * needs you.
 */
export function PaneActivityIcon(props: {
  state: RowState;
  detail: string | null;
}): React.JSX.Element {
  const Icon = ICONS[props.state];
  return (
    <ActivityTooltip
      label={props.detail ?? HEADLINE[props.state]}
    >
      <span className={`pane-tab-activity ${props.state}`} aria-hidden="true">
        <Icon size={11} />
      </span>
    </ActivityTooltip>
  );
}

/**
 * What a screen reader should hear about a row's activity, or `undefined`.
 *
 * Separate from the glyph so the row can put it in `aria-description` — after the
 * alias — rather than having it folded into the accessible name ahead of it. Same
 * reason the away icon beside the alias is `aria-hidden`.
 */
export function inboxDescription(summary: RowSummary): string | undefined {
  if (summary.state === null) return undefined;
  const count = summary.entries.length;
  if (summary.state === "working" && count === 0) return "working";
  return `${HEADLINE[summary.state]}, ${count} unseen`;
}
