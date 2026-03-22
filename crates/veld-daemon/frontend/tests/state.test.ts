// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";

describe("initState", () => {
  it("initializes all state fields", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    const hostEl = document.createElement("veld-feedback");
    initState(shadow, hostEl);

    expect(refs).toBeDefined();
    expect(refs.shadow).toBe(shadow);
    expect(refs.hostEl).toBe(hostEl);
    expect(getState().threads).toEqual([]);
    expect(getState().lastEventSeq).toBe(0);
    expect(getState().lastSeenAt).toEqual({});
    expect(getState().agentListening).toBe(false);
    expect(getState().panelOpen).toBe(false);
    expect(getState().activeMode).toBeNull();
    expect(getState().toolbarOpen).toBe(false);
    expect(getState().hidden).toBe(false);
    expect(getState().shortcutsDisabled).toBe(false);
    expect(getState().theme).toBe("auto");
    expect(getState().pins).toEqual({});
    expect(getState().captureStream).toBeNull();
    expect(getState().drawLoaded).toBe(false);
  });

  it("state is mutable singleton", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow, document.createElement("div"));

    dispatch({ type: "ADD_THREAD", thread: { id: "t1" } as any });
    expect(getState().threads.length).toBe(1);

    dispatch({ type: "SET_PANEL_OPEN", open: true });
    expect(getState().panelOpen).toBe(true);
  });

  it("reinitializing resets state", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow, document.createElement("div"));
    dispatch({ type: "ADD_THREAD", thread: { id: "t1" } as any });

    // Reinitialize
    const shadow2 = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow2, document.createElement("div"));
    expect(getState().threads).toEqual([]);
    expect(refs.shadow).toBe(shadow2);
  });
});
