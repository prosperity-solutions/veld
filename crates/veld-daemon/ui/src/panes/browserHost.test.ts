import { describe, expect, it } from "vitest";
import {
  type BrowserState,
  addressFor,
  isCommit,
  paneCovers,
  settlePending,
} from "./browserHost";

const state = (over: Partial<BrowserState> = {}): BrowserState => ({
  url: "",
  title: "",
  loading: false,
  canGoBack: false,
  canGoForward: false,
  pendingUrl: null,
  error: null,
  nested: null,
  profile: "default",
  loaded: false,
  emulationScale: 1,
  deviceX: 0,
  deviceY: 0,
  deviceWidth: 0,
  deviceHeight: 0,
  resizing: false,
  touchActive: false,
  mediaActive: false,
  safeAreaActive: false,
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

  it("covers a refused nested /ide, however far the page had got", () => {
    // The refusal screen is DOM, and under Electron the native view paints over
    // DOM — so if this predicate did not cover, the screen would be invisible in
    // the desktop app and visible in a browser tab. There is also no view behind
    // it at all (`ensure` does not create one while refused), so an uncovered
    // pane would simply be blank.
    expect(paneCovers(state({ url: "http://veld.localhost/ide", nested: "http://veld.localhost/ide" }))).toBe(true);
    // Outranks a loaded page, for the same reason an error does: it replaced one.
    expect(
      paneCovers(
        state({ url: "http://veld.localhost/ide", nested: "http://veld.localhost/ide", loaded: true }),
      ),
    ).toBe(true);
    // And the pane goes back to showing the page once it is forced through.
    expect(
      paneCovers(state({ url: "http://veld.localhost/ide", nested: null, loaded: true })),
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

/**
 * `pendingUrl` is the address bar's answer to "where are we going", and it exists
 * because `url` cannot be: the shell reads `url` off `webContents.getURL()`,
 * which keeps returning the page being *left* for the whole of a pending load.
 *
 * Its lifetime has two ends, and they are separate functions because they answer
 * separate questions: `settlePending` retires it when the load *stops*, however
 * it stopped, and `isCommit` retires it when the destination *arrives*. Neither
 * subsumes the other — a stopped load never commits, and a committed page can go
 * on loading subresources for seconds — so both are tested here.
 */
describe("settlePending", () => {
  it("keeps the destination while the load is in flight", () => {
    expect(settlePending(state({ loading: true, pendingUrl: "https://new/" }))).toBe(
      "https://new/",
    );
  });

  it("drops it the moment the load ends, however it ended", () => {
    // Stopped by the user, failed, or simply arrived — all three reach here as
    // `loading: false`, and all three mean the bar stops naming a destination.
    expect(settlePending(state({ loading: false, pendingUrl: "https://new/" }))).toBeNull();
    expect(
      settlePending(
        state({
          loading: false,
          pendingUrl: "https://new/",
          error: { kind: "load", code: -102, text: "refused", url: "https://new/" },
        }),
      ),
    ).toBeNull();
  });
});

describe("isCommit", () => {
  it("does not count the shell re-reporting the page being left", () => {
    // `did-start-loading` fires with `webContents.getURL()` still on the old page.
    // Reading that as a commit is the original bug, restated.
    expect(isCommit("https://old/", "https://old/")).toBe(false);
  });

  it("counts a commit to a URL nobody typed", () => {
    // The redirect case, which an equality test against the *typed* address
    // cannot see: three review angles landed on this line independently.
    expect(isCommit("https://www.example.com/", "https://example.com/")).toBe(true);
  });

  it("counts the first page in a view that has never reported one", () => {
    expect(isCommit("https://first/", "")).toBe(true);
  });

  it("ignores an event that says nothing about the URL", () => {
    // A title change, a devtools toggle: `onState` leaves `url` off the patch
    // entirely, and that must not retire a destination still in flight.
    expect(isCommit(undefined, "https://old/")).toBe(false);
  });
});

describe("addressFor", () => {
  it("shows the destination over the page being left", () => {
    expect(
      addressFor(state({ url: "https://old/", loading: true, pendingUrl: "https://new/" })),
    ).toBe("https://new/");
  });

  it("shows the committed page once nothing is pending", () => {
    expect(addressFor(state({ url: "https://old/" }))).toBe("https://old/");
  });

  it("keeps the URL that failed, rather than the page it was left on", () => {
    // A failed load never commits, so `url` is still the old page — while the
    // error screen under the bar names the one that failed. Showing both at once
    // is the contradiction this rung exists to prevent.
    expect(
      addressFor(
        state({
          url: "https://old/",
          error: { kind: "load", code: -102, text: "refused", url: "https://bad/" },
        }),
      ),
    ).toBe("https://bad/");
  });

  it("falls through an error that names no URL", () => {
    // A crashed renderer and a locally-raised failure both carry `url: ""`.
    expect(
      addressFor(
        state({
          url: "https://old/",
          error: { kind: "crash", code: null, text: "oom", url: "" },
        }),
      ),
    ).toBe("https://old/");
  });

  it("falls back to the tab's stored URL before any view has reported", () => {
    expect(addressFor(state(), "https://stored/")).toBe("https://stored/");
    expect(addressFor(state())).toBe("");
  });
});
