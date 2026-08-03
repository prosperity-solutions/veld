// Keyboard navigation for a tab strip, as arithmetic.
//
// The handler in `PaneArea.tsx` owns the DOM half — finding the tablist, reading
// its tabs, moving focus. This owns the decision, which is the part with edge
// cases (wrapping, a single tab, an unknown key) and the part the test runner can
// drive without a DOM (`environment: "node"` — see the note in #167 §16 about why
// that is the trade rather than a gap).

/** What a key means in a tab strip. */
export type TabKeyAction =
  | { kind: "focus"; index: number }
  | { kind: "close" }
  | { kind: "ignore" };

/**
 * Resolve a key press in a horizontal tab strip.
 *
 * `at` is the focused tab's index and `count` the number of tabs. Focus wraps at
 * both ends, per the ARIA tabs pattern for a horizontal tablist — the end of the
 * strip is not a wall.
 *
 * Deliberately **manual activation**: this only ever moves focus. Arrow-selects
 * (the pattern's other half) would mount a `WebContentsView` for every browser
 * pane walked past, and replace the visible pane on the way.
 */
export function tabKeyAction(key: string, at: number, count: number): TabKeyAction {
  // Delete before the bounds check: closing does not depend on where in the strip
  // the tab is, and a strip of one is exactly when closing it matters.
  if (key === "Delete" || key === "Backspace") return { kind: "close" };
  if (count <= 0 || at < 0 || at >= count) return { kind: "ignore" };
  switch (key) {
    case "ArrowRight":
      return { kind: "focus", index: (at + 1) % count };
    case "ArrowLeft":
      return { kind: "focus", index: (at - 1 + count) % count };
    case "Home":
      return { kind: "focus", index: 0 };
    case "End":
      return { kind: "focus", index: count - 1 };
    default:
      // Everything else belongs to somebody else — Enter and Space are the
      // button's own (they select), and every other key is the app's.
      return { kind: "ignore" };
  }
}
