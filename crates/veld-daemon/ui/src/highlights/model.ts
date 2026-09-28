/**
 * Feature highlights — a one-line bubble pinned to the control a change added,
 * shown once, the first time somebody reaches it.
 *
 * The in-place sibling of the What's-new panel (`promotions/`). A card says "this
 * exists" to everybody, once, wherever they are; a highlight says "this is it" to
 * whoever arrives at the control, at the moment they are looking at it. So a card
 * is for a change you would not otherwise find, and a highlight is for an option
 * you would scroll past without noticing it is new.
 *
 * **Same store, same rules, no new daemon surface.** Seen-state lives in the
 * promotions map (`kv` → `promotions.state`), which the daemon treats as opaque
 * ids — so a highlight is a UI-only change, and it roams across windows the way a
 * card does. Its id sits under its own namespace (`hint:<slug>`), which is what
 * the `:` reservation in `promotions/model.ts`'s `ID_PATTERN` is for: no Veld card
 * can ever be written with that id, and the What's-new badge never counts it,
 * because the badge only counts cards it built.
 *
 * And the same date gate: a highlight is for someone who knew the control
 * *before* it changed. A user who arrived after `since` is meeting every option
 * for the first time, and pointing at one of them as new would be noise.
 */

import { ID_PATTERN, NAMESPACE_SEPARATOR, type PromotionState, utcDay } from "../promotions/model";

export interface Highlight {
  /** Kebab-case, like a card's — and **forever**, like a card's: a rename shows
   *  the bubble again to everyone who already closed it. */
  slug: string;
  /** The day it shipped, `YYYY-MM-DD`. */
  since: string;
  /** One short line, bold. */
  title: string;
  /** One sentence. What they can do with it, not what it is. */
  body: string;
}

/** Caps, for the same reason the cards have them: the bubble covers the UI. */
export const MAX_TITLE = 40;
export const MAX_BODY = 140;

const PREFIX = "hint";

/** The id a highlight is stored under. */
export function highlightId(slug: string): string {
  return `${PREFIX}${NAMESPACE_SEPARATOR}${slug}`;
}

/** Everything wrong with a highlight's content, or `[]`. */
export function highlightProblems(h: Highlight): string[] {
  const problems: string[] = [];
  if (!ID_PATTERN.test(h.slug)) problems.push(`${h.slug || "(empty)"}: slug must be kebab-case`);
  if (!/^\d{4}-\d{2}-\d{2}$/u.test(h.since)) problems.push(`${h.slug}: since must be YYYY-MM-DD`);
  if (!h.title || h.title.length > MAX_TITLE)
    problems.push(`${h.slug}: title must be 1-${MAX_TITLE} chars (is ${h.title.length})`);
  if (!h.body || h.body.length > MAX_BODY)
    problems.push(`${h.slug}: body must be 1-${MAX_BODY} chars (is ${h.body.length})`);
  return problems;
}

/**
 * Whether a highlight may be shown right now.
 *
 * - **Nothing before the store answers.** A null map means the request failed or
 *   has not landed; showing then would show the bubble again to everyone who
 *   closed it, on every flaky load.
 * - **Any stored state ends it.** Unlike a card there is no "dismissed but still
 *   unread" — there is nowhere to find a highlight again later, so both answers
 *   mean "done".
 * - **Arrived on or before `since`**, the cards' own gate (`predatesUser`), so a
 *   highlight and the card announcing the same change reach the same people.
 * - **Not before `since`**, so a highlight written ahead of its release does not
 *   leak out on a build carrying it early.
 */
export function highlightEligible(
  h: Highlight,
  states: Readonly<Record<string, PromotionState>> | null,
  firstUseIso: string | null,
  today: string,
): boolean {
  if (states === null || firstUseIso === null) return false;
  if (states[highlightId(h.slug)] !== undefined) return false;
  if (h.since > today) return false;
  return !(h.since < utcDay(firstUseIso));
}
