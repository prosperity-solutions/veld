import { describe, expect, it } from "vitest";

import {
  autoWhileSharingKey,
  keepAwakePrefs,
  KEEP_AWAKE_SHARING_ON_BATTERY,
  KEEP_AWAKE_SHARING_ON_POWER,
  hasMarkerColor,
  markerFace,
  detachGraceMinutes,
  externalOrigins,
  focusPrefs,
  focusSuppresses,
  FOCUS_SUPPRESS_BELL,
  FOCUS_SUPPRESS_TOASTS,
  FOCUS_SUPPRESS_OS_NOTIFICATIONS,
  logsTimeZone,
  markerStyle,
  quickSwitchPrefs,
  searchUrl,
  terminalAgentIntegration,
  terminalInterceptSystemOpen,
  terminalOpenUrlsInApp,
  terminalPrefs,
  terminalShellIntegration,
  terminalShell,
  hideDisabledActions,
  showProjectColumn,
  showProjectNews,
  extensionsSource,
  newsSource,
  gitCreateFrom,
  worktreeStorageMode,
  worktreeStorageDir,
  stalenessHue,
} from "./settings";

describe("terminalPrefs", () => {
  it("reads what the daemon sent", () => {
    const p = terminalPrefs({
      "terminal.fontSize": 15,
      "terminal.fontFamily": "Fira Code",
      "terminal.cursorStyle": "bar",
      "terminal.cursorBlink": false,
      "terminal.ligatures": true,
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
      ligatures: true,
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
    expect(p.ligatures).toBe(false);
  });

  it("rejects a wrong-typed value rather than passing it to xterm", () => {
    const p = terminalPrefs({
      "terminal.fontSize": "big" as unknown as number,
      "terminal.cursorBlink": 1 as unknown as boolean,
      "terminal.cursorStyle": "wobble",
      "terminal.ligatures": "yes" as unknown as boolean,
    });
    expect(p.fontSize).toBe(12);
    expect(p.cursorBlink).toBe(true);
    expect(p.cursorStyle).toBe("block");
    expect(p.ligatures).toBe(false);
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

describe("focusPrefs", () => {
  it("reads the master switch and all three channels", () => {
    expect(
      focusPrefs({
        "focus.enabled": true,
        [FOCUS_SUPPRESS_BELL]: false,
        [FOCUS_SUPPRESS_TOASTS]: true,
        [FOCUS_SUPPRESS_OS_NOTIFICATIONS]: false,
      }),
    ).toEqual({
      enabled: true,
      suppress: {
        [FOCUS_SUPPRESS_BELL]: false,
        [FOCUS_SUPPRESS_TOASTS]: true,
        [FOCUS_SUPPRESS_OS_NOTIFICATIONS]: false,
      },
    });
  });

  it("defaults off, with all three channels suppressed once turned on", () => {
    // A daemon that predates the keys: the master switch takes the previous
    // release's behaviour (there was no focus mode), but the three channels
    // take the shipped default — see the `quickSwitch*` exception in FALLBACK's
    // docblock. Turning the master on must silence something immediately.
    expect(focusPrefs({})).toEqual({
      enabled: false,
      suppress: {
        [FOCUS_SUPPRESS_BELL]: true,
        [FOCUS_SUPPRESS_TOASTS]: true,
        [FOCUS_SUPPRESS_OS_NOTIFICATIONS]: true,
      },
    });
  });
});

describe("focusSuppresses", () => {
  it("requires both the master switch and the channel's own row", () => {
    const on = (channel: string) =>
      focusSuppresses({ enabled: true, suppress: { [channel]: true } }, channel);
    const off = (channel: string) =>
      focusSuppresses({ enabled: true, suppress: { [channel]: false } }, channel);
    expect(on(FOCUS_SUPPRESS_BELL)).toBe(true);
    expect(off(FOCUS_SUPPRESS_BELL)).toBe(false);
    // Master off silences nothing, whatever the row says — the row is only a
    // preview of what turning the master on would do.
    expect(
      focusSuppresses({ enabled: false, suppress: { [FOCUS_SUPPRESS_TOASTS]: true } },
        FOCUS_SUPPRESS_TOASTS),
    ).toBe(false);
    // A channel absent from `suppress` (an older daemon's document) reads as
    // not-suppressed rather than throwing.
    expect(
      focusSuppresses({ enabled: true, suppress: {} }, FOCUS_SUPPRESS_OS_NOTIFICATIONS),
    ).toBe(false);
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

describe("showProjectNews", () => {
  it("defaults on, because an opt-in news channel launches to nobody", () => {
    expect(showProjectNews({})).toBe(true);
  });

  it("is the one thing that can silence a project's cards", () => {
    expect(showProjectNews({ "ui.showProjectNews": false })).toBe(false);
    expect(showProjectNews({ "ui.showProjectNews": true })).toBe(true);
  });

  it("falls back rather than trusting a non-boolean", () => {
    for (const bad of [null, 0, "false", []]) {
      expect(showProjectNews({ "ui.showProjectNews": bad as unknown as boolean })).toBe(true);
    }
  });
});

describe("showProjectColumn", () => {
  /**
   * Off, matching the Rust default — and the same answer for the reason the
   * `quickSwitch*` exception gives: a key that decides whether a *control appears*
   * takes the shipped default, so a daemon too old to know it keeps rendering what
   * it always did rather than growing a column nobody asked for.
   */
  it("defaults off, so an older daemon renders what it always did", () => {
    expect(showProjectColumn({})).toBe(false);
  });

  it("reads the stored value", () => {
    expect(showProjectColumn({ "ui.showProjectColumn": true })).toBe(true);
    expect(showProjectColumn({ "ui.showProjectColumn": false })).toBe(false);
  });

  it("falls back rather than trusting a non-boolean", () => {
    for (const bad of [null, 1, "true", []]) {
      expect(showProjectColumn({ "ui.showProjectColumn": bad as unknown as boolean })).toBe(
        false,
      );
    }
  });
});

describe("extensionsSource", () => {
  it("falls back to worktree — the previous release's only, hardcoded behaviour — when the daemon has never heard of the key", () => {
    expect(extensionsSource({})).toBe("worktree");
  });

  it("reads the stored value, including the daemon's new main default", () => {
    expect(extensionsSource({ "extensions.source": "main" })).toBe("main");
    expect(extensionsSource({ "extensions.source": "worktree" })).toBe(
      "worktree",
    );
  });

  it("degrades to the worktree fallback for anything that is not a real value", () => {
    for (const bad of ["Main", "", 0, null, "origin"]) {
      expect(
        extensionsSource({ "extensions.source": bad as unknown as string }),
      ).toBe("worktree");
    }
  });
});

describe("newsSource", () => {
  it("defaults to main, unchanged from the hardcoded behaviour before this key", () => {
    expect(newsSource({})).toBe("main");
  });

  it("reads the stored value", () => {
    expect(newsSource({ "news.source": "worktree" })).toBe("worktree");
    expect(newsSource({ "news.source": "main" })).toBe("main");
  });

  it("degrades to main for anything that is not a real value", () => {
    for (const bad of ["Worktree", "", 0, null]) {
      expect(newsSource({ "news.source": bad as unknown as string })).toBe(
        "main",
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

describe("worktreeStorageMode", () => {
  it("defaults to sibling (today's only behaviour)", () => {
    expect(worktreeStorageMode({})).toBe("sibling");
  });

  it("reads the stored value", () => {
    expect(worktreeStorageMode({ "worktree.storageMode": "custom" })).toBe(
      "custom",
    );
    expect(worktreeStorageMode({ "worktree.storageMode": "sibling" })).toBe(
      "sibling",
    );
  });

  it("degrades to sibling for anything that is not a real value", () => {
    for (const bad of ["Custom", "", 0, null]) {
      expect(
        worktreeStorageMode({
          "worktree.storageMode": bad as unknown as string,
        }),
      ).toBe("sibling");
    }
  });
});

describe("worktreeStorageDir", () => {
  it("defaults to empty (no folder chosen yet)", () => {
    expect(worktreeStorageDir({})).toBe("");
  });

  it("reads and trims the stored value", () => {
    expect(
      worktreeStorageDir({ "worktree.storageDir": " /Users/me/worktrees " }),
    ).toBe("/Users/me/worktrees");
  });

  it("degrades to empty — not the fallback of a non-string field — for anything that is not a string", () => {
    for (const bad of [0, null, ["/tmp"]]) {
      expect(
        worktreeStorageDir({ "worktree.storageDir": bad as unknown as string }),
      ).toBe("");
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

describe("terminalShell", () => {
  it("returns the stored path verbatim and defaults to auto", () => {
    expect(terminalShell({ "terminal.shell": "/bin/bash" })).toBe("/bin/bash");
    expect(terminalShell({})).toBe("auto");
    // A path this machine does not have is still returned: the picker shows it as
    // the chosen value rather than silently resetting a choice the user made, and
    // the *daemon* is what falls back at spawn time.
    expect(terminalShell({ "terminal.shell": "/opt/gone/fish" })).toBe(
      "/opt/gone/fish",
    );
    // A wrong-typed or empty value falls back rather than reaching the picker as
    // a selected option with no label.
    expect(terminalShell({ "terminal.shell": "" })).toBe("auto");
    expect(
      terminalShell({ "terminal.shell": 7 as unknown as string }),
    ).toBe("auto");
  });
});

describe("terminal integration switches", () => {
  it("default on, and each reads only its own key", () => {
    expect(terminalShellIntegration({})).toBe(true);
    expect(terminalAgentIntegration({})).toBe(true);
    // Independent: one off leaves the other alone. All three integration switches ride
    // one generated shell file, and the coupling that shipped once made turning off
    // `interceptSystemOpen` silently remove the rail's unread badge.
    const doc = {
      "terminal.shellIntegration": false,
      "terminal.interceptSystemOpen": false,
    };
    expect(terminalShellIntegration(doc)).toBe(false);
    expect(terminalAgentIntegration(doc)).toBe(true);
  });

  it("ignores a non-boolean rather than reading it as off", () => {
    // `bool` is a typeof check, so a stored `0` or `""` from a hand-edited row falls
    // back to the shipped default instead of quietly disabling a feature.
    expect(terminalShellIntegration({ "terminal.shellIntegration": 0 })).toBe(true);
    expect(terminalAgentIntegration({ "terminal.agentIntegration": "off" })).toBe(true);
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

describe("searchUrl", () => {
  it("keeps an empty template, because empty is the off switch", () => {
    // The one thing this reader exists for. `str()` — which every other string
    // setting goes through — treats `""` as absent and substitutes the fallback, so
    // routing this key through it would make "no search" the single preference a user
    // cannot save. A future simplification to `str(doc, "browser.searchUrl")` has to
    // fail here rather than silently delete the off switch.
    expect(searchUrl({ "browser.searchUrl": "" })).toBe("");
    expect(searchUrl({ "browser.searchUrl": "   " })).toBe("");
  });

  it("falls back to the shipped engine only when the daemon said nothing", () => {
    // Mirrors `DEFAULT_SEARCH_URL` in `veld-core/src/db/settings.rs`. Nothing ties the
    // two strings together, so this assertion is the drift alarm: an older daemon that
    // has never heard of the key must still leave a client able to search, because the
    // address bar's placeholder has already promised it can.
    expect(searchUrl({})).toBe("https://www.google.com/search?q=%s");
    expect(searchUrl({ "browser.searchUrl": 7 as unknown as string })).toBe(
      "https://www.google.com/search?q=%s",
    );
  });

  it("trims, so a template is compared and used in one spelling", () => {
    expect(searchUrl({ "browser.searchUrl": "  https://d.example/?q=%s " })).toBe(
      "https://d.example/?q=%s",
    );
  });
});

describe("keepAwakePrefs", () => {
  it("defaults both automatic halves on, with the shorter allowance on battery", () => {
    // Mirrors the Rust defaults in `veld-core/src/db/settings.rs`. Nothing ties
    // the two sets of numbers together, so this is the drift alarm — and the
    // *asymmetry* is the part worth pinning: equal caps would mean the split
    // settings bought nothing, since the whole reason there are two is that a
    // hold on mains spends nothing and a hold on battery spends somebody's charge.
    const prefs = keepAwakePrefs({});
    expect(prefs.sharingOnPower).toBe(true);
    expect(prefs.sharingOnBattery).toBe(true);
    expect(prefs.sharingOnPowerMinutes).toBe(120);
    expect(prefs.sharingOnBatteryMinutes).toBe(30);
    expect(prefs.sharingOnBatteryMinutes).toBeLessThan(prefs.sharingOnPowerMinutes);
  });

  it("defaults the manual battery reach on, because that is what it already did", () => {
    // This one is an off switch for existing behaviour, not a new default. If it
    // ever fell back to `false`, upgrading would silently take away the lid
    // coverage somebody had been relying on with no message anywhere.
    expect(keepAwakePrefs({}).manualOnBattery).toBe(true);
  });

  it("reads what the daemon actually stored", () => {
    const prefs = keepAwakePrefs({
      [KEEP_AWAKE_SHARING_ON_POWER]: false,
      "keepAwake.sharingOnPowerMinutes": 240,
      [KEEP_AWAKE_SHARING_ON_BATTERY]: false,
      "keepAwake.sharingOnBatteryMinutes": 15,
      "keepAwake.manualOnBattery": false,
    });
    expect(prefs).toEqual({
      sharingOnPower: false,
      sharingOnPowerMinutes: 240,
      sharingOnBattery: false,
      sharingOnBatteryMinutes: 15,
      manualOnBattery: false,
    });
  });

  it("ignores a value of the wrong shape rather than rendering it", () => {
    const prefs = keepAwakePrefs({
      [KEEP_AWAKE_SHARING_ON_POWER]: "yes" as unknown as boolean,
      "keepAwake.sharingOnPowerMinutes": Number.NaN,
    });
    expect(prefs.sharingOnPower).toBe(true);
    expect(prefs.sharingOnPowerMinutes).toBe(120);
  });
});

describe("autoWhileSharingKey", () => {
  it("picks the switch for the power source in force", () => {
    // The cup's menu shows one switch, and flipping it must write the one that
    // applies right now — writing the mains key while running on battery would
    // be a control that visibly does nothing.
    expect(autoWhileSharingKey("battery")).toBe(KEEP_AWAKE_SHARING_ON_BATTERY);
    expect(autoWhileSharingKey("mains")).toBe(KEEP_AWAKE_SHARING_ON_POWER);
  });
});
