// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { positionPopover, closeActivePopover, restoreComposer } from "../src/feedback-overlay/popover";
import { getComposerDraft, saveComposerDraft, type ComposerDraft } from "../src/feedback-overlay/persist";
import { PREFIX } from "../src/feedback-overlay/constants";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { registerDeps } from "../src/shared/registry";
import { vi } from "vitest";

function mockEl(): HTMLElement {
  return document.createElement("div");
}

function makeFakeDeps() {
  return {
    setMode: vi.fn(),
    toggleToolbar: vi.fn(),
    togglePanel: vi.fn(),
    togglePageComment: vi.fn(),
    hideOverlay: vi.fn(),
    showOverlay: vi.fn(),
    closeActivePopover: vi.fn(),
    addPin: vi.fn(),
    removePin: vi.fn(),
    renderAllPins: vi.fn(),
    renderPanel: vi.fn(),
    openThreadInPanel: vi.fn(),
    scrollToThread: vi.fn(),
    checkPendingScroll: vi.fn(),
    restoreSession: vi.fn(),
    updateBadge: vi.fn(),
    captureScreenshot: vi.fn(),
    showCreatePopover: vi.fn(),
    positionTooltip: vi.fn(),
  };
}

describe("positionPopover", () => {
  it("positions below anchor with gap", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 200, y: 100, width: 150, height: 40 });
    const top = parseFloat(pop.style.top!);
    // Should be below: y + height + gap (10)
    expect(top).toBeGreaterThanOrEqual(150); // 100 + 40 + 10
  });

  it("centers horizontally on anchor", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 400, y: 100, width: 200, height: 40 });
    const left = parseFloat(pop.style.left!);
    // Centered: x + width/2 - popWidth/2 = 400 + 100 - 180 = 320
    expect(left).toBeGreaterThan(200);
    expect(left).toBeLessThan(500);
  });

  it("clamps left position to minimum margin", () => {
    const pop = mockEl();
    // Anchor at far left — popover should not go negative
    positionPopover(pop, { x: 0, y: 100, width: 10, height: 10 });
    const left = parseFloat(pop.style.left!);
    expect(left).toBeGreaterThanOrEqual(0);
  });

  it("outputs pixel values", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 500, y: 200, width: 100, height: 50 });
    expect(pop.style.top).toMatch(/^\d+(\.\d+)?px$/);
    expect(pop.style.left).toMatch(/^\d+(\.\d+)?px$/);
  });
});

describe("closeActivePopover", () => {
  beforeEach(() => {
    const host = document.createElement("veld-feedback");
    const shadow = host.attachShadow({ mode: "open" });
    initState(shadow, host);
    refs.hoverOutline = document.createElement("div");
    refs.componentTraceEl = document.createElement("div");
    refs.toolBtnPageComment = document.createElement("div");
    refs.toolBtnScreenshot = document.createElement("div");
    registerDeps(makeFakeDeps());
  });

  it("removes popover element and nulls reference", () => {
    const pop = document.createElement("div");
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });

    closeActivePopover();

    expect(getState().activePopover).toBeNull();
    expect(pop.parentNode).toBeNull();
  });

  it("calls _veldCleanup if present", () => {
    const cleanup = vi.fn();
    const pop = document.createElement("div");
    (pop as any)._veldCleanup = cleanup;
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });

    closeActivePopover();

    expect(cleanup).toHaveBeenCalledOnce();
  });

  it("clears locked element and hides outlines", () => {
    const pop = document.createElement("div");
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });
    dispatch({ type: "SET_LOCKED", el: document.createElement("div") });
    refs.hoverOutline.style.display = "block";

    closeActivePopover();

    expect(getState().lockedEl).toBeNull();
    expect(refs.hoverOutline.style.display).toBe("none");
  });

  it("does nothing when no active popover", () => {
    dispatch({ type: "SET_POPOVER", popover: null });
    dispatch({ type: "SET_LOCKED", el: null });
    // Should not throw
    closeActivePopover();
    expect(getState().activePopover).toBeNull();
  });

  it("clears any persisted composer draft (dismiss must not resurrect on reload)", () => {
    sessionStorage.clear();
    saveComposerDraft({
      text: "typed then cancelled",
      isPage: true,
      selector: null,
      tagInfo: null,
      trace: null,
      elementText: null,
      sourceFile: null,
      sourceLine: null,
      rect: { x: 0, y: 0, width: 0, height: 0 },
    });
    closeActivePopover();
    expect(getComposerDraft()).toBeNull();
  });
});

describe("restoreComposer", () => {
  function draft(overrides: Partial<ComposerDraft> = {}): ComposerDraft {
    return {
      text: "half-typed comment",
      isPage: false,
      selector: "#target",
      tagInfo: "button.btn",
      trace: ["App", "Toolbar"],
      elementText: "Click me",
      sourceFile: "src/Toolbar.tsx",
      sourceLine: 42,
      rect: { x: 10, y: 20, width: 30, height: 40 },
      ...overrides,
    };
  }

  function textareaOf(): HTMLTextAreaElement | null {
    const pop = getState().activePopover;
    return pop ? pop.querySelector("textarea." + PREFIX + "textarea") : null;
  }

  beforeEach(() => {
    sessionStorage.clear();
    window.history.pushState({}, "", "/");
    document.body.innerHTML = "";
    const host = document.createElement("veld-feedback");
    const shadow = host.attachShadow({ mode: "open" });
    initState(shadow, host);
    refs.hoverOutline = document.createElement("div");
    refs.componentTraceEl = document.createElement("div");
    refs.toolBtnPageComment = document.createElement("div");
    refs.toolBtnScreenshot = document.createElement("div");
    registerDeps(makeFakeDeps());
  });

  it("re-anchors to the live element and refills the draft text", () => {
    const el = document.createElement("button");
    el.id = "target";
    document.body.appendChild(el);

    restoreComposer(draft());

    const ta = textareaOf();
    expect(ta).not.toBeNull();
    expect(ta!.value).toBe("half-typed comment");
    // Locked to the re-found element so the thread re-attaches to it on send.
    expect(getState().lockedEl).toBe(el);
    // Re-persisted (so a second reload before typing keeps the text).
    expect(getComposerDraft()?.text).toBe("half-typed comment");
  });

  it("keeps the draft + element scope when the element is gone (graceful degrade)", () => {
    // No #target in the DOM.
    restoreComposer(draft());

    const ta = textareaOf();
    expect(ta).not.toBeNull();
    expect(ta!.value).toBe("half-typed comment");
    // No live element to lock onto, but the popover still opened with the text.
    expect(getState().lockedEl).toBeNull();
    // The element selector is preserved so the thread still attaches on send.
    expect(getComposerDraft()?.selector).toBe("#target");
  });

  it("restores a page-scoped draft with no selector", () => {
    restoreComposer(draft({ isPage: true, selector: null, tagInfo: null, trace: null, elementText: null, sourceFile: null, sourceLine: null }));

    const ta = textareaOf();
    expect(ta!.value).toBe("half-typed comment");
    expect(getComposerDraft()?.isPage).toBe(true);
    expect(getComposerDraft()?.selector).toBeNull();
  });

  it("re-centres a page draft in the viewport instead of using a stale off-screen rect", () => {
    // A page comment saved while scrolled far down carries a huge document-Y.
    restoreComposer(draft({
      isPage: true, selector: null, tagInfo: null, trace: null,
      elementText: null, sourceFile: null, sourceLine: null,
      rect: { x: 0, y: 999999, width: 0, height: 0 },
    }));
    const pop = getState().activePopover!;
    const top = parseFloat(pop.style.top);
    // Must land within the current viewport, not at the stale y=999999.
    expect(top).toBeLessThan(window.scrollY + window.innerHeight);
  });

  it("falls back safely when the saved rect is non-finite", () => {
    restoreComposer(draft({
      selector: null, isPage: false, // element scope but no selector → uses rect
      rect: { x: NaN, y: NaN, width: 0, height: 0 },
    }));
    const pop = getState().activePopover!;
    expect(pop.style.top).toMatch(/^-?\d+(\.\d+)?px$/); // a real pixel value, not "NaNpx"
    expect(pop.style.top).not.toContain("NaN");
  });

  it("stops persisting once the app client-navigates away from the open-time URL", () => {
    const el = document.createElement("button");
    el.id = "target";
    document.body.appendChild(el);
    restoreComposer(draft());
    expect(getComposerDraft()?.text).toBe("half-typed comment"); // saved under "/"

    // SPA navigation to a different URL, composer still open.
    window.history.pushState({}, "", "/other?x=1");
    const ta = textareaOf()!;
    ta.value = "typed on the new page";
    ta.dispatchEvent(new Event("input", { bubbles: true }));

    // Nothing written under the new URL's key — the draft belongs to "/".
    expect(getComposerDraft()).toBeNull();
    window.history.pushState({}, "", "/");
    expect(getComposerDraft()?.text).toBe("half-typed comment");
  });

  it("dismissing after a client-navigation clears the draft's ORIGIN page, not the current one", () => {
    restoreComposer(draft({ isPage: true, selector: null, tagInfo: null, trace: null, elementText: null, sourceFile: null, sourceLine: null }));
    expect(getComposerDraft()?.text).toBe("half-typed comment"); // under "/"

    // App client-navigates away, composer still open, then the user cancels.
    window.history.pushState({}, "", "/elsewhere");
    closeActivePopover();

    // The cancelled composer's origin page ("/") must not resurrect on reload.
    window.history.pushState({}, "", "/");
    expect(getComposerDraft()).toBeNull();
  });
});
