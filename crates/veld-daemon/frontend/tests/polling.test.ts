// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from "vitest";
import { showAgentReplyToast, loadThreads } from "../src/feedback-overlay/polling";
import { refs } from "../src/feedback-overlay/refs";
import { dispatch, getState } from "../src/feedback-overlay/store";
import { PREFIX } from "../src/feedback-overlay/constants";
import { setupMockRefs, makeThread, makeMessage } from "./test-helpers";

// Mock the api module
vi.mock("../src/feedback-overlay/api", () => ({
  api: vi.fn(),
}));

import { api } from "../src/feedback-overlay/api";
const mockApi = vi.mocked(api);

describe("showAgentReplyToast", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("creates agent-toast element in shadow DOM", () => {
    showAgentReplyToast("t1", "Hello from agent");
    const toast = refs.shadow.querySelector("." + PREFIX + "agent-toast");
    expect(toast).not.toBeNull();
  });

  it("shows preview text in toast body", () => {
    showAgentReplyToast("t1", "Hello from agent");
    const body = refs.shadow.querySelector("." + PREFIX + "agent-toast-body");
    expect(body!.textContent).toBe("Hello from agent");
  });

  it("truncates long preview text", () => {
    const longText = "A".repeat(100);
    showAgentReplyToast("t1", longText);
    const body = refs.shadow.querySelector("." + PREFIX + "agent-toast-body");
    expect(body!.textContent!.length).toBeLessThan(100);
    expect(body!.textContent).toContain("...");
  });

  it("returns early when panelOpen is true (fix #4)", () => {
    dispatch({ type: "SET_PANEL_OPEN", open: true });
    showAgentReplyToast("t1", "Hello");
    const toast = refs.shadow.querySelector("." + PREFIX + "agent-toast");
    expect(toast).toBeNull();
  });

  it("shows toast when panelOpen is false", () => {
    dispatch({ type: "SET_PANEL_OPEN", open: false });
    showAgentReplyToast("t1", "Hello");
    const toast = refs.shadow.querySelector("." + PREFIX + "agent-toast");
    expect(toast).not.toBeNull();
  });

  it("includes Go to thread link", () => {
    showAgentReplyToast("t1", "Hello");
    const link = refs.shadow.querySelector("." + PREFIX + "agent-toast-link");
    expect(link).not.toBeNull();
    expect(link!.textContent).toContain("Go to thread");
  });
});

describe("loadThreads", () => {
  let fakeDeps: ReturnType<typeof setupMockRefs>["deps"];

  beforeEach(() => {
    const env = setupMockRefs();
    fakeDeps = env.deps;
    mockApi.mockReset();
  });

  it("fetches threads and dispatches SET_THREADS", async () => {
    const threads = [makeThread({ id: "t1" }), makeThread({ id: "t2" })];
    mockApi.mockResolvedValueOnce(threads);

    loadThreads();
    await vi.waitFor(() => {
      expect(getState().threads.length).toBe(2);
    });
  });

  it("calls renderAllPins and checkPendingScroll after loading", async () => {
    mockApi.mockResolvedValueOnce([]);

    loadThreads();
    await vi.waitFor(() => {
      expect(fakeDeps.renderAllPins).toHaveBeenCalled();
      expect(fakeDeps.checkPendingScroll).toHaveBeenCalled();
    });
  });

  it("handles API errors gracefully", async () => {
    mockApi.mockRejectedValueOnce(new Error("Network error"));

    // Should not throw
    loadThreads();
    // Wait a tick for the promise to settle
    await new Promise((r) => setTimeout(r, 10));
    // State unchanged
    expect(getState().threads).toEqual([]);
  });
});
