// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import {
  saveComposerDraft,
  clearComposerDraft,
  getComposerDraft,
  saveReplyDraft,
  clearReplyDraft,
  getReplyDraft,
  savePanelState,
  getPanelState,
  clearSession,
  pruneReplyDrafts,
  type ComposerDraft,
} from "../src/feedback-overlay/persist";
import { initState } from "../src/feedback-overlay/state";
import { dispatch } from "../src/feedback-overlay/store";

function makeComposer(overrides: Partial<ComposerDraft> = {}): ComposerDraft {
  return {
    text: "hello",
    isPage: false,
    selector: "div > button:nth-child(2)",
    tagInfo: "button.btn",
    trace: ["App", "Toolbar"],
    elementText: "Click me",
    sourceFile: "src/Toolbar.tsx",
    sourceLine: 42,
    rect: { x: 10, y: 20, width: 30, height: 40 },
    ...overrides,
  };
}

describe("persist", () => {
  beforeEach(() => {
    sessionStorage.clear();
    window.history.pushState({}, "", "/");
    const host = document.createElement("veld-feedback");
    const shadow = host.attachShadow({ mode: "open" });
    initState(shadow, host);
  });

  describe("composer draft", () => {
    it("round-trips a saved draft", () => {
      const draft = makeComposer();
      saveComposerDraft(draft);
      expect(getComposerDraft()).toEqual(draft);
    });

    it("returns null when none saved", () => {
      expect(getComposerDraft()).toBeNull();
    });

    it("clears a saved draft", () => {
      saveComposerDraft(makeComposer());
      clearComposerDraft();
      expect(getComposerDraft()).toBeNull();
    });
  });

  describe("reply drafts", () => {
    it("round-trips per thread", () => {
      saveReplyDraft("t-1", "reply one");
      saveReplyDraft("t-2", "reply two");
      expect(getReplyDraft("t-1")).toBe("reply one");
      expect(getReplyDraft("t-2")).toBe("reply two");
    });

    it("empty text clears the entry rather than storing it", () => {
      saveReplyDraft("t-1", "typed");
      saveReplyDraft("t-1", "");
      expect(getReplyDraft("t-1")).toBe("");
    });

    it("clearReplyDraft removes one without touching others", () => {
      saveReplyDraft("t-1", "a");
      saveReplyDraft("t-2", "b");
      clearReplyDraft("t-1");
      expect(getReplyDraft("t-1")).toBe("");
      expect(getReplyDraft("t-2")).toBe("b");
    });

    it("pruneReplyDrafts keeps only the given thread ids", () => {
      saveReplyDraft("t-live", "keep");
      saveReplyDraft("t-gone", "drop");
      pruneReplyDrafts(["t-live"]);
      expect(getReplyDraft("t-live")).toBe("keep");
      expect(getReplyDraft("t-gone")).toBe("");
    });

    it("returns empty string for an unknown thread", () => {
      expect(getReplyDraft("nope")).toBe("");
    });
  });

  describe("panel state", () => {
    it("snapshots open/tab/expanded from the store", () => {
      dispatch({ type: "SET_PANEL_OPEN", open: true });
      dispatch({ type: "SET_PANEL_TAB", tab: "resolved" });
      dispatch({ type: "SET_EXPANDED_THREAD", threadId: "t-9" });
      savePanelState();
      const p = getPanelState();
      expect(p.open).toBe(true);
      expect(p.tab).toBe("resolved");
      expect(p.expandedThreadId).toBe("t-9");
    });

    it("defaults sensibly when nothing is stored", () => {
      const p = getPanelState();
      expect(p).toEqual({ open: false, tab: "active", expandedThreadId: null, scrollTop: 0 });
    });
  });

  describe("scoping + lifecycle", () => {
    it("keys drafts per pathname — a draft on one page does not leak to another", () => {
      saveComposerDraft(makeComposer({ text: "page A draft" }));
      window.history.pushState({}, "", "/other");
      expect(getComposerDraft()).toBeNull();
      window.history.pushState({}, "", "/");
      expect(getComposerDraft()?.text).toBe("page A draft");
    });

    it("keys drafts per query string — hash/query-routed SPA pages don't collide", () => {
      window.history.pushState({}, "", "/view?id=1");
      saveComposerDraft(makeComposer({ text: "draft for id=1" }));
      window.history.pushState({}, "", "/view?id=2");
      expect(getComposerDraft()).toBeNull();
      window.history.pushState({}, "", "/view?id=1");
      expect(getComposerDraft()?.text).toBe("draft for id=1");
    });

    it("clearSession wipes composer + replies for the current page", () => {
      saveComposerDraft(makeComposer());
      saveReplyDraft("t-1", "reply");
      clearSession();
      expect(getComposerDraft()).toBeNull();
      expect(getReplyDraft("t-1")).toBe("");
    });

    it("clearSession wipes drafts on OTHER pages too (Done ends the session globally)", () => {
      saveComposerDraft(makeComposer({ text: "draft on /" }));
      window.history.pushState({}, "", "/other");
      saveComposerDraft(makeComposer({ text: "draft on /other" }));
      clearSession();
      expect(getComposerDraft()).toBeNull();
      window.history.pushState({}, "", "/");
      expect(getComposerDraft()).toBeNull();
    });

    it("ignores a stored blob written under a different schema version", () => {
      sessionStorage.setItem(
        "veld-feedback-session:/",
        JSON.stringify({ v: 999, composer: makeComposer() }),
      );
      expect(getComposerDraft()).toBeNull();
    });

    it("survives a corrupt (non-JSON) blob", () => {
      sessionStorage.setItem("veld-feedback-session:/", "{not json");
      expect(getComposerDraft()).toBeNull();
      expect(getPanelState().open).toBe(false);
    });
  });
});
