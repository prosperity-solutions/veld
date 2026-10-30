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

  test("Move to trash hands off to the trash flow", async () => {
    const p = open();
    fireEvent.click(screen.getByRole("button", { name: "Move to trash" }));
    await waitFor(() => expect(p.onDiscard).toHaveBeenCalled());
    expect(p.onStart).not.toHaveBeenCalled();
  });

  test("Start's menu starts another agent with the same prompt", async () => {
    const p = open({ others: [{ id: "codex", label: "Codex" }] });
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "Fix it" } });
    fireEvent.click(screen.getByRole("button", { name: "Start with another agent" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Start Codex" }));
    expect(p.onStart).toHaveBeenCalledWith("Fix it", false, "codex");
  });

  test("Start and its menu are one button group, not two buttons", () => {
    open({ others: [{ id: "codex", label: "Codex" }] });
    const start = screen.getByRole("button", { name: "Start Claude" });
    const menu = screen.getByRole("button", { name: "Start with another agent" });
    expect(menu.parentElement).toBe(start.parentElement);
  });

  test("no menu when the checkout has no other agent", () => {
    open();
    expect(screen.queryByRole("button", { name: "Start with another agent" })).toBeNull();
  });

  test("an emptied prompt cannot be started from the menu either", () => {
    open({ others: [{ id: "codex", label: "Codex" }] });
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "" } });
    const menu = screen.getByRole("button", { name: "Start with another agent" });
    expect((menu as HTMLButtonElement).disabled).toBe(true);
  });

  test("the footer runs Start, then Not now, then the destructive action", () => {
    open({ others: [{ id: "codex", label: "Codex" }] });
    const names = screen
      .getAllByRole("button")
      .map((b) => b.getAttribute("aria-label") ?? b.textContent)
      .filter((n) => n !== null && /trash|Not now|Start/.test(n));
    expect(names).toEqual([
      "Start Claude",
      "Start with another agent",
      "Not now",
      "Move to trash",
    ]);
  });

  test("says when another agent stands in for the one asked for", () => {
    open({ substitutedFor: "codex" });
    expect(screen.getAllByText(/asked for the "codex" pane/).length).toBe(1);
  });
});
