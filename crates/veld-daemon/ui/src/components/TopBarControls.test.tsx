import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { render } from "../shared/testRender";
import { TopBarControls } from "./TopBarControls";

/**
 * The Next button's states.
 *
 * Only this control is covered here: search and focus mode are a click through to a
 * handler the app owns, and keep-awake owns the machine's state and answers for
 * itself. Next is the one with decisions in it — whether to render at all, what to
 * colour the word, and what to call the place it goes.
 *
 * Plain assertions rather than `toBeDisabled()`: this package does not install
 * `@testing-library/jest-dom`, and `ShortcutsDialog.test.tsx` is the same shape.
 */
const controls = (props: Partial<Parameters<typeof TopBarControls>[0]> = {}) => (
  <TopBarControls
    settings={{}}
    onSetting={() => {}}
    onSearch={() => {}}
    next={null}
    onNext={() => {}}
    {...props}
  />
);

const WAITING = {
  kind: "attention" as const,
  tooltip: "veld · next-button · claude — Waiting for you (⌘⇧J)",
};

const nextButton = () =>
  screen.getByRole("button", { name: /Next/ }) as HTMLButtonElement;

describe("the Next button", () => {
  it("offers the place it would take you as its accessible name", () => {
    // The visible word is "Next", which on its own answers "next what?" with
    // nothing — and it still has to be *contained* in the accessible name, so a
    // voice-control user saying "click Next" lands on it (WCAG 2.5.3).
    render(controls({ next: WAITING }));
    expect(nextButton().getAttribute("aria-label")).toBe(`Next: ${WAITING.tooltip}`);
    expect(nextButton().textContent).toBe("Next");
  });

  it("fires the app's handler when pressed", () => {
    const onNext = vi.fn();
    render(controls({ next: WAITING, onNext }));
    nextButton().click();
    expect(onNext).toHaveBeenCalledOnce();
  });

  /** The same three CSS vars the rail glyph and the pane-tab dot use, so the word
   *  tells you what kind of thing is waiting in a vocabulary already learned. */
  it("colours the word by what is waiting", () => {
    for (const kind of ["attention", "failed", "finished"] as const) {
      const { unmount } = render(
        controls({ next: { kind, tooltip: "somewhere — something" } }),
      );
      expect(nextButton().className).toContain(kind);
      unmount();
    }
  });

  /** `ui.hideDisabledActions` defaults to **on**, so the quiet state is no button
   *  at all — the bar is the densest row in the app and a permanently dead word in
   *  it is worse than nothing. */
  it("is not there at all when nothing is waiting", () => {
    render(controls({ next: null }));
    expect(screen.queryByRole("button", { name: /Next/ })).toBe(null);
  });

  /** With the setting off, the same state is greyed rather than gone: a control
   *  that vanishes teaches nobody it exists, and this one is most discoverable on
   *  a quiet day. The same call `KeepAwakeButton` makes. */
  it("is present but dead when nothing is waiting and disabled actions are shown", () => {
    render(controls({ next: null, settings: { "ui.hideDisabledActions": false } }));
    expect(nextButton().disabled).toBe(true);
  });

  /** The setting hides an *inapplicable* control, never a live one — the whole
   *  point of the button is to be there when something needs you. */
  it("stays under that setting while something is waiting", () => {
    render(controls({ next: WAITING, settings: { "ui.hideDisabledActions": true } }));
    expect(nextButton().disabled).toBe(false);
  });
});
