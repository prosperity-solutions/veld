import { describe, expect, it } from "vitest";
// `?raw` rather than `node:fs`, matching `paneAreaContract.test.ts`: this
// package's tsconfig carries `vite/client` and not node's types.
import TERMINAL_HOST from "./terminalHost.ts?raw";
import {
  clipboardImageIndex,
  clipboardImageName,
  escapePath,
  isFileDrop,
  isPastable,
  pathPayload,
  shouldSwallowDrop,
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

  it("backslashes an apostrophe", () => {
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

describe("the paths reach the terminal as a paste", () => {
  // **A source-level check, and deliberately so** — the same tactic, for the same
  // reason, as `desktop/src/preload.test.js`: the code that matters lives in
  // `terminalHost.ts` around a live xterm `Terminal`, which cannot be constructed
  // under this runner's `environment: "node"`, and the property is exactly "the
  // payload goes out by this route and not that one".
  //
  // Why it is worth pinning at all: a coding agent decides whether a path is a
  // file to attach or merely text by **whether it arrived as a paste**. Measured
  // against a real Claude Code, with the identical characters both ways:
  //
  //     typed one at a time   -> the composer shows `/tmp/…/red.png`
  //     sent via term.paste   -> the composer shows `[Image #1]`
  //
  // So `send(payload)` and `term.paste(payload)` are not two spellings of one
  // thing; one of them silently loses the entire feature. That is not visible at
  // the call site, and nothing else in the build would catch the swap.
  it("hands the payload to term.paste", () => {
    expect(TERMINAL_HOST).toContain("s.term.paste(payload)");
  });

  it("never writes the payload straight to the socket", () => {
    // `send(payload)` is the regression: it is what shipped first, and it typed
    // the path instead of attaching the image.
    expect(TERMINAL_HOST).not.toContain("send(payload)");
  });
});

describe("isPastable", () => {
  it("refuses a path carrying a newline or carriage return", () => {
    // **No quoting saves these.** The payload goes out through `term.paste`, and
    // xterm's `prepareTextForTerminal` runs `text.replace(/\r?\n/g, '\r')` BEFORE
    // it brackets anything — so a newline arrives as a carriage return, i.e. a
    // submit, whatever it was wrapped in. The first version single-quoted such a
    // path on the stated grounds that backslash-newline is a line continuation;
    // that reasoning never applied to this send path.
    expect(isPastable("/tmp/two\nlines.png")).toBe(false);
    expect(isPastable("/tmp/cr\rname.png")).toBe(false);
  });

  it("accepts an ordinary path, spaces and quotes included", () => {
    expect(isPastable("/tmp/a.png")).toBe(true);
    expect(isPastable("/tmp/My Photo.png")).toBe(true);
    expect(isPastable("/tmp/it's here.png")).toBe(true);
  });

  it("refuses an empty path", () => {
    expect(isPastable("")).toBe(false);
  });
});

describe("pathPayload drops what a terminal cannot carry", () => {
  it("omits a newline path and keeps the rest", () => {
    expect(pathPayload(["/tmp/a.png", "/tmp/b\nc.png"])).toBe("/tmp/a.png ");
  });

  it("is empty when every path is unusable, so the caller reports a failure", () => {
    expect(pathPayload(["/tmp/b\nc.png"])).toBe("");
  });
});

describe("shouldSwallowDrop", () => {
  // The window-level guard's whole decision. Extracted precisely because its
  // failure is the expensive one: a file dropped a few pixels off a pane would
  // otherwise navigate the browser away and take the whole /ide view with it.
  it("swallows a stray file drop nothing else claimed", () => {
    expect(shouldSwallowDrop(["Files"], false)).toBe(true);
  });

  it("defers to a pane that already claimed the drop", () => {
    // Load-bearing: the guard runs LAST in the bubble phase, so without this it
    // would repaint the one target that works with a `no drop` cursor.
    expect(shouldSwallowDrop(["Files"], true)).toBe(false);
  });

  it("ignores a drag that is not files at all", () => {
    expect(shouldSwallowDrop(["text/plain"], false)).toBe(false);
    expect(shouldSwallowDrop([], false)).toBe(false);
  });

  it("ignores a pane tab being dragged, which PaneArea owns", () => {
    expect(shouldSwallowDrop([TAB_MIME, "Files"], false)).toBe(false);
  });
});

/**
 * The `restarting` flag's one clearing point.
 *
 * `Session.restarting` is what decides whether a connecting pane shows the
 * full-pane "Restarting…" card or the corner `connecting…` chip, and it is set
 * by three call sites but cleared by exactly one: `setState`, whenever the
 * session leaves `connecting`. Clearing it there rather than at each outcome is
 * load-bearing — a restart that fails to spawn lands in `error`, and a flag left
 * set would cover the message saying why with a spinner that never resolves.
 *
 * A source assertion for the same reason the two above are: the flag lives in
 * module-level session state behind a live WebSocket, which the `node` test
 * environment cannot stand up, and nothing else in the build ties the set sites
 * to the clear.
 */
describe("a restart's presentation flag", () => {
  it("is cleared whenever the session stops connecting", () => {
    expect(TERMINAL_HOST).toContain(
      'if (state !== "connecting") s.restarting = null;',
    );
  });

  it("clears it inside setState, not at an individual outcome", () => {
    // Anchored on the function, so moving the line to (say) the `exit` handler —
    // which would leave every *other* ending stuck on the overlay — fails here.
    const body = /function setState\([\s\S]*?\n}/.exec(TERMINAL_HOST)?.[0];
    expect(body, "setState not found — update this test with it").toBeTruthy();
    expect(body).toContain("s.restarting = null");
  });
});
