import { describe, expect, it } from "vitest";
import {
  INLINE_FILES_MAX_AGE_MS,
  fileDir,
  fileKindOf,
  filterPlaces,
  inlineFiles,
  pickSuggestion,
  placesFor,
  stepIndex,
  suggestionsFor,
  timeAgo,
} from "./places";

const URLS: Array<[string, string]> = [
  ["api", "https://api.dev.veld.localhost/"],
  ["web", "https://web.dev.veld.localhost/"],
];
const LINKS = [
  { label: "GitHub", url: "https://github.com/prosperity-solutions/veld" },
  { label: "Staging", url: "https://staging.example.com" },
];
const ENGINE = "https://www.google.com/search?q=%s";

describe("placesFor", () => {
  it("puts the run's URLs before the project's bookmarks", () => {
    const places = placesFor(URLS, LINKS);
    expect(places.map((p) => p.kind)).toEqual([
      "run",
      "run",
      "bookmark",
      "bookmark",
    ]);
    // The two kinds are carried as data, not as a caption above a group: the rows
    // render differently, and a filtered list interleaves nothing.
    expect(places[0]).toEqual({
      kind: "run",
      name: "api",
      url: "https://api.dev.veld.localhost/",
    });
    expect(places[3]?.name).toBe("Staging");
  });
});

describe("filterPlaces", () => {
  const places = placesFor(URLS, LINKS);

  it("matches the name or the URL, since a user does not know which they typed", () => {
    expect(filterPlaces(places, "web").map((p) => p.name)).toEqual(["web"]);
    expect(filterPlaces(places, "GITHUB").map((p) => p.name)).toEqual(["GitHub"]);
    // By URL only — the string never appears in a name.
    expect(filterPlaces(places, "staging.example").map((p) => p.name)).toEqual([
      "Staging",
    ]);
    expect(filterPlaces(places, "veld").map((p) => p.name)).toEqual([
      "api",
      "web",
      "GitHub",
    ]);
  });

  it("shows everything for an empty or blank query", () => {
    expect(filterPlaces(places, "")).toHaveLength(4);
    expect(filterPlaces(places, "   ")).toHaveLength(4);
    expect(filterPlaces(places, "nothing here")).toEqual([]);
  });
});

describe("suggestionsFor", () => {
  const places = placesFor(URLS, LINKS);

  it("offers no action row while the field is empty", () => {
    const s = suggestionsFor(places, "", ENGINE);
    expect(s.action).toBeNull();
    // Two, not four: with nothing typed the bookmarks are behind a button.
    expect(s.count).toBe(2);
  });

  it("collapses the bookmarks while nothing is typed", () => {
    const s = suggestionsFor(places, "", ENGINE);
    expect(s.narrowed).toBe(false);
    expect(s.places.map((p) => p.name)).toEqual(["api", "web"]);
    expect(s.bookmarks.map((p) => p.name)).toEqual(["GitHub", "Staging"]);
    // `count` indexes `places` alone, so the arrow keys can never land on a
    // collapsed bookmark.
    expect(s.count).toBe(s.places.length);
    // The unfiltered size still counts every place: it answers "does this project
    // have anywhere to go", which a collapsed bookmark does.
    expect(s.total).toBe(4);
  });

  it("brings the bookmarks back inline the moment something is typed", () => {
    // A filter that could not see a bookmark would be a filter lying about its
    // scope — typing `github` has to find the bookmark that is only reachable
    // through the button otherwise.
    const s = suggestionsFor(places, "github", ENGINE);
    expect(s.narrowed).toBe(true);
    expect(s.places.map((p) => p.name)).toEqual(["GitHub"]);
    expect(s.bookmarks).toEqual([]);
  });

  it("collapses bookmarks even when the run has no URLs of its own", () => {
    // The surface with no run URLs is the one that must not read as empty: `places`
    // is empty, `total` is not, and the button is the only way in.
    const s = suggestionsFor(placesFor([], LINKS), "", ENGINE);
    expect(s.places).toEqual([]);
    expect(s.count).toBe(0);
    expect(s.total).toBe(2);
    expect(s.bookmarks).toHaveLength(2);
  });

  it("heads the list with the literal thing typed", () => {
    const url = suggestionsFor(places, "example.com/x", ENGINE);
    expect(url.action).toEqual({ kind: "url", url: "https://example.com/x" });
    // The action is a row, so it counts — the arrow keys and the click handler must
    // agree about how many rows there are.
    expect(url.count).toBe(1 + url.places.length);

    const query = suggestionsFor(places, "how do i use veld", ENGINE);
    expect(query.action?.kind).toBe("search");
  });

  it("reports the unfiltered size, so a renderer can tell no-places from no-matches", () => {
    // Without `total`, a filter that matched nothing was indistinguishable from a run
    // with no URLs — and the renderer put the app's "start the run and its services
    // appear here" hint on screen while a run was up with four places.
    const matched = suggestionsFor(places, "zzz nothing", ENGINE);
    expect(matched.places).toEqual([]);
    expect(matched.total).toBe(4);

    const none = suggestionsFor([], "zzz nothing", ENGINE);
    expect(none.total).toBe(0);
  });

  it("offers no action row for text that resolves nowhere", () => {
    // A broken address is not a search — see `resolveAddress`.
    expect(suggestionsFor(places, "http://", ENGINE).action).toBeNull();
    // Search off, and the words are not an address: there is nothing to open, so
    // there is no row. The places still filter.
    const off = suggestionsFor(places, "web", "");
    expect(off.action).toBeNull();
    expect(off.count).toBe(1);
  });
});

describe("stepIndex", () => {
  it("wraps, and starts from either end depending on the direction", () => {
    expect(stepIndex(3, -1, 1)).toBe(0);
    expect(stepIndex(3, -1, -1)).toBe(2);
    expect(stepIndex(3, 2, 1)).toBe(0);
    expect(stepIndex(3, 0, -1)).toBe(2);
    expect(stepIndex(3, 1, 1)).toBe(2);
  });

  it("selects nothing when there is nothing to select", () => {
    expect(stepIndex(0, -1, 1)).toBe(-1);
    expect(stepIndex(0, 4, -1)).toBe(-1);
  });
});

describe("pickSuggestion", () => {
  const places = placesFor(URLS, LINKS);

  it("counts the action row as row zero when there is one", () => {
    const s = suggestionsFor(places, "web", ENGINE);
    // "web" is not an address, so row 0 is the search and row 1 is the matched run
    // URL. Off by one here would open a search when the user picked their app.
    expect(s.action?.kind).toBe("search");
    expect(pickSuggestion(s, 0)?.url).toContain("google.com/search?q=web");
    expect(pickSuggestion(s, 1)).toEqual({
      url: "https://web.dev.veld.localhost/",
      title: "web",
    });
  });

  it("indexes the places directly when there is no action row", () => {
    const s = suggestionsFor(places, "", ENGINE);
    expect(pickSuggestion(s, 0)).toEqual({
      url: "https://api.dev.veld.localhost/",
      title: "api",
    });
    expect(pickSuggestion(s, 1)?.title).toBe("web");
    // Row 2 does not exist with the bookmarks collapsed. A picker that still reached
    // into them would open a place the list is not showing.
    expect(pickSuggestion(s, 2)).toBeNull();
  });

  it("returns null for an index that selects nothing", () => {
    const s = suggestionsFor(places, "", ENGINE);
    expect(pickSuggestion(s, -1)).toBeNull();
    expect(pickSuggestion(s, 4)).toBeNull();
    expect(pickSuggestion(suggestionsFor([], "", ENGINE), 0)).toBeNull();
  });
});

const FILES = [
  { name: "notes/deck.html", url: "https://files.veld.localhost/g/notes/deck.html", mtimeMs: 300 },
  { name: "report.pdf", url: "https://files.veld.localhost/g/report.pdf", mtimeMs: 200 },
];

describe("local files as places", () => {
  it("sits between the run's URLs and the project's bookmarks", () => {
    const places = placesFor(URLS, LINKS, FILES);
    expect(places.map((p) => p.kind)).toEqual([
      "run",
      "run",
      "file",
      "file",
      "bookmark",
      "bookmark",
    ]);
  });

  it("keeps the daemon's newest-first order rather than sorting by name", () => {
    // `report.pdf` sorts before `notes/deck.html` alphabetically and after it by
    // recency. Recency is the ordering the list exists for, so re-sorting here
    // would quietly destroy the feature's whole premise.
    const places = placesFor([], [], FILES);
    expect(places.map((p) => p.name)).toEqual(["notes/deck.html", "report.pdf"]);
  });

  it("carries the relative path, which is what a pane watches", () => {
    const places = placesFor([], [], FILES);
    expect(places[0]?.path).toBe("notes/deck.html");
    // Only a file has one: a path on a run URL would be a field nothing can use.
    expect(placesFor(URLS, LINKS)[0]?.path).toBeUndefined();
  });

  it("shows files with nothing typed, unlike bookmarks", () => {
    const s = suggestionsFor(placesFor(URLS, LINKS, FILES), "", ENGINE);
    expect(s.places.map((p) => p.kind)).toEqual(["run", "run", "file", "file"]);
    // Bookmarks are still behind their button.
    expect(s.bookmarks).toHaveLength(2);
  });

  it("caps the untyped list, and typing lifts the cap", () => {
    const many = Array.from({ length: 20 }, (_, i) => ({
      name: `deck-${i}.html`,
      url: `https://files.veld.localhost/g/deck-${i}.html`,
      mtimeMs: 1000 - i,
    }));
    const places = placesFor([], [], many);
    expect(suggestionsFor(places, "", ENGINE).places).toHaveLength(8);
    // The cap is on the *untyped* default. A query is a question, and it gets
    // every answer — here, all twenty match `deck`.
    expect(suggestionsFor(places, "deck", ENGINE).places).toHaveLength(20);
  });

  it("is findable by path fragment, not only by file name", () => {
    const places = placesFor(URLS, LINKS, FILES);
    expect(filterPlaces(places, "notes/").map((p) => p.name)).toEqual([
      "notes/deck.html",
    ]);
  });

  it("hands the path to the pane when a file row is picked", () => {
    const s = suggestionsFor(placesFor([], [], FILES), "", ENGINE);
    expect(pickSuggestion(s, 0)).toEqual({
      url: "https://files.veld.localhost/g/notes/deck.html",
      title: "notes/deck.html",
      path: "notes/deck.html",
    });
  });
});

describe("what a full-size screen offers unprompted", () => {
  const at = (mtimeMs: number, name = `f-${mtimeMs}.html`) => ({
    name,
    url: `https://files.veld.localhost/g/${name}`,
    mtimeMs,
  });
  const NOW = 1_000_000_000_000;
  const hoursAgo = (h: number) => NOW - h * 60 * 60 * 1000;

  it("offers nothing while the run is serving URLs of its own", () => {
    const files = [at(NOW), at(hoursAgo(1))];
    expect(inlineFiles(files, { hasRunUrls: true, now: NOW })).toEqual([]);
    // …and the same list is offered when there is no run.
    expect(inlineFiles(files, { hasRunUrls: false, now: NOW })).toHaveLength(2);
  });

  it("offers at most three", () => {
    const files = [at(NOW), at(hoursAgo(1)), at(hoursAgo(2)), at(hoursAgo(3))];
    const shown = inlineFiles(files, { hasRunUrls: false, now: NOW });
    expect(shown).toHaveLength(3);
    // The newest three, in the order they arrived — never re-sorted here.
    expect(shown.map((f) => f.mtimeMs)).toEqual([NOW, hoursAgo(1), hoursAgo(2)]);
  });

  it("drops anything older than a day", () => {
    const files = [at(hoursAgo(23)), at(hoursAgo(25))];
    expect(
      inlineFiles(files, { hasRunUrls: false, now: NOW }).map((f) => f.mtimeMs),
    ).toEqual([hoursAgo(23)]);
    // Exactly a day old still counts — the boundary is inclusive, so a file does
    // not vanish from the list while somebody is looking at it.
    expect(
      inlineFiles([at(NOW - INLINE_FILES_MAX_AGE_MS)], {
        hasRunUrls: false,
        now: NOW,
      }),
    ).toHaveLength(1);
  });

  it("caps by count after filtering by age, not before", () => {
    // Four fresh files behind one stale one: a cap applied first would have
    // returned two fresh rows and silently dropped the third.
    const files = [at(hoursAgo(30)), at(NOW), at(hoursAgo(1)), at(hoursAgo(2))];
    expect(inlineFiles(files, { hasRunUrls: false, now: NOW })).toHaveLength(3);
  });
});

describe("how a file row reads", () => {
  it("classifies the kinds that get their own glyph", () => {
    expect(fileKindOf("deck.html")).toBe("html");
    expect(fileKindOf("a/b/index.HTM")).toBe("html");
    expect(fileKindOf("slides.pdf")).toBe("pdf");
    expect(fileKindOf("shot.PNG")).toBe("image");
    expect(fileKindOf("logo.svg")).toBe("image");
    expect(fileKindOf("notes.md")).toBe("text");
    expect(fileKindOf("data.json")).toBe("text");
    // Total for anything unlisted — a pattern-matched file still gets a row.
    expect(fileKindOf("chart.mmd")).toBe("other");
    expect(fileKindOf("Makefile")).toBe("other");
    // A dotfile's leading dot is not an extension.
    expect(fileKindOf(".gitignore")).toBe("other");
    expect(fileKindOf("a/.env")).toBe("other");
  });

  it("separates the folder from the file name", () => {
    expect(fileDir("notes/deck/index.html")).toBe("notes/deck");
    // Null, not "", so the caller decides what the root reads as rather than
    // rendering a blank line that looks like missing data.
    expect(fileDir("deck.html")).toBeNull();
    expect(fileDir("/leading.html")).toBeNull();
  });

  it("rounds an age down, so it never overstates how old a file is", () => {
    const now = 1_000_000_000_000;
    const ago = (ms: number) => timeAgo(now - ms, now);
    expect(ago(0)).toBe("just now");
    expect(ago(59_000)).toBe("just now");
    expect(ago(60_000)).toBe("1 min ago");
    // 119s is one minute and 59 seconds: "1 min", never "2 min".
    expect(ago(119_000)).toBe("1 min ago");
    expect(ago(60 * 60_000)).toBe("1 h ago");
    expect(ago(23.9 * 60 * 60_000)).toBe("23 h ago");
    expect(ago(24 * 60 * 60_000)).toBe("1 d ago");
    expect(ago(7 * 24 * 60 * 60_000)).toBe("1 w ago");
    expect(ago(60 * 24 * 60 * 60_000)).toBe("2 mo ago");
    expect(ago(400 * 24 * 60 * 60_000)).toBe("1 y ago");
    // A file stamped in the future (a bad clock, a copied mtime) must not read as
    // a negative age.
    expect(timeAgo(now + 60_000, now)).toBe("just now");
  });
});

describe("what a full-size screen actually renders", () => {
  // The exact composition `PaneChooser` and `BrowserPane`'s start page both use.
  // Worth pinning as one expression: the rule lives in `inlineFiles`, but the rows
  // that reach the screen go through `placesFor` and `suggestionsFor` after it, and
  // a cap applied in the wrong one of the three is invisible in a unit test of any
  // single one.
  const NOW = 1_000_000_000_000;
  const hoursAgo = (h: number) => NOW - h * 60 * 60 * 1000;
  const file = (name: string, h: number) => ({
    name,
    url: `https://files.veld.localhost/g/${name}`,
    mtimeMs: hoursAgo(h),
  });
  const rendered = (files: ReturnType<typeof file>[], hasRunUrls: boolean) =>
    suggestionsFor(
      placesFor(
        hasRunUrls ? URLS : [],
        LINKS,
        inlineFiles(files, { hasRunUrls, now: NOW }),
      ),
      "",
      ENGINE,
    ).places;

  it("renders nothing for a worktree whose files are all stale", () => {
    // The case that looked like a bug on screen: every file days old, so the
    // screen must offer none of them rather than the newest few.
    const stale = [file("a.html", 72), file("b.html", 24 * 7), file("c.html", 24 * 30)];
    expect(rendered(stale, false)).toEqual([]);
  });

  it("renders at most three file rows, and none once the run has URLs", () => {
    const fresh = [
      file("a.html", 0),
      file("b.html", 1),
      file("c.html", 2),
      file("d.html", 3),
    ];
    expect(rendered(fresh, false).map((p) => p.name)).toEqual([
      "a.html",
      "b.html",
      "c.html",
    ]);
    // With a run up, the rows are the run's URLs and no files at all.
    expect(rendered(fresh, true).map((p) => p.kind)).toEqual(["run", "run"]);
  });

  it("still finds a stale file once something is typed", () => {
    // The cap is on what is offered *unprompted*. Typing searches everything, which
    // is why the typed path uses the unfiltered list.
    const stale = [file("deck.html", 24 * 30)];
    const typed = suggestionsFor(placesFor([], LINKS, stale), "deck", ENGINE);
    expect(typed.places.map((p) => p.name)).toEqual(["deck.html"]);
  });
});
