// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from "vitest";
import { initStore, getState, dispatch } from "../src/feedback-overlay/store";

describe("store — panelSide", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("SET_PANEL_SIDE updates panelSide", () => {
    initStore();
    dispatch({ type: "SET_PANEL_SIDE", side: "left" });
    expect(getState().panelSide).toBe("left");
    dispatch({ type: "SET_PANEL_SIDE", side: "right" });
    expect(getState().panelSide).toBe("right");
  });

  it("defaults to right when localStorage is empty", () => {
    initStore();
    expect(getState().panelSide).toBe("right");
  });

  it("reads left from localStorage", () => {
    localStorage.setItem("veld-panel-side", "left");
    initStore();
    expect(getState().panelSide).toBe("left");
  });

  it("defaults to right for invalid localStorage value", () => {
    localStorage.setItem("veld-panel-side", "center");
    initStore();
    expect(getState().panelSide).toBe("right");
  });

  it("defaults to right for empty string localStorage value", () => {
    localStorage.setItem("veld-panel-side", "");
    initStore();
    expect(getState().panelSide).toBe("right");
  });
});
