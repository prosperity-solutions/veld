// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { initState, S } from "../src/feedback-overlay/state";

describe("initState", () => {
  it("initializes all state fields", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    const hostEl = document.createElement("veld-feedback");
    initState(shadow, hostEl);

    expect(S).toBeDefined();
    expect(S.shadow).toBe(shadow);
    expect(S.hostEl).toBe(hostEl);
    expect(S.threads).toEqual([]);
    expect(S.lastEventSeq).toBe(0);
    expect(S.lastSeenAt).toEqual({});
    expect(S.agentListening).toBe(false);
    expect(S.panelOpen).toBe(false);
    expect(S.activeMode).toBeNull();
    expect(S.toolbarOpen).toBe(false);
    expect(S.hidden).toBe(false);
    expect(S.shortcutsDisabled).toBe(false);
    expect(S.theme).toBe("auto");
    expect(S.pins).toEqual({});
    expect(S.captureStream).toBeNull();
    expect(S.drawLoaded).toBe(false);
  });

  it("state is mutable singleton", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow, document.createElement("div"));

    S.threads.push({ id: "t1" } as any);
    expect(S.threads.length).toBe(1);

    S.panelOpen = true;
    expect(S.panelOpen).toBe(true);
  });

  it("reinitializing resets state", () => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow, document.createElement("div"));
    S.threads.push({ id: "t1" } as any);

    // Reinitialize
    const shadow2 = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow2, document.createElement("div"));
    expect(S.threads).toEqual([]);
    expect(S.shadow).toBe(shadow2);
  });
});
