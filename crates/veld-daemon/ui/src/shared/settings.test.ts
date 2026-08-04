import { describe, expect, it } from "vitest";

import {
  hasMarkerColor,
  markerFace,
  detachGraceMinutes,
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
    });
    expect(p).toEqual({
      fontSize: 15,
      fontFamily: "Fira Code",
      cursorStyle: "bar",
      cursorBlink: false,
      scrollback: 1000,
      shiftEnterNewline: false,
    });
  });

  it("falls back for a key an older daemon never sent", () => {
    // The downgrade case: this client knows a key the daemon does not, so the
    // effective document arrives without it.
    const p = terminalPrefs({});
    expect(p.fontSize).toBe(12);
    expect(p.scrollback).toBe(10000);
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

describe("hasMarkerColor", () => {
  it("treats the unassigned sentinel as absent", () => {
    expect(hasMarkerColor("")).toBe(false);
    expect(hasMarkerColor("#008cff")).toBe(true);
    // Shape-checked because the value goes into a CSS colour position; the daemon
    // stores only lowercase #rrggbb.
    expect(hasMarkerColor("#008CFF")).toBe(false);
    expect(hasMarkerColor("#08f")).toBe(false);
    expect(hasMarkerColor("red")).toBe(false);
  });
});

describe("markerFace", () => {
  const both = { emoji: "🦊", marker_color: "#008cff" };

  it("follows the style when both faces exist", () => {
    expect(markerFace({}, both)).toEqual({ kind: "color", color: "#008cff" });
    expect(markerFace({ "worktree.markerStyle": "emoji" }, both)).toEqual({
      kind: "emoji",
      emoji: "🦊",
    });
  });

  it("uses the glyph while a colour is still unassigned", () => {
    // The upgrade window: a row migrated from before the colour column, whose
    // hue arrives on the next sync. Colour is the default style, so without this
    // the rail would render nothing at all for every existing worktree.
    expect(markerFace({}, { emoji: "🦊", marker_color: "" })).toEqual({
      kind: "emoji",
      emoji: "🦊",
    });
  });

  it("uses the colour when the glyph is the missing face", () => {
    expect(
      markerFace(
        { "worktree.markerStyle": "emoji" },
        { emoji: "", marker_color: "#ff3502" },
      ),
    ).toEqual({ kind: "color", color: "#ff3502" });
  });

  it("is null only when neither face exists", () => {
    expect(markerFace({}, { emoji: "", marker_color: "" })).toBeNull();
  });
});

describe("quickSwitchPrefs", () => {
  it("reads both switches and defaults them on", () => {
    expect(
      quickSwitchPrefs({
        "browser.quickSwitch.responsive": false,
        "browser.quickSwitch.colorScheme": true,
      }),
    ).toEqual({ responsive: false, colorScheme: true });
    // A daemon that predates the keys shows both, which is the shipped default.
    expect(quickSwitchPrefs({})).toEqual({ responsive: true, colorScheme: true });
  });

  it("type-checks rather than coercing", () => {
    // `0` distinguishes the two readings: a truthiness test would report the
    // switch as hidden, while a type check falls back to the default. The daemon
    // rejects a non-bool on write, so this is the path where one got in another
    // way — a hand-edited row, or a key a newer build gave a different type.
    expect(
      quickSwitchPrefs({
        "browser.quickSwitch.colorScheme": 0 as unknown as boolean,
      }).colorScheme,
    ).toBe(true);
  });
});

describe("detachGraceMinutes", () => {
  it("reads the stored value and falls back for an older daemon", () => {
    expect(detachGraceMinutes({ "terminal.detachGraceMinutes": 90 })).toBe(90);
    expect(detachGraceMinutes({})).toBe(30);
    // A wrong-typed value must not reach a NumberInput as a string.
    expect(
      detachGraceMinutes({ "terminal.detachGraceMinutes": "soon" as unknown as number }),
    ).toBe(30);
  });
});
