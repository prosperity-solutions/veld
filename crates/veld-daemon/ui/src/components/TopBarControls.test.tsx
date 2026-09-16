import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { render } from "../shared/testRender";
import { TopBarControls } from "./TopBarControls";

/**
 * The Next unread button's states.
 *
 * Only this control is covered here: search and focus mode are a click through to a
 * handler the app owns, and keep-awake owns the machine's state and answers for
 * itself. Next unread is the one with decisions in it — whether to render at all,
 * what to
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
  screen.getByRole("button", { name: /Next unread/ }) as HTMLButtonElement;

describe("the Next unread button", () => {
  it("offers the place it would take you as its accessible name", () => {
    // The label names the kind of thing without naming which — and it still has
    // to be *contained* in the accessible name, so a voice-control user saying
    // "click Next unread" lands on it (WCAG 2.5.3).
    render(controls({ next: WAITING }));
    expect(nextButton().getAttribute("aria-label")).toBe(
      `Next unread: ${WAITING.tooltip}`,
    );
    expect(nextButton().textContent).toBe("Next unread");
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

  /** **No idle state, and therefore no disabled state.** With nothing unread
   *  there is nowhere to go, and a greyed word in the densest row of the app
   *  would be a permanent fixture saying only that it has nothing to say. */
  it("is not there at all when nothing is waiting", () => {
    render(controls({ next: null }));
    expect(screen.queryByRole("button", { name: /Next unread/ })).toBe(null);
  });

  /** And it stays gone with the setting off, which is the only other way it can
   *  be absent — never greyed, in either case. */
  it("is not there when nothing is waiting and the setting is off too", () => {
    render(controls({ next: null, settings: { "ui.showNextUnread": false } }));
    expect(screen.queryByRole("button", { name: /Next unread/ })).toBe(null);
  });

  it("goes away under ui.showNextUnread even with something waiting", () => {
    render(controls({ next: WAITING, settings: { "ui.showNextUnread": false } }));
    expect(screen.queryByRole("button", { name: /Next unread/ })).toBe(null);
  });

  /** Defaults on, so an empty settings document still shows it. The same rule
   *  `hideDisabledActions` follows: a new key that decides whether a control
   *  appears takes the shipped default, so an older daemon that cannot know the
   *  key does not look like a UI with a piece missing. */
  it("is shown by default, and unaffected by hideDisabledActions", () => {
    render(controls({ next: WAITING, settings: { "ui.hideDisabledActions": false } }));
    expect(nextButton().disabled).toBe(false);
  });
});
