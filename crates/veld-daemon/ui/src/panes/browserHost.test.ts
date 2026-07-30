import { describe, expect, it } from "vitest";
import { type BrowserState, paneCovers } from "./browserHost";

const state = (over: Partial<BrowserState> = {}): BrowserState => ({
  url: "",
  title: "",
  loading: false,
  canGoBack: false,
  canGoForward: false,
  error: null,
  profile: "default",
  loaded: false,
  emulationScale: 1,
  touchActive: false,
  devToolsOpen: false,
  ...over,
});

/**
 * `paneCovers` decides two things at once: which screen a browser pane renders,
 * and whether the native view is hidden. They have to be one decision — a native
 * view paints over DOM, so a disagreement is either a screen painted under a live
 * page or a pane that stays blank, and neither is observable in the browser build
 * (z-index works there). It was two restatements of the rule before; this pins it.
 */
describe("paneCovers", () => {
  it("covers a pane with nothing to show", () => {
    // No URL at all: the start page, listing the run's URLs.
    expect(paneCovers(state())).toBe(true);
    // A tab restored with a URL is not blank even before its view exists, which is
    // what the fallback is for — the pane's first render happens before mount.
    expect(paneCovers(state(), "http://x.test/")).toBe(false);
  });

  it("covers a first load, but not a reload", () => {
    // Opening: a spinner over nothing is the honest thing to show.
    expect(paneCovers(state({ url: "http://x.test/", loading: true }))).toBe(true);
    // Reloading: the page underneath is still worth looking at.
    expect(
      paneCovers(state({ url: "http://x.test/", loading: true, loaded: true })),
    ).toBe(false);
    expect(
      paneCovers(state({ url: "http://x.test/", loading: false, loaded: true })),
    ).toBe(false);
  });

  it("covers any error, however far the page had got", () => {
    const error = { kind: "load" as const, code: -102, text: "refused", url: "http://x.test/" };
    expect(paneCovers(state({ url: "http://x.test/", loaded: true, error }))).toBe(true);
    // An error outranks a loaded page: the error screen is the message, and the
    // view has to be out of the way for it to be visible at all.
    expect(paneCovers(state({ url: "http://x.test/", loading: true, error }))).toBe(true);
  });
});
