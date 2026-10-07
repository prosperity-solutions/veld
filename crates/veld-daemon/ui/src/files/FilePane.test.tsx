import { fireEvent, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { api, type FileText } from "../api";
import { fileTab } from "../panes/model";
import { render } from "../shared/testRender";
import { Virtualizer } from "@pierre/diffs";
import { FilePane, linePositionsOf, refusalText, revealScrollTop } from "./FilePane";

/**
 * The file pane against a mocked daemon. The code view (`@pierre/diffs`) is not
 * driven here — it measures layout jsdom does not have — so these cover the parts
 * that are this package's own: the refusals, the rendered Markdown and its block
 * references, the table, and adopting the daemon's spelling of a path.
 */

function text(path: string, body: string, extra: Partial<FileText> = {}): FileText {
  return {
    path,
    absPath: `/repo/${path}`,
    text: body,
    etag: "e1",
    mtimeMs: 1,
    size: body.length,
    insideWorktree: true,
    ...extra,
  };
}

// The code view's virtualizer watches rows with an IntersectionObserver, which
// jsdom does not have. Nothing here asserts on what it would report.
class NoIntersections {
  observe() {}
  unobserve() {}
  disconnect() {}
  takeRecords() {
    return [];
  }
}
globalThis.IntersectionObserver ??= NoIntersections as unknown as typeof IntersectionObserver;
// …and no scrolling, which a jump to a change asks for.
Element.prototype.scrollTo ??= () => {};
Element.prototype.scrollIntoView ??= () => {};

afterEach(() => {
  vi.restoreAllMocks();
});

describe("the file pane", () => {
  it("says why it cannot show a file", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({ kind: "error", status: 404, message: "no such file" });
    render(
      <FilePane
        tab={fileTab({ path: "bin/tool" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      refusalText(404, "no such file"),
    );
  });

  it("renders Markdown and copies a reference to the block you click", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("docs/plan.md", "# Plan\n\nStep one\nand two\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });

    render(
      <FilePane
        tab={fileTab({ path: "docs/plan.md" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    const para = await screen.findByText(/Step one/);
    fireEvent.click(para);
    fireEvent.click(screen.getByRole("button", { name: /Copy reference/ }));
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("docs/plan.md:3-4\n> Step one\n> and two"),
    );
  });

  it("copies the whole file as written, not as rendered, and not while it fails", async () => {
    // A prompt an agent wrote into Markdown: the clipboard gets the source.
    const body = "# Prompt\n\nDo **this**, then `that`.\n";
    vi.spyOn(api, "fileText").mockResolvedValue({ kind: "text", file: text("notes/p.md", body) });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const pane = (path: string) => (
      <FilePane
        tab={fileTab({ path })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />
    );

    const { unmount } = render(pane("notes/p.md"));
    // In the rendered view, which is where a `.md` file opens.
    await screen.findByText(/then/);
    fireEvent.click(screen.getByRole("button", { name: "Copy file contents" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(body));
    unmount();

    vi.spyOn(api, "fileText").mockResolvedValue({ kind: "error", status: 404, message: "gone" });
    render(pane("notes/gone.md"));
    await screen.findByRole("alert");
    expect(screen.getByRole("button", { name: "Copy file contents" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("opens a relative Markdown link in a file pane rather than navigating", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("docs/a.md", "See [b](./b.md#L4).\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    const onOpenFile = vi.fn();
    render(
      <FilePane
        tab={fileTab({ path: "docs/a.md" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={onOpenFile}
      />,
    );
    // One click, straight after the first paint. This flaked once, and the cause
    // was real: the rendered body re-set its `innerHTML` on every render (see
    // `MarkdownView`), so a re-render landing between find and click detached the
    // link it had found.
    fireEvent.click(await screen.findByText("b"));
    expect(onOpenFile).toHaveBeenCalledWith("docs/b.md", 4);
  });

  it("adopts the daemon's spelling of its path", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("notes/x.md", "hi\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    const onTab = vi.fn();
    render(
      <FilePane
        tab={fileTab({ path: "notes/../notes/x.md" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={onTab}
        onOpenFile={() => {}}
      />,
    );
    await waitFor(() => expect(onTab).toHaveBeenCalledWith({ path: "notes/x.md", title: "x.md" }));
  });

  it("shows a CSV as a sortable table", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("data/t.csv", "name,score\nada,10\nbob,2\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    render(
      <FilePane
        tab={fileTab({ path: "data/t.csv" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    const score = await screen.findByRole("button", { name: "score" });
    fireEvent.click(score);
    expect(score.getAttribute("aria-sort")).toBe("ascending");
  });

  it("copies a reference from text selected with the mouse", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("docs/plan.md", "# Plan\n\nfirst\n\nsecond\n\nthird\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(
      <FilePane
        tab={fileTab({ path: "docs/plan.md" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    const first = await screen.findByText("first");
    const second = screen.getByText("second");
    const range = document.createRange();
    range.setStart(first.firstChild as Node, 2);
    range.setEnd(second.firstChild as Node, 3);
    const sel = document.getSelection();
    sel?.removeAllRanges();
    sel?.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
    const button = screen.getByRole("button", { name: /Copy reference/ });
    await waitFor(() => expect(button.getAttribute("data-disabled")).toBeNull());
    fireEvent.click(button);
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("docs/plan.md:3-5\n> first\n>\n> second"),
    );
    sel?.removeAllRanges();
  });

  it("opens a file from Changed files at its first change, and steps through the rest", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("notes.txt", "a\nb\nc\nd\ne\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({
      added: [[4, 5]],
      modified: [[2, 2]],
      deleted: [],
    });
    const onTab = vi.fn();
    render(
      <FilePane
        tab={{ ...fileTab({ path: "notes.txt" }), jump: "change" }}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={onTab}
        onOpenFile={() => {}}
      />,
    );
    const at = () => document.querySelector(".file-pane-changes")?.getAttribute("data-at");
    await waitFor(() => expect(at()).toBe("1/2"));
    await waitFor(() => expect(onTab).toHaveBeenCalledWith({ jump: undefined }));
    fireEvent.click(screen.getByRole("button", { name: "Next change" }));
    expect(at()).toBe("2/2");
    fireEvent.click(screen.getByRole("button", { name: "Next change" }));
    expect(at()).toBe("1/2");
    fireEvent.click(screen.getByRole("button", { name: "Previous change" }));
    expect(at()).toBe("2/2");
  });

  it("writes the wrap setting from its toggle", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({ kind: "text", file: text("a.txt", "x\n") });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    const onWrapLines = vi.fn();
    render(
      <FilePane
        tab={fileTab({ path: "a.txt" })}
        worktreeId={1}
        wrapLines
        onWrapLines={onWrapLines}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Wrap long lines" }));
    expect(onWrapLines).toHaveBeenCalledWith(false);
    expect((screen.getByRole("button", { name: "Next change" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("keeps the change jumper to the Source view, and does not jump rendered Markdown", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("docs/plan.md", "# Plan\n\nnew line\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [[3, 3]], modified: [], deleted: [] });
    const onTab = vi.fn();
    render(
      <FilePane
        tab={{ ...fileTab({ path: "docs/plan.md" }), jump: "change" }}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={onTab}
        onOpenFile={() => {}}
      />,
    );
    await screen.findByText("new line");
    // The request is consumed rather than left waiting for a view switch.
    await waitFor(() => expect(onTab).toHaveBeenCalledWith({ jump: undefined }));
    expect(screen.queryByRole("button", { name: "Next change" })).toBeNull();
  });

  it("offers Show deletions only beside the change jumper, and writes the setting", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("docs/plan.md", "# Plan\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    const fileBase = vi.spyOn(api, "fileBase").mockResolvedValue({ base: "abc", text: "# Plan\n" });
    const onShowDeletions = vi.fn();
    render(
      <FilePane
        tab={fileTab({ path: "docs/plan.md" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions={false}
        onShowDeletions={onShowDeletions}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    await screen.findByText("Plan");
    // Rendered Markdown draws no changes, so there is nothing to show deletions in.
    expect(screen.queryByRole("button", { name: "Show deletions" })).toBeNull();
    fireEvent.click(screen.getByText("Source"));
    const toggle = await screen.findByRole("button", { name: "Show deletions" });
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(toggle);
    expect(onShowDeletions).toHaveBeenCalledWith(true);
    // Off is today's view: the base is never asked for.
    expect(fileBase).not.toHaveBeenCalled();
  });

  it("has no Show deletions for a file outside the worktree", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("/notes/a.txt", "x\n", { insideWorktree: false }),
    });
    render(
      <FilePane
        tab={fileTab({ path: "/notes/a.txt" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    await screen.findByRole("button", { name: "Wrap long lines" });
    expect(screen.queryByRole("button", { name: "Show deletions" })).toBeNull();
  });

  it("draws a diff against the base when on, with its own change stops", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("notes.txt", "a\nB\nc\nd\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({
      added: [[4, 4]],
      modified: [[2, 2]],
      deleted: [],
    });
    const fileBase = vi
      .spyOn(api, "fileBase")
      .mockResolvedValue({ base: "abc", text: "a\nb\nc\n" });
    render(
      <FilePane
        tab={{ ...fileTab({ path: "notes.txt" }), jump: "change" }}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    await waitFor(() => expect(fileBase).toHaveBeenCalledWith(1, "notes.txt"));
    await waitFor(() => expect(document.querySelector(".file-diff-scroll")).not.toBeNull());
    // Unified unless asked: one column, no split class.
    expect(document.querySelector(".file-diff-split")).toBeNull();
    await waitFor(() =>
      expect(
        document.querySelector(".file-diff-scroll diffs-container")?.shadowRoot?.querySelector(
          "code[data-unified]",
        ),
      ).toBeTruthy(),
    );
    const toggle = screen.getByRole("button", { name: "Show deletions" });
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    // b→B and the added d are two blocks of the diff.
    const at = () => document.querySelector(".file-pane-changes")?.getAttribute("data-at");
    await waitFor(() => expect(at()).toMatch(/\/2$/));
  });

  it("always offers Side by side, and turns the diff on with it", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({ kind: "text", file: text("notes.txt", "a\n") });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [], modified: [], deleted: [] });
    vi.spyOn(api, "fileBase").mockResolvedValue({ base: "abc", text: "a\n" });
    // Both toggles write settings the app reads back; this stands in.
    function Pane() {
      const [showDeletions, setShowDeletions] = useState(false);
      const [splitDiff, setSplitDiff] = useState(false);
      return (
        <FilePane
          tab={fileTab({ path: "notes.txt" })}
          worktreeId={1}
          wrapLines
          onWrapLines={() => {}}
          showDeletions={showDeletions}
          onShowDeletions={setShowDeletions}
          splitDiff={splitDiff}
          onSplitDiff={setSplitDiff}
          onTab={() => {}}
          onOpenFile={() => {}}
        />
      );
    }
    render(<Pane />);
    const pressed = (name: string) =>
      screen.getByRole("button", { name }).getAttribute("aria-pressed");
    const split = await screen.findByRole("button", { name: "Side by side" });
    expect(pressed("Side by side")).toBe("false");
    fireEvent.click(split);
    expect(pressed("Side by side")).toBe("true");
    expect(pressed("Show deletions")).toBe("true");
    // Back to inline: the diff stays on.
    fireEvent.click(screen.getByRole("button", { name: "Side by side" }));
    expect(pressed("Side by side")).toBe("false");
    expect(pressed("Show deletions")).toBe("true");
    // The diff off: Side by side is unlit, even with the setting still on.
    fireEvent.click(screen.getByRole("button", { name: "Side by side" }));
    fireEvent.click(screen.getByRole("button", { name: "Show deletions" }));
    expect(pressed("Side by side")).toBe("false");
  });

  it("lays the diff out side by side when that is on", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("notes.txt", "a\nB\nc\nd\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({
      added: [[4, 4]],
      modified: [[2, 2]],
      deleted: [],
    });
    vi.spyOn(api, "fileBase").mockResolvedValue({ base: "abc", text: "a\nb\nc\n" });
    render(
      <FilePane
        tab={fileTab({ path: "notes.txt" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions
        onShowDeletions={() => {}}
        splitDiff
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    await waitFor(() => expect(document.querySelector(".file-diff-split")).not.toBeNull());
    // The library's own markup for the layout: a column per side.
    const shadow = () => document.querySelector(".file-diff-split diffs-container")?.shadowRoot;
    await waitFor(() => expect(shadow()?.querySelector("code[data-additions]")).toBeTruthy());
    expect(shadow()?.querySelector("code[data-deletions]")).toBeTruthy();
    expect(shadow()?.querySelector("code[data-unified]")).toBeNull();
    expect(screen.getByRole("button", { name: "Side by side" }).getAttribute("aria-pressed")).toBe(
      "true",
    );
  });

  it("falls back to the plain view when the file has no base", async () => {
    vi.spyOn(api, "fileText").mockResolvedValue({
      kind: "text",
      file: text("new.txt", "fresh\n"),
    });
    vi.spyOn(api, "fileLineChanges").mockResolvedValue({ added: [[1, 1]], modified: [], deleted: [] });
    const fileBase = vi.spyOn(api, "fileBase").mockResolvedValue({ base: "abc", text: null });
    render(
      <FilePane
        tab={fileTab({ path: "new.txt" })}
        worktreeId={1}
        wrapLines
        onWrapLines={() => {}}
        showDeletions
        onShowDeletions={() => {}}
        splitDiff={false}
        onSplitDiff={() => {}}
        onTab={() => {}}
        onOpenFile={() => {}}
      />,
    );
    await waitFor(() => expect(fileBase).toHaveBeenCalled());
    await waitFor(() => expect(document.querySelector(".file-code-scroll")).not.toBeNull());
    expect(document.querySelector(".file-diff-scroll")).toBeNull();
  });
});

describe("revealing a line", () => {
  it("puts it two rows below the top, within what the scroller can reach", () => {
    expect(revealScrollTop(1000, 300, 5000)).toBe(960);
    // Already on screen is no reason to stay put: a press brings it up.
    expect(revealScrollTop(1150, 300, 5000)).toBe(1110);
    // Near the top: no scrolling above the content.
    expect(revealScrollTop(50, 300, 5000)).toBe(10);
    expect(revealScrollTop(30, 300, 5000)).toBe(0);
    // Near the bottom: no further than the last screenful.
    expect(revealScrollTop(4950, 300, 5000)).toBe(4700);
    // Content shorter than the view: nowhere to go.
    expect(revealScrollTop(100, 300, 200)).toBe(0);
  });

  // The instance is reached through the virtualizer's private registry — this is
  // the test that trips when an `@pierre/diffs` upgrade moves it.
  it("finds the instance drawing into a file element through the virtualizer", () => {
    globalThis.ResizeObserver ??= NoIntersections as unknown as typeof ResizeObserver;
    const virtualizer = new Virtualizer();
    const root = document.createElement("div");
    root.append(document.createElement("div"));
    virtualizer.setup(root);
    const host = document.createElement("diffs-container");
    const instance = {
      getLinePosition: () => ({ top: 40, height: 20 }),
      onRender: () => false,
      reconcileHeights: () => false,
      setVisibility: () => {},
    };
    virtualizer.connect(host, instance);
    expect(linePositionsOf(virtualizer, host)).toBe(instance);
    expect(linePositionsOf(virtualizer, document.createElement("div"))).toBeUndefined();
    expect(linePositionsOf(undefined, host)).toBeUndefined();
    virtualizer.cleanUp();
  });
});
