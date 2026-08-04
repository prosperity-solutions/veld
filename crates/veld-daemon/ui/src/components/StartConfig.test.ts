import { beforeEach, describe, expect, it } from "vitest";
import type { Preset, Worktree } from "../api";
import {
  defaultStartSelection,
  parseStartSelection,
  presetHeading,
  pruneStartSelection,
  resolveStartSelection,
  startBody,
  startSelectionLabel,
  startStorageKey,
} from "./StartConfig";

/** A preset as the daemon sends it, with the keys already assigned. */
const preset = (name: string, over: Partial<Preset> = {}): Preset => ({
  name,
  key: 1,
  pinned: false,
  selections: [`${name}:dev`],
  is_default: false,
  ...over,
});

const wt = (over: Partial<Worktree> = {}): Worktree => ({
  id: 1,
  repo_root: "/repo",
  path: "/wts/chk",
  branch: "feat/checkout-v2",
  alias: "chk",
  emoji: "🦊",
  marker_hue: 3,
  is_main: false,
  created_at: "2026-01-01T00:00:00Z",
  has_veld_config: true,
  presets: [],
  nodes: [],
  ide: { quicklinks: [], permissions: [] },
  ...over,
});

describe("parseStartSelection", () => {
  it("round-trips both shapes", () => {
    expect(parseStartSelection(JSON.stringify({ kind: "preset", name: "a" })))
      .toEqual({ kind: "preset", name: "a" });
    expect(
      parseStartSelection(
        JSON.stringify({ kind: "nodes", selections: ["api:dev"] }),
      ),
    ).toEqual({ kind: "nodes", selections: ["api:dev"] });
  });

  it("rejects anything that isn't a selection", () => {
    // Stored values outlive schema changes and survive hand-editing; a throw
    // here would take down the whole app on boot.
    expect(parseStartSelection("")).toBeNull();
    expect(parseStartSelection("not json")).toBeNull();
    expect(parseStartSelection("null")).toBeNull();
    expect(parseStartSelection('{"kind":"bogus"}')).toBeNull();
    expect(parseStartSelection('{"kind":"preset"}')).toBeNull();
    expect(parseStartSelection('{"kind":"nodes","selections":"api"}')).toBeNull();
  });
});

describe("pruneStartSelection", () => {
  const w = wt({
    presets: [preset("full")],
    nodes: [{ name: "api", variants: ["dev", "prod"], default_variant: "dev" }],
  });

  it("keeps selections the config still offers", () => {
    expect(pruneStartSelection(w, { kind: "preset", name: "full" })).toEqual({
      kind: "preset",
      name: "full",
    });
    expect(
      pruneStartSelection(w, { kind: "nodes", selections: ["api:prod"] }),
    ).toEqual({ kind: "nodes", selections: ["api:prod"] });
  });

  it("drops a preset that was renamed away", () => {
    expect(pruneStartSelection(w, { kind: "preset", name: "gone" })).toBeNull();
  });

  it("drops node selections the config no longer has", () => {
    // Partial survival: the valid half is kept.
    expect(
      pruneStartSelection(w, {
        kind: "nodes",
        selections: ["api:dev", "removed:dev", "api:gone"],
      }),
    ).toEqual({ kind: "nodes", selections: ["api:dev"] });
    // Nothing valid left → null, so the caller falls back to the default
    // rather than sending an empty list `veld start` would reject.
    expect(
      pruneStartSelection(w, { kind: "nodes", selections: ["removed:dev"] }),
    ).toBeNull();
  });

  it("passes null through", () => {
    expect(pruneStartSelection(w, null)).toBeNull();
  });
});

describe("defaultStartSelection", () => {
  it("prefers the first preset", () => {
    expect(
      defaultStartSelection(wt({ presets: [preset("a"), preset("b")] })),
    ).toEqual({ kind: "preset", name: "a" });
  });

  it("prefers the config's default_preset over list position", () => {
    // The author said which preset is the default; "first in the file" is a
    // guess, and picking it would start the wrong thing for every project whose
    // default isn't declared first.
    expect(
      defaultStartSelection(
        wt({
          presets: [preset("a"), preset("b", { is_default: true })],
        }),
      ),
    ).toEqual({ kind: "preset", name: "b" });
  });

  it("falls back to every node at its default variant", () => {
    expect(
      defaultStartSelection(
        wt({
          nodes: [
            { name: "api", variants: ["dev", "prod"], default_variant: "prod" },
            { name: "web", variants: ["dev"] },
          ],
        }),
      ),
    ).toEqual({ kind: "nodes", selections: ["api:prod", "web:dev"] });
  });

  it("is null when there is nothing to start", () => {
    expect(defaultStartSelection(wt())).toBeNull();
  });
});

/**
 * Minimal localStorage for the node test environment (the UI suite runs
 * without a DOM). `resolveStartSelection` reads through `globalThis`, so
 * installing this is enough to exercise the real code path.
 */
const store = (() => {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    clear: () => map.clear(),
    key: (i: number) => [...map.keys()][i] ?? null,
    get length() {
      return map.size;
    },
  } satisfies Storage;
})();

describe("resolveStartSelection", () => {
  beforeEach(() => {
    globalThis.localStorage = store;
    store.clear();
  });

  const w = wt({
    presets: [preset("full")],
    nodes: [{ name: "api", variants: ["dev"], default_variant: "dev" }],
  });

  it("reads the worktree's stored choice", () => {
    store.setItem(
      startStorageKey(w.path),
      JSON.stringify({ kind: "preset", name: "full" }),
    );
    expect(resolveStartSelection(w)).toEqual({ kind: "preset", name: "full" });
  });

  it("keys storage per worktree", () => {
    store.setItem(
      startStorageKey("/other"),
      JSON.stringify({ kind: "nodes", selections: ["api:dev"] }),
    );
    // Another worktree's choice must not leak in — the rail starts rows
    // independently and would otherwise launch the wrong configuration.
    expect(resolveStartSelection(w)).toEqual({ kind: "preset", name: "full" });
  });

  it("falls back to the default for stale or absent storage", () => {
    expect(resolveStartSelection(w)).toEqual({ kind: "preset", name: "full" });
    store.setItem(
      startStorageKey(w.path),
      JSON.stringify({ kind: "preset", name: "deleted-preset" }),
    );
    expect(resolveStartSelection(w)).toEqual({ kind: "preset", name: "full" });
    store.setItem(startStorageKey(w.path), "garbage");
    expect(resolveStartSelection(w)).toEqual({ kind: "preset", name: "full" });
  });

  it("is null when the worktree has nothing to start", () => {
    expect(resolveStartSelection(wt())).toBeNull();
  });
});

describe("presetHeading", () => {
  it("labels the ungrouped bucket 'Other' so it can't read as part of a group", () => {
    // The daemon's resolver orders groups by their lowest key, so the ungrouped
    // bucket can land *between* two groups. Without its own heading those radios
    // render under the preceding group's title.
    const presets = [
      preset("dev", { key: 1, group: "Everyday" }),
      preset("loose", { key: 2 }),
      preset("docker", { key: 5, group: "Docker" }),
    ];
    expect(presetHeading(presets, 0)).toBe("Everyday");
    expect(presetHeading(presets, 1)).toBe("Other");
    expect(presetHeading(presets, 2)).toBe("Docker");
  });

  it("repeats no heading within a run of the same group", () => {
    const presets = [
      preset("a", { key: 1, group: "Everyday" }),
      preset("b", { key: 2, group: "Everyday" }),
    ];
    expect(presetHeading(presets, 0)).toBe("Everyday");
    expect(presetHeading(presets, 1)).toBeNull();
  });

  it("renders no headings at all when no preset declares a group", () => {
    const presets = [preset("a", { key: 1 }), preset("b", { key: 2 })];
    expect(presetHeading(presets, 0)).toBeNull();
    expect(presetHeading(presets, 1)).toBeNull();
  });
});

describe("startSelectionLabel", () => {
  it("shows a preset's label, falling back to its config key", () => {
    const w = wt({
      presets: [preset("web-prod-stg", { label: "Site preview" })],
    });
    expect(startSelectionLabel({ kind: "preset", name: "web-prod-stg" }, w))
      .toBe("Site preview");
    // No worktree in hand, or no label declared: the config key is all there is.
    expect(startSelectionLabel({ kind: "preset", name: "web-prod-stg" }))
      .toBe("web-prod-stg");
    expect(
      startSelectionLabel(
        { kind: "preset", name: "plain" },
        wt({ presets: [preset("plain")] }),
      ),
    ).toBe("plain");
  });
});

describe("startBody", () => {
  it("sends exactly one of preset / selections", () => {
    // `veld start` treats the two as mutually exclusive. Sending neither is not a
    // safe no-op either: it falls back to the project's `default_preset` when one
    // is declared, and only fails "No selections provided" when there isn't one.
    expect(startBody({ kind: "preset", name: "full" })).toEqual({
      preset: "full",
    });
    expect(startBody({ kind: "nodes", selections: ["api:dev"] })).toEqual({
      selections: ["api:dev"],
    });
  });
});
