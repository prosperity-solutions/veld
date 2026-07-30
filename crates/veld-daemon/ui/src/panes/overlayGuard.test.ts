import { describe, expect, it } from "vitest";
import { type OverlayCandidate, hasOverlay } from "./overlayGuard";

/**
 * A stand-in for a DOM element.
 *
 * The runner has no DOM (`environment: "node"`), and this rule is the one whose
 * regressions are invisible in the browser build: z-index works there, so a
 * missed overlay only shows up as a dropdown drawn behind a native view in the
 * desktop shell — and an over-eager match shows up as a pane that is blank
 * forever. Both of those shipped once.
 */
interface Fake extends OverlayCandidate {
  children: Fake[];
  /** Self-and-below flattening, so `querySelectorAll` matches at any depth. */
  descendants(): Fake[];
}

function el(tokens: string[], children: Fake[] = [], rendered = true): Fake {
  const self: Fake = {
    children,
    matches: (selector) =>
      selector
        .split(",")
        .map((s) => s.trim())
        .some((s) => tokens.includes(s)),
    descendants: () => children.flatMap((c) => [c, ...c.descendants()]),
    querySelectorAll: (selector) => self.descendants().filter((n) => n.matches(selector)),
    checkVisibility: () => rendered,
    getClientRects: () => ({ length: rendered ? 1 : 0 }),
  };
  return self;
}

const body = (children: Fake[]) => ({ children });

describe("hasOverlay", () => {
  it("is false for an app with no overlay open", () => {
    expect(hasOverlay(body([]))).toBe(false);
    expect(hasOverlay(body([el(["#root"], [el([".dock"]), el([".term-host"])])]))).toBe(false);
  });

  it("sees a portalled overlay through its wrapper div", () => {
    // Mantine renders the overlay inside a container div, so the wrapper itself
    // matches nothing — the match has to come from the subtree.
    for (const token of [
      '[role="dialog"]',
      '[aria-modal="true"]',
      '[role="menu"]',
      ".mantine-contextmenu",
      "[data-veld-overlay]",
    ]) {
      expect(hasOverlay(body([el(["div"], [el([token])])]))).toBe(true);
    }
  });

  it("suspends for a toast, but not for the empty notifications container", () => {
    // `shared/notify.ts` marks every toast `[data-veld-overlay]`: without it a
    // toast landing on a browser pane is painted over by the native view and the
    // error is simply never seen. The `<Notifications />` container is mounted for
    // the life of the page, so it must NOT carry the attribute itself — that would
    // hide every pane forever, which is the failure this file's other tests exist
    // for.
    const idle = el([".mantine-Notifications-root"]);
    expect(hasOverlay(body([idle]))).toBe(false);
    const showing = el([".mantine-Notifications-root"], [el(["[data-veld-overlay]"])]);
    expect(hasOverlay(body([showing]))).toBe(true);
  });

  it("is false while Mantine's shared portal node is empty", () => {
    // `Portal` reuses one container: it is appended to body the first time any
    // overlay opens and then stays there forever. An empty one must not read as
    // "an overlay is open", or every pane would be hidden for the rest of the
    // session after the first menu.
    const shared = el(["[data-mantine-shared-portal-node]"]);
    expect(hasOverlay(body([el(["#root"]), shared]))).toBe(false);
  });

  it("sees an overlay nested inside the shared portal node", () => {
    const shared = el(
      ["[data-mantine-shared-portal-node]"],
      [el(["div"], [el(['[role="menu"]'])])],
    );
    expect(hasOverlay(body([el(["#root"]), shared]))).toBe(true);
  });

  it("ignores a kept-mounted dropdown that is not painted", () => {
    // Mantine's Combobox keeps its dropdown mounted and hides it with
    // `display: none`, so a *closed* Select is indistinguishable from an open one
    // by selector alone. Presence is not the signal; being rendered is.
    const closed = el(['[class*="-dropdown"]'], [], false);
    expect(hasOverlay(body([el(["div"], [closed])]))).toBe(false);

    const open = el(['[class*="-dropdown"]'], [], true);
    expect(hasOverlay(body([el(["div"], [open])]))).toBe(true);
  });

  it("does not let a hidden overlay mask a visible sibling", () => {
    // Both live in the shared portal node: the stale hidden one is found first.
    const shared = el(
      ["[data-mantine-shared-portal-node]"],
      [el(['[class*="-dropdown"]'], [], false), el(['[role="menu"]'], [], true)],
    );
    expect(hasOverlay(body([shared]))).toBe(true);
  });

  it("matches an overlay that is itself a direct child", () => {
    expect(hasOverlay(body([el(['[role="dialog"]'])]))).toBe(true);
    expect(hasOverlay(body([el(['[role="dialog"]'], [], false)]))).toBe(false);
  });

  it("ignores tooltips and notifications", () => {
    // Deliberate: they are transient and mostly pointer-triggered, so hiding a
    // pane for them would flicker it on every hover — and nobody needs to read
    // *through* a browser pane to a tooltip.
    const tip = el([".mantine-Tooltip-tooltip", '[role="tooltip"]']);
    expect(hasOverlay(body([el(["div"], [tip])]))).toBe(false);
  });
});
