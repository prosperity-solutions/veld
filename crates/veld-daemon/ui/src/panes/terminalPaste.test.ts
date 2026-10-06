import { afterEach, describe, expect, it, vi } from "vitest";
// `?raw` rather than `node:fs`, matching `paneAreaContract.test.ts`: this
// package's tsconfig carries `vite/client` and not node's types.
import TERMINAL_HOST from "./terminalHost.ts?raw";
import {
  clipboardImageIndex,
  clipboardImageName,
  echoed,
  escapePath,
  isFileDrop,
  isImagePath,
  isPastable,
  launchPrompt,
  pasteLanded,
  pathPayload,
  promptPayload,
  promptPastes,
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

  it("pastes a prompt's attachments, one paste each, the same way", () => {
    // The prompt path is the second route a path takes into a pane, and the
    // property above applies to it unchanged.
    expect(TERMINAL_HOST).toContain("s.term.paste(paste)");
  });

  it("listens for a prompt paste's echo before pasting, and waits for it", () => {
    // Armed after the paste, a fast echo lands before anyone listens and every
    // paste waits out the cap instead.
    expect(TERMINAL_HOST).toMatch(
      /const shown =[^;]*echoed\(s\.term,[^;]*\);\s*s\.term\.paste\(paste\);\s*await shown;/,
    );
  });

  it("an image path waits for its placeholder, not for the first redraw", () => {
    // Claude Code drops a Return that arrives while it is still reading an
    // image, so the `\r` after one must wait for `[Image #N]` itself.
    expect(TERMINAL_HOST).toMatch(/echoed\(s\.term, PROMPT_ECHO_QUIET_MS, PROMPT_IMAGE_CAP_MS, \(\) =>\s*pasteLanded\(/);
  });

  it("types a prompt a launch was handed only after claiming it back", () => {
    // The agent's wrapper and the window race for one file; the window asks only
    // once the agent is up, so its claim is the answer to "did the launch send
    // it?" — and typing without it sends the message twice.
    const deliver = /const deliver = async \(\) => \{[\s\S]*?\n {2}\};/.exec(TERMINAL_HOST)?.[0] ?? "";
    expect(deliver).toMatch(/const mine = await owed\(\);\s*if \(mine === null\) return unsure\(\);\s*if \(!mine\) return;/);
    expect(deliver.indexOf("await owed()")).toBeLessThan(deliver.indexOf("s.term.paste(paste)"));
    // A pane that never opened an input is not "your prompt was not sent" when
    // its launch may have sent it.
    expect(TERMINAL_HOST).not.toMatch(/dropInitialPrompt\(s\.id\);\s*report\(\);/);
  });

  it("offers the prompt to a fresh launch only, and only once", () => {
    expect(TERMINAL_HOST).toContain('const prompt = pane?.mode === "fresh" ? await offerAtLaunch(s) : undefined;');
    expect(TERMINAL_HOST).toMatch(/if \(queued === undefined \|\| queued\.offered\) return undefined;\s*queued\.offered = true;/);
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

describe("promptPayload", () => {
  it("keeps ordinary prose exactly as it was typed", () => {
    expect(promptPayload("Fix the login redirect loop")).toBe(
      "Fix the login redirect loop",
    );
    // A multi-line prompt is the case bracketed paste exists for, and a tab is
    // legitimate inside pasted prose.
    expect(promptPayload("Do this:\n\t- and this")).toBe("Do this:\n\t- and this");
  });

  it("removes a literal bracketed-paste terminator", () => {
    // **The one that matters.** xterm wraps the payload in ESC[200~/ESC[201~ and
    // does nothing else to it, so an ESC[201~ inside a prompt ends the bracket
    // early and the program reads the rest as keystrokes — for an agent
    // composer, as commands.
    expect(promptPayload("safe\u001b[201~rm -rf /")).toBe("saferm -rf /");
    expect(promptPayload("a\u001b[200~b")).toBe("ab");
  });

  it("normalises every line ending to \\n rather than dropping it", () => {
    // A prompt pasted from a Windows editor means its line breaks. `\r` is also
    // the one control byte that means "submit", so it must not survive as
    // itself: xterm's own paste collapses `\r\n` to `\r`, which inside the
    // bracket is a line break and outside it is a keypress.
    expect(promptPayload("one\r\ntwo")).toBe("one\ntwo");
    expect(promptPayload("one\rtwo")).toBe("one\ntwo");
    expect(promptPayload("one\r\ntwo").includes("\r")).toBe(false);
  });

  it("removes control bytes a pasted terminal capture carries", () => {
    // An ESC from an ANSI colour run starts an escape sequence in the TUI; a
    // stray \x03 is Ctrl-C.
    expect(promptPayload("\u001b[31mred\u001b[0m text")).toBe("[31mred[0m text");
    expect(promptPayload("a\u0003b\u007fc")).toBe("abc");
  });

  it("removes every character that reorders or hides what you read", () => {
    // The claim in the doc comment is that this is `is_forbidden` exactly, so
    // the test is the whole set: bidi overrides and isolates, the line and
    // paragraph separators, the BOM, and the interlinear annotation marks.
    expect(promptPayload("a\u202eb\u2066c")).toBe("abc");
    expect(promptPayload("a\u2028b\u2029c\ufeffd\ufff9e\ufffbf")).toBe("abcdef");
  });

  it("can empty a prompt that was nothing but control bytes", () => {
    // `queueInitialPrompt` trims and drops an empty payload, so this is the
    // signal that there was never a prompt to send.
    expect(promptPayload("\u001b\u0003\u007f").trim()).toBe("");
  });
});

describe("promptPastes", () => {
  it("is the text alone when nothing is attached", () => {
    expect(promptPastes("fix the bug", [])).toEqual(["fix the bug"]);
  });

  it("closes the prose with a blank line, then pastes each path on its own", () => {
    expect(promptPastes("look at these", ["/tmp/a.png", "/tmp/My Shot.png"])).toEqual([
      "look at these\n\n",
      "/tmp/a.png ",
      "/tmp/My\\ Shot.png ",
    ]);
  });

  it("is only the paths for a files-only prompt", () => {
    expect(promptPastes("", ["/tmp/a.png"])).toEqual(["/tmp/a.png "]);
  });

  it("leaves out a path a terminal cannot carry, and does not close the prose for it", () => {
    expect(promptPastes("hi", ["/tmp/bad\nname.png"])).toEqual(["hi"]);
    expect(promptPastes("", ["/tmp/bad\nname.png"])).toEqual([]);
  });
});

describe("launchPrompt", () => {
  it("is the text, a blank line, then each file as a quoted mention", () => {
    expect(launchPrompt("look at these", ["/tmp/a.png", "/Users/me/My Shot.png"])).toBe(
      'look at these\n\n@"/tmp/a.png" @"/Users/me/My Shot.png"',
    );
  });

  it("is the text alone, or the mentions alone", () => {
    expect(launchPrompt("fix the bug", [])).toBe("fix the bug");
    expect(launchPrompt("", ["/tmp/a.png"])).toBe('@"/tmp/a.png"');
  });

  it("keeps a prompt that looks like a flag as it is — the wrapper puts it after `--`", () => {
    expect(launchPrompt("--help me", [])).toBe("--help me");
  });

  it("gives up on a path the quotes cannot carry, so the window types them instead", () => {
    expect(launchPrompt("hi", ['/tmp/say "hi".png'])).toBeNull();
    expect(launchPrompt("hi", ["/tmp/bad\nname.png"])).toBeNull();
  });

  it("is nothing for nothing", () => {
    expect(launchPrompt("", [])).toBeNull();
  });
});

describe("echoed", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  /** A terminal whose program writes when the test says so. */
  const program = () => {
    const listeners = new Set<() => void>();
    return {
      onWriteParsed(listener: () => void) {
        listeners.add(listener);
        return { dispose: () => listeners.delete(listener) };
      },
      write: () => {
        for (const l of listeners) l();
      },
      listening: () => listeners.size,
    };
  };

  it("waits out the time an agent spends reading an image before answering", async () => {
    vi.useFakeTimers();
    const term = program();
    let answered: boolean | undefined;
    void echoed(term, 250, 5_000).then((v) => {
      answered = v;
    });
    // A fixed 150ms pause submitted here, before the placeholder existed.
    await vi.advanceTimersByTimeAsync(900);
    expect(answered).toBeUndefined();
    term.write();
    await vi.advanceTimersByTimeAsync(249);
    expect(answered).toBeUndefined();
    await vi.advanceTimersByTimeAsync(1);
    expect(answered).toBe(true);
    expect(term.listening()).toBe(0);
  });

  it("lets a redraw that comes in several writes finish", async () => {
    vi.useFakeTimers();
    const term = program();
    let answered: boolean | undefined;
    void echoed(term, 250, 5_000).then((v) => {
      answered = v;
    });
    term.write();
    await vi.advanceTimersByTimeAsync(200);
    term.write();
    await vi.advanceTimersByTimeAsync(200);
    expect(answered).toBeUndefined();
    await vi.advanceTimersByTimeAsync(50);
    expect(answered).toBe(true);
  });

  it("with `until`, ignores output that is not the answer", async () => {
    vi.useFakeTimers();
    const term = program();
    let ready = false;
    let answered: boolean | undefined;
    void echoed(term, 250, 15_000, () => ready).then((v) => {
      answered = v;
    });
    // The agent redraws (a "pasting…" hint, a spinner) while it reads.
    term.write();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(answered).toBeUndefined();
    ready = true;
    term.write();
    await vi.advanceTimersByTimeAsync(250);
    expect(answered).toBe(true);
  });

  it("gives up at the cap, and stops listening", async () => {
    vi.useFakeTimers();
    const term = program();
    let answered: boolean | undefined;
    void echoed(term, 250, 5_000).then((v) => {
      answered = v;
    });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(answered).toBe(false);
    expect(term.listening()).toBe(0);
  });
});

describe("isImagePath", () => {
  it("takes the extensions Claude Code reads as images", () => {
    for (const p of ["/a/b.png", "/a/b.JPG", "/a/b.jpeg", "/a/b.gif", "/a/b.webp"]) {
      expect(isImagePath(p)).toBe(true);
    }
    for (const p of ["/a/b.json", "/a/b.svg", "/a/b.pdf", "/a/png"]) expect(isImagePath(p)).toBe(false);
  });
});

describe("pasteLanded", () => {
  const path = "/Users/me/.veld/pastes/e22a-image.png";

  it("is not done while the agent is still reading", () => {
    expect(pasteLanded("> fix this", "> fix this", path)).toBe(false);
  });

  it("is done when one more placeholder shows", () => {
    expect(pasteLanded("> fix this", "> fix this [Image #1]", path)).toBe(true);
    expect(pasteLanded("[Image #1]", "[Image #1] [Image #2]", path)).toBe(true);
    expect(pasteLanded("[Image #1]", "[Image #1]", path)).toBe(false);
  });

  it("is done when the agent shows the path as text instead", () => {
    expect(pasteLanded("> ", `> ${path}`, path)).toBe(true);
    const spaced = "/tmp/My Shot.png";
    expect(pasteLanded("> ", "> /tmp/My\\ Shot.png", spaced)).toBe(true);
  });
});
