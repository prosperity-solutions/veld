import { describe, expect, it } from "vitest";
import { SHIFT_ENTER_SEQUENCE, handleKeyEvent } from "./terminalKeys";

/** Minimal stand-in for the fields the handler reads, plus a preventDefault spy. */
function key(init: {
  code: string;
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

  it("leaves Enter with other modifiers alone", () => {
    // Alt+Enter already sends ESC CR through xterm itself, and Ctrl/⌘+Enter are
    // chords programs bind. Swallowing any of them here would be eating
    // somebody else's keybinding.
    for (const mod of ["alt", "ctrl", "meta"] as const) {
      const r = run({ code: "Enter", shift: true, [mod]: true });
      expect(r.handled, `Shift+${mod}+Enter must pass through`).toBe(true);
      expect(r.sent).toEqual([]);
    }
  });

  it("lets the palette accelerator reach the app", () => {
    for (const mod of ["ctrl", "meta"] as const) {
      const r = run({ code: "KeyP", shift: true, [mod]: true });
      expect(r.handled).toBe(false);
      // Returning false is the whole mechanism: xterm ignores the event without
      // cancelling it, so it keeps propagating to the window listener.
      expect(r.prevented).toBe(false);
      expect(r.sent).toEqual([]);
    }
  });

  it("leaves Ctrl+K to readline", () => {
    // kill-to-end-of-line belongs to the shell, which is why the palette has a
    // second accelerator at all.
    expect(run({ code: "KeyK", ctrl: true }).handled).toBe(true);
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

  it("leaves the palette chord alone regardless of the preference", () => {
    for (const pref of [true, false]) {
      expect(
        handleKeyEvent(key({ code: "KeyP", shift: true, meta: true }).e, () => {}, pref),
      ).toBe(false);
    }
  });
});
