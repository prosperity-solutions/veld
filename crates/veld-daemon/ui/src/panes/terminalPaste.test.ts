import { describe, expect, it } from "vitest";
import {
  CTRL_V,
  clipboardImageIndex,
  clipboardImageName,
  escapePath,
  isFileDrop,
  imageAction,
  isImageType,
  pathPayload,
  TAB_MIME,
} from "./terminalPaste";

describe("escapePath", () => {
  it("leaves an ordinary absolute path exactly as it is", () => {
    expect(escapePath("/Users/dev/project/src/main.rs")).toBe("/Users/dev/project/src/main.rs");
  });

  it("backslashes a space, which is the whole reason this exists", () => {
    expect(escapePath("/Users/dev/My Photo.png")).toBe("/Users/dev/My\\ Photo.png");
  });

  it("escapes every shell metacharacter, not just the ones one shell cares about", () => {
    // `$`, backtick and `"` are substitution in sh; `(`/`)` and `#` are in zsh
    // and fish. The allow-list means none of them need enumerating here to be
    // covered — this asserts the outcome for the ones that would execute.
    expect(escapePath("/tmp/a$(id).txt")).toBe("/tmp/a\\$\\(id\\).txt");
    expect(escapePath("/tmp/`id`")).toBe("/tmp/\\`id\\`");
    expect(escapePath("/tmp/a;rm -rf b")).toBe("/tmp/a\\;rm\\ -rf\\ b");
    expect(escapePath("/tmp/a&b|c")).toBe("/tmp/a\\&b\\|c");
    expect(escapePath("/tmp/*.png")).toBe("/tmp/\\*.png");
  });

  it("escapes a backslash already in the name, so it stays one character", () => {
    expect(escapePath("/tmp/a\\b")).toBe("/tmp/a\\\\b");
  });

  it("leaves non-ASCII alone — no shell gives it a meaning", () => {
    expect(escapePath("/Users/josé/日本語/写真.png")).toBe("/Users/josé/日本語/写真.png");
  });

  it("single-quotes a path containing a newline, because backslash-newline deletes it", () => {
    // The failure this prevents: `\` + LF is a line continuation, so the
    // backslash form would paste a *different, shorter* path and submit early.
    expect(escapePath("/tmp/two\nlines.png")).toBe("'/tmp/two\nlines.png'");
    expect(escapePath("/tmp/cr\rname")).toBe("'/tmp/cr\rname'");
  });

  it("closes and reopens the quote around an embedded single quote", () => {
    expect(escapePath("/tmp/it's\nhere")).toBe("'/tmp/it'\\''s\nhere'");
  });

  it("backslashes an apostrophe when there is no newline forcing quotes", () => {
    expect(escapePath("/tmp/it's here")).toBe("/tmp/it\\'s\\ here");
  });
});

describe("pathPayload", () => {
  it("ends with a space so the next thing typed is not glued to the path", () => {
    expect(pathPayload(["/tmp/a.png"])).toBe("/tmp/a.png ");
  });

  it("separates several paths with a space", () => {
    expect(pathPayload(["/tmp/a.png", "/tmp/b c.png"])).toBe("/tmp/a.png /tmp/b\\ c.png ");
  });

  it("never ends with a newline — a drop must not submit the line", () => {
    expect(pathPayload(["/tmp/a.png"])).not.toContain("\n");
    expect(pathPayload(["/tmp/a.png"])).not.toContain("\r");
  });

  it("is empty when nothing resolved, so the caller need not special-case it", () => {
    expect(pathPayload([])).toBe("");
    expect(pathPayload(["", ""])).toBe("");
  });

  it("drops only the empty entries, keeping the paths that did resolve", () => {
    expect(pathPayload(["", "/tmp/a.png"])).toBe("/tmp/a.png ");
  });
});

describe("isFileDrop", () => {
  it("takes a drag carrying files", () => {
    expect(isFileDrop(["Files"])).toBe(true);
    expect(isFileDrop(["text/uri-list", "Files"])).toBe(true);
  });

  it("refuses a drag with no files", () => {
    expect(isFileDrop([])).toBe(false);
    expect(isFileDrop(["text/plain"])).toBe(false);
  });

  it("refuses a pane tab even when the drag also advertises files", () => {
    // A cross-window tab drag can carry `Files` alongside its own type; the tab
    // is what the user is moving, and PaneArea owns that gesture.
    expect(isFileDrop([TAB_MIME, "Files"])).toBe(false);
  });
});

describe("clipboardImageName", () => {
  it("names the common image types by their real extension", () => {
    expect(clipboardImageName("image/png")).toBe("pasted-image.png");
    expect(clipboardImageName("image/jpeg")).toBe("pasted-image.jpg");
    expect(clipboardImageName("image/webp")).toBe("pasted-image.webp");
  });

  it("ignores case and a MIME parameter", () => {
    expect(clipboardImageName("IMAGE/PNG")).toBe("pasted-image.png");
    expect(clipboardImageName("image/png; charset=binary")).toBe("pasted-image.png");
  });

  it("never uses the subtype as an extension — a page controls that string", () => {
    expect(clipboardImageName("image/../../etc/passwd")).toBe("pasted-image.bin");
    expect(clipboardImageName("image/whatever")).toBe("pasted-image.bin");
  });
});

describe("clipboardImageIndex", () => {
  const file = (type: string) => ({ kind: "file", type });
  const str = (type: string) => ({ kind: "string", type });

  it("finds a screenshot, which arrives as a lone image file", () => {
    // The case the whole feature exists for: ⌘⇧⌃4 on macOS puts exactly this
    // on the clipboard. Note there is no `image/png` in `clipboardData.types`
    // for it at all — only `Files` — which is why this reads the items.
    expect(clipboardImageIndex([file("image/png")])).toBe(0);
  });

  it("takes an image copied out of a web page, which carries text/html beside it", () => {
    expect(clipboardImageIndex([str("text/html"), file("image/png")])).toBe(1);
  });

  it("hands a real text copy to xterm, even when something put an image alongside", () => {
    // `text/plain` is the discriminator: a genuine text copy always has it, and
    // never has an image file with it.
    expect(clipboardImageIndex([str("text/plain"), str("text/html")])).toBe(-1);
    expect(clipboardImageIndex([str("text/plain"), file("image/png")])).toBe(-1);
  });

  it("ignores a non-image file — that is a file paste, not an image paste", () => {
    expect(clipboardImageIndex([file("application/pdf")])).toBe(-1);
  });

  it("ignores an image MIME on a string entry", () => {
    expect(clipboardImageIndex([str("image/png")])).toBe(-1);
  });

  it("ignores case and MIME parameters", () => {
    expect(clipboardImageIndex([file("IMAGE/PNG; foo=bar")])).toBe(0);
    expect(clipboardImageIndex([str("TEXT/PLAIN"), file("image/png")])).toBe(-1);
  });

  it("is -1 for an empty clipboard", () => {
    expect(clipboardImageIndex([])).toBe(-1);
  });
});

describe("CTRL_V", () => {
  it("is the single byte a terminal sends for Ctrl+V", () => {
    // Pinned as a value because it is a wire fact, not a preference: this is the
    // byte a coding agent binds to "read the OS clipboard", and it is the only
    // route from "the user has an image" to "the agent has an image" — a pty
    // cannot carry a picture. Measured against a real Claude Code in a pty:
    // \x16 produces `[Image #1]`.
    expect(CTRL_V).toBe("\x16");
    expect(CTRL_V).toHaveLength(1);
  });
});

describe("isImageType", () => {
  it("recognises an image, ignoring case and parameters", () => {
    expect(isImageType("image/png")).toBe(true);
    expect(isImageType("IMAGE/JPEG; foo=bar")).toBe(true);
  });

  it("refuses everything else, including an empty type", () => {
    // A drop from some sources reports no type at all; that must read as "not an
    // image" so it takes the path branch rather than being handed to a clipboard.
    expect(isImageType("")).toBe(false);
    expect(isImageType("application/pdf")).toBe(false);
    expect(isImageType("text/plain")).toBe(false);
    // Not a prefix match on the word: `image` is not `image/`.
    expect(isImageType("imagex/png")).toBe(false);
  });
});

describe("imageAction", () => {
  it("pastes only when an agent is in the pane AND something is reading the tty", () => {
    expect(imageAction({ agent: true, foregroundApp: true })).toBe("paste");
  });

  it("types a path at a shell prompt, even with an agent flag still set", () => {
    // The case that makes this two signals rather than one: the agent wrapper
    // `exec`s the real binary and can never report its own exit, so the flag is
    // cleared by the shell's OSC 133 mark — which needs shellIntegration on. With
    // it off the flag goes stale, and `^V` at a zsh prompt corrupts the next
    // character the user types (`^V` then `echo hi` → `eecho hi`). Measured.
    expect(imageAction({ agent: true, foregroundApp: false })).toBe("path");
  });

  it("types a path for a full-screen program that is not an agent", () => {
    // `vim` is the case: busy says "something is reading the tty" but `^V` there
    // is visual-block mode, and a typed path is both harmless and useful.
    expect(imageAction({ agent: false, foregroundApp: true })).toBe("path");
  });

  it("types a path when nothing is known", () => {
    expect(imageAction({ agent: false, foregroundApp: false })).toBe("path");
  });

  it("never pastes unless both signals agree", () => {
    // Stated as the property rather than as four cases, because the property is
    // what must survive a future third signal being added.
    for (const agent of [true, false]) {
      for (const foregroundApp of [true, false]) {
        const expected = agent && foregroundApp ? "paste" : "path";
        expect(imageAction({ agent, foregroundApp }), `${agent}/${foregroundApp}`).toBe(expected);
      }
    }
  });
});
