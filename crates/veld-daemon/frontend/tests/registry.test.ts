import { describe, it, expect, beforeEach } from "vitest";

// We need a fresh module for each test to reset the internal _deps state.
// vitest supports dynamic import + vi.resetModules for this.
import { vi } from "vitest";

describe("shared/registry", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("deps() throws before registerDeps() is called", async () => {
    const { deps } = await import("../src/shared/registry");
    expect(() => deps()).toThrow("deps not registered");
  });

  it("deps() returns the registered object after registerDeps()", async () => {
    const { deps, registerDeps } = await import("../src/shared/registry");
    const fakeDeps = makeFakeDeps();
    registerDeps(fakeDeps);
    expect(deps()).toBe(fakeDeps);
  });

  it("all fields are accessible on the returned deps", async () => {
    const { deps, registerDeps } = await import("../src/shared/registry");
    const fakeDeps = makeFakeDeps();
    registerDeps(fakeDeps);

    const d = deps();
    const keys: (keyof typeof fakeDeps)[] = [
      "setMode", "toggleToolbar", "togglePanel", "togglePageComment",
      "hideOverlay", "showOverlay", "closeActivePopover",
      "addPin", "removePin", "renderAllPins", "renderPanel",
      "openThreadInPanel", "scrollToThread", "checkPendingScroll",
      "updateBadge", "captureScreenshot", "showCreatePopover",
      "positionTooltip", "ensureDrawScript",
    ];

    for (const key of keys) {
      expect(typeof d[key]).toBe("function");
    }
    expect(keys.length).toBe(19);
  });

  it("deps functions are callable", async () => {
    const { deps, registerDeps } = await import("../src/shared/registry");
    const fakeDeps = makeFakeDeps();
    registerDeps(fakeDeps);

    // Call a few to make sure they don't throw
    deps().setMode(null);
    deps().toggleToolbar();
    deps().updateBadge();
    deps().captureScreenshot(0, 0, 100, 100);
  });
});

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
    updateBadge: vi.fn(),
    captureScreenshot: vi.fn(),
    showCreatePopover: vi.fn(),
    positionTooltip: vi.fn(),
    ensureDrawScript: vi.fn().mockResolvedValue(undefined),
  };
}
