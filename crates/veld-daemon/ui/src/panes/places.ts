/**
 * Where a browser pane can go, and how typing narrows that down.
 *
 * The pure half of three surfaces that used to be two lists and a button: the new
 * pane's "Open a page" group, a blank pane's start page, and the address bar's
 * suggestions. One list, three renderings — the previous shape had the run's URLs
 * rendered by one component and the *decision* to open a browser rendered as a
 * button in a different group, which is why users clicked the button and never
 * connected it to the URLs five rows below.
 *
 * Two kinds of place, and the distinction is the point rather than a label: a run
 * URL is something veld started and is serving *now*, a bookmark is a string in the
 * project's config that nobody has probed. They were previously the same row under
 * two captions, which is exactly the pair a first-time user could not tell apart.
 *
 * Pure and dependency-free (beyond the address rules) so `vitest`'s node
 * environment can test the filtering and keyboard arithmetic — the two things a
 * component test cannot check without a DOM.
 */

import type { Quicklink } from "../api";
import { type Target, resolveAddress } from "./model";

export type PlaceKind = "run" | "bookmark";

export interface Place {
  kind: PlaceKind;
  /** Service name for a run URL, the configured label for a bookmark. */
  name: string;
  url: string;
}

/**
 * The run's URLs and the project's bookmarks, as one ordered list.
 *
 * Run URLs first, always: they are why the pane is open. Both keep the order they
 * arrive in — the run's are already service-name-sorted by `sortedUrls`, and a
 * project's bookmarks are in the order the config declares, which is the only
 * ordering their author controls.
 */
export function placesFor(
  urls: Array<[string, string]>,
  quicklinks: Quicklink[],
): Place[] {
  return [
    ...urls.map(([name, url]): Place => ({ kind: "run", name, url })),
    ...quicklinks.map((link): Place => ({
      kind: "bookmark",
      name: link.label,
      url: link.url,
    })),
  ];
}

/**
 * The places a query still matches.
 *
 * Substring over the name and the URL, case-insensitively: typing `3000` should find
 * the service on that port and typing `web` should find it by name, and a user does
 * not know which of the two they are typing. No fuzzy matching — a list this short
 * gains nothing from it, and fuzzy ranking makes the row under the cursor move for
 * reasons the user cannot see.
 */
export function filterPlaces(places: Place[], query: string): Place[] {
  const q = query.trim().toLowerCase();
  if (q === "") return places;
  return places.filter(
    (p) =>
      p.name.toLowerCase().includes(q) || p.url.toLowerCase().includes(q),
  );
}

/** What the address bar offers for what has been typed so far. */
export interface Suggestions {
  /**
   * The literal thing typed, when it resolves to somewhere — `Go to …` or
   * `Search for …`. Null while the field is empty, and null for text that is
   * neither (a broken address with search off), because a row that cannot be
   * opened is a row that lies about being one.
   */
  action: Target | null;
  places: Place[];
  /** Rows in total, action included. What the arrow keys move within. */
  count: number;
  /**
   * Places before filtering.
   *
   * Carried so a renderer can tell "this run has no URLs" from "your query matched
   * none of them" — two states that were one, which put the app's *there is no run*
   * hint on screen while a run was up with five URLs, simply because the filter was
   * narrower than the list.
   */
  total: number;
}

export function suggestionsFor(
  places: Place[],
  query: string,
  searchUrl: string,
): Suggestions {
  const matched = filterPlaces(places, query);
  const resolved = query.trim() === "" ? null : resolveAddress(query, searchUrl);
  const action = resolved && resolved.kind !== "invalid" ? resolved : null;
  return {
    action,
    places: matched,
    count: (action ? 1 : 0) + matched.length,
    total: places.length,
  };
}

/**
 * Where the arrows land.
 *
 * Wraps, because a list this short has no scrollback to get lost in and pressing
 * Down at the end to reach the top is what every address bar does. `-1` for an empty
 * list, which is also the "nothing selected" value — so a caller cannot accidentally
 * hold an index into a list that no longer has rows.
 */
export function stepIndex(count: number, current: number, delta: number): number {
  if (count <= 0) return -1;
  // From "nothing selected", Down goes to the first row and Up to the last, rather
  // than both landing on row 0.
  if (current < 0) return delta > 0 ? 0 : count - 1;
  return (current + delta + count) % count;
}

/** What opening row `index` means, or null when the index selects nothing. */
export function pickSuggestion(
  s: Suggestions,
  index: number,
): { url: string; title?: string } | null {
  if (index < 0 || index >= s.count) return null;
  if (s.action) {
    if (index === 0) return { url: s.action.url };
    const place = s.places[index - 1];
    return place ? { url: place.url, title: place.name } : null;
  }
  const place = s.places[index];
  return place ? { url: place.url, title: place.name } : null;
}
