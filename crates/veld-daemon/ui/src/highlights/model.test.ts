import { describe, expect, it } from "vitest";

import { ID_PATTERN } from "../promotions/model";
import { HIGHLIGHTS } from "./content";
import { type Highlight, highlightEligible, highlightId, highlightProblems } from "./model";

const H: Highlight = {
  slug: "some-option",
  since: "2026-09-28",
  title: "New: something",
  body: "One sentence.",
};

describe("HIGHLIGHTS", () => {
  it("are all well-formed", () => {
    for (const h of Object.values(HIGHLIGHTS)) expect(highlightProblems(h)).toEqual([]);
  });

  it("are keyed by their own slug", () => {
    // The key is what a call site names; the slug is what gets stored. Two
    // spellings of one id is one of them drifting.
    for (const [key, h] of Object.entries(HIGHLIGHTS)) expect(h.slug).toBe(key);
  });
});

/**
 * Every slug that has ever shipped. **Append only.** A slug is the stored id, so
 * renaming one shows the bubble again to everyone who already answered it, and
 * dropping one lets a later highlight reuse the id and be silently suppressed for
 * them. Retiring a highlight means keeping its slug here, not deleting it.
 */
const SHIPPED_SLUGS = ["pixel-markers"];

describe("shipped slugs", () => {
  it("are never renamed or removed", () => {
    for (const slug of SHIPPED_SLUGS) expect(Object.keys(HIGHLIGHTS)).toContain(slug);
  });
});

describe("highlightId", () => {
  it("sits in a namespace no Veld card id can occupy", () => {
    // The What's-new badge counts only cards it built, and a card id is
    // kebab-case — so a namespaced id can never be mistaken for one.
    expect(highlightId("pixel-markers")).toBe("hint:pixel-markers");
    expect(ID_PATTERN.test(highlightId("pixel-markers"))).toBe(false);
  });
});

describe("highlightEligible", () => {
  const today = "2026-10-01";

  it("waits for the store", () => {
    expect(highlightEligible(H, null, null, today)).toBe(false);
    expect(highlightEligible(H, {}, null, today)).toBe(false);
  });

  it("shows to somebody who arrived before or on the day it shipped", () => {
    expect(highlightEligible(H, {}, "2026-01-01T00:00:00Z", today)).toBe(true);
    expect(highlightEligible(H, {}, "2026-09-28T12:00:00Z", today)).toBe(true);
  });

  it("does not show to somebody who arrived after it — every option is new to them", () => {
    expect(highlightEligible(H, {}, "2026-09-29T00:00:00Z", today)).toBe(false);
  });

  it("ends on either stored answer", () => {
    const arrived = "2026-01-01T00:00:00Z";
    expect(highlightEligible(H, { "hint:some-option": "read" }, arrived, today)).toBe(false);
    expect(highlightEligible(H, { "hint:some-option": "dismissed" }, arrived, today)).toBe(false);
    // A card sharing the slug is a different id and does not end it.
    expect(highlightEligible(H, { "some-option": "read" }, arrived, today)).toBe(true);
  });

  it("does not show before its own day", () => {
    expect(highlightEligible(H, {}, "2026-01-01T00:00:00Z", "2026-09-27")).toBe(false);
  });
});
