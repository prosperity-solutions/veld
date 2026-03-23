// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from "vitest";
import { addPin, removePin, renderAllPins, repositionPins, scheduleReposition } from "../src/feedback-overlay/pins";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { PREFIX } from "../src/feedback-overlay/constants";
import { setupMockRefs, makeThread, makeMessage } from "./test-helpers";

describe("addPin", () => {
  beforeEach(() => {
    setupMockRefs();
    // Mock window.location.pathname
    Object.defineProperty(window, "location", {
      value: { pathname: "/", href: "http://localhost/", port: "3000" },
      writable: true,
    });
  });

  it("creates pin element for open thread on current page", () => {
    const thread = makeThread({
      id: "t1",
      scope: {
        type: "element",
        page_url: "/",
        selector: "body",
        position: { x: 100, y: 200, width: 50, height: 30 },
      },
      messages: [makeMessage()],
    });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    addPin(thread);
    expect(getState().pins["t1"]).toBeDefined();
    const pin = getState().pins["t1"];
    expect(pin.classList.contains(PREFIX + "pin")).toBe(true);
  });

  it("positions pin at element top-right corner", () => {
    const thread = makeThread({
      id: "t1",
      scope: {
        type: "element",
        page_url: "/",
        selector: "body",
        position: { x: 100, y: 200, width: 50, height: 30 },
      },
    });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    addPin(thread);
    const pin = getState().pins["t1"];
    expect(pin.style.top).toBe("188px"); // 200 - 12
    expect(pin.style.left).toBe("138px"); // 100 + 50 - 12
  });

  it("skips resolved threads", () => {
    const thread = makeThread({
      id: "t1",
      status: "resolved",
      scope: {
        type: "element",
        page_url: "/",
        selector: "body",
        position: { x: 0, y: 0, width: 10, height: 10 },
      },
    });
    addPin(thread);
    expect(getState().pins["t1"]).toBeUndefined();
  });

  it("skips threads not on current page", () => {
    const thread = makeThread({
      id: "t1",
      scope: {
        type: "element",
        page_url: "/other-page",
        selector: "body",
        position: { x: 0, y: 0, width: 10, height: 10 },
      },
    });
    addPin(thread);
    expect(getState().pins["t1"]).toBeUndefined();
  });

  it("shows unread dot for unread threads", () => {
    const thread = makeThread({
      id: "t1",
      scope: {
        type: "element",
        page_url: "/",
        selector: "body",
        position: { x: 0, y: 0, width: 10, height: 10 },
      },
      messages: [makeMessage({ author: "agent" })],
    });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    addPin(thread);
    const pin = getState().pins["t1"];
    const dot = pin.querySelector("." + PREFIX + "pin-unread-dot");
    expect(dot).not.toBeNull();
  });

  it("shows message count badge when > 1 message", () => {
    const thread = makeThread({
      id: "t1",
      scope: {
        type: "element",
        page_url: "/",
        selector: "body",
        position: { x: 0, y: 0, width: 10, height: 10 },
      },
      messages: [makeMessage(), makeMessage()],
    });
    dispatch({ type: "SET_THREADS", threads: [thread] });
    addPin(thread);
    const pin = getState().pins["t1"];
    const count = pin.querySelector("." + PREFIX + "pin-count");
    expect(count).not.toBeNull();
    expect(count!.textContent).toBe("2");
  });
});

describe("removePin", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("removes pin from DOM and state", () => {
    const pin = document.createElement("div");
    document.body.appendChild(pin);
    dispatch({ type: "SET_PIN", threadId: "t1", el: pin });

    removePin("t1");
    expect(getState().pins["t1"]).toBeUndefined();
    expect(pin.parentNode).toBeNull();
  });

  it("does nothing for non-existent pin", () => {
    // Should not throw
    removePin("nonexistent");
  });
});

describe("renderAllPins", () => {
  beforeEach(() => {
    setupMockRefs();
    Object.defineProperty(window, "location", {
      value: { pathname: "/", href: "http://localhost/", port: "3000" },
      writable: true,
    });
  });

  it("clears existing pins and recreates for open threads", () => {
    const oldPin = document.createElement("div");
    document.body.appendChild(oldPin);
    dispatch({ type: "SET_PIN", threadId: "old", el: oldPin });

    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({
          id: "t1",
          scope: {
            type: "element",
            page_url: "/",
            selector: "body",
            position: { x: 0, y: 0, width: 10, height: 10 },
          },
        }),
      ],
    });

    renderAllPins();
    expect(getState().pins["old"]).toBeUndefined();
    expect(getState().pins["t1"]).toBeDefined();
  });
});

describe("repositionPins", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("hides pin when target element has zero dimensions (fix #6)", () => {
    const pin = document.createElement("div");
    pin.style.display = "";  // explicitly visible
    document.body.appendChild(pin);
    dispatch({ type: "SET_PIN", threadId: "t1", el: pin });

    const target = document.createElement("div");
    target.id = "target-el";
    document.body.appendChild(target);

    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({
          id: "t1",
          scope: {
            type: "element",
            page_url: "/",
            selector: "#target-el",
            position: { x: 100, y: 100, width: 50, height: 50 },
          },
        }),
      ],
    });

    // First: element is visible (non-zero) — pin should stay visible
    vi.spyOn(target, "getBoundingClientRect").mockReturnValue({
      x: 100, y: 100, width: 50, height: 50, top: 100, left: 100, right: 150, bottom: 150, toJSON: () => {},
    });
    repositionPins();
    expect(pin.style.display).toBe("");

    // Now: element collapses to zero — pin should hide
    vi.spyOn(target, "getBoundingClientRect").mockReturnValue({
      x: 0, y: 0, width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0, toJSON: () => {},
    });
    repositionPins();
    expect(pin.style.display).toBe("none");
  });

  it("hides pin when target element not found (fix #6)", () => {
    const pin = document.createElement("div");
    document.body.appendChild(pin);
    dispatch({ type: "SET_PIN", threadId: "t1", el: pin });
    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({
          id: "t1",
          scope: {
            type: "element",
            page_url: "/",
            selector: "#nonexistent",
            position: { x: 0, y: 0, width: 10, height: 10 },
          },
        }),
      ],
    });

    repositionPins();
    expect(pin.style.display).toBe("none");
  });

  it("shows pin when element becomes visible again", () => {
    const pin = document.createElement("div");
    pin.style.display = "none";
    document.body.appendChild(pin);
    dispatch({ type: "SET_PIN", threadId: "t1", el: pin });

    const target = document.createElement("div");
    target.id = "visible-el";
    document.body.appendChild(target);
    vi.spyOn(target, "getBoundingClientRect").mockReturnValue({
      x: 50, y: 100, width: 200, height: 40, top: 100, left: 50, right: 250, bottom: 140, toJSON: () => {},
    });

    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({
          id: "t1",
          scope: {
            type: "element",
            page_url: "/",
            selector: "#visible-el",
            position: { x: 50, y: 100, width: 200, height: 40 },
          },
        }),
      ],
    });

    repositionPins();
    expect(pin.style.display).toBe("");
    expect(pin.style.top).toBe("88px"); // 100 - 12
    expect(pin.style.left).toBe("238px"); // 50 + 200 - 12
  });
});

describe("scheduleReposition", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("sets rafPending and schedules RAF", () => {
    const rafSpy = vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 0);
    expect(getState().rafPending).toBe(false);
    scheduleReposition();
    expect(getState().rafPending).toBe(true);
    expect(rafSpy).toHaveBeenCalledOnce();
    rafSpy.mockRestore();
  });

  it("does not double-schedule when rafPending is true", () => {
    const rafSpy = vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 0);
    dispatch({ type: "SET_RAF_PENDING", pending: true });
    scheduleReposition();
    expect(rafSpy).not.toHaveBeenCalled();
    rafSpy.mockRestore();
  });
});
