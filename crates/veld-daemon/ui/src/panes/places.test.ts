import { describe, expect, it } from "vitest";
import {
  filterPlaces,
  pickSuggestion,
  placesFor,
  stepIndex,
  suggestionsFor,
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
    expect(s.count).toBe(4);
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
    expect(pickSuggestion(s, 3)?.title).toBe("Staging");
  });

  it("returns null for an index that selects nothing", () => {
    const s = suggestionsFor(places, "", ENGINE);
    expect(pickSuggestion(s, -1)).toBeNull();
    expect(pickSuggestion(s, 4)).toBeNull();
    expect(pickSuggestion(suggestionsFor([], "", ENGINE), 0)).toBeNull();
  });
});
