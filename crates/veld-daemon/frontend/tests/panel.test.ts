// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  togglePanel,
  togglePanelSide,
  syncPanelSideClass,
  openThreadInPanel,
  renderPanel,
  showThreadDetail,
  showThreadList,
} from "../src/feedback-overlay/panel";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { PREFIX } from "../src/feedback-overlay/constants";
import { setupMockRefs, makeThread, makeMessage } from "./test-helpers";

describe("togglePanel", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("opens panel and adds panel-open class", () => {
    togglePanel();
    expect(getState().panelOpen).toBe(true);
    expect(refs.panel.classList.contains(PREFIX + "panel-open")).toBe(true);
  });

  it("closes panel and removes panel-open class", () => {
    togglePanel(); // open
    togglePanel(); // close
    expect(getState().panelOpen).toBe(false);
    expect(refs.panel.classList.contains(PREFIX + "panel-open")).toBe(false);
  });

  it("resets expandedThreadId when opening", () => {
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: "abc" });
    dispatch({ type: "SET_PANEL_OPEN", open: false });
    togglePanel();
    expect(getState().expandedThreadId).toBeNull();
  });
});

describe("togglePanelSide", () => {
  beforeEach(() => {
    localStorage.clear();
    setupMockRefs();
  });

  it("toggles from right to left", () => {
    togglePanelSide();
    expect(getState().panelSide).toBe("left");
    expect(refs.panel.classList.contains(PREFIX + "panel-left")).toBe(true);
  });

  it("toggles from left back to right", () => {
    togglePanelSide(); // right -> left
    togglePanelSide(); // left -> right
    expect(getState().panelSide).toBe("right");
    expect(refs.panel.classList.contains(PREFIX + "panel-left")).toBe(false);
  });

  it("persists to localStorage", () => {
    togglePanelSide();
    expect(localStorage.getItem("veld-panel-side")).toBe("left");
  });
});

describe("syncPanelSideClass", () => {
  beforeEach(() => {
    localStorage.clear();
    setupMockRefs();
  });

  it("applies panel-left class when state is left", () => {
    dispatch({ type: "SET_PANEL_SIDE", side: "left" });
    syncPanelSideClass();
    expect(refs.panel.classList.contains(PREFIX + "panel-left")).toBe(true);
  });

  it("removes panel-left class when state is right", () => {
    refs.panel.classList.add(PREFIX + "panel-left");
    dispatch({ type: "SET_PANEL_SIDE", side: "right" });
    syncPanelSideClass();
    expect(refs.panel.classList.contains(PREFIX + "panel-left")).toBe(false);
  });
});

describe("openThreadInPanel", () => {
  beforeEach(() => {
    setupMockRefs();
    dispatch({
      type: "SET_THREADS",
      threads: [makeThread({ id: "t1", messages: [makeMessage()] })],
    });
  });

  it("opens panel and expands thread", () => {
    openThreadInPanel("t1");
    expect(getState().panelOpen).toBe(true);
    expect(getState().expandedThreadId).toBe("t1");
    expect(getState().panelTab).toBe("active");
    expect(refs.panel.classList.contains(PREFIX + "panel-open")).toBe(true);
  });
});

describe("renderPanel — list view", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("shows active threads grouped by page", () => {
    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({ id: "t1", scope: { type: "page", page_url: "/" }, messages: [makeMessage()] }),
        makeThread({ id: "t2", scope: { type: "page", page_url: "/about" }, messages: [makeMessage()] }),
      ],
    });
    dispatch({ type: "SET_PANEL_TAB", tab: "active" });
    renderPanel();
    const sections = refs.panelBody.querySelectorAll("." + PREFIX + "panel-section");
    expect(sections.length).toBe(2);
  });

  it("removes panel-body-thread class in list view", () => {
    refs.panelBody.classList.add(PREFIX + "panel-body-thread");
    dispatch({ type: "SET_THREADS", threads: [] });
    renderPanel();
    expect(refs.panelBody.classList.contains(PREFIX + "panel-body-thread")).toBe(false);
  });

  it("shows empty message when no active threads", () => {
    dispatch({ type: "SET_THREADS", threads: [] });
    dispatch({ type: "SET_PANEL_TAB", tab: "active" });
    renderPanel();
    const empty = refs.panelBody.querySelector("." + PREFIX + "panel-empty");
    expect(empty).not.toBeNull();
    expect(empty!.textContent).toContain("No active threads");
  });

  it("shows resolved tab content", () => {
    dispatch({
      type: "SET_THREADS",
      threads: [makeThread({ status: "resolved", messages: [makeMessage()] })],
    });
    dispatch({ type: "SET_PANEL_TAB", tab: "resolved" });
    renderPanel();
    const cards = refs.panelBody.querySelectorAll("." + PREFIX + "thread-card");
    expect(cards.length).toBe(1);
  });
});

describe("renderPanel — detail view", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("adds panel-body-thread class for open thread (fix #1)", () => {
    const thread = makeThread({ id: "t1", status: "open", messages: [makeMessage()] });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: "t1" });
    renderPanel();
    expect(refs.panelBody.classList.contains(PREFIX + "panel-body-thread")).toBe(true);
  });

  it("does NOT add panel-body-thread for resolved thread", () => {
    const thread = makeThread({ id: "t1", status: "resolved", messages: [makeMessage()] });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: "t1" });
    renderPanel();
    expect(refs.panelBody.classList.contains(PREFIX + "panel-body-thread")).toBe(false);
  });

  it("shows back button and hides segmented control", () => {
    const thread = makeThread({ id: "t1", messages: [makeMessage()] });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: "t1" });
    renderPanel();
    expect(refs.panelBackBtn.style.display).not.toBe("none");
    expect(refs.panelHeadTitle.textContent).toBe("Thread");
  });

  it("renders messages in detail view", () => {
    const thread = makeThread({
      id: "t1",
      messages: [
        makeMessage({ body: "hello", author: "human" }),
        makeMessage({ body: "reply", author: "agent" }),
      ],
    });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: "t1" });
    renderPanel();
    const messages = refs.panelBody.querySelectorAll("." + PREFIX + "message");
    expect(messages.length).toBe(2);
  });

  it("falls back to list view if thread not found", () => {
    dispatch({ type: "SET_THREADS", threads: [] });
    dispatch({ type: "SET_EXPANDED_THREAD", threadId: "nonexistent" });
    renderPanel();
    expect(getState().expandedThreadId).toBeNull();
    expect(refs.panelBackBtn.style.display).toBe("none");
  });
});
