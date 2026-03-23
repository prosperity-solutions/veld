// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { updateBadge } from "../src/feedback-overlay/badge";
import { refs } from "../src/feedback-overlay/refs";
import { dispatch } from "../src/feedback-overlay/store";
import { PREFIX } from "../src/feedback-overlay/constants";
import { setupMockRefs, makeThread, makeMessage } from "./test-helpers";

describe("updateBadge", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("shows unread count on FAB badge when toolbar is closed", () => {
    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({
          id: "t1",
          messages: [makeMessage({ author: "agent" })],
        }),
      ],
    });
    dispatch({ type: "SET_TOOLBAR_OPEN", open: false });
    updateBadge();
    expect(refs.fabBadge.textContent).toBe("1");
    expect(refs.fabBadge.className).toBe(PREFIX + "badge");
  });

  it("hides FAB badge when no unread threads", () => {
    dispatch({ type: "SET_THREADS", threads: [makeThread()] });
    updateBadge();
    expect(refs.fabBadge.textContent).toBe("");
    expect(refs.fabBadge.className).toContain(PREFIX + "badge-hidden");
  });

  it("hides FAB badge when toolbar is open even with unread", () => {
    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({
          id: "t1",
          messages: [makeMessage({ author: "agent" })],
        }),
      ],
    });
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    updateBadge();
    expect(refs.fabBadge.className).toContain(PREFIX + "badge-hidden");
  });

  it("shows badge on comments button when toolbar is open", () => {
    dispatch({
      type: "SET_THREADS",
      threads: [
        makeThread({
          id: "t1",
          messages: [makeMessage({ author: "agent" })],
        }),
      ],
    });
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    updateBadge();
    const btnBadge = refs.toolBtnComments.querySelector("." + PREFIX + "tool-badge");
    expect(btnBadge).not.toBeNull();
    expect(btnBadge!.textContent).toBe("1");
  });

  it("hides comments button badge when no unread", () => {
    dispatch({ type: "SET_THREADS", threads: [] });
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    updateBadge();
    const btnBadge = refs.toolBtnComments.querySelector("." + PREFIX + "tool-badge");
    expect(btnBadge).toBeNull();
  });
});
