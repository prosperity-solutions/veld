import { describe, expect, it } from "vitest";
import type { Preset, StartOrigin } from "../api";
import { presetState, startOriginLabel } from "./startOrigin";

/**
 * `expanded` here is shorthand: a token list means the daemon expanded it, `null`
 * means the expansion failed, and `"skipped"` means the listing did not try.
 */
const preset = (
  name: string,
  expanded: string[] | null | "skipped",
): Preset => ({
  name,
  key: 1,
  pinned: false,
  // Raw config entries. Present whatever happened to the expansion — the config
  // still says what the preset lists, it just may not resolve today.
  selections: Array.isArray(expanded) ? expanded : [],
  expansion: Array.isArray(expanded)
    ? { state: "ok", tokens: expanded }
    : expanded === null
      ? { state: "failed" }
      : { state: "skipped" },
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

  it("separates 'cannot expand' from 'redefined'", () => {
    // `null` is what the daemon sends when the preset exists but does not expand
    // (dangling `@ref`, removed node, over the expansion budget). Calling that
    // `redefined` claims the name means something else now, which is a different —
    // and false — statement, and it is the one `veld status` does NOT make.
    const broken = [preset("web", null)];
    expect(presetState(origin("web", ["web:local"]), broken)).toBe("unexpandable");
    expect(presetState(origin("web", []), broken)).toBe("unexpandable");

    // An empty *array* is a real expansion: a preset whose selections are empty
    // compares like any other.
    const empty = [preset("web", [])];
    expect(presetState(origin("web", ["web:local"]), empty)).toBe("redefined");
    expect(presetState(origin("web", []), empty)).toBe("current");
  });

  it("does not blame the config for a preset the listing skipped", () => {
    // `skipped` is our ignorance — the daemon caps how many presets it expands per
    // poll. Rendering it as `unexpandable` would point the reader at `veld lint`,
    // and lint reports nothing, so the one actionable thing in the message is
    // wrong.
    const skipped = [preset("web", "skipped")];
    expect(presetState(origin("web", ["web:local"]), skipped)).toBe("unknown");
    expect(startOriginLabel(origin("web", ["web:local"]), skipped)).toBe(
      "preset web · web:local",
    );
  });

  it("labels an unexpandable preset in the CLI's exact words", () => {
    // One config state, one description. `veld status` prints
    // "(cannot be expanded — see `veld lint`)" for this, and two surfaces wording
    // it differently is how a reader ends up trusting the wrong one.
    expect(startOriginLabel(origin("web", ["a:x"]), [preset("web", null)])).toBe(
      "preset web (cannot be expanded — see `veld lint`)",
    );
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
