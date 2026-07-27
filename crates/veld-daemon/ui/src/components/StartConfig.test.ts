import { beforeEach, describe, expect, it } from "vitest";
import type { Worktree } from "../api";
import {
  defaultStartSelection,
  parseStartSelection,
  pruneStartSelection,
  resolveStartSelection,
  startBody,
  startStorageKey,
} from "./StartConfig";

const wt = (over: Partial<Worktree> = {}): Worktree => ({
  id: 1,
  repo_root: "/repo",
  path: "/wts/chk",
  branch: "feat/checkout-v2",
  alias: "chk",
  emoji: "🦊",
  is_main: false,
  created_at: "2026-01-01T00:00:00Z",
  has_veld_config: true,
  presets: [],
  nodes: [],
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
    presets: ["full"],
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
    expect(defaultStartSelection(wt({ presets: ["a", "b"] }))).toEqual({
      kind: "preset",
      name: "a",
    });
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
    presets: ["full"],
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

describe("startBody", () => {
  it("sends exactly one of preset / selections", () => {
    // `veld start` treats the two as mutually exclusive; sending both, or
    // neither, fails with "No selections provided".
    expect(startBody({ kind: "preset", name: "full" })).toEqual({
      preset: "full",
    });
    expect(startBody({ kind: "nodes", selections: ["api:dev"] })).toEqual({
      selections: ["api:dev"],
    });
  });
});
