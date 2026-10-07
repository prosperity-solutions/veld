import { screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { api } from "../api";
import { render } from "../shared/testRender";
import { ChangedFilesModal } from "./ChangedFiles";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Changed files", () => {
  it("opens text files and disables the rows the file pane cannot show", async () => {
    vi.spyOn(api, "worktreeChanges").mockResolvedValue({
      base: "a".repeat(40),
      files: [
        { path: "docs/plan.md", status: "modified" },
        { path: "Dockerfile", status: "added" },
        { path: "assets/logo.png", status: "added" },
        { path: "LICENSE", status: "modified" },
        { path: "old.md", status: "deleted" },
      ],
    });
    const onOpen = vi.fn();
    render(<ChangedFilesModal worktreeId={1} opened onClose={() => {}} onOpen={onOpen} />);

    const row = async (title: string) => (await screen.findByTitle(title)) as HTMLButtonElement;
    expect((await row("Open docs/plan.md")).disabled).toBe(false);
    expect((await row("Open Dockerfile")).disabled).toBe(false);
    const refused = screen.getAllByTitle("Veld's file view shows text files only");
    expect(refused.map((b) => b.textContent)).toEqual([
      expect.stringContaining("logo.png"),
      expect.stringContaining("LICENSE"),
    ]);
    for (const b of refused) expect((b as HTMLButtonElement).disabled).toBe(true);
    expect((await row("old.md was deleted")).disabled).toBe(true);

    (await row("Open docs/plan.md")).click();
    expect(onOpen).toHaveBeenCalledWith("docs/plan.md");
  });
});
