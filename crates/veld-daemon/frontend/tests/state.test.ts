// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { store, dispatch } from "../src/feedback-overlay/store";

describe("initState", () => {
  it("initializes all state fields", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    const hostEl = document.createElement("veld-feedback");
    initState(shadow, hostEl);

    expect(refs).toBeDefined();
    expect(refs.shadow).toBe(shadow);
    expect(refs.hostEl).toBe(hostEl);
    expect(store.threads).toEqual([]);
    expect(store.lastEventSeq).toBe(0);
    expect(store.lastSeenAt).toEqual({});
    expect(store.agentListening).toBe(false);
    expect(store.panelOpen).toBe(false);
    expect(store.activeMode).toBeNull();
    expect(store.toolbarOpen).toBe(false);
    expect(store.hidden).toBe(false);
    expect(store.shortcutsDisabled).toBe(false);
    expect(store.theme).toBe("auto");
    expect(store.pins).toEqual({});
    expect(store.captureStream).toBeNull();
    expect(store.drawLoaded).toBe(false);
  });

  it("state is mutable singleton", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow, document.createElement("div"));

    dispatch({ type: "ADD_THREAD", thread: { id: "t1" } as any });
    expect(store.threads.length).toBe(1);

    dispatch({ type: "SET_PANEL_OPEN", open: true });
    expect(store.panelOpen).toBe(true);
  });

  it("reinitializing resets state", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow, document.createElement("div"));
    dispatch({ type: "ADD_THREAD", thread: { id: "t1" } as any });

    // Reinitialize
    const shadow2 = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow2, document.createElement("div"));
    expect(store.threads).toEqual([]);
    expect(refs.shadow).toBe(shadow2);
  });
});
