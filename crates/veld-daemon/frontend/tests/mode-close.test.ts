// @vitest-environment jsdom
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { initState } from "../src/feedback-overlay/state";
import { registerDeps } from "../src/shared/registry";
import { buildDOM } from "../src/feedback-overlay/dom";
import { getArc } from "../src/feedback-overlay/toolbar";
import { setMode } from "../src/feedback-overlay/modes";
import { hideOverlay } from "../src/feedback-overlay/visibility";
import { getState } from "../src/feedback-overlay/store";
import { makeFakeDeps } from "./test-helpers";

/**
 * Regression: the arc engine is the sole authority for open state. Entering a
 * full-screen mode (screenshot / draw) or hiding the overlay must actually
 * CLOSE the engine — not just flip a store bool — otherwise the menu stays
 * visually open (and the rAF loop runs forever) over the draw canvas / behind
 * the hidden overlay. See closeToolbar() wiring in modes.ts / visibility.ts.
 */
describe("mode entry / hide closes the arc engine", () => {
  beforeEach(() => {
    // jsdom has no rAF loop; make it a no-op scheduler so open() doesn't throw.
    vi.stubGlobal("requestAnimationFrame", () => 1 as unknown as number);
    vi.stubGlobal("cancelAnimationFrame", () => {});
    document.body.innerHTML = "";
    const host = document.createElement("veld-feedback");
    const shadow = host.attachShadow({ mode: "open" });
    document.body.appendChild(host);
    initState(shadow, host);
    registerDeps(makeFakeDeps());
    buildDOM();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("screenshot mode entry closes the open menu", () => {
    getArc()!.open();
    expect(getArc()!.isOpen()).toBe(true);
    setMode("screenshot");
    expect(getArc()!.isOpen()).toBe(false);
    expect(getState().toolbarOpen).toBe(false);
  });

  it("hideOverlay closes the open menu", () => {
    getArc()!.open();
    expect(getArc()!.isOpen()).toBe(true);
    hideOverlay();
    expect(getArc()!.isOpen()).toBe(false);
    expect(getState().toolbarOpen).toBe(false);
  });
});
