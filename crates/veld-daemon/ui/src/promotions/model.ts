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
export type GlyphName = "terminal" | "panes" | "device" | "inbox";

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

const ID_PATTERN = /^[a-z0-9]+(-[a-z0-9]+)*$/u;

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
 * Two kinds, and the difference is *who* a promotion is for.
 *
 * - `onboarding` — orientation. Shown to anyone who has not read it, whenever
 *   they arrived. It does not go stale.
 * - `news` — a change. **Auto-read** for a user who arrived after it, so
 *   installing Veld today never means a modal about something that changed last
 *   spring.
 *
 * Both carry `since`, and it is mandatory for both: every card shows when it
 * landed, whatever kind it is. The kind decides only whether that date *gates*
 * the card, never whether it exists — an onboarding item is still something that
 * was written on a day, and a reader catching up deserves to know which.
 */
export interface Promotion extends Section {
  kind: "onboarding" | "news";
  /** The day this shipped, `YYYY-MM-DD`. */
  since: string;
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

/**
 * Whether a `news` item predates the user, and is therefore not theirs to read.
 *
 * Strictly *before* the arrival day, not on or before: somebody who installs
 * Veld on release day should still be told what shipped that day, and the
 * alternative silently swallows every launch-day announcement.
 */
function predatesUser(p: Promotion, firstUseIso: string): boolean {
  return p.kind === "news" && p.since < utcDay(firstUseIso);
}

/**
 * Where one promotion stands for this user.
 *
 * Stored state wins over the date gate, in both directions and deliberately: an
 * explicit read is a fact about the user and must not be overwritten by
 * arithmetic, and an item they dismissed cannot also be one that predates them.
 */
export function visibilityOf(
  p: Promotion,
  states: Readonly<Record<string, PromotionState>>,
  firstUseIso: string,
): Visibility {
  const stored = states[p.id];
  if (stored === "read") return "read";
  if (predatesUser(p, firstUseIso)) return "auto-read";
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
  promotions: readonly Promotion[],
  states: Readonly<Record<string, PromotionState>>,
  firstUseIso: string,
): Promotion[] {
  return promotions.filter((p) => visibilityOf(p, states, firstUseIso) === "unread");
}

/**
 * What the unread indicator shows.
 *
 * Counts `dismissed` as well as `unread` — that is the whole point of keeping
 * the two apart. Dismissing stops the modal; only reading clears the badge.
 * `auto-read` items never count: they are visible in the history but were never
 * this user's news.
 */
export function unreadCount(
  promotions: readonly Promotion[],
  states: Readonly<Record<string, PromotionState>>,
  firstUseIso: string,
): number {
  return promotions.filter((p) => {
    const v = visibilityOf(p, states, firstUseIso);
    return v === "unread" || v === "dismissed";
  }).length;
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
  promotions: readonly Promotion[],
  states: Readonly<Record<string, PromotionState>>,
  firstUseIso: string,
): string[] {
  return promotions
    .filter((p) => {
      const v = visibilityOf(p, states, firstUseIso);
      return v === "unread" || v === "dismissed";
    })
    .map((p) => p.id);
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
