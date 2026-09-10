import { describe, expect, it } from "vitest";

import type { KeyValueStore } from "./promptDraft";
import {
  promptDraftName,
  recallPromptDraft,
  rememberPromptDraft,
} from "./promptDraft";

const fake = (seed: Record<string, string> = {}) => {
  const map = new Map(Object.entries(seed));
  return {
    map,
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      map.set(k, v);
    },
    removeItem: (k: string) => {
      map.delete(k);
    },
  };
};

/** A store that refuses everything, the way a private window or a full quota can. */
const hostile: KeyValueStore = {
  getItem: () => {
    throw new Error("nope");
  },
  setItem: () => {
    throw new Error("nope");
  },
  removeItem: () => {
    throw new Error("nope");
  },
};

describe("promptDraft", () => {
  it("keeps a draft per project", () => {
    const store = fake();
    rememberPromptDraft(store, "/git/veld", "Fix the login redirect loop");
    rememberPromptDraft(store, "/git/other", "Rewrite the parser");
    expect(recallPromptDraft(store, "/git/veld")).toBe("Fix the login redirect loop");
    expect(recallPromptDraft(store, "/git/other")).toBe("Rewrite the parser");
  });

  it("keeps whitespace and newlines inside a draft verbatim", () => {
    // The point of the feature is a paragraph somebody composed; normalising it
    // on the way through would hand back something they did not write.
    const store = fake();
    const text = "Do this:\n\n  - and this\n  - and that\n";
    rememberPromptDraft(store, "/git/veld", text);
    expect(recallPromptDraft(store, "/git/veld")).toBe(text);
  });

  it("is silent about a project with nothing in flight", () => {
    expect(recallPromptDraft(fake(), "/git/veld")).toBe("");
  });

  it("forgets a field the user emptied, rather than resurrecting it", () => {
    const store = fake();
    rememberPromptDraft(store, "/git/veld", "half a thought");
    rememberPromptDraft(store, "/git/veld", "");
    expect(store.map.has(promptDraftName("/git/veld"))).toBe(false);
    expect(recallPromptDraft(store, "/git/veld")).toBe("");
    // Whitespace is empty too: a field holding a stray newline is not a draft.
    rememberPromptDraft(store, "/git/veld", "something");
    rememberPromptDraft(store, "/git/veld", "  \n ");
    expect(recallPromptDraft(store, "/git/veld")).toBe("");
  });

  it("records nothing without a project to record it against", () => {
    const store = fake();
    rememberPromptDraft(store, "", "orphan");
    expect(store.map.size).toBe(0);
    expect(recallPromptDraft(store, "")).toBe("");
  });

  it("caps a draft rather than failing to save it", () => {
    // Written from a keystroke handler into a quota shared with the layouts, the
    // settings mirror and every other project's draft. Keeping the first 8 KB of
    // a pasted log beats keeping none of it.
    const store = fake();
    rememberPromptDraft(store, "/git/veld", "x".repeat(20000));
    expect(recallPromptDraft(store, "/git/veld").length).toBe(8192);
  });

  it("survives storage that throws", () => {
    expect(() => rememberPromptDraft(hostile, "/git/veld", "text")).not.toThrow();
    expect(() => rememberPromptDraft(hostile, "/git/veld", "")).not.toThrow();
    expect(recallPromptDraft(hostile, "/git/veld")).toBe("");
  });
});
