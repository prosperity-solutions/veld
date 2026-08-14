import { describe, expect, it } from "vitest";
import {
  type CatalogEntry,
  type Choice,
  presetOptions,
  requirementMet,
  settingValue,
} from "./catalog";

function entry(over: Partial<CatalogEntry> = {}): CatalogEntry {
  return {
    key: "terminal.cursorBlink",
    title: "Blinking cursor",
    help: "…",
    group: "terminal",
    groupLabel: "Terminal",
    type: "bool",
    default: true,
    choices: { kind: "free" },
    ...over,
  };
}

describe("settingValue", () => {
  it("prefers the stored value over the catalog default", () => {
    expect(settingValue({ "terminal.cursorBlink": false }, entry())).toBe(false);
  });

  // The daemon serves *effective* values, so a missing key means the document has
  // not arrived yet — which is exactly when every control is disabled anyway.
  it("falls back to the catalog's default, never to a literal of its own", () => {
    expect(settingValue(null, entry())).toBe(true);
    expect(settingValue({}, entry())).toBe(true);
    expect(settingValue(null, entry({ default: 12 }))).toBe(12);
  });

  // `false`, `0` and `""` are real stored values. A `||` fallback would quietly
  // turn each of them back into the default, which for a boolean setting means a
  // switch that will not stay off.
  it("keeps a stored falsy value instead of treating it as absent", () => {
    expect(settingValue({ "terminal.cursorBlink": false }, entry())).toBe(false);
    const n = entry({ key: "terminal.bellVolume", type: "int", default: 50 });
    expect(settingValue({ "terminal.bellVolume": 0 }, n)).toBe(0);
    const s = entry({ key: "browser.searchUrl", type: "text", default: "https://x/?q=%s" });
    expect(settingValue({ "browser.searchUrl": "" }, s)).toBe("");
  });
});

describe("requirementMet", () => {
  const enabled = entry({ key: "focus.enabled", default: false });
  const byKey = new Map([["focus.enabled", enabled]]);

  it("opens the gate for a setting that has none", () => {
    expect(requirementMet({}, undefined, byKey)).toBe(true);
  });

  it("reads a boolean master switch", () => {
    const req = { key: "focus.enabled", equals: null };
    expect(requirementMet({ "focus.enabled": true }, req, byKey)).toBe(true);
    expect(requirementMet({ "focus.enabled": false }, req, byKey)).toBe(false);
  });

  it("compares a mode against a stored string", () => {
    const mode = entry({ key: "worktree.storageMode", type: "text", default: "sibling" });
    const map = new Map([["worktree.storageMode", mode]]);
    const req = { key: "worktree.storageMode", equals: "custom" };
    expect(requirementMet({ "worktree.storageMode": "custom" }, req, map)).toBe(true);
    expect(requirementMet({ "worktree.storageMode": "sibling" }, req, map)).toBe(false);
  });

  // The dependency's own default, not `false`. A master switch that ships **on**
  // must not grey out its dependants for the frame before the document lands.
  it("uses the dependency's default while the document is still loading", () => {
    const on = entry({ key: "terminal.openUrlsInApp", default: true });
    const map = new Map([["terminal.openUrlsInApp", on]]);
    const req = { key: "terminal.openUrlsInApp", equals: null };
    expect(requirementMet(null, req, map)).toBe(true);
  });

  // An older bundle against a newer daemon: refusing would make the dependent
  // setting permanently unreachable, and the daemon validates the write anyway.
  it("opens the gate when the dependency is unknown to this build", () => {
    const req = { key: "some.futureSetting", equals: null };
    expect(requirementMet({}, req, new Map())).toBe(true);
  });
});

describe("presetOptions", () => {
  const offered: Choice[] = [
    { value: "15", label: "15 minutes" },
    { value: "30", label: "30 minutes" },
    { value: "60", label: "1 hour" },
  ];

  it("leaves the offered list alone when the stored value is one of them", () => {
    expect(presetOptions(offered, 30, "min")).toEqual(offered);
  });

  // `Choices::Presets` accepts a whole range and offers a few values from it, so
  // a stored 45 is legal and unlisted. A NativeSelect whose value matches no
  // option renders blank — the control would claim the machine is set to
  // something it is not.
  it("splices an accepted-but-unoffered value in, in numeric order", () => {
    const got = presetOptions(offered, 45, "min");
    expect(got.map((o) => o.value)).toEqual(["15", "30", "45", "60"]);
    // The same register as its neighbours ("15 minutes", "1 hour"), not the
    // NumberInput abbreviation the catalog carries for a suffix.
    expect(got.find((o) => o.value === "45")?.label).toBe("45 minutes");
  });

  it("sorts numerically rather than lexicographically", () => {
    // "120" sorts before "15" as a string; the menu must not read 120, 15, 30.
    const got = presetOptions(offered, 120, "min");
    expect(got.map((o) => o.value)).toEqual(["15", "30", "60", "120"]);
  });

  it("labels an unoffered value without a unit when there is none", () => {
    expect(presetOptions(offered, 45, null).find((o) => o.value === "45")?.label).toBe("45");
  });
});
