import { describe, expect, it } from "vitest";
import { SHIFT_ENTER_SEQUENCE, handleKeyEvent } from "./terminalKeys";

/** Minimal stand-in for the fields the handler reads, plus a preventDefault spy. */
function key(init: {
  code: string;
  /** Defaults to `code` — fine for every existing case here, none of which
   *  match on `.key`. The app-shortcut cases below pass this explicitly. */
  key?: string;
  type?: string;
  shift?: boolean;
  ctrl?: boolean;
  alt?: boolean;
  meta?: boolean;
}) {
  let prevented = false;
  const e = {
    type: init.type ?? "keydown",
    code: init.code,
    key: init.key ?? init.code,
    shiftKey: init.shift ?? false,
    ctrlKey: init.ctrl ?? false,
    altKey: init.alt ?? false,
    metaKey: init.meta ?? false,
    preventDefault: () => {
      prevented = true;
    },
  } as unknown as KeyboardEvent;
  return { e, wasPrevented: () => prevented };
}

function run(init: Parameters<typeof key>[0]) {
  const { e, wasPrevented } = key(init);
  const sent: string[] = [];
  const handled = handleKeyEvent(e, (d) => sent.push(d));
  return { handled, sent, prevented: wasPrevented() };
}

describe("handleKeyEvent", () => {
  it("sends ESC CR for Shift+Enter and keeps it away from xterm", () => {
    const r = run({ code: "Enter", shift: true });
    expect(r.sent).toEqual([SHIFT_ENTER_SEQUENCE]);
    expect(r.handled).toBe(false);
    // Without preventDefault the shell also receives a bare CR — a newline
    // *and* a submit, which is the bug this would ship as.
    expect(r.prevented).toBe(true);
  });

  it("is what Claude Code's /terminal-setup writes", () => {
    // Pinned as bytes: this is an interop contract with other terminals, not an
    // internal choice. `\x1b\r`, nothing else.
    expect(SHIFT_ENTER_SEQUENCE).toBe("\u001b\r");
    expect([...SHIFT_ENTER_SEQUENCE].map((c) => c.charCodeAt(0))).toEqual([0x1b, 0x0d]);
  });

  it("treats the numpad's Enter the same", () => {
    expect(run({ code: "NumpadEnter", shift: true }).sent).toEqual([SHIFT_ENTER_SEQUENCE]);
  });

  it("leaves plain Enter to the shell", () => {
    const r = run({ code: "Enter" });
    expect(r.sent).toEqual([]);
    expect(r.handled).toBe(true);
    expect(r.prevented).toBe(false);
  });

  it("leaves Alt+Shift+Enter alone", () => {
    // Alt+Enter already sends ESC CR through xterm itself. Ctrl/⌘+Shift+Enter
    // is its own case below — it is now the app's start/stop chord, which is
    // the opposite answer from this one.
    const r = run({ code: "Enter", key: "Enter", shift: true, alt: true });
    expect(r.handled).toBe(true);
    expect(r.sent).toEqual([]);
  });

  it("leaves Ctrl+K to readline", () => {
    // kill-to-end-of-line belongs to the shell — the palette is reachable
    // from a focused terminal only via ⌘K, not Ctrl+K.
    expect(run({ code: "KeyK", ctrl: true }).handled).toBe(true);
  });

  it("returns every new window-level shortcut to the window listener (not to xterm)", () => {
    // Mirrors exactly what App.tsx's keydown effect matches, per chord — a
    // mismatch here would have xterm swallow the key on one layout while the
    // window listener expects a different physical key on another. This only
    // pins `handleKeyEvent`'s own contract (`false` = xterm ignores the event
    // without cancelling it, so it keeps bubbling); whether `App.tsx` then
    // acts on it is `isEditableTarget`'s xterm-textarea exemption, asserted
    // separately where that function lives.
    for (const mod of ["ctrl", "meta"] as const) {
      // `l`/`x`, not `f`/`v`: the veld feedback overlay claims mod+Shift+F/V
      // for its own bindings, so App.tsx moved focus mode and the view
      // switch off them — see the matching comment there and in
      // `isAppShortcutChord`.
      for (const letter of ["l", "x", "u", "o", "k"]) {
        const r = run({ code: `Key${letter.toUpperCase()}`, key: letter, shift: true, [mod]: true });
        expect(r.handled, `${mod}+shift+${letter} must pass through`).toBe(false);
        expect(r.sent).toEqual([]);
      }
      expect(
        run({ code: "Enter", key: "Enter", shift: true, [mod]: true }).handled,
        `${mod}+shift+Enter must pass through (not the Shift+Enter substitution)`,
      ).toBe(false);
      expect(
        run({ code: "ArrowUp", key: "ArrowUp", [mod]: true }).handled,
        `${mod}+ArrowUp must pass through`,
      ).toBe(false);
      expect(
        run({ code: "ArrowDown", key: "ArrowDown", [mod]: true }).handled,
        `${mod}+ArrowDown must pass through`,
      ).toBe(false);
      // ←/→ alias worktree-nav's ↑/↓, and — with Shift held too — tab-cycling.
      for (const arrowKey of ["ArrowLeft", "ArrowRight"]) {
        expect(
          run({ code: arrowKey, key: arrowKey, [mod]: true }).handled,
          `${mod}+${arrowKey} must pass through`,
        ).toBe(false);
        expect(
          run({ code: arrowKey, key: arrowKey, shift: true, [mod]: true }).handled,
          `${mod}+shift+${arrowKey} must pass through`,
        ).toBe(false);
      }
    }
    // Plain Tab (no modifier) is unrelated typing and must stay untouched.
    expect(run({ code: "Tab", key: "Tab" }).handled).toBe(true);
  });

  it("opens the Shortcuts overview from a terminal on ⌘/, but leaves Ctrl+/ to readline", () => {
    // `Ctrl+/` is readline's undo (`C-_`) — the same class of shell binding
    // `Ctrl+K` is already reserved for below — so only the literal Meta key
    // reaches the window listener for this one chord, unlike every other
    // shortcut above where either counts as `mod`.
    expect(run({ code: "Slash", key: "/", meta: true }).handled).toBe(false);
    expect(run({ code: "Slash", key: "/", meta: true, shift: true }).handled).toBe(false);
    expect(run({ code: "Slash", key: "/", ctrl: true }).handled).toBe(true);
  });

  it("opens the Shortcuts overview on German/Spanish ⌘⇧7, the Digit7-fallback for ⌘/", () => {
    // macOS Chromium's Cmd+Shift+digit quirk reports `key: "7"`, not `"/"`, on
    // these layouts — this is the fallback that catches it. Gated on
    // `e.shiftKey` so it never fires the shiftless ⌘7 "go to project 7" chord.
    expect(run({ code: "Digit7", key: "7", meta: true, shift: true }).handled).toBe(false);
    // Without Shift it is an ordinary ⌘7 and must stay untouched here.
    expect(run({ code: "Digit7", key: "7", meta: true }).handled).toBe(true);
  });

  it("leaves an app-shortcut letter alone with no modifier, or with only mod (no shift)", () => {
    // `l`/`x`/`u`/`o`/`k` are ordinary typing without Shift, and ⌘F
    // specifically is find-in-page — a different, unrelated chord this
    // handler must not eat.
    expect(run({ code: "KeyF", key: "f" }).handled).toBe(true);
    expect(run({ code: "KeyF", key: "f", meta: true }).handled).toBe(true);
  });

  it("sends nothing on keyup, and hides the keyup from xterm too", () => {
    // A keyup with no matching keydown is an event xterm has no state for.
    const up = run({ code: "Enter", shift: true, type: "keyup" });
    expect(up.sent).toEqual([]);
    expect(up.handled).toBe(false);

    const other = run({ code: "KeyA", type: "keyup" });
    expect(other.handled).toBe(true);
  });
});

describe("the shiftEnterNewline preference", () => {
  it("hands Shift+Enter to xterm when off", () => {
    // A TUI that binds meta-Enter needs the key to reach it, so the handler must
    // claim neither the keydown nor its keyup.
    const sent: string[] = [];
    expect(
      handleKeyEvent(key({ code: "Enter", shift: true }).e, (d) => sent.push(d), false),
    ).toBe(true);
    expect(sent).toEqual([]);
    // Swallowing the keyup after not claiming the keydown would drop a release
    // xterm is waiting for.
    expect(
      handleKeyEvent(
        key({ code: "Enter", shift: true, type: "keyup" }).e,
        (d) => sent.push(d),
        false,
      ),
    ).toBe(true);
  });

  it("still sends ESC CR when on, and defaults to on", () => {
    const sent: string[] = [];
    expect(
      handleKeyEvent(key({ code: "Enter", shift: true }).e, (d) => sent.push(d), true),
    ).toBe(false);
    expect(sent).toEqual([SHIFT_ENTER_SEQUENCE]);
    // No third argument behaves like the release that shipped this hardcoded.
    expect(
      handleKeyEvent(key({ code: "Enter", shift: true }).e, (d) => sent.push(d)),
    ).toBe(false);
    expect(sent).toEqual([SHIFT_ENTER_SEQUENCE, SHIFT_ENTER_SEQUENCE]);
  });
});
