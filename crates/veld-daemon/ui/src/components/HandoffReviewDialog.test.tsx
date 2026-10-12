import { fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { render } from "../shared/testRender";
import { HandoffReviewDialog } from "./dialogs";

/**
 * The review a handed-off prompt gets before its agent starts. What the dialog
 * is for is that the human can change their mind at three points — the text, the
 * start, and whether to be asked again — so each is pinned as a handler a
 * refactor could otherwise drop without failing anything typed.
 */
function open(over: Partial<Parameters<typeof HandoffReviewDialog>[0]> = {}) {
  const props = {
    worktree: "fix-login",
    agent: "Claude",
    prompt: "Fix the login timeout",
    onStart: vi.fn(),
    onDiscard: vi.fn(() => Promise.resolve()),
    onClose: vi.fn(),
    ...over,
  };
  render(<HandoffReviewDialog {...props} />);
  return props;
}

describe("HandoffReviewDialog", () => {
  test("starts with the prompt as edited, not as handed off", () => {
    const p = open();
    const box = screen.getByLabelText("Prompt");
    expect((box as HTMLTextAreaElement).value).toBe("Fix the login timeout");
    fireEvent.change(box, { target: { value: "Fix the login timeout, with a test" } });
    fireEvent.click(screen.getByRole("button", { name: "Start Claude" }));
    expect(p.onStart).toHaveBeenCalledWith("Fix the login timeout, with a test", false);
  });

  test("the always box is passed along with Start, and does nothing alone", () => {
    const p = open();
    fireEvent.click(screen.getByLabelText("Start prompts from agents without asking"));
    expect(p.onStart).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Start Claude" }));
    expect(p.onStart).toHaveBeenCalledWith("Fix the login timeout", true);
  });

  test("an emptied prompt cannot be started", () => {
    open();
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "   " } });
    expect((screen.getByRole("button", { name: "Start Claude" }) as HTMLButtonElement).disabled).toBe(true);
  });

  test("Not now closes without starting or discarding", () => {
    const p = open();
    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(p.onClose).toHaveBeenCalled();
    expect(p.onStart).not.toHaveBeenCalled();
    expect(p.onDiscard).not.toHaveBeenCalled();
  });

  test("Discard drops the hand-off", async () => {
    const p = open();
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(p.onDiscard).toHaveBeenCalled());
    expect(p.onStart).not.toHaveBeenCalled();
  });

  test("says when another agent stands in for the one asked for", () => {
    open({ substitutedFor: "codex" });
    expect(screen.getAllByText(/asked for the "codex" pane/).length).toBe(1);
  });
});
