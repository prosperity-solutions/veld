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
      // **Every arrow chord belongs to the terminal now.** Worktree navigation
      // shipped on `mod`+arrow and was let past xterm here, so `⌘←`/`⌘→` moved
      // the rail instead of the caret inside Claude Code and Codex — a reported
      // bug. Navigation is `⌃Tab`/`⌥Tab` now, asserted just above — it is
      // page-dispatched, so it does need this function.
      for (const arrowKey of ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]) {
        for (const extra of [{}, { shift: true }, { alt: true }]) {
          expect(
            run({ code: arrowKey, key: arrowKey, [mod]: true, ...extra }).handled,
            `${mod}+${Object.keys(extra).join("+")}${arrowKey} belongs to the terminal`,
          ).toBe(true);
        }
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

/** `run`, but on a Mac. The chords below exist nowhere else. */
function runMac(init: Parameters<typeof key>[0]) {
  const { e, wasPrevented } = key(init);
  const sent: string[] = [];
  const handled = handleKeyEvent(e, (d) => sent.push(d), true, true);
  return { handled, sent, prevented: wasPrevented() };
}

describe("the macOS line-editing chords", () => {
  it("sends ^A for ⌘← and ^E for ⌘→, and keeps them from xterm", () => {
    // What every mac terminal emulator binds, and what Veld sent *nothing* for
    // before this: xterm.js's arrow arm has an explicit `if (ev.metaKey) break`.
    const left = runMac({ code: "ArrowLeft", key: "ArrowLeft", meta: true });
    expect(left.sent).toEqual(["\x01"]);
    expect(left.handled).toBe(false);
    expect(left.prevented).toBe(true);

    const right = runMac({ code: "ArrowRight", key: "ArrowRight", meta: true });
    expect(right.sent).toEqual(["\x05"]);
    expect(right.handled).toBe(false);
  });

  it("sends ^U for ⌘⌫ — the one chord that previously did the wrong thing", () => {
    // xterm's Backspace arm checks only Shift and Alt, so ⌘⌫ reached the shell
    // as a bare DEL: one character, where a mac text field deletes the line.
    // `preventDefault` is what stops the shell getting the kill *and* the DEL.
    const r = runMac({ code: "Backspace", key: "Backspace", meta: true });
    expect(r.sent).toEqual(["\x15"]);
    expect(r.handled).toBe(false);
    expect(r.prevented).toBe(true);
  });

  it("swallows the matching keyup, which xterm has no keydown for", () => {
    const r = runMac({ code: "ArrowLeft", key: "ArrowLeft", meta: true, type: "keyup" });
    expect(r.sent).toEqual([]);
    expect(r.handled).toBe(false);
  });

  it("claims none of these off a Mac, where ⌘ is Super and belongs to the WM", () => {
    for (const k of ["ArrowLeft", "ArrowRight", "Backspace"]) {
      const r = run({ code: k, key: k, meta: true });
      expect(r.sent, `${k} must reach xterm off a Mac`).toEqual([]);
      expect(r.handled).toBe(true);
    }
  });

  it("leaves every other modifier's version of the chord alone", () => {
    // ⌘⇧← selects in a text field and is a chord programs bind for themselves;
    // ⌃⌘← is a Spaces gesture. Claiming a superset eats somebody else's binding.
    for (const extra of [{ shift: true }, { alt: true }, { ctrl: true }]) {
      const r = runMac({ code: "ArrowLeft", key: "ArrowLeft", meta: true, ...extra });
      expect(r.sent).toEqual([]);
      expect(r.handled).toBe(true);
    }
  });

  it("leaves ⌥ arrows and ⌥⌫ to xterm, which already sends word motions for them", () => {
    // Verified on a real Mac before this shipped: these work today. Re-spelling
    // them would only change bytes a TUI may already have bound.
    for (const k of ["ArrowLeft", "ArrowRight", "Backspace"]) {
      const r = runMac({ code: k, key: k, alt: true });
      expect(r.sent).toEqual([]);
      expect(r.handled).toBe(true);
    }
  });

  it("claims no ⌘↑/⌘↓ — Cocoa means 'the document', and a shell line has none", () => {
    for (const k of ["ArrowUp", "ArrowDown"]) {
      const r = runMac({ code: k, key: k, meta: true });
      expect(r.sent).toEqual([]);
      expect(r.handled).toBe(true);
    }
  });

  it("defaults to off, so a caller that has not been updated is unchanged", () => {
    const sent: string[] = [];
    // Three arguments: the signature before `mac` existed.
    expect(
      handleKeyEvent(key({ code: "ArrowLeft", key: "ArrowLeft", meta: true }).e, (d) => sent.push(d), true),
    ).toBe(true);
    expect(sent).toEqual([]);
  });

  it("does not shadow the one ⌘ chord the app claims from a terminal", () => {
    // ⌘/ opens the Shortcuts overview and must still reach the window listener.
    const r = runMac({ code: "Slash", key: "/", meta: true });
    expect(r.sent).toEqual([]);
    expect(r.handled).toBe(false);
  });
});
