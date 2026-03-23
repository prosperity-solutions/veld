// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from "vitest";
import { showCreatePopover, closeActivePopover } from "../src/feedback-overlay/popover";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { PREFIX } from "../src/feedback-overlay/constants";
import { setupMockRefs, makeThread } from "./test-helpers";

// Mock the api module
vi.mock("../src/feedback-overlay/api", () => ({
  api: vi.fn(),
}));

import { api } from "../src/feedback-overlay/api";
const mockApi = vi.mocked(api);

describe("showCreatePopover", () => {
  let fakeDeps: ReturnType<typeof setupMockRefs>["deps"];

  beforeEach(() => {
    const env = setupMockRefs();
    fakeDeps = env.deps;
    mockApi.mockReset();
  });

  it("creates popover with textarea and send button", () => {
    showCreatePopover(
      { x: 100, y: 100, width: 50, height: 30 },
      ".some-el", null, null, null,
    );
    const popover = refs.shadow.querySelector("." + PREFIX + "popover");
    expect(popover).not.toBeNull();
    const textarea = popover!.querySelector("textarea");
    expect(textarea).not.toBeNull();
    const sendBtn = popover!.querySelector("." + PREFIX + "btn-primary");
    expect(sendBtn).not.toBeNull();
  });

  it("shows selector in popover when provided", () => {
    showCreatePopover(
      { x: 100, y: 100, width: 50, height: 30 },
      ".my-selector", null, null, null,
    );
    const selector = refs.shadow.querySelector("." + PREFIX + "popover-selector");
    expect(selector).not.toBeNull();
    expect(selector!.textContent).toBe(".my-selector");
  });

  it("send button click creates thread and calls setMode(null) (fix #3)", async () => {
    const thread = makeThread({ id: "t-new" });
    mockApi.mockResolvedValueOnce(thread);

    showCreatePopover(
      { x: 100, y: 100, width: 50, height: 30 },
      ".el", null, null, null,
    );

    const popover = refs.shadow.querySelector("." + PREFIX + "popover")!;
    const textarea = popover.querySelector("textarea")!;
    const sendBtn = popover.querySelector("." + PREFIX + "btn-primary") as HTMLButtonElement;

    textarea.value = "test feedback";
    sendBtn.click();

    await vi.waitFor(() => {
      expect(mockApi).toHaveBeenCalledWith(
        "POST",
        "/threads",
        expect.objectContaining({ message: "test feedback" }),
      );
    });

    await vi.waitFor(() => {
      // Regression: setMode(null) must be called after send
      expect(fakeDeps.setMode).toHaveBeenCalledWith(null);
      expect(fakeDeps.addPin).toHaveBeenCalled();
      expect(fakeDeps.updateBadge).toHaveBeenCalled();
    });
  });

  it("send button does nothing with empty text", () => {
    showCreatePopover(
      { x: 100, y: 100, width: 50, height: 30 },
      null, null, null, null,
    );

    const popover = refs.shadow.querySelector("." + PREFIX + "popover")!;
    const textarea = popover.querySelector("textarea")!;
    const sendBtn = popover.querySelector("." + PREFIX + "btn-primary") as HTMLButtonElement;

    textarea.value = "";
    sendBtn.click();

    expect(mockApi).not.toHaveBeenCalled();
  });

  it("cancel button closes popover", () => {
    showCreatePopover(
      { x: 100, y: 100, width: 50, height: 30 },
      null, null, null, null,
    );

    expect(getState().activePopover).not.toBeNull();
    const popover = refs.shadow.querySelector("." + PREFIX + "popover")!;
    const cancelBtn = popover.querySelector("." + PREFIX + "btn-secondary") as HTMLButtonElement;
    cancelBtn.click();

    expect(getState().activePopover).toBeNull();
  });

  it("send disables button during request", () => {
    // Use a promise that never resolves to check intermediate state
    mockApi.mockReturnValueOnce(new Promise(() => {}));

    showCreatePopover(
      { x: 100, y: 100, width: 50, height: 30 },
      null, null, null, null,
    );

    const popover = refs.shadow.querySelector("." + PREFIX + "popover")!;
    const textarea = popover.querySelector("textarea")!;
    const sendBtn = popover.querySelector("." + PREFIX + "btn-primary") as HTMLButtonElement;

    expect(sendBtn.disabled).toBe(false);
    textarea.value = "test";
    sendBtn.click();
    expect(sendBtn.disabled).toBe(true);
  });

  it("send failure re-enables button", async () => {
    mockApi.mockRejectedValueOnce(new Error("fail"));

    showCreatePopover(
      { x: 100, y: 100, width: 50, height: 30 },
      null, null, null, null,
    );

    const popover = refs.shadow.querySelector("." + PREFIX + "popover")!;
    const textarea = popover.querySelector("textarea")!;
    const sendBtn = popover.querySelector("." + PREFIX + "btn-primary") as HTMLButtonElement;

    textarea.value = "test";
    sendBtn.click();
    // Button should be disabled immediately
    expect(sendBtn.disabled).toBe(true);

    await vi.waitFor(() => {
      // After rejection, button should be re-enabled
      expect(sendBtn.disabled).toBe(false);
    });
  });
});
