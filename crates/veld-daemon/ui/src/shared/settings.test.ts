import { describe, expect, it } from "vitest";

import {
  hasMarkerColor,
  markerFace,
  detachGraceMinutes,
  externalOrigins,
  logsTimeZone,
  markerStyle,
  quickSwitchPrefs,
  terminalInterceptSystemOpen,
  terminalOpenUrlsInApp,
  terminalPrefs,
  hideDisabledActions,
  gitCreateFrom,
  stalenessHue,
} from "./settings";

describe("terminalPrefs", () => {
  it("reads what the daemon sent", () => {
    const p = terminalPrefs({
      "terminal.fontSize": 15,
      "terminal.fontFamily": "Fira Code",
      "terminal.cursorStyle": "bar",
      "terminal.cursorBlink": false,
      "terminal.scrollback": 1000,
      "terminal.bellVolume": 60,
      "terminal.shiftEnterNewline": false,
      "terminal.reconnectTries": 5,
      "terminal.reconnectBackoffSeconds": 12,
      "terminal.reconnectFirstDelaySeconds": 2,
    });
    expect(p).toEqual({
      fontSize: 15,
      fontFamily: "Fira Code",
      cursorStyle: "bar",
      cursorBlink: false,
      scrollback: 1000,
      bellVolume: 60,
      shiftEnterNewline: false,
      reconnectTries: 5,
      reconnectBackoffSeconds: 12,
      reconnectFirstDelaySeconds: 2,
    });
  });

  it("defaults auto-reconnect on at three near-immediate tries", () => {
    // The shipped default: a dropped socket reconnects on its own a few times,
    // starting nearly immediately — the case the feature exists for — and 0 is
    // the off switch.
    const p = terminalPrefs({});
    expect(p.reconnectTries).toBe(3);
    expect(p.reconnectBackoffSeconds).toBe(5);
    expect(p.reconnectFirstDelaySeconds).toBe(1);
    expect(terminalPrefs({ "terminal.reconnectTries": 0 }).reconnectTries).toBe(0);
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

describe("logsTimeZone", () => {
  it("reads the key and defaults to local", () => {
    expect(logsTimeZone({ "logs.timeZone": "utc" })).toBe("utc");
    expect(logsTimeZone({ "logs.timeZone": "local" })).toBe("local");
    // A daemon that predates the key: local is both the shipped default and what this
    // view already did before the setting existed, so the two rules agree here.
    expect(logsTimeZone({})).toBe("local");
  });

  it("falls back rather than trusting a value it does not know", () => {
    // The daemon rejects these on write, so this is the path where one got in another
    // way — a hand-edited row, or a newer build that added a named zone. Showing
    // local is the honest degrade: it is what the control will report, too.
    for (const bad of ["UTC", "Europe/Berlin", "", true, 0]) {
      expect(logsTimeZone({ "logs.timeZone": bad as unknown as string })).toBe(
        "local",
      );
    }
  });
});

describe("hideDisabledActions", () => {
  it("defaults on (the shipped behaviour for a new control)", () => {
    expect(hideDisabledActions({})).toBe(true);
  });

  it("reads the stored boolean", () => {
    expect(hideDisabledActions({ "ui.hideDisabledActions": false })).toBe(false);
    expect(hideDisabledActions({ "ui.hideDisabledActions": true })).toBe(true);
  });

  it("ignores a non-boolean value rather than trusting it", () => {
    for (const bad of ["off", 0, null]) {
      expect(
        hideDisabledActions({ "ui.hideDisabledActions": bad as unknown as boolean }),
      ).toBe(true);
    }
  });
});

describe("gitCreateFrom", () => {
  it("defaults to origin (the born-current behaviour for a new worktree)", () => {
    expect(gitCreateFrom({})).toBe("origin");
  });

  it("reads the stored value", () => {
    expect(gitCreateFrom({ "git.createFrom": "local" })).toBe("local");
    expect(gitCreateFrom({ "git.createFrom": "origin" })).toBe("origin");
  });

  it("degrades to origin for anything that is not a real value", () => {
    for (const bad of ["origin ", "", 0, null, "merge"]) {
      expect(
        gitCreateFrom({ "git.createFrom": bad as unknown as string }),
      ).toBe("origin");
    }
  });
});

describe("stalenessHue", () => {
  // The baseline: a single commit a week old, or fifty commits today, are both
  // at the top of the scale (red).
  const WEEK = 7 * 86_400;

  it("baseline: a week-old single commit is red", () => {
    expect(stalenessHue(1, WEEK)).toBeLessThan(20);
  });

  it("baseline: fifty commits in a day is red", () => {
    expect(stalenessHue(50, 86_400)).toBeLessThan(20);
  });

  it("a fresh single commit stays green", () => {
    expect(stalenessHue(1, 60 * 60 * 4)).toBeGreaterThan(100);
  });

  it("mixes count and age — a big pile is urgent even if recent", () => {
    // 50 commits from today: far more red than one commit today.
    expect(stalenessHue(50, 0)).toBeLessThan(stalenessHue(1, 0));
  });

  it("sensitivity scales both thresholds", () => {
    // 25 commits today is half of the baseline's 50, so it reads mid-scale at
    // sensitivity 1 but red at sensitivity 2.
    expect(stalenessHue(25, 0, 1)).toBeGreaterThan(stalenessHue(25, 0, 2));
    // A half-week-old commit: green at 0.5, red at 2.
    expect(stalenessHue(1, WEEK / 2, 0.5)).toBeGreaterThan(stalenessHue(1, WEEK / 2, 2));
  });

  it("clamps so extreme values stay on the scale and never divide by zero", () => {
    expect(stalenessHue(1000, 999 * 86_400)).toBeGreaterThanOrEqual(0);
    expect(stalenessHue(0, 0)).toBeLessThanOrEqual(140);
    // A 0 sensitivity must not invert or throw.
    expect(stalenessHue(10, WEEK, 0)).toBeLessThanOrEqual(140);
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

describe("terminal URL routing", () => {
  it("defaults to opening links in Veld, including against an older daemon", () => {
    // The `quickSwitch*` exception: this build's UI says links open here, so a
    // daemon that has never heard of the key must not make it look broken.
    expect(terminalOpenUrlsInApp({})).toBe(true);
    expect(terminalOpenUrlsInApp({ "terminal.openUrlsInApp": false })).toBe(false);
    // A wrong-typed value falls back rather than being coerced — `0` would read as
    // "off" under a truthiness test.
    expect(
      terminalOpenUrlsInApp({
        "terminal.openUrlsInApp": 0 as unknown as boolean,
      }),
    ).toBe(true);
  });

  it("defaults to catching open/xdg-open as well", () => {
    expect(terminalInterceptSystemOpen({})).toBe(true);
    expect(
      terminalInterceptSystemOpen({ "terminal.interceptSystemOpen": false }),
    ).toBe(false);
  });

  it("reads the exempt list and degrades to no exemptions", () => {
    expect(
      externalOrigins({ "browser.externalOrigins": ["https://a.example", "https://*.b.example"] }),
    ).toEqual(["https://a.example", "https://*.b.example"]);
    // Absent, wrong-typed, or holding non-strings: an empty list, which means
    // "nothing is exempt" — the direction that shows a URL in a pane rather than
    // silently sending it somewhere the user did not choose.
    expect(externalOrigins({})).toEqual([]);
    expect(
      externalOrigins({ "browser.externalOrigins": "https://a.example" }),
    ).toEqual([]);
    expect(
      externalOrigins({
        "browser.externalOrigins": ["https://a.example", 7 as unknown as string],
      }),
    ).toEqual(["https://a.example"]);
  });
});
