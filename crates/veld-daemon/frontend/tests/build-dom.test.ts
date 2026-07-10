// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { registerDeps } from "../src/shared/registry";
import { buildDOM } from "../src/feedback-overlay/dom";
import { makeFakeDeps } from "./test-helpers";

/**
 * Smoke test for the real buildDOM() (as opposed to the mock refs used
 * elsewhere). Guards against dropping a DOM element during refactors — a
 * missing ref (e.g. screenshotRect) surfaces as a null-deref at runtime, not
 * in the mock-based unit tests.
 */
describe("buildDOM", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    const host = document.createElement("veld-feedback");
    const shadow = host.attachShadow({ mode: "open" });
    document.body.appendChild(host);
    initState(shadow, host);
    registerDeps(makeFakeDeps());
  });

  it("initializes every DOM ref", () => {
    buildDOM();
    const required = [
      "toolbarContainer", "fab", "fabBadge", "toolbar", "toolBtnSelect",
      "toolBtnScreenshot", "toolBtnPageComment", "toolBtnComments",
      "toolBtnHide", "toolbarOverflow", "listeningModule", "moreBtn", "lightRoot",
      "overlay",
      "hoverOutline", "componentTraceEl", "screenshotRect", "panel", "panelBody",
      "panelHeadTitle", "panelBackBtn", "markReadBtn", "segBtnActive",
      "segBtnResolved", "tooltip",
    ] as const;
    for (const key of required) {
      expect(refs[key], `refs.${key} should be initialized`).toBeTruthy();
    }
    expect(refs.radialButtons.length).toBeGreaterThan(0);
    expect(refs.overflowButtons.length).toBeGreaterThan(0);
  });

  it("puts the theme attribute on lightRoot, never on <html>", () => {
    // Regression guard: setting data-veld-theme on document.documentElement
    // mutated the host app's SSR-owned <html>, causing React hydration
    // mismatches in Next.js. The attribute must live on our own light root.
    buildDOM();
    expect(refs.lightRoot.hasAttribute("data-veld-theme")).toBe(true);
    expect(document.documentElement.hasAttribute("data-veld-theme")).toBe(false);
  });

  it("re-parents light-DOM elements under lightRoot, not straight into <body>", () => {
    buildDOM();
    // Theme override CSS scopes to `.veld-feedback-light-root[data-veld-theme]`,
    // so themed light elements must inherit through lightRoot.
    expect(refs.overlay.parentElement).toBe(refs.lightRoot);
    expect(refs.screenshotBanner.parentElement).toBe(refs.lightRoot);
    expect(refs.componentTraceEl.parentElement).toBe(refs.lightRoot);
  });

  it("screenshot mode teardown does not throw", async () => {
    buildDOM();
    const { setMode } = await import("../src/feedback-overlay/modes");
    setMode("screenshot");
    // Teardown touches refs.screenshotRect — must exist.
    expect(() => setMode(null)).not.toThrow();
  });
});
