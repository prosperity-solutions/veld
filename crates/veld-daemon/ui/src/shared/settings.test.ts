import { describe, expect, it } from "vitest";

import {
  hasMarkerHue,
  markerFace,
  markerGlyph,
  markerHueVar,
  markerStyle,
  quickSwitchPrefs,
  terminalPrefs,
} from "./settings";

describe("terminalPrefs", () => {
  it("reads what the daemon sent", () => {
    const p = terminalPrefs({
      "terminal.fontSize": 15,
      "terminal.fontFamily": "Fira Code",
      "terminal.cursorStyle": "bar",
      "terminal.cursorBlink": false,
      "terminal.scrollback": 1000,
      "terminal.shiftEnterNewline": false,
      "terminal.copyOnSelect": true,
      "terminal.middleClickPaste": true,
    });
    expect(p).toEqual({
      fontSize: 15,
      fontFamily: "Fira Code",
      cursorStyle: "bar",
      cursorBlink: false,
      scrollback: 1000,
      shiftEnterNewline: false,
      copyOnSelect: true,
      middleClickPaste: true,
    });
  });

  it("falls back for a key an older daemon never sent", () => {
    // The downgrade case: this client knows a key the daemon does not, so the
    // effective document arrives without it.
    const p = terminalPrefs({});
    expect(p.fontSize).toBe(12);
    expect(p.scrollback).toBe(5000);
    expect(p.cursorStyle).toBe("block");
  });

  it("rejects a wrong-typed value rather than passing it to xterm", () => {
    const p = terminalPrefs({
      "terminal.fontSize": "big" as unknown as number,
      "terminal.cursorBlink": 1 as unknown as boolean,
      "terminal.cursorStyle": "wobble",
    });
    expect(p.fontSize).toBe(12);
    expect(p.cursorBlink).toBe(true);
    expect(p.cursorStyle).toBe("block");
  });

  it("rejects a non-finite font size", () => {
    // `typeof NaN === "number"`, and NaN reaches xterm as a font size that
    // renders nothing at all — so the guard has to be Number.isFinite.
    expect(terminalPrefs({ "terminal.fontSize": NaN }).fontSize).toBe(12);
    expect(terminalPrefs({ "terminal.fontSize": Infinity }).fontSize).toBe(12);
  });

  it("rejects an empty font family", () => {
    // Would render as the browser's default and read as a bug.
    expect(terminalPrefs({ "terminal.fontFamily": "   " }).fontFamily).toContain(
      "JetBrains Mono",
    );
  });
});

describe("markerStyle", () => {
  it("defaults to colour and accepts only the two faces", () => {
    expect(markerStyle({})).toBe("color");
    expect(markerStyle({ "worktree.markerStyle": "emoji" })).toBe("emoji");
    expect(markerStyle({ "worktree.markerStyle": "plaid" })).toBe("color");
  });
});

describe("hasMarkerHue", () => {
  it("treats the unassigned sentinel as absent", () => {
    expect(hasMarkerHue(-1)).toBe(false);
    expect(hasMarkerHue(0)).toBe(true);
    expect(hasMarkerHue(11)).toBe(true);
    // A fractional index would build a `--wt-hue-1.5` that resolves to nothing.
    expect(hasMarkerHue(1.5)).toBe(false);
  });
});

describe("markerFace", () => {
  const both = { emoji: "🦊", marker_hue: 3 };

  it("follows the style when both faces exist", () => {
    expect(markerFace({}, both)).toEqual({ kind: "color", hue: 3 });
    expect(markerFace({ "worktree.markerStyle": "emoji" }, both)).toEqual({
      kind: "emoji",
      emoji: "🦊",
    });
  });

  it("uses the glyph while a hue is still unassigned", () => {
    // The upgrade window: a row migrated from before the colour column, whose
    // hue arrives on the next sync. Colour is the default style, so without this
    // the rail would render nothing at all for every existing worktree.
    expect(markerFace({}, { emoji: "🦊", marker_hue: -1 })).toEqual({
      kind: "emoji",
      emoji: "🦊",
    });
  });

  it("uses the colour when the glyph is the missing face", () => {
    expect(
      markerFace({ "worktree.markerStyle": "emoji" }, { emoji: "", marker_hue: 2 }),
    ).toEqual({ kind: "color", hue: 2 });
  });

  it("is null only when neither face exists", () => {
    expect(markerFace({}, { emoji: "", marker_hue: -1 })).toBeNull();
  });
});

describe("markerHueVar", () => {
  it("names the per-theme custom property", () => {
    expect(markerHueVar(7)).toBe("var(--wt-hue-7)");
  });
});

describe("markerGlyph", () => {
  it("ignores the style, because an OS string cannot carry a colour", () => {
    // The native tray menu label and the window title are plain strings handed
    // to the OS; a CSS custom property means nothing there.
    expect(markerGlyph({ emoji: "🦊" })).toBe("🦊");
    expect(markerGlyph({ emoji: "" })).toBe("");
  });
});

describe("quickSwitchPrefs", () => {
  it("shows both switches unless told otherwise", () => {
    expect(quickSwitchPrefs({})).toEqual({
      responsive: true,
      colorScheme: true,
    });
    expect(
      quickSwitchPrefs({ "browser.quickSwitch.colorScheme": false }),
    ).toEqual({ responsive: true, colorScheme: false });
  });
});
