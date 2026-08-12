/**
 * The promotion vocabulary: one content atom, two collections, no layout field.
 *
 * The thing that makes this survive is what a `Section` deliberately does *not*
 * carry. There is no `layout`, no `variant`, no `size`. The moment content can
 * say how it wants to be drawn, every future surface is a new enum member and
 * the author is asked to decide something they have no information to decide.
 * Instead: content declares only what it *is*, and a surface — `StartScreen`,
 * `WhatsNew` — decides how to arrange it. Two layouts composing one atom.
 *
 * There is also no CTA on a section, for the same reason. The start screen has
 * exactly one call to action ("import a project") and the what's-new panel has
 * none; that is a property of the surface, not of the sentence being said.
 *
 * Everything in this file is pure so it can be tested — the UI suite runs under
 * `environment: "node"` with no jsdom, so anything living in TSX is untestable
 * in CI. The caps below are therefore enforced by `model.test.ts` rather than by
 * a convention someone has to remember at review time.
 */

// Type-only, so this file stays runtime-dependency-free the way its header
// claims — the same import `shared/settings.ts` takes for `SettingsDoc`. Wire
// shapes live in `api.ts`; what they *mean* lives here.
import type { ProjectNewsItem } from "../api";

export type { ProjectNewsItem };

/**
 * The closed set of illustrations.
 *
 * Line art drawn with `currentColor`, never a raster and never a screenshot. Two
 * reasons, both hard: the `/ide` bundle is a single self-contained HTML file, so
 * every image is base64 inside it forever, and Veld ships two themes several
 * times a week — a screenshot is wrong in one theme the day it lands and wrong
 * in both within a fortnight.
 *
 * Adding a glyph is a deliberate act. If a promotion cannot be illustrated by
 * one of these, prefer reusing the closest one over growing the set: the set
 * staying small is what keeps these looking like one family.
 */
export const GLYPH_NAMES = ["terminal", "panes", "device", "inbox"] as const;

export type GlyphName = (typeof GLYPH_NAMES)[number];

export interface Section {
  /**
   * Stable id, kebab-case. This is the string the daemon persists in the acked
   * set, so **it is never renamed and never reused.** Renaming one re-promotes
   * it to every existing user; reusing a retired one silently suppresses the new
   * promotion for everyone who saw the old one.
   */
  id: string;
  /** Two or three words above the headline. */
  eyebrow: string;
  headline: string;
  /** One sentence. Not two. */
  body: string;
  glyph: GlyphName;
}

/** Caps. Not style advice — `model.test.ts` fails the build on a breach. */
export const MAX_EYEBROW = 24;
export const MAX_HEADLINE = 44;
export const MAX_BODY = 160;
/**
 * The front door says what Veld *is*, in three claims. The cap is the whole
 * mechanism: a fourth aspect costs removing one of the three, which is the only
 * thing that keeps a first-run screen good when the product ships weekly.
 */
export const IDENTITY_COUNT = 3;

/**
 * Namespace separator, reserved **now** so a later channel cannot collide with
 * this one.
 *
 * The daemon stores promotion ids as opaque strings in one set, which is what
 * lets a second source of promotions — a project declaring its own news in
 * `veld.json`, say — share this storage with no daemon change at all. The
 * hazard that comes with that is a project shipping `new-topbar-button` and
 * silently suppressing, or being suppressed by, a Veld promotion of the same
 * name. Ids live in users' databases forever, so the namespace has to be
 * reserved before anything ships, not once there is something to collide with.
 *
 * The guard is `ID_PATTERN`, not a comment: kebab-case admits no `:`, so a
 * Veld-authored id **cannot** be written in a namespaced form, and a namespaced
 * id cannot be mistaken for a Veld one. Do not loosen that pattern to admit
 * `:` — `a_veld_promotion_id_can_never_occupy_a_namespace` fails if you do.
 */
export const NAMESPACE_SEPARATOR = ":";

/**
 * Exported so `model.test.ts` can hold it against the published schema's
 * `$defs.newsItem.properties.id.pattern`. A schema that admitted `:` while this
 * did not would bless, in an author's editor, an id the parser must refuse — and
 * the namespace claim rests on the two agreeing.
 */
export const ID_PATTERN = /^[a-z0-9]+(-[a-z0-9]+)*$/u;

/**
 * Everything wrong with a section, as human-readable lines. Empty means valid.
 *
 * Returns all problems rather than the first, so an author fixing content sees
 * the whole list in one test run instead of one per iteration.
 */
export function sectionProblems(s: Section): string[] {
  const problems: string[] = [];
  if (!ID_PATTERN.test(s.id)) problems.push(`${s.id || "(empty)"}: id must be kebab-case`);
  if (!s.eyebrow || s.eyebrow.length > MAX_EYEBROW)
    problems.push(`${s.id}: eyebrow must be 1-${MAX_EYEBROW} chars (is ${s.eyebrow.length})`);
  if (!s.headline || s.headline.length > MAX_HEADLINE)
    problems.push(`${s.id}: headline must be 1-${MAX_HEADLINE} chars (is ${s.headline.length})`);
  if (!s.body || s.body.length > MAX_BODY)
    problems.push(`${s.id}: body must be 1-${MAX_BODY} chars (is ${s.body.length})`);
  return problems;
}

/**
 * What a user has done about a promotion, as the daemon stores it. Unread is the
 * absence of an entry.
 */
export type PromotionState = "dismissed" | "read";

/**
 * Why a promotion is or is not in front of the user right now.
 *
 * Four states rather than two, because **dismissing is not reading**. "Stop
 * putting this in front of me" and "I have taken this in" are different answers
 * and the indicator has to tell them apart: a dismissed card never prompts again
 * but still counts toward the unread badge, which is what lets someone clear a
 * modal in the middle of something and still find it later.
 */
export type Visibility = "unread" | "dismissed" | "read" | "auto-read";

const DAY_PATTERN = /^\d{4}-\d{2}-\d{2}$/u;

/**
 * A promotion: a section, plus the day it shipped.
 *
 * **Every promotion is a change, and `since` gates it.** A reader who arrived
 * after the date never sees the card, so installing Veld today never means a modal
 * about something that changed last spring.
 *
 * There is deliberately no second kind. An evergreen "good to know" card was
 * built and removed: with no date gate, nothing ever stops it reaching people, so
 * it is the only card shape that *can* rot — the author who wrote it is gone, the
 * reader has no reason to reread it, and a channel that spends attention on
 * standing facts teaches people to close it unread. Orientation is the first-run
 * screen's job (`IDENTITY`), which is derived from state and un-shows itself, and
 * "we changed how we work" is news like anything else.
 */
export interface Promotion extends Section {
  /** The day this shipped, `YYYY-MM-DD`. */
  since: string;
}

/**
 * Where a card came from, and therefore whose word it is.
 *
 * Rendered on every card, and this is a **trust requirement rather than a
 * credit line**: project cards are repo-authored text drawn in a page that is
 * same-origin with the daemon API and with terminal tickets, so a card a
 * teammate wrote must never be mistakable for one Veld wrote. Veld's carry the
 * wordmark; a project's carries the project's name.
 *
 * It is also what makes the history view filterable, which is the other half of
 * the same property: "who has been spending my attention" has to be answerable.
 */
export type Source = { kind: "veld" } | { kind: "project"; name: string };

/**
 * A promotion as a surface actually renders it: what it says, where it came
 * from, and when *this reader* arrived at that source.
 *
 * `arrivedAt` is on the card rather than passed alongside it because the two
 * channels have different arrivals and one gate. Veld's cards gate on
 * `promotions.firstUse` — when this user arrived at Veld. A project's gate on
 * when this user imported *that project*, so a teammate who has had the repo
 * for a year and a new hire who cloned it this morning get different answers
 * about a card dated six months ago. Threading two arrival dates through every
 * decision function is how one of them ends up compared against the wrong
 * channel; one value per card cannot be got wrong.
 */
export interface Card extends Promotion {
  source: Source;
  /** ISO instant this reader arrived at {@link Card.source}. */
  arrivedAt: string;
}

/** The repo fields a project's cards are built from. */
export interface NewsRepo {
  /** Absolute path to the main checkout — the registry's primary key. */
  root: string;
  name: string;
  /** RFC3339, when this user imported the repo. Their arrival at the project. */
  created_at: string;
}

/** The namespace every project card's id sits under. */
const PROJECT_PREFIX = "proj";

const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const U64 = 0xffffffffffffffffn;

/**
 * A project's namespace: 16 hex characters of FNV-1a over the repo root.
 *
 * **Why a hash and not the path itself.** Three reasons, in order of how badly
 * each bites. A unix directory name may legally contain `:`, so a raw path in
 * the id would make `proj:<x>:<slug>` ambiguous — the one property the whole
 * namespace exists to have. It is unbounded, where the promotions endpoint caps
 * an id at 128 characters. And it would put a user's home-directory layout into
 * a database row, which is nobody's business.
 *
 * **Why the repo root and not something that survives `mv`.** The semantically
 * right key is the project id (main-checkout root plus the config's path within
 * it), but computing it shells out to `git`, and the endpoint that carries this
 * data is polled by every window and forbidden from spawning subprocesses. So:
 * the root path, which the client already has. Moving a checkout re-shows that
 * project's live cards once — the same thing that already happens to its machine
 * var overrides, runs, logs, stats and feedback, all keyed by a raw path. That
 * failure is loud, bounded by the live-item cap, and self-healing; the
 * alternative of author-chosen global ids fails the other way, silently
 * suppressing one repo's card because an unrelated repo shipped the same slug
 * first, forever.
 *
 * A 64-bit collision would mean two repos sharing a namespace, so a card in one
 * could suppress a same-slug card in the other. Across the handful of repos one
 * person imports, that is not a risk worth a wider hash.
 *
 * FNV-1a is not collision-resistant, so be honest about the other case: the hashed
 * input ends in a directory name that is usually the remote's own repo name, so a
 * collision here is **constructible**, not merely improbable — somebody who knows
 * where a target keeps their checkouts could name a repo to land on another repo's
 * namespace and pre-claim one state slot. The whole prize is one card of theirs not
 * being shown, on a machine where the attacker already got the target to clone
 * their repository. Not worth a wider hash either; worth not overstating.
 */
export function projectNamespace(repoRoot: string): string {
  let hash = FNV_OFFSET;
  // Bytes, not UTF-16 code units, so a path with an emoji in it hashes to one
  // thing rather than to whatever the runtime's string iteration does.
  for (const byte of new TextEncoder().encode(repoRoot)) {
    hash = ((hash ^ BigInt(byte)) * FNV_PRIME) & U64;
  }
  return hash.toString(16).padStart(16, "0");
}

/**
 * The id a project card is stored against.
 *
 * This is the **only** id a project card ever has: {@link projectCards} writes it
 * into `Card.id`, so nothing downstream holds an author's bare slug that it
 * could accidentally persist. Two ids for one card is how the wrong one reaches
 * the state map, and the state map is forever.
 */
export function namespacedId(repoRoot: string, slug: string): string {
  return [PROJECT_PREFIX, projectNamespace(repoRoot), slug].join(NAMESPACE_SEPARATOR);
}

/**
 * Veld's own cards, gated by when this user arrived at Veld.
 */
export function veldCards(promotions: readonly Promotion[], firstUseIso: string): Card[] {
  return promotions.map((p) => ({ ...p, source: { kind: "veld" }, arrivedAt: firstUseIso }));
}

/**
 * One project's cards, gated by when this user imported that project.
 *
 * Three guards, all cheap, and all about invariants rather than about validation
 * the parser already did — this is the boundary untrusted content crosses, so a
 * guard here is worth having even where the daemon already has one:
 *
 * - **A slug containing `:` is dropped.** `veld_core::ide::valid_news_id` cannot
 *   admit one, so this only fires if that grammar is ever loosened — at which
 *   point a repo could write an id indistinguishable from one of Veld's, and
 *   dropping it here is what keeps the namespace claim locally true instead of
 *   locally assumed.
 * - **A future-dated item is dropped.** `since` is the only thing that retires a
 *   card, so a day that has not happened yet is after *every* arrival, forever —
 *   the never-expiring card this channel deleted the `onboarding` kind to be rid
 *   of. `parse_news` refuses it too and tells the author, which is where the fix
 *   belongs; this is what protects a reader whose daemon predates that check, or
 *   whose clock disagrees with the author's.
 * - **An unknown glyph falls back to `inbox`.** Also parser-enforced; the
 *   fallback exists so a newer daemon naming a glyph this bundle has not learned
 *   renders a card with the wrong mark rather than no card at all.
 *
 * `today` is passed in rather than read from the clock so this stays pure and the
 * date gate stays testable — the same reason `arrivedAt` rides on the card.
 */
export function projectCards(
  repo: NewsRepo,
  news: readonly ProjectNewsItem[],
  today: string,
): Card[] {
  const source: Source = { kind: "project", name: repo.name };
  return news
    .filter((item) => !item.id.includes(NAMESPACE_SEPARATOR) && item.since <= today)
    .map((item) => ({
      ...item,
      id: namespacedId(repo.root, item.id),
      glyph: (GLYPH_NAMES as readonly string[]).includes(item.glyph)
        ? (item.glyph as GlyphName)
        : "inbox",
      source,
      arrivedAt: repo.created_at,
    }));
}

const MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];

/**
 * `2026-08-12` as `12 Aug 2026`.
 *
 * Hand-rolled rather than `toLocaleDateString`: the input is a plain day with no
 * timezone, and handing it to `Date` invites exactly the off-by-one this format
 * was chosen to avoid — `new Date("2026-08-12")` is midnight *UTC*, which prints
 * as the 11th for anyone west of it. A malformed value renders as itself rather
 * than as `NaN`.
 */
export function formatDay(day: string): string {
  if (!DAY_PATTERN.test(day)) return day;
  const [year, month, dayOfMonth] = day.split("-");
  return `${Number(dayOfMonth)} ${MONTHS[Number(month) - 1]} ${year}`;
}

/**
 * The UTC day of an ISO timestamp, as `YYYY-MM-DD`.
 *
 * Day-granularity strings compared lexicographically, which is exact for this
 * format and avoids parsing dates and the timezone bugs that come with it. The
 * one consequence worth knowing: the boundary is the **UTC** day, so a user well
 * west of UTC arriving late in their evening can auto-read an item dated that
 * same local day. A one-day edge on a "show this card or not" decision, traded
 * for having no date arithmetic anywhere in the system.
 */
export function utcDay(iso: string): string {
  return iso.slice(0, 10);
}

/** What the surface hands {@link buildCards} — both channels, and the two gates. */
export interface CardSources {
  /** Veld's own, from `content.ts`. */
  promotions: readonly Promotion[];
  /** `promotions.firstUse`, or `null` while it has not loaded. */
  firstUseIso: string | null;
  /** The selected project and its main checkout's news, or `null` for none. */
  project: (NewsRepo & { news: readonly ProjectNewsItem[] }) | null;
  /** `ui.showProjectNews` — the reader's own switch. */
  showProject: boolean;
  /** Today, `YYYY-MM-DD`. See {@link projectCards}. */
  today: string;
}

/**
 * Every card a surface should consider, from both channels.
 *
 * Pure, and here rather than in the hook that calls it, because this is where the
 * feature's user-visible promises actually live: that `ui.showProjectNews: false`
 * removes a project's cards from the prompt *and* the badge, that a project's
 * cards are gated on that project's own arrival, and that **nothing** is built
 * before `firstUse` has loaded. The UI suite runs with no jsdom, so a decision
 * left in a `.tsx`/hook is a decision nothing can test — and the switch this gates
 * is the only mitigation a reader has against repo-authored modals.
 *
 * Empty while `firstUseIso` is null: with no arrival there is no date gate, and
 * guessing one is how a fresh install gets a modal about last spring. That gates
 * the project's cards on Veld's own stamp too, which is right — the request that
 * carries `firstUse` is the same one that carries the read/dismissed map, so
 * without it there is nothing to compare a read against either.
 */
export function buildCards(sources: CardSources): Card[] {
  if (!sources.firstUseIso) return [];
  const project =
    sources.showProject && sources.project
      ? projectCards(sources.project, sources.project.news, sources.today)
      : [];
  return [...veldCards(sources.promotions, sources.firstUseIso), ...project];
}

/** One tab on the history view's source filter. */
export interface FilterOption {
  value: SourceFilter;
  label: string;
  disabled: boolean;
}

/**
 * The filter tabs, or `null` when there is nothing worth filtering between.
 *
 * Pure and here for the same reason as {@link buildCards}: the rule that a
 * selected project with **no** news still gets a tab — present but disabled — is
 * the kind of thing three docs assert and nothing checks. "This project has told
 * you nothing" is an answer worth being able to see; a missing tab reads as the
 * feature not existing at all.
 *
 * `null` for the interrupting entrance, where a filter would ask the reader to
 * curate a list of things they have not read yet.
 */
export function filterOptions(
  cards: readonly Card[],
  projectName: string | null,
  automatic: boolean,
): FilterOption[] | null {
  const hasProjectCards = sourcesOf(cards).some((s) => s.kind === "project");
  if (automatic || (!hasProjectCards && projectName === null)) return null;
  return [
    { value: "all", label: "Everything", disabled: false },
    // "Official" rather than "Veld": this repo is *named* veld, so a "Veld" tab
    // beside a "veld" tab distinguishes nothing.
    { value: "veld", label: sourceLabel({ kind: "veld" }), disabled: false },
    { value: "project", label: projectName ?? "Project", disabled: !hasProjectCards },
  ];
}

/**
 * Whether an item predates the user, and is therefore not theirs to read.
 *
 * Strictly *before* the arrival day, not on or before: somebody who installs
 * Veld on release day should still be told what shipped that day, and the
 * alternative silently swallows every launch-day announcement.
 *
 * Unconditional, which is the property that makes a zombie card unrepresentable:
 * there is no kind that opts out of the date gate, so every card has an audience
 * that shrinks to nobody whether or not its author ever comes back to delete it.
 */
function predatesUser(c: Card): boolean {
  return c.since < utcDay(c.arrivedAt);
}

/**
 * Where one promotion stands for this user.
 *
 * Stored state wins over the date gate, in both directions and deliberately: an
 * explicit read is a fact about the user and must not be overwritten by
 * arithmetic, and an item they dismissed cannot also be one that predates them.
 */
export function visibilityOf(
  c: Card,
  states: Readonly<Record<string, PromotionState>>,
): Visibility {
  const stored = states[c.id];
  if (stored === "read") return "read";
  if (predatesUser(c)) return "auto-read";
  if (stored === "dismissed") return "dismissed";
  return "unread";
}

/**
 * The promotions to put in front of the user right now.
 *
 * `unread` only. A dismissed card is deliberately absent: the user already said
 * "not now", and re-prompting is how a news channel becomes something people
 * learn to close without reading.
 */
export function toPrompt(
  cards: readonly Card[],
  states: Readonly<Record<string, PromotionState>>,
): Card[] {
  return cards.filter((c) => visibilityOf(c, states) === "unread");
}

/**
 * What the unread indicator shows.
 *
 * Counts `dismissed` as well as `unread` — that is the whole point of keeping
 * the two apart. Dismissing stops the modal; only reading clears the badge.
 * `auto-read` items never count: they are visible in the history but were never
 * this user's news.
 *
 * Defined as the length of {@link unreadOf} rather than as its own filter,
 * because "what the badge claims is outstanding" and "what closing the panel
 * writes" have to be the same set. Two copies of that predicate is one copy that
 * can drift, and the symptom is a badge that never quite clears.
 */
export function unreadCount(
  cards: readonly Card[],
  states: Readonly<Record<string, PromotionState>>,
): number {
  return unreadOf(cards, states).length;
}

/**
 * Apply a state to `ids`, the way the daemon does.
 *
 * A second copy of the daemon's merge, and it has to be: the panel must stop
 * re-prompting the instant it closes, and what may prompt is recomputed from
 * this map. It lives here rather than in the hook so the node-environment test
 * suite can hold it against the Rust tests' own cases — a merge rule duplicated
 * into TSX is a rule nothing checks.
 *
 * Monotone, like the daemon's: `read` wins and neither state is ever undone.
 */
export function mergeStates(
  current: Readonly<Record<string, PromotionState>>,
  ids: readonly string[],
  state: PromotionState,
): Record<string, PromotionState> {
  const next = { ...current };
  for (const id of ids) {
    if (next[id] !== "read") next[id] = state;
  }
  return next;
}

/**
 * The subset of `ids` worth sending — those the user has not already read.
 *
 * Browsing the panel from the menu shows everything the build ships, including
 * items auto-read because they predate the user. Marking all of those `read`
 * would write a row per promotion for cards this user never had, against the
 * store's own rule that the map stays proportional to what they acted on.
 */
export function unreadOf(
  cards: readonly Card[],
  states: Readonly<Record<string, PromotionState>>,
): string[] {
  return cards
    .filter((c) => {
      const v = visibilityOf(c, states);
      return v === "unread" || v === "dismissed";
    })
    .map((c) => c.id);
}

/** Problems with a promotion, on top of {@link sectionProblems}. */
export function promotionProblems(p: Promotion): string[] {
  const problems = sectionProblems(p);
  if (!DAY_PATTERN.test(p.since))
    problems.push(`${p.id}: needs a YYYY-MM-DD 'since' (is ${JSON.stringify(p.since)})`);
  return problems;
}

/** Duplicate ids across a collection — two sections sharing one acked slot. */
export function duplicateIds(sections: readonly Section[]): string[] {
  const seen = new Set<string>();
  const dupes = new Set<string>();
  for (const s of sections) {
    if (seen.has(s.id)) dupes.add(s.id);
    seen.add(s.id);
  }
  return [...dupes];
}

/** Just the ids, in author order. */
export function manifestIds(sections: readonly Section[]): string[] {
  return sections.map((s) => s.id);
}

/** Which sources the history view is showing. */
export type SourceFilter = "all" | "veld" | "project";

/**
 * What a card's source is called, on the card and on the filter control.
 *
 * Veld's is **"Official"** rather than "Veld", and that is the whole distinction
 * doing its job: a repo may be *named* `veld` — this one is — at which point a
 * label of "Veld" beside a label of "veld" tells the reader nothing. "Official"
 * is a claim about provenance that no project name can imitate, which is exactly
 * what a byline on untrusted content has to be.
 */
export function sourceLabel(source: Source): string {
  return source.kind === "veld" ? "Official" : source.name;
}

/**
 * The byline a card carries, as a whole sentence rather than a one-word tag.
 *
 * Separate from {@link sourceLabel} because the two answer at different lengths:
 * a filter tab has room for one word, a byline under a sentence has room to say
 * what it means. Both halves are stated in full — "Official veld news" and "News
 * from your project" — so neither reads as a category the reader has to decode.
 * A bare "Official" only works if you already know what the alternative was.
 *
 * The project half deliberately stops before the name: the name is rendered
 * separately so it can carry the weight and be truncated on its own, and so a repo
 * called `Official` cannot write itself into the middle of Veld's claim.
 */
export function sourceByline(source: Source): string {
  return source.kind === "veld" ? "Official veld news" : "News from your project";
}

/**
 * The history view's list: newest first, optionally by source.
 *
 * Sorted by `since` and **not** by the order the two channels were concatenated
 * in. A history that reads "Veld's five, then the project's three" is two lists
 * with one heading; what the reader wants is what changed most recently, whoever
 * changed it. The comparison is on the plain `YYYY-MM-DD` day, so it is
 * lexicographic and needs no date parsing.
 *
 * **The tie-break is reverse input order, and it is load-bearing.** Everything
 * ships on the day it ships, so a release that adds two cards has two cards
 * sharing a date — and both `content.ts` and a project's `ide.news` are
 * *appended* to, which makes the last element the newest thing. A stable sort
 * alone therefore put the newest addition wherever it happened to sit in the
 * array (measured: second of three, under an older card). Comparing indices
 * descending is what makes "newest first" true within a day as well as across
 * days.
 */
export function historyOf(cards: readonly Card[], filter: SourceFilter): Card[] {
  return cards
    .map((card, index) => ({ card, index }))
    .filter(({ card }) => filter === "all" || card.source.kind === filter)
    .sort((a, b) => b.card.since.localeCompare(a.card.since) || b.index - a.index)
    .map(({ card }) => card);
}

/**
 * The distinct sources present, in the order the filter control should offer
 * them: Veld first, then each project once.
 *
 * Derived from the cards rather than from the repo list, so a source that is on
 * screen always has a way to be filtered to. The *converse* is handled by the
 * caller: a selected project with no news still gets a tab, disabled, because
 * "this project has told you nothing" is an answer worth being able to see —
 * silently omitting the tab reads as the feature being missing.
 */
export function sourcesOf(cards: readonly Card[]): Source[] {
  const seen = new Set<string>();
  const out: Source[] = [];
  for (const card of cards) {
    const key = `${card.source.kind}:${sourceLabel(card.source)}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(card.source);
  }
  return out.sort((a, b) => (a.kind === b.kind ? 0 : a.kind === "veld" ? -1 : 1));
}
