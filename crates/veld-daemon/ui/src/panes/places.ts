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
 * Three kinds of place, and the distinctions are the point rather than labels: a run
 * URL is something veld started and is serving *now*, a bookmark is a string in the
 * project's config that nobody has probed, and a file is something on disk that
 * changed recently. The first two were previously the same row under two captions,
 * which is exactly the pair a first-time user could not tell apart.
 *
 * Pure and dependency-free (beyond the address rules) so `vitest`'s node
 * environment can test the filtering and keyboard arithmetic — the two things a
 * component test cannot check without a DOM.
 */

import type { Quicklink, ViewableFile } from "../api";
import { type Target, resolveAddress } from "./model";

export type PlaceKind = "run" | "bookmark" | "file";

export interface Place {
  kind: PlaceKind;
  /** Service name for a run URL, the configured label for a bookmark, the
   *  worktree-relative path for a file. */
  name: string;
  url: string;
  /** `file` only: the worktree-relative path. Also `name`, because that is what a
   *  row is matched against — but carried separately so opening a row does not
   *  have to re-derive it from a label. */
  path?: string;
  /** `file` only: when it was last written, milliseconds. Rendered as "2 min ago",
   *  which is the field that explains why the row is where it is in the list. */
  mtimeMs?: number;
}

/**
 * The run's URLs, the worktree's recent files, and the project's bookmarks, as one
 * ordered list.
 *
 * Run URLs first, always: they are why the pane is open. All three keep the order
 * they arrive in — the run's are already service-name-sorted by `sortedUrls`, files
 * are newest-first from the daemon's scan, and a project's bookmarks are in the
 * order the config declares, which is the only ordering their author controls.
 */
export function placesFor(
  urls: Array<[string, string]>,
  quicklinks: Quicklink[],
  files: ViewableFile[] = [],
): Place[] {
  return [
    ...urls.map(([name, url]): Place => ({ kind: "run", name, url })),
    // Between the two, and that ordering is the argument for the whole feature:
    // more immediate than an address somebody wrote in a config once, less
    // immediate than the servers running right now. Already newest-first from the
    // daemon — the scan sorts, so this must not re-sort and must not re-order.
    ...files.map((f): Place => ({
      kind: "file",
      name: f.name,
      url: f.url,
      path: f.name,
      mtimeMs: f.mtimeMs,
    })),
    ...quicklinks.map((link): Place => ({
      kind: "bookmark",
      name: link.label,
      url: link.url,
    })),
  ];
}

/**
 * How many recent files a full-size screen offers unprompted, and how recent they
 * have to be.
 *
 * Three, from the last day, and only when the run has no URLs of its own — see
 * [`inlineFiles`]. The daemon returns up to a hundred; the surface these land on is
 * a screen whose whole job is the four or five things you might do next, so the list
 * is a *hint* rather than the file manager. Everything else is one click away in the
 * Files dialog, which has a search field precisely because it is the unbounded view.
 */
export const INLINE_FILES_SHOWN = 3;
/** How old a file can be and still be offered unprompted: one day. */
export const INLINE_FILES_MAX_AGE_MS = 24 * 60 * 60 * 1000;

/**
 * The recent files a full-size screen shows without being asked.
 *
 * Three rules, and each answers a way the first version was wrong on a real
 * repository:
 *
 * - **Nothing when the run has URLs.** A running dev server is why the pane is
 *   open, and a list of files under it competes with the thing the person came for.
 *   With no run there is nothing else on that part of the screen, and a recent file
 *   is the most useful thing Veld can offer.
 * - **At most three.** Enough to catch "the thing I just made"; short enough not to
 *   become a directory listing.
 * - **Nothing older than a day.** A file from last week is not news, and offering it
 *   unprompted makes the list look arbitrary rather than recent.
 *
 * `now` is passed in rather than read from the clock, so this stays pure and the
 * age boundary is testable without freezing time.
 */
export function inlineFiles(
  files: ViewableFile[],
  opts: { hasRunUrls: boolean; now: number },
): ViewableFile[] {
  if (opts.hasRunUrls) return [];
  return files
    .filter((f) => opts.now - f.mtimeMs <= INLINE_FILES_MAX_AGE_MS)
    .slice(0, INLINE_FILES_SHOWN);
}

/**
 * How many recent files the address bar's panel offers while nothing is typed.
 *
 * Larger than the full-size screens' three, and deliberately a different number
 * for a different question: the panel opens *because* somebody is choosing where to
 * go, so a slightly longer list is the answer rather than a distraction. Typing
 * lifts the cap entirely.
 */
const RECENT_FILES_SHOWN = 8;

/**
 * The rows shown when nothing has been typed: the run, plus the newest few files.
 *
 * Bookmarks are dropped here rather than capped — they come back as their own
 * button, which is the arrangement a user test settled (see `PlaceList`). Files are
 * capped rather than dropped, because unlike a bookmark a recent file *is* the
 * answer often enough to earn a row without being asked for.
 *
 * Order is preserved, never re-sorted: the daemon's newest-first ordering is the
 * point of the list, and `filterPlaces` keeps it.
 */
function untypedRows(matched: Place[]): Place[] {
  let files = 0;
  return matched.filter((p) => {
    if (p.kind === "bookmark") return false;
    if (p.kind !== "file") return true;
    files += 1;
    return files <= RECENT_FILES_SHOWN;
  });
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
  /**
   * The bookmarks that are *not* in `places`, for the surface that offers them
   * behind a button.
   *
   * Always empty while {@link Suggestions.narrowed} — see `suggestionsFor`.
   */
  bookmarks: Place[];
  /** Whether a query is narrowing the list, i.e. the user has typed something. */
  narrowed: boolean;
}

/**
 * What to offer, and — with nothing typed — what to keep behind a button.
 *
 * **Bookmarks collapse out of the list while the query is empty.** A project with
 * four to eight services per run puts the thing you came for (a run URL veld is
 * serving *now*) below however many addresses somebody wrote in a config, on every
 * one of the three surfaces this feeds. So the default list is the run, and the
 * bookmarks go behind one button that opens all of them.
 *
 * The moment something *is* typed, they come back inline. A query is a search over
 * every place the project knows, and a bookmark you can find by typing `github` but
 * not by typing at all would be a filter that lies about its scope. One rule, three
 * surfaces: not typing shows the run, typing searches everything.
 *
 * `count` therefore indexes `places` alone — the collapsed bookmarks are not rows,
 * so the arrow keys cannot land on them.
 */
export function suggestionsFor(
  places: Place[],
  query: string,
  searchUrl: string,
): Suggestions {
  const narrowed = query.trim() !== "";
  const matched = filterPlaces(places, query);
  const shown = narrowed ? matched : untypedRows(matched);
  const bookmarks = narrowed ? [] : matched.filter((p) => p.kind === "bookmark");
  const resolved = narrowed ? resolveAddress(query, searchUrl) : null;
  const action = resolved && resolved.kind !== "invalid" ? resolved : null;
  return {
    action,
    places: shown,
    count: (action ? 1 : 0) + shown.length,
    total: places.length,
    bookmarks,
    narrowed,
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
): { url: string; title?: string; path?: string } | null {
  if (index < 0 || index >= s.count) return null;
  if (s.action) {
    if (index === 0) return { url: s.action.url };
    const place = s.places[index - 1];
    return place ? picked(place) : null;
  }
  const place = s.places[index];
  return place ? picked(place) : null;
}

/**
 * One place, as the thing a pane needs to open it.
 *
 * The `path` only travels for a file, because it is only meaningful for one: it is
 * what the pane watches for changes, and a `path` on a run URL would be a field
 * nothing could use and something would eventually trust.
 */
function picked(place: Place): { url: string; title?: string; path?: string } {
  return place.kind === "file"
    ? { url: place.url, title: place.name, path: place.path }
    : { url: place.url, title: place.name };
}

/**
 * Which broad kind of file a path is, for picking a glyph.
 *
 * Deliberately coarser than the daemon's extension table and **not** derived from
 * it: that table answers "may these bytes be served" and has thirty entries, while
 * this answers "which of five icons" and must stay total for an extension nobody
 * listed. Keeping them separate means adding a servable type never silently changes
 * an icon, and a pattern-matched file with an unknown extension still renders.
 */
export type FileKind = "html" | "pdf" | "image" | "text" | "other";

const HTML_EXTS = ["html", "htm"];
const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "ico", "bmp"];
const TEXT_EXTS = ["txt", "log", "md", "json", "csv", "tsv", "yaml", "yml", "toml", "xml"];

export function fileKindOf(path: string): FileKind {
  const name = path.split("/").pop() ?? path;
  const dot = name.lastIndexOf(".");
  // No extension, or a dotfile whose only dot is its first character.
  if (dot <= 0) return "other";
  const ext = name.slice(dot + 1).toLowerCase();
  if (HTML_EXTS.includes(ext)) return "html";
  if (ext === "pdf") return "pdf";
  if (IMAGE_EXTS.includes(ext)) return "image";
  if (TEXT_EXTS.includes(ext)) return "text";
  return "other";
}

/**
 * The directory a file sits in, or `null` at the worktree root.
 *
 * `null` rather than `""` so a caller has to decide what the root reads as, instead
 * of rendering an empty line that looks like a missing value.
 */
export function fileDir(path: string): string | null {
  const cut = path.lastIndexOf("/");
  return cut <= 0 ? null : path.slice(0, cut);
}

/**
 * How long ago, in the shortest form that is still true.
 *
 * Rounded **down** at every step, and that is the point: a file written 119 seconds
 * ago reads as "1 min ago", never "2 min ago", so the number never claims the file
 * is older than it is. Anything under a minute is "just now" — a seconds count on a
 * row that re-renders on its own schedule is a number that is wrong as often as it
 * is right.
 *
 * `now` is a parameter so this is pure. A negative difference (a file written in the
 * future, which a bad clock or a copied mtime produces) also reads as "just now"
 * rather than as a negative age.
 */
export function timeAgo(mtimeMs: number, now: number): string {
  const seconds = Math.floor((now - mtimeMs) / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} d ago`;
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return `${weeks} w ago`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months} mo ago`;
  return `${Math.floor(days / 365)} y ago`;
}
