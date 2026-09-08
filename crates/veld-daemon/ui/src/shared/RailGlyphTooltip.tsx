import { Tooltip } from "@mantine/core";

/**
 * The tooltip every small status glyph in a rail row uses.
 *
 * **Mantine's, never the browser's `title`.** A `title` on a 12px outline glyph is
 * the slow ~1s browser-chrome tooltip nothing else in this app uses, and handed to
 * a Tabler icon component it becomes an inner SVG `<title>` that only opens on the
 * painted stroke — nearly un-hoverable at this size. This one honours the theme's
 * 400ms `openDelay` and can be styled. (AGENTS.md → *reach for a Mantine
 * primitive*, which that badge shipped wrong twice before landing on.)
 *
 * The props are the load-bearing part, which is why they live in one place rather
 * than being written out per glyph. **The activity glyph and the git glyph share a
 * single slot** — `rowstate/rowState.ts` picks which one renders, never both — so
 * they alternate in the same position as a worktree's state changes. A difference
 * in delay, placement or wrapping between them would read as the row glitching
 * rather than as two components, which is exactly the bug nobody would think to
 * look for. (The pane tab's glyph uses this too, where it is the only one.)
 *
 * - `multiline` + `w={260}` + `pre-line` — the body is one line per fact, and
 *   enumerating them is the whole job of a tooltip on a glyph that can only show
 *   one state.
 * - `position="left"` — the row is draggable and scrolls; a tooltip that followed
 *   the pointer would fight both.
 */
export function RailGlyphTooltip(props: {
  label: string;
  children: React.ReactNode;
}): React.JSX.Element {
  return (
    <Tooltip
      label={props.label}
      multiline
      w={260}
      withArrow
      position="left"
      style={{ whiteSpace: "pre-line" }}
    >
      {props.children}
    </Tooltip>
  );
}
