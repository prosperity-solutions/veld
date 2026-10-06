import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { PaneSpec } from "../api";
import { render } from "../shared/testRender";
import { NewWorktreeDialog } from "./dialogs";

/**
 * The New worktree dialog's prompt attachments, rendered.
 *
 * `ide/promptAttachments.test.ts` pins what is kept and what is refused. What it
 * cannot reach is the wiring, which is all JSX and event handlers: that the
 * *panel* takes a drop, that a pasted screenshot becomes a row instead of
 * nothing, that the picker adds, that a row's ✕ takes it back, and that files
 * alone are enough to launch the agent — each one an `onX` a refactor can drop
 * without failing anything typed.
 */

// The dialog fetches its branch list and marker choices on mount. Branches
// resolve (empty) because Create stays disabled until they have; the marker
// lists never do, which leaves the daemon's own assignment in charge — a state
// the dialog already handles.
vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return {
    ...real,
    api: {
      repoBranches: () => Promise.resolve({ local: [], remote: [] }),
      worktreeEmoji: () => new Promise(() => {}),
      worktreeColors: () => new Promise(() => {}),
    },
  };
});

const claude = {
  id: "claude",
  label: "Claude",
  kind: "terminal",
  available: true,
  can_resume: false,
  has_sessions: false,
  agent: true,
  auto_resume: false,
  close_on_exit: false,
  fixed_label: true,
} as PaneSpec;

type Body = Parameters<Parameters<typeof NewWorktreeDialog>[0]["onCreate"]>[0];

function open(pathOf: (file: File) => string | null = () => null) {
  const onCreate = vi.fn<(body: Body) => Promise<void>>(() => Promise.resolve());
  render(
    <NewWorktreeDialog
      onCreate={onCreate}
      repoRoot="/repo"
      sources={[]}
      takenAliases={[]}
      agents={[claude]}
      newMode="prompt"
      onNewModeChange={() => {}}
      generatesNames={false}
      rememberedAgent=""
      promptDraft=""
      onPromptDraft={() => {}}
      onAgentPicked={() => {}}
      pathOf={pathOf}
      lane=""
      usedBy={{}}
      colorUsedBy={{}}
      markerStyle="emoji"
      onStyleChange={() => {}}
      createFrom="origin"
      onClose={() => {}}
    />,
  );
  return { onCreate };
}

/** The panel — exactly one element, which is what styling it by attribute buys. */
const surface = () => {
  expect(document.querySelectorAll("[data-drop-surface]")).toHaveLength(1);
  const el = document.querySelector("[data-drop-surface]");
  if (!el) throw new Error("the dialog panel is not rendered");
  return el;
};

const png = (name = "shot.png") => new File([new Uint8Array([1, 2, 3])], name, { type: "image/png" });
const json = () => new File(['{"a":1}'], "data.json", { type: "application/json" });

/** The `DataTransfer` a file drag carries, as far as the handlers read it. */
const fileDrag = (files: File[]) => ({ dataTransfer: { types: ["Files"], files, dropEffect: "none" } });

// jsdom has no `document.fonts`, and the prompt's autosizing `Textarea` listens
// on it for webfonts finishing loading. Local to this file: it is the first
// test to render that field.
if (!("fonts" in document)) {
  Object.defineProperty(document, "fonts", {
    value: { addEventListener() {}, removeEventListener() {} },
  });
}

beforeEach(() => {
  // jsdom has no object URLs; the thumbnail only needs *a* URL to render.
  URL.createObjectURL = vi.fn(() => "blob:test");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  vi.restoreAllMocks();
});

test("this file runs in the `dom` project, with a DOM", () => {
  expect(typeof document).toBe("object");
});

describe("NewWorktreeDialog attachments", () => {
  test("a drop anywhere on the panel attaches, with the veil shown while dragging", () => {
    open((f) => `/Users/me/${f.name}`);
    // The header is part of the panel, so it is as good a target as the field.
    const title = screen.getByText("New worktree");
    fireEvent.dragEnter(title, fileDrag([]));
    expect(screen.getByText("Drop to attach to the prompt")).toBeTruthy();
    fireEvent.drop(title, fileDrag([png(), json()]));
    expect(screen.queryByText("Drop to attach to the prompt")).toBeNull();

    expect(screen.getByText("shot.png")).toBeTruthy();
    expect(screen.getByText("data.json")).toBeTruthy();
    // The image is shown as itself, the JSON as its name.
    expect(surface().querySelectorAll("img.prompt-attachment-thumb")).toHaveLength(1);
    expect(surface().querySelectorAll(".prompt-attachment.is-image")).toHaveLength(1);
  });

  test("the veil survives the drag crossing from one child to the next", () => {
    open();
    const field = screen.getByRole("textbox", { name: "Prompt" });
    const title = screen.getByText("New worktree");
    // Enter the next child before leaving the last, the order browsers use.
    fireEvent.dragEnter(title, fileDrag([]));
    fireEvent.dragEnter(field, fileDrag([]));
    fireEvent.dragLeave(title, fileDrag([]));
    expect(screen.getByText("Drop to attach to the prompt")).toBeTruthy();
    fireEvent.dragLeave(field, fileDrag([]));
    expect(screen.queryByText("Drop to attach to the prompt")).toBeNull();
  });

  test("a drag that is not files is left alone", () => {
    open();
    const event = fireEvent.dragOver(surface(), { dataTransfer: { types: ["text/plain"], files: [] } });
    // `true` means nothing called `preventDefault`: the window guard decides.
    expect(event).toBe(true);
    expect(screen.queryByText("Drop to attach to the prompt")).toBeNull();
  });

  test("every tile shows its size; a browser tab says once that paths are hidden", () => {
    open();
    fireEvent.drop(surface(), fileDrag([json(), png()]));
    // A file leads with its name and size; an image is the picture, with its
    // type and size on the chip and its name kept for a screen reader.
    expect(screen.getByText("7 B")).toBeTruthy();
    expect(screen.getByText("JSON")).toBeTruthy();
    expect(screen.getByText("PNG · 3 B")).toBeTruthy();
    expect(screen.getByText("shot.png")).toBeTruthy();
    expect(
      screen.getAllByText("This browser tab can't see where files live, so the agent gets a copy of each."),
    ).toHaveLength(1);
  });

  test("a desktop file's path is on its tile's tooltip, and there is no browser note", () => {
    open((f) => `/Users/me/${f.name}`);
    fireEvent.drop(surface(), fileDrag([json()]));
    const tile = screen.getByText("data.json").closest("li");
    expect(tile?.getAttribute("title")).toBe("data.json\n7 B\n/Users/me/data.json");
    expect(screen.queryByText(/This browser tab can't see/)).toBeNull();
  });

  // A macOS screenshot dragged from its corner thumbnail lives in a temporary
  // folder that is gone by the time the agent reads it — the agent then gets
  // the path as plain text. So its bytes are read at drop time and uploaded.
  test("a temporary file is copied at drop time and travels without its path", async () => {
    const temp = "/var/folders/x1/abc/T/TemporaryItems/NSIRD_screencaptureui_A/Bildschirmfoto.png";
    const { onCreate } = open(() => temp);
    fireEvent.drop(surface(), fileDrag([png("Bildschirmfoto.png")]));
    await waitFor(() =>
      expect(screen.getByText("Bildschirmfoto.png").closest("li")?.getAttribute("title")).toBe(
        `Bildschirmfoto.png\n3 B\nCopied from a temporary file: ${temp}`,
      ),
    );
    expect(screen.queryByText(/This browser tab can't see/)).toBeNull();

    const create = screen.getByRole("button", { name: "Create worktree" });
    await waitFor(() => expect((create as HTMLButtonElement).disabled).toBe(false));
    await act(async () => {
      fireEvent.click(create);
    });
    const body = onCreate.mock.calls[0][0];
    expect(body.attachments?.map((a) => [a.file.name, a.file.size, a.path])).toEqual([
      ["Bildschirmfoto.png", 3, null],
    ]);
  });

  test("a pasted screenshot becomes an attachment, a pasted sentence does not", () => {
    open();
    const field = screen.getByRole("textbox", { name: "Prompt" });
    const shot = new File([new Uint8Array([1])], "", { type: "image/png" });
    fireEvent.paste(field, {
      clipboardData: { items: [{ kind: "file", type: "image/png", getAsFile: () => shot }] },
    });
    // Unnamed clipboard bytes are named by their type.
    expect(screen.getByText("pasted-image.png")).toBeTruthy();

    fireEvent.paste(field, {
      clipboardData: {
        items: [
          { kind: "string", type: "text/plain", getAsFile: () => null },
          { kind: "file", type: "image/png", getAsFile: () => png("other.png") },
        ],
      },
    });
    expect(screen.queryByText("other.png")).toBeNull();
  });

  test("Add files picks files, and a row's remove button takes one back", () => {
    open();
    const input = document.querySelector<HTMLInputElement>('input[type="file"]');
    if (!input) throw new Error("no file input behind Add files");
    expect(input.multiple).toBe(true);
    Object.defineProperty(input, "files", { value: [png(), json()], configurable: true });
    fireEvent.change(input);
    expect(screen.getByText("shot.png")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Remove shot.png" }));
    expect(screen.queryByText("shot.png")).toBeNull();
    expect(screen.getByText("data.json")).toBeTruthy();
  });

  test("the files sit inside the prompt's box", () => {
    open((f) => `/Users/me/${f.name}`);
    fireEvent.drop(surface(), fileDrag([png()]));
    const box = screen.getByRole("textbox", { name: "Prompt" }).closest(".prompt-composer");
    expect(box?.querySelector('ul[aria-label="Attached files"]')).toBeTruthy();
  });

  // jsdom lays nothing out, so the row's geometry is stubbed: eleven 112px tiles
  // 8px apart after 10px of padding, in a 500px row — what the dialog shows.
  test("one row scrolls, and a fade says how many files are past its end until none are", () => {
    const row = (el: HTMLElement) => el.matches('ul[aria-label="Attached files"]');
    const index = (el: HTMLElement) => Array.prototype.indexOf.call(el.parentElement?.children ?? [], el);
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(function (this: HTMLElement) {
      return row(this) ? 500 : 0;
    });
    // jsdom ignores a written `scrollLeft`, so the row's scroll position is this.
    let scrolled = 0;
    vi.spyOn(HTMLElement.prototype, "scrollLeft", "get").mockImplementation(function (this: HTMLElement) {
      return row(this) ? scrolled : 0;
    });
    vi.spyOn(HTMLElement.prototype, "offsetLeft", "get").mockImplementation(function (this: HTMLElement) {
      return this.matches("li.prompt-attachment") ? 10 + index(this) * 120 : 0;
    });
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
      return this.matches("li.prompt-attachment") ? 112 : 0;
    });
    open((f) => `/Users/me/${f.name}`);
    fireEvent.drop(
      surface(),
      fileDrag(Array.from({ length: 11 }, (_, i) => new File(["x"], `f${i}.json`, { type: "application/json" }))),
    );
    // Tiles 4 to 10 are cut or past the end.
    expect(screen.getByText("7 more files")).toBeTruthy();

    const list = document.querySelector<HTMLElement>('ul[aria-label="Attached files"]');
    if (!list) throw new Error("no attachment row");
    scrolled = 821; // the last tile still 1px short
    fireEvent.scroll(list);
    expect(screen.getByText("1 more file")).toBeTruthy();
    scrolled = 832; // the end of the row: 1322 + 10 - 500
    fireEvent.scroll(list);
    expect(screen.queryByText(/more files?$/)).toBeNull();
  });

  test("an empty browser file is refused out loud", () => {
    open();
    fireEvent.drop(surface(), fileDrag([new File([], "folder")]));
    expect(
      screen.getByText("folder is empty or a folder — a browser tab can only attach files with content"),
    ).toBeTruthy();
  });

  test("files alone launch the agent, and travel with the create", async () => {
    const { onCreate } = open((f) => `/Users/me/${f.name}`);
    fireEvent.drop(surface(), fileDrag([png()]));
    expect(
      screen.getByText("Opens Claude in the new checkout with the files as its first message."),
    ).toBeTruthy();

    const create = screen.getByRole("button", { name: "Create worktree" });
    await waitFor(() => expect((create as HTMLButtonElement).disabled).toBe(false));
    await act(async () => {
      fireEvent.click(create);
    });
    expect(onCreate).toHaveBeenCalledTimes(1);
    const body = onCreate.mock.calls[0][0];
    expect(body.agent).toBe("claude");
    expect(body.prompt).toBe("");
    expect(body.name_prompt).toBeUndefined();
    expect(body.attachments?.map((a) => [a.file.name, a.path])).toEqual([
      ["shot.png", "/Users/me/shot.png"],
    ]);
  });

  test("with neither text nor files, nothing is launched", async () => {
    const { onCreate } = open();
    const create = screen.getByRole("button", { name: "Create worktree" });
    await waitFor(() => expect((create as HTMLButtonElement).disabled).toBe(false));
    await act(async () => {
      fireEvent.click(create);
    });
    const body = onCreate.mock.calls[0][0];
    expect(body.agent).toBeUndefined();
    expect(body.attachments).toBeUndefined();
  });
});
