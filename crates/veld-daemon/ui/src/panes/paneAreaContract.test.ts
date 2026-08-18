/**
 * A source-level guard on `PaneArea.tsx`, for an invariant the type system
 * cannot express and the `environment: "node"` test runner cannot exercise.
 *
 * Reading source in a test is not a pattern to reach for casually. It is used
 * here for the same reason `desktop/src/validate.test.js` reads `model.ts` — the
 * two ends cannot be tied together by a compiler — and because the bug it guards
 * has now shipped **twice**, in #315 and again in its isolated retry, both times
 * as "the tab takes the focus outline and never opens".
 *
 * The mechanism, once, so the next reader does not have to rediscover it:
 * React's `onFocus` is `focusin`, which **bubbles**. Focusing a tab button
 * therefore runs the dock's focus handler in the same tick. If that handler
 * writes a layout computed from *its render's* `layout` — the value form of
 * `PaneLayoutUpdate` — it commits a pre-focus snapshot on top of whatever the
 * code that moved focus had just written, silently undoing it. A mouse click is
 * immune by accident: the browser focuses on mousedown and fires `click` after,
 * so there the activation is the second write and wins. That asymmetry is
 * exactly why #315 concluded "a real click works where re-deriving one does not"
 * and shipped a dispatched `.focus()` + `.click()` around a root cause it never
 * found.
 *
 * A jsdom component test would be the better instrument and is the honest
 * follow-up; `PaneArea` pulls in Mantine, the terminal and browser hosts and the
 * API client, so standing one up is its own piece of work rather than part of
 * this change.
 */

// `?raw` rather than `node:fs`: this package's tsconfig carries `vite/client`
// and not `@types/node`, so the bundler's own text import is the one that
// typechecks here — and it resolves relative to this file, so moving either file
// is a build error rather than a test that quietly reads nothing.
import RAW from "./PaneArea.tsx?raw";
import { describe, expect, it } from "vitest";

/**
 * The file with its comments blanked out.
 *
 * Load-bearing, not tidiness: this file's comments *discuss* the very
 * identifiers these assertions look for — the first draft of the ordering check
 * below passed and failed on a sentence explaining `activateTab` rather than on
 * the call to it. Newlines are preserved so the reported offsets still line up
 * with the real file.
 */
const SOURCE = RAW.replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/g, (m: string) =>
  m.replace(/[^\n]/g, " "),
);

describe("PaneArea's layout writes", () => {
  it("never computes a focus-driven layout write from the render's own layout", () => {
    // Every `onFocus=`/`onBlur=` handler in the file, with the expression it
    // runs. Deliberately loose about formatting — the point is to catch a
    // handler that was rewritten into the value form, not to pin a style.
    const handlers = [...SOURCE.matchAll(/\bon(?:Focus|Blur)=\{([^}]*(?:\}[^}]*)*?)\}\s*$/gm)];
    expect(handlers.length, "no focus handlers found — has the pattern changed?").toBeGreaterThan(
      0,
    );

    for (const [, body] of handlers) {
      if (!body.includes("onLayout(")) continue;
      // `onLayout((prev) => …)`, never `onLayout(focusDock(layout, …))`. The
      // updater is handed the layout as it is *at commit time*, so it composes
      // with a write made earlier in the same tick instead of replacing it.
      expect(
        /onLayout\(\s*\(?\s*\w+\s*\)?\s*=>/.test(body),
        `a focus handler writes the layout as a value, not an updater — it will ` +
          `undo whatever moved focus:\n  ${body.trim()}`,
      ).toBe(true);
    }
  });

  it("activates a tab after moving DOM focus onto it, not before", () => {
    // `selectTab` is the keyboard/menu counterpart to clicking a tab, and the
    // ordering is the half of that equivalence a reader can actually see. If
    // these two lines are ever swapped back, the dock's focus handler gets the
    // last word again and this whole class of bug returns even if that handler
    // is correct today.
    const body = /selectTab:\s*\(id: string\)\s*=>\s*\{([\s\S]*?)\n {4}\},/.exec(SOURCE)?.[1];
    expect(body, "selectTab not found — update this test with it").toBeTruthy();
    const focusAt = (body as string).indexOf("getElementById");
    const activateAt = (body as string).indexOf("activateTab");
    expect(focusAt, "selectTab no longer moves DOM focus").toBeGreaterThan(-1);
    expect(activateAt, "selectTab no longer activates the tab").toBeGreaterThan(-1);
    expect(
      focusAt < activateAt,
      "selectTab activates before focusing; a real mouse click does the reverse, " +
        "and the dock's focusin handler runs in between",
    ).toBe(true);
  });
});
