import { describe, expect, it } from "vitest";
import type { Preset, StartOrigin } from "../api";
import { originIsStale, presetState, startOriginLabel } from "./startOrigin";

const preset = (name: string, expanded: string[]): Preset => ({
  name,
  key: 1,
  pinned: false,
  selections: expanded,
  expanded,
  is_default: false,
});

const origin = (preset: string | null, selections: string[]): StartOrigin => ({
  preset,
  selections,
});

describe("presetState", () => {
  it("is current only when the expansion still matches", () => {
    const presets = [preset("web", ["api:local", "web:local"])];
    expect(presetState(origin("web", ["api:local", "web:local"]), presets)).toBe(
      "current",
    );
    // Edited after the run started: the name resolves, but not to what ran.
    expect(presetState(origin("web", ["web:local"]), presets)).toBe("redefined");
    // Renamed or deleted.
    expect(presetState(origin("gone", ["web:local"]), presets)).toBe("removed");
  });

  it("treats a preset that no longer expands as redefined, not current", () => {
    // The daemon sends an empty expansion when the preset exists but its
    // `@ref` or node is gone. Whatever the name means now, it is not what ran.
    const presets = [preset("web", [])];
    expect(presetState(origin("web", ["web:local"]), presets)).toBe("redefined");
    // And an empty-vs-empty comparison must not read as agreement about a
    // preset that cannot start anything.
    expect(presetState(origin("web", []), presets)).toBe("current");
  });

  it("compares element-wise on the sorted lists both sides ship", () => {
    const presets = [preset("web", ["a:x", "b:y"])];
    expect(presetState(origin("web", ["b:y", "a:x"]), presets)).toBe("redefined");
  });
});

describe("startOriginLabel", () => {
  const presets = [preset("web", ["api:local"])];

  it("says nothing at all for a run recorded before the origin existed", () => {
    // Not "unknown": that reads as a property of the run rather than of the
    // recording, and every surface here omits the line instead.
    expect(startOriginLabel(null, presets)).toBeNull();
    expect(startOriginLabel(undefined, presets)).toBeNull();
  });

  it("names the preset, and states when it no longer means the same thing", () => {
    expect(startOriginLabel(origin("web", ["api:local"]), presets)).toBe(
      "preset web",
    );
    expect(startOriginLabel(origin("web", ["api:local", "db:local"]), presets)).toBe(
      "preset web (redefined since start)",
    );
    expect(startOriginLabel(origin("old", ["api:local"]), presets)).toBe(
      "preset old (no longer defined)",
    );
  });

  it("states the preset in the past tense when the config is not known", () => {
    // Runs mode polls /api/environments only. Passing `[]` there would render
    // "no longer defined" for every run — a confident falsehood from missing
    // data — so `null` means "cannot compare" and the tokens carry the truth.
    expect(startOriginLabel(origin("web", ["api:local"]), null)).toBe(
      "preset web · api:local",
    );
    expect(startOriginLabel(origin("web", []), null)).toBe("preset web");
  });

  it("names the tokens for an explicit-selection start", () => {
    expect(startOriginLabel(origin(null, ["api:local", "web:local"]), presets)).toBe(
      "api:local, web:local",
    );
    expect(startOriginLabel(origin(null, []), presets)).toBe(
      "no selections recorded",
    );
  });
});

describe("originIsStale", () => {
  it("is false for an explicit-selection start, which cannot drift", () => {
    expect(originIsStale(origin(null, ["api:local"]), [])).toBe(false);
  });

  it("is true exactly when the named preset disagrees with the config now", () => {
    const presets = [preset("web", ["api:local"])];
    expect(originIsStale(origin("web", ["api:local"]), presets)).toBe(false);
    expect(originIsStale(origin("web", ["web:local"]), presets)).toBe(true);
    // An empty list IS knowledge: this project defines no presets, so a run
    // that named one is naming something gone.
    expect(originIsStale(origin("web", ["api:local"]), [])).toBe(true);
  });

  it("marks nothing when the config is not known to the caller", () => {
    expect(originIsStale(origin("web", ["api:local"]), null)).toBe(false);
  });
});
