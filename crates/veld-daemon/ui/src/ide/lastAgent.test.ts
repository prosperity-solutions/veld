import { describe, expect, it } from "vitest";

import type { KeyValueStore } from "./lastAgent";
import { lastAgentName, recallLastAgent, rememberLastAgent } from "./lastAgent";

const fake = (seed: Record<string, string> = {}) => {
  const map = new Map(Object.entries(seed));
  return {
    map,
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      map.set(k, v);
    },
  };
};

/** A store that refuses everything, the way a private window can. */
const hostile: KeyValueStore = {
  getItem: () => {
    throw new Error("nope");
  },
  setItem: () => {
    throw new Error("nope");
  },
};

describe("lastAgent", () => {
  it("remembers per project, keyed by repo root", () => {
    const store = fake();
    rememberLastAgent(store, "/git/veld", "codex");
    rememberLastAgent(store, "/git/other", "claude");
    expect(recallLastAgent(store, "/git/veld")).toBe("codex");
    expect(recallLastAgent(store, "/git/other")).toBe("claude");
    expect(store.map.get(lastAgentName("/git/veld"))).toBe("codex");
  });

  it("is silent about a project it has never seen", () => {
    // `""` means "no opinion", which is what makes the caller's fallback to the
    // project's first declared pane the answer rather than an error.
    expect(recallLastAgent(fake(), "/git/veld")).toBe("");
  });

  it("records nothing for an empty root or an empty agent", () => {
    const store = fake();
    rememberLastAgent(store, "", "claude");
    rememberLastAgent(store, "/git/veld", "");
    expect(store.map.size).toBe(0);
    expect(recallLastAgent(store, "")).toBe("");
  });

  it("survives storage that throws", () => {
    // The cost of a private window is the dialog opening on the project's first
    // pane — which is where it opened before this module existed.
    expect(() => rememberLastAgent(hostile, "/git/veld", "codex")).not.toThrow();
    expect(recallLastAgent(hostile, "/git/veld")).toBe("");
  });
});
