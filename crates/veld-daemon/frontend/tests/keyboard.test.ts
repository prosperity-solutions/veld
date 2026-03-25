// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { onKeyDown } from "../src/feedback-overlay/keyboard";
import { dispatch, getState } from "../src/feedback-overlay/store";
import { setupMockRefs } from "./test-helpers";

function makeKeyEvent(code: string, extra: Partial<KeyboardEvent> = {}): KeyboardEvent {
  // jsdom navigator.platform is empty, so IS_MAC is false → modKey checks ctrlKey
  return new KeyboardEvent("keydown", {
    code,
    key: extra.key || code,
    ctrlKey: true,
    shiftKey: true,
    bubbles: true,
    cancelable: true,
    ...extra,
  });
}

describe("onKeyDown", () => {
  let deps: ReturnType<typeof setupMockRefs>["deps"];

  beforeEach(() => {
    const env = setupMockRefs();
    deps = env.deps;
  });

  it("Mod+Shift+F toggles select-element mode", () => {
    onKeyDown(makeKeyEvent("KeyF"));
    expect(deps.setMode).toHaveBeenCalledWith("select-element");
  });

  it("Mod+Shift+F opens toolbar if closed", () => {
    onKeyDown(makeKeyEvent("KeyF"));
    expect(deps.toggleToolbar).toHaveBeenCalled();
  });

  it("Mod+Shift+F does not toggle toolbar if already open", () => {
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    onKeyDown(makeKeyEvent("KeyF"));
    expect(deps.toggleToolbar).not.toHaveBeenCalled();
    expect(deps.setMode).toHaveBeenCalledWith("select-element");
  });

  it("Mod+Shift+F toggles off if already in select-element", () => {
    dispatch({ type: "SET_MODE", mode: "select-element" });
    onKeyDown(makeKeyEvent("KeyF"));
    expect(deps.setMode).toHaveBeenCalledWith(null);
  });

  it("Mod+Shift+S toggles screenshot mode", () => {
    onKeyDown(makeKeyEvent("KeyS"));
    expect(deps.setMode).toHaveBeenCalledWith("screenshot");
  });

  it("Mod+Shift+D toggles draw mode", () => {
    onKeyDown(makeKeyEvent("KeyD"));
    expect(deps.setMode).toHaveBeenCalledWith("draw");
  });

  it("Mod+Shift+P toggles page comment", () => {
    onKeyDown(makeKeyEvent("KeyP"));
    expect(deps.togglePageComment).toHaveBeenCalled();
  });

  it("Mod+Shift+C toggles panel", () => {
    onKeyDown(makeKeyEvent("KeyC"));
    expect(deps.togglePanel).toHaveBeenCalled();
  });

  it("Mod+Shift+V toggles toolbar", () => {
    onKeyDown(makeKeyEvent("KeyV"));
    expect(deps.toggleToolbar).toHaveBeenCalled();
  });

  it("Mod+Shift+. hides overlay", () => {
    onKeyDown(makeKeyEvent("Period"));
    expect(deps.hideOverlay).toHaveBeenCalled();
  });

  it("Mod+Shift+. shows overlay when hidden", () => {
    dispatch({ type: "SET_HIDDEN", hidden: true });
    onKeyDown(makeKeyEvent("Period"));
    expect(deps.showOverlay).toHaveBeenCalled();
  });

  it("Escape closes popover if active", () => {
    const pop = document.createElement("div");
    dispatch({ type: "SET_POPOVER", popover: pop });
    onKeyDown(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(deps.closeActivePopover).toHaveBeenCalled();
  });

  it("Escape clears active mode if no popover", () => {
    dispatch({ type: "SET_MODE", mode: "select-element" });
    onKeyDown(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(deps.setMode).toHaveBeenCalledWith(null);
  });

  it("Escape in draw mode is ignored", () => {
    dispatch({ type: "SET_MODE", mode: "draw" });
    onKeyDown(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(deps.setMode).not.toHaveBeenCalled();
    expect(deps.closeActivePopover).not.toHaveBeenCalled();
  });

  it("shortcuts disabled when shortcutsDisabled is true", () => {
    dispatch({ type: "SET_SHORTCUTS_DISABLED", disabled: true });
    onKeyDown(makeKeyEvent("KeyF"));
    expect(deps.setMode).not.toHaveBeenCalled();
  });

  it("shortcuts ignored when overlay is hidden (except V and Period)", () => {
    dispatch({ type: "SET_HIDDEN", hidden: true });
    onKeyDown(makeKeyEvent("KeyF"));
    expect(deps.setMode).not.toHaveBeenCalled();
  });

  it("Mod+Shift+F from draw mode switches to select-element", () => {
    dispatch({ type: "SET_MODE", mode: "draw" });
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    onKeyDown(makeKeyEvent("KeyF"));
    expect(deps.setMode).toHaveBeenCalledWith("select-element");
  });

  it("Mod+Shift+S from draw mode switches to screenshot", () => {
    dispatch({ type: "SET_MODE", mode: "draw" });
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    onKeyDown(makeKeyEvent("KeyS"));
    expect(deps.setMode).toHaveBeenCalledWith("screenshot");
  });
});
