import { describe, expect, it } from "vitest";

// The published schema itself, imported rather than read: `resolveJsonModule` is
// on, so this is one drift gate with no filesystem and no new dependency.
import schema from "../../../../../schema/v3/veld.schema.json";

import { IDENTITY, PROMOTIONS } from "./content";
import {
  buildCards,
  type Card,
  duplicateIds,
  filterOptions,
  formatDay,
  GLYPH_NAMES,
  historyOf,
  ID_PATTERN,
  IDENTITY_COUNT,
  MAX_BODY,
  MAX_EYEBROW,
  MAX_HEADLINE,
  manifestIds,
  mergeStates,
  NAMESPACE_SEPARATOR,
  namespacedId,
  type ProjectNewsItem,
  type PromotionState,
  projectCards,
  projectNamespace,
  promotionProblems,
  type Section,
  type Source,
  sectionProblems,
  sourceByline,
  sourceLabel,
  sourcesOf,
  toPrompt,
  UNKNOWN_ARRIVAL,
  unreadCount,
  unreadOf,
  utcDay,
  veldCards,
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

/** An arrival on 2026-06-15. */
const ARRIVED = "2026-06-15T09:30:00Z";

/**
 * A card as a surface sees one: content, a source, and when this reader arrived
 * at that source. Veld's own unless a test says otherwise.
 */
const card = (over: Partial<Card> = {}): Card => ({
  ...section(),
  since: "2026-07-01",
  source: { kind: "veld" },
  arrivedAt: ARRIVED,
  ...over,
});

const news = (id: string, since: string): Card => card({ id, since });

/** The same cards as somebody who arrived on a different day sees them. */
const arrivingAt = (cards: readonly Card[], iso: string): Card[] =>
  cards.map((c) => ({ ...c, arrivedAt: iso }));
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

  it("rejects any promotion whose date is not YYYY-MM-DD", () => {
    // Mandatory, with no default: it is shown on the card *and* it decides who
    // the card reaches, so a missing one would gate wrongly and silently.
    expect(promotionProblems(news("a-promo", "2026-06-15"))).toEqual([]);
    expect(promotionProblems(news("a-promo", "15/06/2026"))).not.toEqual([]);
    expect(promotionProblems(news("a-promo", ""))).not.toEqual([]);
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
  it("every shipped card is date-gated — none of them opts out", () => {
    // The property that makes a zombie card unrepresentable rather than merely
    // discouraged. An evergreen kind used to exist here, and with no date gate
    // nothing ever stopped one reaching people; if a card ever escapes this
    // again, it is a card whose audience never shrinks.
    const long = arrivingAt(
      PROMOTIONS.map((p) => ({ ...p, source: { kind: "veld" } as const, arrivedAt: ARRIVED })),
      "2099-01-01T00:00:00Z",
    );
    for (const c of long) {
      expect(visibilityOf(c, NONE)).toBe("auto-read");
    }
  });

  it("news predating the user's arrival is auto-read", () => {
    expect(visibilityOf(news("a-promo", "2026-06-14"), NONE)).toBe("auto-read");
  });

  it("news shipped on the arrival day still reaches them", () => {
    // The launch-day case: `<` not `<=`, or every release-day announcement is
    // silently swallowed for everyone who installed that morning.
    expect(visibilityOf(news("a-promo", "2026-06-15"), NONE)).toBe("unread");
  });

  it("news shipped after the user arrived is theirs to read", () => {
    expect(visibilityOf(news("a-promo", "2026-07-01"), NONE)).toBe("unread");
  });

  it("an explicit read is never overwritten by the date gate", () => {
    const states: Record<string, PromotionState> = { "a-promo": "read" };
    expect(visibilityOf(news("a-promo", "2026-06-14"), states)).toBe("read");
  });

  it("dismissed is its own state, not a read", () => {
    const states: Record<string, PromotionState> = { "a-promo": "dismissed" };
    expect(visibilityOf(news("a-promo", "2026-07-01"), states)).toBe("dismissed");
  });

  it("reads the UTC day off a timestamp", () => {
    expect(utcDay("2026-06-15T09:30:00Z")).toBe("2026-06-15");
  });
});

describe("prompting and the unread count", () => {
  const all = [
    news("later", "2026-08-01"),
    news("fresh", "2026-07-01"),
    news("ancient", "2020-01-01"),
  ];

  it("prompts only what is unread — never a dismissed card again", () => {
    expect(manifestIds(toPrompt(all, NONE))).toEqual(["later", "fresh"]);
    const dismissed: Record<string, PromotionState> = { fresh: "dismissed" };
    expect(manifestIds(toPrompt(all, dismissed))).toEqual(["later"]);
  });

  it("a dismissed card still counts as unread — that is the point of the split", () => {
    // Dismissing clears the modal; only reading clears the badge. If this ever
    // returns 1, dismiss and read have been collapsed back into one state.
    const dismissed: Record<string, PromotionState> = { fresh: "dismissed" };
    expect(unreadCount(all, dismissed)).toBe(2);
  });

  it("reading is what clears the count", () => {
    const read: Record<string, PromotionState> = { later: "read", fresh: "read" };
    expect(unreadCount(all, read)).toBe(0);
  });

  it("auto-read news never counts and never prompts", () => {
    // `ancient` is in `all` and is absent from both, without the user ever
    // having touched it.
    expect(unreadCount(all, NONE)).toBe(2);
    expect(manifestIds(toPrompt(all, NONE))).not.toContain("ancient");
  });

  it("a brand-new user is prompted with nothing at all", () => {
    // The whole back-catalogue is behind them, and there is no evergreen card to
    // greet them either: orientation is the first-run screen's job, which is
    // derived from having no projects rather than tracked as seen.
    const fresh = arrivingAt(all, "2027-01-01T00:00:00Z");
    expect(toPrompt(fresh, NONE)).toEqual([]);
    expect(unreadCount(fresh, NONE)).toBe(0);
  });
});

describe("mergeStates — the client's copy of the daemon's merge", () => {
  // Held against the Rust tests' own cases in `kv.rs`. The two merges must agree
  // or the panel and the badge disagree with the server until a reload.
  it("read wins over dismissed and neither is ever undone", () => {
    expect(mergeStates({ a: "dismissed" }, ["a"], "read")).toEqual({ a: "read" });
    expect(mergeStates({ a: "read" }, ["a"], "dismissed")).toEqual({ a: "read" });
  });

  it("leaves ids it was not given alone", () => {
    expect(mergeStates({ a: "read" }, ["b"], "dismissed")).toEqual({ a: "read", b: "dismissed" });
  });

  it("does not mutate the map it was handed", () => {
    const before: Record<string, PromotionState> = { a: "dismissed" };
    mergeStates(before, ["a"], "read");
    expect(before).toEqual({ a: "dismissed" });
  });
});

describe("a project's own news", () => {
  const REPO = {
    root: "/Users/dev/git/acme-api",
    name: "acme-api",
    // The teammate imported this project on 2026-06-15 — their arrival at it,
    // which is a different question from when they arrived at Veld.
    created_at: ARRIVED,
  };

  /**
   * A day after every `since` in this block, so the future-date guard is never what
   * is accidentally under test. The guard has its own case below.
   */
  const TODAY = "2026-12-31";
  const cardsFor = (repo: typeof REPO, news: ProjectNewsItem[]) =>
    projectCards(repo, news, TODAY);

  const item = (over: Partial<ProjectNewsItem> = {}): ProjectNewsItem => ({
    id: "build-moved",
    since: "2026-07-01",
    eyebrow: "Heads up",
    headline: "Stop guessing which test script works",
    body: "The wrappers are gone.",
    glyph: "terminal",
    ...over,
  });

  it("namespaces every id, so two repos shipping one slug never collide", () => {
    const a = cardsFor(REPO, [item()]);
    const b = cardsFor({ ...REPO, root: "/Users/dev/git/other", name: "other" }, [item()]);
    expect(a[0].id).not.toBe(b[0].id);
    // Both are `proj:<namespace>:<slug>`, and the slug is the last field — so the
    // author's own id is still legible in a database row.
    expect(a[0].id.split(NAMESPACE_SEPARATOR)).toEqual([
      "proj",
      projectNamespace(REPO.root),
      "build-moved",
    ]);
    expect(a[0].id).toBe(namespacedId(REPO.root, "build-moved"));
  });

  it("can never collide with a Veld id, in either direction", () => {
    // The reservation this whole scheme rests on. A Veld id cannot contain `:`
    // (`sectionProblems` rejects it), and a project id always does.
    const project = cardsFor(REPO, [item({ id: "worktree-inbox" })])[0];
    expect(project.id).toContain(NAMESPACE_SEPARATOR);
    expect(PROMOTIONS.some((p) => p.id === project.id)).toBe(false);
    // Even when the author picks the exact slug of a shipped Veld promotion.
    expect(project.id).not.toBe("worktree-inbox");
  });

  it("keeps the stored id as the card's ONLY id", () => {
    // Two ids for one card is how the wrong one reaches the state map, and the
    // state map is forever. `id` is the namespaced one, everywhere.
    const [c] = cardsFor(REPO, [item()]);
    expect(c.id.startsWith("proj:")).toBe(true);
    expect(unreadOf([c], {})).toEqual([c.id]);
    expect(visibilityOf(c, { [c.id]: "read" })).toBe("read");
    // And the author's bare slug is not a key anything responds to.
    expect(visibilityOf(c, { "build-moved": "read" })).toBe("unread");
  });

  it("hashes the repo root to a bounded, `:`-free namespace", () => {
    const ns = projectNamespace(REPO.root);
    expect(ns).toMatch(/^[0-9a-f]{16}$/u);
    expect(ns).toBe(projectNamespace(REPO.root));
    expect(ns).not.toBe(projectNamespace(`${REPO.root}/`));
    // A path may legally contain a colon, which is exactly why the raw path is
    // not in the id: it would make the namespace ambiguous.
    expect(projectNamespace("/tmp/a:b")).toMatch(/^[0-9a-f]{16}$/u);
    // Bounded well inside the daemon's 128-character ceiling, even at the 64-char
    // slug limit `valid_news_id` allows.
    expect(namespacedId(REPO.root, "a".repeat(64)).length).toBeLessThanOrEqual(128);
  });

  it("gates a project's news on when the reader imported THAT project", () => {
    // The whole reason `arrivedAt` is per-card. Same item, two teammates.
    const veteran = cardsFor({ ...REPO, created_at: "2025-01-01T00:00:00Z" }, [
      item({ since: "2026-01-15" }),
    ]);
    const newHire = cardsFor({ ...REPO, created_at: "2026-08-01T00:00:00Z" }, [
      item({ since: "2026-01-15" }),
    ]);
    expect(visibilityOf(veteran[0], {})).toBe("unread");
    expect(visibilityOf(newHire[0], {})).toBe("auto-read");
  });

  it("drops a future-dated item rather than letting it never expire", () => {
    // `since` is the ONLY thing that retires a card, so a day that has not
    // happened yet is after every arrival — present and future. That is the
    // never-expiring card the `onboarding` kind was deleted to be rid of, arriving
    // through the one channel veld does not author, and `2062` for `2026` is one
    // keystroke. `parse_news` refuses it and tells the author; this is the guard
    // for a reader whose daemon predates that check.
    expect(projectCards(REPO, [item({ since: "2062-08-12" })], TODAY)).toEqual([]);
    // Today itself ships — a card written and merged the same day is the normal
    // case, and it is the reader's own UTC day that decides.
    expect(projectCards(REPO, [item({ since: TODAY })], TODAY)).toHaveLength(1);
  });

  it("attributes every card to the project by name", () => {
    const [c] = cardsFor(REPO, [item()]);
    expect(c.source).toEqual({ kind: "project", name: "acme-api" });
    expect(sourceLabel(c.source)).toBe("acme-api");
    // "Official", not "Veld" — this very repo is named `veld`, so a label of
    // "Veld" beside a label of "veld" distinguishes nothing. Provenance is the
    // claim, and no project name can imitate it.
    expect(sourceLabel({ kind: "veld" })).toBe("Official");
    expect(sourceLabel({ kind: "project", name: "veld" })).toBe("veld");
  });

  it("says what each byline means in full, and never puts the name inside it", () => {
    // A filter tab has room for one word; a byline under a sentence has room to
    // say what that word meant. Both halves are whole phrases for that reason.
    expect(sourceByline({ kind: "veld" })).toBe("Official veld news");
    // The project's byline stops before the name — it is rendered as its own
    // element, so a repo cannot name itself into the middle of the claim.
    const byline = sourceByline({ kind: "project", name: "Official veld news" });
    expect(byline).toBe("News from your project");
    expect(byline).not.toContain("Official");
  });

  it("drops a slug carrying the namespace separator rather than trusting it", () => {
    // Unreachable while `valid_news_id` holds — which is the point. If that
    // grammar is ever loosened, a repo could write `proj:<other>:<slug>` and
    // suppress another project's card; this is the guard that keeps the namespace
    // claim true here rather than assumed.
    expect(cardsFor(REPO, [item({ id: "proj:deadbeef:hi" })])).toEqual([]);
  });

  it("falls back to a known glyph rather than dropping the card", () => {
    // A newer daemon naming a glyph this bundle has not learned should cost the
    // right mark, never the whole card.
    const [c] = cardsFor(REPO, [item({ glyph: "rocket" })]);
    expect(c.glyph).toBe("inbox");
    expect(GLYPH_NAMES).toContain(c.glyph);
    // Every name the schema allows still renders as itself.
    for (const glyph of GLYPH_NAMES) {
      expect(cardsFor(REPO, [item({ glyph })])[0].glyph).toBe(glyph);
    }
  });

  it("shares one unread count and one merge with Veld's own cards", () => {
    const cards = [...veldCards(PROMOTIONS, ARRIVED), ...cardsFor(REPO, [item()])];
    const outstanding = unreadOf(cards, {});
    expect(outstanding).toContain(namespacedId(REPO.root, "build-moved"));
    expect(unreadCount(cards, {})).toBe(outstanding.length);
    // Reading clears both channels through the same monotone merge.
    expect(unreadCount(cards, mergeStates({}, outstanding, "read"))).toBe(0);
    // Dismissing clears neither — that split is not per-channel either.
    expect(unreadCount(cards, mergeStates({}, outstanding, "dismissed"))).toBe(
      outstanding.length,
    );
  });

  it("matches the published schema's glyph set and every cap", () => {
    // The bundle's half of the drift gate; `the_glyph_set_matches_the_published_schema`
    // in `veld-core/src/ide.rs` is the parser's. Three surfaces, one list.
    const fields = schema.$defs.newsItem.properties;
    expect(fields.glyph.enum).toEqual([...GLYPH_NAMES]);
    expect(fields.glyph.default).toBe("inbox");
    // **All three caps, not just one.** The caps are the mechanism rather than
    // style advice, and the failure they guard against is silent in the worst
    // direction: widen the schema alone and an author's editor accepts a headline
    // the daemon then drops, so the change the card announced never reaches the
    // team. `veld lint` says why, to whoever runs it.
    expect(fields.eyebrow.maxLength).toBe(MAX_EYEBROW);
    expect(fields.headline.maxLength).toBe(MAX_HEADLINE);
    expect(fields.body.maxLength).toBe(MAX_BODY);
    // And the id grammar, which is what the namespace claim rests on: a schema
    // that admits `:` would let an editor bless an id the parser must refuse.
    expect(fields.id.pattern).toBe(ID_PATTERN.source);
    // `maxItems` is pinned against `MAX_NEWS_ITEMS` on the Rust side, where that
    // constant lives — see `the_glyph_set_matches_the_published_schema`.
  });
});

describe("buildCards — the two channels, and the gates on each", () => {
  const REPO = {
    root: "/Users/dev/git/acme-api",
    name: "acme-api",
    created_at: ARRIVED,
    news: [
      {
        id: "build-moved",
        since: "2026-07-01",
        eyebrow: "Heads up",
        headline: "A headline",
        body: "A sentence.",
        glyph: "terminal",
      },
    ],
  };
  const base = {
    promotions: PROMOTIONS,
    firstUseIso: ARRIVED,
    project: REPO,
    showProject: true,
    today: "2026-12-31",
  };

  it("still builds a readable list when the arrival stamp is unknown", () => {
    // Emptying the list here hid the whole feature once: `any` is derived from it,
    // so one failed state request removed *What's new…* from the ⋯ menu for the
    // rest of the page load — the only route back to a dismissed card. The arrival
    // is needed for the date gate, not for the list.
    const cards = buildCards({ ...base, firstUseIso: null });
    expect(cards.length).toBeGreaterThan(0);
    // Only VELD's cards lose their arrival — a project's is the repo's own
    // `created_at`, which this reader has regardless of the promotions request.
    const veld = cards.filter((c) => c.source.kind === "veld");
    const theirs = cards.filter((c) => c.source.kind === "project");
    expect(veld.every((c) => c.arrivedAt === UNKNOWN_ARRIVAL)).toBe(true);
    expect(theirs.every((c) => c.arrivedAt === ARRIVED)).toBe(true);
    // With no arrival, nothing of Veld's is auto-read — so the reader sees the
    // whole list rather than a panel that silently hid its own back-catalogue.
    expect(veld.every((c) => visibilityOf(c, NONE) === "unread")).toBe(true);
  });

  it("builds no project cards until the reader's settings are known", () => {
    // `ui.showProjectNews` defaults to ON, so resolving an unloaded document to
    // its default would put a project's cards in front of somebody who switched
    // them off — and the prompt latches, so settings arriving later cannot undo it.
    const unknown = buildCards({ ...base, showProject: null });
    expect(unknown.every((c) => c.source.kind === "veld")).toBe(true);
  });

  it("honours ui.showProjectNews — the reader's only switch", () => {
    // This is the whole mitigation against repo-authored modals, so it has to
    // remove the cards from the count as well as from the prompt, not just hide
    // them in the panel.
    const off = buildCards({ ...base, showProject: false });
    expect(off.every((c) => c.source.kind === "veld")).toBe(true);
    expect(unreadCount(off, {})).toBe(unreadCount(veldCards(PROMOTIONS, ARRIVED), {}));
    const on = buildCards(base);
    expect(on.some((c) => c.source.kind === "project")).toBe(true);
  });

  it("gates each channel on its own arrival", () => {
    // Veld's cards on `firstUse`, the project's on when this reader imported that
    // repo. One card each way, so a swapped comparison shows up here.
    const newHire = buildCards({
      ...base,
      project: { ...REPO, created_at: "2026-12-01T00:00:00Z" },
    });
    const theirs = newHire.find((c) => c.source.kind === "project");
    expect(theirs && visibilityOf(theirs, {})).toBe("auto-read");
    expect(buildCards(base).find((c) => c.source.kind === "project")).toBeDefined();
  });

  it("survives having no project selected", () => {
    const none = buildCards({ ...base, project: null });
    expect(none.every((c) => c.source.kind === "veld")).toBe(true);
  });
});

describe("filterOptions", () => {
  const veld = news("veld-thing", "2026-07-01");
  const project = card({
    id: "proj:abc:mine",
    since: "2026-07-15",
    source: { kind: "project", name: "acme-api" },
  });

  it("offers nothing to the interrupting entrance", () => {
    // Filtering a list of things you have not read yet is asking the reader to
    // curate their own interruption.
    expect(filterOptions([veld, project], "acme-api", true)).toBeNull();
  });

  it("offers nothing when there is nothing to filter between", () => {
    expect(filterOptions([veld], null, false)).toBeNull();
  });

  it("gives a project with no news a tab anyway, disabled", () => {
    // "This project has told you nothing" is an answer worth being able to see; a
    // missing tab reads as the feature not existing. Documented in three places
    // and, before this, enforced in none.
    const tabs = filterOptions([veld], "acme-api", false);
    expect(tabs?.map((t) => [t.label, t.disabled])).toEqual([
      ["Everything", false],
      ["Official", false],
      ["acme-api", true],
    ]);
  });

  it("enables the project tab once it has said something", () => {
    const tabs = filterOptions([veld, project], "acme-api", false);
    expect(tabs?.find((t) => t.value === "project")?.disabled).toBe(false);
  });
});

describe("the history view", () => {
  const veld = news("veld-thing", "2026-07-01");
  const project = card({
    id: "proj:abc:mine",
    since: "2026-07-15",
    source: { kind: "project", name: "acme-api" },
  });
  const older = card({ id: "proj:abc:older", since: "2026-02-01", source: project.source });
  const all = [veld, project, older];

  it("reads newest first, whoever said it", () => {
    // Not "Veld's, then the project's" — that is two lists under one heading.
    expect(manifestIds(historyOf(all, "all"))).toEqual([
      "proj:abc:mine",
      "veld-thing",
      "proj:abc:older",
    ]);
  });

  it("puts the most recently ADDED card first when a day is shared", () => {
    // The bug this pins, observed in the shipped panel: everything ships on the
    // day it ships, so a release adding two cards has two cards sharing a date.
    // A stable sort alone left the newest addition wherever it sat in the array —
    // second of three, under an older card. `content.ts` and a project's
    // `ide.news` are both *appended* to, so the last element is the newest thing.
    const sameDay = [
      news("added-first", "2026-08-12"),
      news("added-second", "2026-08-12"),
      news("added-third", "2026-08-12"),
    ];
    expect(manifestIds(historyOf(sameDay, "all"))).toEqual([
      "added-third",
      "added-second",
      "added-first",
    ]);
    // And the day still wins over the position: an older card appended later
    // does not jump the queue.
    const mixed = [news("new-but-first", "2026-09-01"), news("old-but-last", "2026-01-01")];
    expect(manifestIds(historyOf(mixed, "all"))).toEqual(["new-but-first", "old-but-last"]);
  });

  it("filters by source", () => {
    expect(manifestIds(historyOf(all, "veld"))).toEqual(["veld-thing"]);
    expect(manifestIds(historyOf(all, "project"))).toEqual(["proj:abc:mine", "proj:abc:older"]);
  });

  it("does not reorder the list it was handed", () => {
    historyOf(all, "all");
    expect(manifestIds(all)).toEqual(["veld-thing", "proj:abc:mine", "proj:abc:older"]);
  });

  it("is one list, with nothing grouped out of the order", () => {
    // No sections. Every card is a change that landed on a day, so date order is
    // the only grouping the reader needs — and a taxonomy over three cards is a
    // heading to skip. An empty filter is an empty list, not an empty section.
    expect(historyOf([], "all")).toEqual([]);
    expect(manifestIds(historyOf(all, "veld"))).toEqual(["veld-thing"]);
  });

  it("offers each source once, Veld first", () => {
    expect(sourcesOf(all)).toEqual([
      { kind: "veld" },
      { kind: "project", name: "acme-api" },
    ] satisfies Source[]);
    // Nothing to filter between when there is only one source — the dialog uses
    // this to decide whether the control is worth showing at all.
    expect(sourcesOf([veld])).toHaveLength(1);
    expect(sourcesOf([])).toEqual([]);
  });
});

describe("unreadOf — what a settle actually writes", () => {
  const all = [
    news("later", "2026-08-01"),
    news("fresh", "2026-07-01"),
    news("ancient", "2020-01-01"),
  ];

  it("never writes a row for a card the user never had", () => {
    // Browsing shows every card the build ships, `ancient` included. Marking that
    // read would store a row for a promotion that predates the user, against the
    // store's rule that the map stays proportional to what they acted on.
    expect(unreadOf(all, NONE)).toEqual(["later", "fresh"]);
  });

  it("includes a dismissed card, because dismissing is not reading", () => {
    expect(unreadOf(all, { fresh: "dismissed" })).toEqual(["later", "fresh"]);
  });

  it("is empty once everything outstanding is read", () => {
    expect(unreadOf(all, { later: "read", fresh: "read" })).toEqual([]);
  });
});
