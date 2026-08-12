import { describe, expect, it } from "vitest";

import { IDENTITY, PROMOTIONS } from "./content";
import {
  duplicateIds,
  formatDay,
  IDENTITY_COUNT,
  MAX_BODY,
  manifestIds,
  NAMESPACE_SEPARATOR,
  type Promotion,
  type PromotionState,
  promotionProblems,
  type Section,
  sectionProblems,
  toPrompt,
  unreadCount,
  utcDay,
  visibilityOf,
} from "./model";

const section = (over: Partial<Section> = {}): Section => ({
  id: "a-promo",
  eyebrow: "New",
  headline: "A headline",
  body: "A sentence.",
  glyph: "terminal",
  ...over,
});

const news = (id: string, since: string): Promotion => ({
  ...section({ id }),
  kind: "news",
  since,
});

const onboarding = (id: string, since = "2026-01-01"): Promotion => ({
  ...section({ id }),
  kind: "onboarding",
  since,
});

/** An arrival on 2026-06-15. */
const ARRIVED = "2026-06-15T09:30:00Z";
const NONE: Record<string, PromotionState> = {};

describe("section validity", () => {
  it("accepts a well-formed section", () => {
    expect(sectionProblems(section())).toEqual([]);
  });

  it("rejects an id that is not kebab-case", () => {
    for (const id of ["", "Not Kebab", "trailing-", "under_score", "UPPER"]) {
      expect(sectionProblems(section({ id }))).not.toEqual([]);
    }
  });

  it("a veld promotion id can never occupy a namespace", () => {
    // The reservation that lets a second source of promotions (a project's own
    // news) share the daemon's one state map without collisions. Ids outlive
    // releases in users' databases, so this has to hold before anything ships.
    // If you loosened ID_PATTERN to admit ":", this is the test telling you the
    // namespace guarantee went with it.
    expect(sectionProblems(section({ id: `proj${NAMESPACE_SEPARATOR}a-promo` }))).not.toEqual([]);
    expect([...IDENTITY, ...PROMOTIONS].some((s) => s.id.includes(NAMESPACE_SEPARATOR))).toBe(
      false,
    );
  });

  it("reports every problem at once, not just the first", () => {
    const problems = sectionProblems(
      section({ id: "Bad Id", eyebrow: "", body: "x".repeat(MAX_BODY + 1) }),
    );
    expect(problems).toHaveLength(3);
  });

  it("rejects an over-long body — the cap is the discipline", () => {
    expect(sectionProblems(section({ body: "x".repeat(MAX_BODY + 1) }))).not.toEqual([]);
    expect(sectionProblems(section({ body: "x".repeat(MAX_BODY) }))).toEqual([]);
  });

  it("rejects any promotion whose date is not YYYY-MM-DD, whatever its kind", () => {
    // Mandatory for both kinds: every card shows when it landed, and the kind
    // decides only whether that date gates the card.
    expect(promotionProblems(news("a-promo", "2026-06-15"))).toEqual([]);
    expect(promotionProblems(onboarding("a-promo", "2026-06-15"))).toEqual([]);
    expect(promotionProblems(news("a-promo", "15/06/2026"))).not.toEqual([]);
    expect(promotionProblems(onboarding("a-promo", ""))).not.toEqual([]);
  });

  it("formats a day for a reader without going near a Date", () => {
    // `new Date("2026-08-12")` is midnight UTC and prints as the 11th west of
    // it. A plain day has no timezone and must not acquire one on the way out.
    expect(formatDay("2026-08-12")).toBe("12 Aug 2026");
    expect(formatDay("2026-01-01")).toBe("1 Jan 2026");
    expect(formatDay("2026-12-31")).toBe("31 Dec 2026");
    // A malformed value renders as itself, never as NaN.
    expect(formatDay("whenever")).toBe("whenever");
  });
});

describe("shipped content", () => {
  // These are the gate. Content is authored by whoever ships the feature —
  // often an agent following a checklist — and nothing else in the toolchain
  // can see that a headline ran long or an id was reused.
  it("identity is exactly three claims", () => {
    expect(IDENTITY).toHaveLength(IDENTITY_COUNT);
  });

  it("every shipped section is within the caps", () => {
    expect(IDENTITY.flatMap(sectionProblems)).toEqual([]);
    expect(PROMOTIONS.flatMap(promotionProblems)).toEqual([]);
  });

  it("no id is used twice, within or across the collections", () => {
    expect(duplicateIds([...IDENTITY, ...PROMOTIONS])).toEqual([]);
  });
});

describe("visibility", () => {
  it("an untouched onboarding item is unread however long ago the user arrived", () => {
    // Orientation does not go stale — that is the whole difference from news.
    // Dated well before the user arrived, and still unread: the kind decides
    // whether the date gates, and onboarding is never gated.
    expect(visibilityOf(onboarding("a-promo", "2019-01-01"), NONE, ARRIVED)).toBe("unread");
    expect(visibilityOf(onboarding("a-promo"), NONE, "2020-01-01T00:00:00Z")).toBe("unread");
  });

  it("news predating the user's arrival is auto-read", () => {
    expect(visibilityOf(news("a-promo", "2026-06-14"), NONE, ARRIVED)).toBe("auto-read");
  });

  it("news shipped on the arrival day still reaches them", () => {
    // The launch-day case: `<` not `<=`, or every release-day announcement is
    // silently swallowed for everyone who installed that morning.
    expect(visibilityOf(news("a-promo", "2026-06-15"), NONE, ARRIVED)).toBe("unread");
  });

  it("news shipped after the user arrived is theirs to read", () => {
    expect(visibilityOf(news("a-promo", "2026-07-01"), NONE, ARRIVED)).toBe("unread");
  });

  it("an explicit read is never overwritten by the date gate", () => {
    const states: Record<string, PromotionState> = { "a-promo": "read" };
    expect(visibilityOf(news("a-promo", "2026-06-14"), states, ARRIVED)).toBe("read");
  });

  it("dismissed is its own state, not a read", () => {
    const states: Record<string, PromotionState> = { "a-promo": "dismissed" };
    expect(visibilityOf(news("a-promo", "2026-07-01"), states, ARRIVED)).toBe("dismissed");
  });

  it("reads the UTC day off a timestamp", () => {
    expect(utcDay("2026-06-15T09:30:00Z")).toBe("2026-06-15");
  });
});

describe("prompting and the unread count", () => {
  const all = [onboarding("intro"), news("fresh", "2026-07-01"), news("ancient", "2020-01-01")];

  it("prompts only what is unread — never a dismissed card again", () => {
    expect(manifestIds(toPrompt(all, NONE, ARRIVED))).toEqual(["intro", "fresh"]);
    const dismissed: Record<string, PromotionState> = { fresh: "dismissed" };
    expect(manifestIds(toPrompt(all, dismissed, ARRIVED))).toEqual(["intro"]);
  });

  it("a dismissed card still counts as unread — that is the point of the split", () => {
    // Dismissing clears the modal; only reading clears the badge. If this ever
    // returns 1, dismiss and read have been collapsed back into one state.
    const dismissed: Record<string, PromotionState> = { fresh: "dismissed" };
    expect(unreadCount(all, dismissed, ARRIVED)).toBe(2);
  });

  it("reading is what clears the count", () => {
    const read: Record<string, PromotionState> = { intro: "read", fresh: "read" };
    expect(unreadCount(all, read, ARRIVED)).toBe(0);
  });

  it("auto-read news never counts and never prompts", () => {
    // `ancient` is in `all` and is absent from both, without the user ever
    // having touched it.
    expect(unreadCount(all, NONE, ARRIVED)).toBe(2);
    expect(manifestIds(toPrompt(all, NONE, ARRIVED))).not.toContain("ancient");
  });

  it("a brand-new user is prompted with onboarding and no back-catalogue", () => {
    const arrivedToday = "2027-01-01T00:00:00Z";
    expect(manifestIds(toPrompt(all, NONE, arrivedToday))).toEqual(["intro"]);
    expect(unreadCount(all, NONE, arrivedToday)).toBe(1);
  });
});
