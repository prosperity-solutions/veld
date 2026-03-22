import { describe, it, expect, beforeEach } from "vitest";
import { initStore, store, dispatch } from "../src/feedback-overlay/store";

describe("store + dispatch", () => {
  beforeEach(() => {
    initStore();
  });

  it("initializes with default values", () => {
    expect(store.threads).toEqual([]);
    expect(store.panelOpen).toBe(false);
    expect(store.activeMode).toBeNull();
    expect(store.theme).toBe("auto");
    expect(store.hidden).toBe(false);
  });

  it("SET_MODE updates activeMode", () => {
    dispatch({ type: "SET_MODE", mode: "draw" });
    expect(store.activeMode).toBe("draw");
    dispatch({ type: "SET_MODE", mode: null });
    expect(store.activeMode).toBeNull();
  });

  it("SET_TOOLBAR_OPEN toggles toolbar", () => {
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    expect(store.toolbarOpen).toBe(true);
    dispatch({ type: "SET_TOOLBAR_OPEN", open: false });
    expect(store.toolbarOpen).toBe(false);
  });

  it("SET_PANEL_OPEN toggles panel", () => {
    dispatch({ type: "SET_PANEL_OPEN", open: true });
    expect(store.panelOpen).toBe(true);
  });

  it("SET_PANEL_TAB switches tab", () => {
    dispatch({ type: "SET_PANEL_TAB", tab: "resolved" });
    expect(store.panelTab).toBe("resolved");
  });

  it("SET_EXPANDED_THREAD sets thread detail view", () => {
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: "abc" });
    expect(store.expandedThreadId).toBe("abc");
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: null });
    expect(store.expandedThreadId).toBeNull();
  });

  it("SET_HIDDEN hides/shows overlay", () => {
    dispatch({ type: "SET_HIDDEN", hidden: true });
    expect(store.hidden).toBe(true);
  });

  it("SET_THEME changes theme", () => {
    dispatch({ type: "SET_THEME", theme: "dark" });
    expect(store.theme).toBe("dark");
    dispatch({ type: "SET_THEME", theme: "light" });
    expect(store.theme).toBe("light");
  });

  it("SET_THREADS replaces threads array", () => {
    const threads = [{ id: "t1" }, { id: "t2" }] as any;
    dispatch({ type: "SET_THREADS", threads });
    expect(store.threads.length).toBe(2);
  });

  it("ADD_THREAD appends without mutating", () => {
    dispatch({ type: "SET_THREADS", threads: [{ id: "t1" }] as any });
    const before = store.threads;
    dispatch({ type: "ADD_THREAD", thread: { id: "t2" } as any });
    expect(store.threads.length).toBe(2);
    // Should be a new array (immutable)
    expect(store.threads).not.toBe(before);
  });

  it("SET_LISTENING updates agent listening state", () => {
    dispatch({ type: "SET_LISTENING", listening: true });
    expect(store.agentListening).toBe(true);
  });

  it("MARK_SEEN records timestamp", () => {
    dispatch({ type: "MARK_SEEN", threadId: "t1" });
    expect(store.lastSeenAt["t1"]).toBeGreaterThan(0);
  });

  it("SET_PIN and REMOVE_PIN manage pins", () => {
    const el = {} as HTMLElement;
    dispatch({ type: "SET_PIN", threadId: "t1", el });
    expect(store.pins["t1"]).toBe(el);

    dispatch({ type: "REMOVE_PIN", threadId: "t1" });
    expect(store.pins["t1"]).toBeUndefined();
  });

  it("CLEAR_PINS removes all pins", () => {
    dispatch({ type: "SET_PIN", threadId: "t1", el: {} as HTMLElement });
    dispatch({ type: "SET_PIN", threadId: "t2", el: {} as HTMLElement });
    dispatch({ type: "CLEAR_PINS" });
    expect(Object.keys(store.pins).length).toBe(0);
  });

  it("SET_FAB_POS updates position", () => {
    dispatch({ type: "SET_FAB_POS", cx: 100, cy: 200 });
    expect(store.fabCX).toBe(100);
    expect(store.fabCY).toBe(200);
  });

  it("SET_LAST_EVENT_SEQ updates sequence", () => {
    dispatch({ type: "SET_LAST_EVENT_SEQ", seq: 42 });
    expect(store.lastEventSeq).toBe(42);
  });

  it("SET_SHORTCUTS_DISABLED toggles shortcuts", () => {
    dispatch({ type: "SET_SHORTCUTS_DISABLED", disabled: true });
    expect(store.shortcutsDisabled).toBe(true);
  });

  it("dispatch is idempotent for unknown action", () => {
    const before = { ...store };
    dispatch({ type: "UNKNOWN_ACTION" } as any);
    expect(store.threads).toEqual(before.threads);
  });
});
