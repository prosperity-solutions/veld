import { describe, expect, it } from "vitest";
import type { OutfitState } from "../api";
import {
  FOLLOW_CLONE,
  initialOutfitChoice,
  outfitForCreate,
  outfitOptions,
  outfitTone,
  outfitTooltip,
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

describe("outfitOptions", () => {
  it("offers following the clone first, naming its profile", () => {
    const o = outfitOptions(state());
    expect(o.map((x) => x.value)).toEqual([FOLLOW_CLONE, "team", "peter"]);
    expect(o[0].label).toBe("Follow the clone's profile (team)");
  });
  it("names each profile and nothing else, however many files it swaps", () => {
    const many = Array.from({ length: 40 }, (_, i) => `.agents/skills/s${i}/SKILL.md`);
    const o = outfitOptions(
      state({ profiles: [{ name: "pitch-roast-ship", tier: "local", files: many }] }),
    );
    expect(o[1]).toEqual({ value: "pitch-roast-ship", label: "pitch-roast-ship" });
  });
  it("does not name a pinned profile as the one followed", () => {
    const o = outfitOptions(state({ current: { name: "peter", pinned: true } }));
    expect(o[0].label).toBe("Follow the clone's profile");
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

describe("outfitTone", () => {
  it("is worn for any profile but base", () => {
    expect(outfitTone(state())).toBe("worn");
  });
  it("is base for base, or when no profile is known", () => {
    expect(outfitTone(state({ current: { name: "base", pinned: false } }))).toBe("base");
    expect(outfitTone(state({ current: undefined }))).toBe("base");
  });
});

describe("outfitTooltip", () => {
  it("says how the profile is worn", () => {
    expect(outfitTooltip(state({ current: { name: "mine", pinned: true } }))).toBe(
      "Outfit: mine (pinned to this worktree)",
    );
    expect(outfitTooltip(state({ current: { name: "base", pinned: false } }))).toBe(
      "Outfit: base (following the clone)",
    );
  });
  it("falls back to a bare label", () => {
    expect(outfitTooltip(null)).toBe("Outfit");
    expect(outfitTooltip(state({ current: undefined }))).toBe("Outfit");
  });
});
