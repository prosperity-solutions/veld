import { describe, expect, it } from "vitest";
import type { OutfitState } from "../api";
import {
  FOLLOW_CLONE,
  initialOutfitChoice,
  outfitForCreate,
  outfitOptions,
  profileSwaps,
} from "./model";

const state = (over: Partial<OutfitState> = {}): OutfitState => ({
  status: "ready",
  required: false,
  current: { name: "team", pinned: false },
  profiles: [
    { name: "team", tier: "builtin", files: [] },
    { name: "peter", tier: "local", files: ["AGENTS.md", ".claude/skills"] },
  ],
  ...over,
});

describe("profileSwaps", () => {
  it("says an empty list is the team's files", () => {
    expect(profileSwaps([])).toBe("the team's files");
  });
  it("lists what is swapped", () => {
    expect(profileSwaps(["AGENTS.md", ".claude/skills"])).toBe("swaps AGENTS.md, .claude/skills");
  });
});

describe("outfitOptions", () => {
  it("offers following the clone first, naming its profile", () => {
    const o = outfitOptions(state());
    expect(o.map((x) => x.value)).toEqual([FOLLOW_CLONE, "team", "peter"]);
    expect(o[0].label).toBe("Follow the clone's profile (team)");
  });
  it("has no empty option when a choice is required", () => {
    const o = outfitOptions(state({ required: true }));
    expect(o.map((x) => x.value)).toEqual(["team", "peter"]);
  });
});

describe("initialOutfitChoice", () => {
  it("prefers the repo default over the clone's current profile", () => {
    expect(initialOutfitChoice(state({ default: "peter" }))).toBe("peter");
  });
  it("follows the clone when nothing is required, rather than pinning its profile", () => {
    expect(initialOutfitChoice(state())).toBe(FOLLOW_CLONE);
    expect(initialOutfitChoice(state({ default: "gone" }))).toBe(FOLLOW_CLONE);
  });
  it("falls back to the current profile when a choice is required", () => {
    expect(initialOutfitChoice(state({ required: true }))).toBe("team");
    expect(initialOutfitChoice(state({ required: true, default: "gone" }))).toBe("team");
  });
  it("follows the clone when there is nothing to name, unless required", () => {
    expect(initialOutfitChoice(state({ current: undefined }))).toBe(FOLLOW_CLONE);
    expect(initialOutfitChoice(state({ current: undefined, required: true }))).toBe("team");
  });
});

describe("outfitForCreate", () => {
  it("omits the key for the follow option", () => {
    expect(outfitForCreate(state(), FOLLOW_CLONE)).toBeUndefined();
  });
  it("sends a chosen profile", () => {
    expect(outfitForCreate(state(), "peter")).toBe("peter");
  });
  it("sends nothing while loading, failed, or not ready", () => {
    expect(outfitForCreate(null, "peter")).toBeUndefined();
    expect(outfitForCreate(state({ status: "off" }), "peter")).toBeUndefined();
    expect(outfitForCreate(state(), null)).toBeUndefined();
  });
  it("never sends a profile the daemon did not list", () => {
    expect(outfitForCreate(state(), "ghost")).toBeUndefined();
  });
});
