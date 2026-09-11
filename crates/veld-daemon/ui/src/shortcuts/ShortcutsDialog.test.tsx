import { screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { render, setPlatform } from "../shared/testRender";
import { ShortcutsDialog } from "./ShortcutsDialog";
import {
  SHORTCUTS,
  categoryLabel,
  combosFor,
  type ShortcutDef,
  visibleShortcuts,
} from "./registry";

/**
 * The Shortcuts overview, rendered.
 *
 * `registry.test.ts` already pins the *data* — that every row is well-formed,
 * that `visibleShortcuts` filters by platform, that a page-dispatched chord is
 * accepted by `isAppShortcutChord`. What it cannot reach is the dialog's own
 * decision, which lives in JSX: `ShortcutsDialog.tsx`'s category loop drops a whole category
 * heading when the platform filter leaves it with no rows. That branch is the
 * reason this file exists — it is one `if` in a component, it is invisible to
 * every pure test in this package, and getting it wrong ships a bare heading
 * with nothing under it.
 *
 * Platform is chosen by `isMac()` in `./registry`, which reads
 * `navigator.platform` and then `navigator.userAgent`. jsdom's own navigator
 * reports neither as mac, so the default render here is the non-mac branch;
 * `asMac` below flips it.
 */

/** Make `isMac()` answer true for the current test. Undone by `testSetup.ts`. */
function asMac() {
  setPlatform("MacIntel");
}

test("this file runs in the `dom` project, with a DOM", () => {
  // The twin of the assertion in `ide/dialogGuards.test.ts`: a `.test.tsx` gets
  // `environment: "jsdom"`. See that one for why the pair exists.
  expect(typeof document).toBe("object");
});

describe("ShortcutsDialog", () => {
  test("shows every row the platform filter keeps, and no others", () => {
    render(<ShortcutsDialog onClose={() => {}} />);

    const shown = visibleShortcuts(SHORTCUTS, false);
    const hidden = SHORTCUTS.filter((s) => !shown.includes(s));

    // The predicate is shared with the component on purpose (`visibleShortcuts`'s doc comment
    // says why), so this asserts the *rendering*, not the predicate: every kept
    // row reached the DOM, and every dropped one did not.
    for (const s of shown) {
      expect(screen.getByText(s.title), `${s.id} should be shown`).toBeTruthy();
    }
    for (const s of hidden) {
      expect(screen.queryByText(s.title), `${s.id} should be hidden`).toBeNull();
    }

    // A guard on the assertion above rather than on the component: if the
    // registry ever loses its platform-specific rows, the `hidden` loop passes
    // vacuously and this test stops testing the thing it was written for.
    expect(hidden.length).toBeGreaterThan(0);
  });

  test("renders a category heading only when that category has visible rows", () => {
    // Two categories, and the platform filter empties exactly one of them: the
    // `layout` row is mac-only, so on a non-mac render `visibleShortcuts` drops
    // it and the `rows.length === 0` guard must drop its heading with it. Today's real
    // `SHORTCUTS` cannot produce this state — every category has rows on both
    // platforms (navigation=4 layout=6 run=4 general=10/8) — which is why the
    // list is injected. Asserted against the real registry this test passed
    // vacuously, and deleting the guard did not fail it.
    const rows: ShortcutDef[] = [
      {
        id: "everywhere",
        category: "navigation",
        title: "Shown on both platforms",
        combos: [{ mod: true, keys: ["J"] }],
      },
      {
        id: "mac-only",
        category: "layout",
        title: "Shown on mac only",
        combos: [{ mod: true, keys: ["K"], platform: "mac" }],
      },
    ];

    render(<ShortcutsDialog onClose={() => {}} shortcuts={rows} />);

    expect(screen.getByText(categoryLabel("navigation"))).toBeTruthy();
    expect(screen.getByText("Shown on both platforms")).toBeTruthy();
    // The heading, not just the row: an empty category must render nothing at
    // all rather than a bare label with no table under it.
    expect(screen.queryByText(categoryLabel("layout"))).toBeNull();
    expect(screen.queryByText("Shown on mac only")).toBeNull();
  });

  test("keeps a category whose rows are all visible on this platform", () => {
    // The other side of the same branch: on mac the same two rows both survive,
    // so both headings must be present. Without this a component that returned
    // `null` unconditionally would still pass the test above.
    asMac();
    const rows: ShortcutDef[] = [
      {
        id: "everywhere",
        category: "navigation",
        title: "Shown on both platforms",
        combos: [{ mod: true, keys: ["J"] }],
      },
      {
        id: "mac-only",
        category: "layout",
        title: "Shown on mac only",
        combos: [{ mod: true, keys: ["K"], platform: "mac" }],
      },
    ];

    render(<ShortcutsDialog onClose={() => {}} shortcuts={rows} />);

    expect(screen.getByText(categoryLabel("navigation"))).toBeTruthy();
    expect(screen.getByText(categoryLabel("layout"))).toBeTruthy();
    expect(screen.getByText("Shown on mac only")).toBeTruthy();
  });

  test("shows the platform's own modifier glyphs, not the other platform's", () => {
    asMac();
    const { unmount } = render(<ShortcutsDialog onClose={() => {}} />);
    // ⌘ is the mac token `comboTokens` emits for `mod`.
    expect(document.body.textContent).toContain("⌘");
    unmount();

    // Same dialog, non-mac: the same `mod` flag has to come out as "Ctrl", and
    // the mac glyphs must be gone entirely — a stale ⌘ on Linux is the bug this
    // catches, and it is invisible to a test that only reads the registry.
    setPlatform("Linux x86_64");
    render(<ShortcutsDialog onClose={() => {}} />);
    expect(document.body.textContent).toContain("Ctrl");
    expect(document.body.textContent).not.toContain("⌘");
    expect(document.body.textContent).not.toContain("⌥");
  });

  test("shows only this platform's combos for a row bound on both", () => {
    // The failure `combosFor`'s own doc comment names: "a
    // macOS user is shown a Linux chord as if it were a second way to do the
    // same thing". The row survives `visibleShortcuts` either way, so the
    // platform filter that matters here is the *inner* one, the
    // `combosFor(s, mac)` call inside the row — and dropping it is invisible to every other
    // test in this file, because the leaked combo's `mod` still renders with the
    // reader's own glyph.
    const rows: ShortcutDef[] = [
      {
        id: "both",
        category: "navigation",
        title: "Bound differently per platform",
        combos: [
          { mod: true, keys: ["MACKEY"], platform: "mac" },
          { mod: true, keys: ["LINUXKEY"], platform: "other" },
        ],
      },
    ];

    asMac();
    const { unmount } = render(
      <ShortcutsDialog onClose={() => {}} shortcuts={rows} />,
    );
    expect(screen.getByText("MACKEY")).toBeTruthy();
    expect(screen.queryByText("LINUXKEY")).toBeNull();
    unmount();

    setPlatform("Linux x86_64");
    render(<ShortcutsDialog onClose={() => {}} shortcuts={rows} />);
    expect(screen.getByText("LINUXKEY")).toBeTruthy();
    expect(screen.queryByText("MACKEY")).toBeNull();
  });

  test("a shortcut bound on one platform only is shown on that platform", () => {
    // The invariant `registry.test.ts` deliberately does not enforce on both
    // sides (a row needs a combo on *at least* one platform), seen from the
    // dialog: whichever platform a tagged combo belongs to must render it.
    const tagged = SHORTCUTS.find((s) => s.combos.some((c) => c.platform));
    expect(tagged, "registry has a platform-tagged combo").toBeTruthy();
    if (!tagged) return;

    const mac = combosFor(tagged, true).length > 0;
    if (mac) asMac();
    render(<ShortcutsDialog onClose={() => {}} />);
    expect(screen.getByText(tagged.title)).toBeTruthy();
  });
});
