import { describe, expect, it } from "vitest";

import type { WorktreeLink } from "../api";
import {
  ageText,
  arrangeLinks,
  groupLabel,
  isStale,
  linkGlyph,
  linkTooltip,
  normalizeLink,
  overflowCount,
 sameLink, withoutBadged,
  worstTone,
} from "./linkModel";

let n = 0;
const link = (over: Partial<WorktreeLink> = {}): WorktreeLink => ({
  id: `l${++n}`,
  kind: "link",
  url: "https://example.com/x",
  label: `link ${n}`,
  icon: null,
  tone: "neutral",
  tooltip: "",
  branch: "",
  updated_at: "2026-01-01T00:00:00Z",
  ...over,
});

const kinds = (ls: WorktreeLink[]) => arrangeLinks(ls).map((s) => s.type);

describe("normalizeLink", () => {
  it("falls back for an unknown kind, tone and opener", () => {
    const l = normalizeLink(link({ kind: "ticket", tone: "purple" }));
    expect([l.kind, l.tone]).toEqual(["link", "neutral"]);
  });

  it("keeps known values", () => {
    const l = normalizeLink(link({ kind: "pr", tone: "danger" }));
    expect([l.kind, l.tone]).toEqual(["pr", "danger"]);
  });

  it("resolves the glyph from the kind unless the link brings its own", () => {
    expect(linkGlyph(normalizeLink(link({ kind: "preview" })))).toEqual({
      kind: "name",
      value: "rocket",
    });
    const own = { kind: "emoji" as const, value: "x" };
    expect(linkGlyph(normalizeLink(link({ icon: own })))).toBe(own);
  });
});

describe("isStale", () => {
  it("is stale only when a recorded branch differs", () => {
    expect(isStale({ branch: "a" }, "b")).toBe(true);
    expect(isStale({ branch: "a" }, "a")).toBe(false);
    expect(isStale({ branch: "" }, "b")).toBe(false);
  });
});

describe("worstTone", () => {
  it("ranks danger > warning > info > success > neutral", () => {
    expect(worstTone([])).toBe("neutral");
    expect(worstTone(["neutral", "success"])).toBe("success");
    expect(worstTone(["success", "info"])).toBe("info");
    expect(worstTone(["info", "warning", "success"])).toBe("warning");
    expect(worstTone(["warning", "danger", "info"])).toBe("danger");
  });
});

describe("arrangeLinks", () => {
  it("is empty for no links", () => {
    expect(arrangeLinks([])).toEqual([]);
  });

  it("makes a single link a pill", () => {
    expect(kinds([link({ kind: "pr" })])).toEqual(["single"]);
  });

  it("groups two or more of a kind, with the worst tone", () => {
    const [s] = arrangeLinks([
      link({ kind: "pr", tone: "success" }),
      link({ kind: "pr", tone: "danger" }),
      link({ kind: "pr", tone: "info" }),
    ]);
    expect(s.type).toBe("group");
    if (s.type !== "group") return;
    expect(s.tone).toBe("danger");
    expect(s.links).toHaveLength(3);
    expect(groupLabel(s.kind, s.links.length)).toBe("3 PRs");
  });

  it("keeps incoming order within a kind (multi-repo PRs)", () => {
    const a = link({ kind: "pr", label: "api#1" });
    const b = link({ kind: "pr", label: "web#2" });
    const [s] = arrangeLinks([a, b]);
    if (s.type !== "group") throw new Error("expected a group");
    expect(s.links.map((l) => l.label)).toEqual(["api#1", "web#2"]);
  });

  it("orders kinds pr, preview, issue, doc, link regardless of input order", () => {
    const slots = arrangeLinks([
      link({ kind: "doc" }),
      link({ kind: "preview" }),
      link({ kind: "pr" }),
    ]);
    expect(
      slots.map((s) => (s.type === "single" ? s.link.kind : s.type === "group" ? s.kind : "")),
    ).toEqual(["pr", "preview", "doc"]);
  });

  it("does not depend on the total count: six links of one kind is one slot", () => {
    expect(kinds(Array.from({ length: 6 }, () => link({ kind: "pr" })))).toEqual(["group"]);
  });

  it("shows three kinds without overflow", () => {
    expect(
      kinds([link({ kind: "pr" }), link({ kind: "preview" }), link({ kind: "issue" })]),
    ).toEqual(["single", "single", "single"]);
  });

  it("folds everything past the first two slots into one overflow", () => {
    const slots = arrangeLinks([
      link({ kind: "pr" }),
      link({ kind: "preview" }),
      link({ kind: "issue" }),
      link({ kind: "doc" }),
      link({ kind: "link" }),
      link({ kind: "link" }),
    ]);
    expect(slots.map((s) => s.type)).toEqual(["single", "single", "overflow"]);
    const o = slots[2];
    if (o.type !== "overflow") throw new Error("expected overflow");
    expect(o.groups.map((g) => [g.kind, g.links.length])).toEqual([
      ["issue", 1],
      ["doc", 1],
      ["link", 2],
    ]);
    expect(overflowCount(o.groups)).toBe(4);
  });

  it("treats unknown kinds as plain links", () => {
    const [s] = arrangeLinks([link({ kind: "ticket" }), link({ kind: "other" })]);
    expect(s.type).toBe("group");
    if (s.type === "group") expect(s.kind).toBe("link");
  });
});

describe("ageText", () => {
  const now = Date.parse("2026-01-01T12:00:00Z");
  it("reads in the coarsest unit", () => {
    expect(ageText("2026-01-01T11:59:40Z", now)).toBe("just now");
    expect(ageText("2026-01-01T11:55:00Z", now)).toBe("set 5m ago");
    expect(ageText("2026-01-01T09:00:00Z", now)).toBe("set 3h ago");
    expect(ageText("2025-12-30T12:00:00Z", now)).toBe("set 2d ago");
  });
  it("is empty for an unparseable timestamp", () => {
    expect(ageText("soon", now)).toBe("");
  });
});

describe("linkTooltip", () => {
  const now = Date.parse("2026-01-01T03:00:00Z");
  it("names the host and the age, and the branch when stale", () => {
    const l = normalizeLink(
      link({ url: "https://github.com/o/r/pull/1", tooltip: "Checks failing", branch: "old" }),
    );
    expect(linkTooltip(l, "old", now)).toBe(
      "Checks failing\ngithub.com\nset by agent 3h ago",
    );
    expect(linkTooltip(l, "new", now)).toBe(
      "Checks failing\ngithub.com\nset by agent 3h ago on branch old",
    );
  });
});

describe("withoutBadged", () => {
  it("drops a link the project's badge already links to, and keeps the rest", () => {
    const pr = link({ url: "https://github.com/o/r/pull/7" });
    const other = link({ url: "https://github.com/o/other/pull/2" });
    const kept = withoutBadged([pr, other], ["https://GitHub.com/o/r/pull/7/?x=1#c"]);
    expect(kept).toEqual([other]);
    expect(withoutBadged([pr, other], [])).toHaveLength(2);
  });

  it("compares URLs the way the daemon files them", () => {
    expect(sameLink("https://A.dev/x/", "https://a.dev/x#y")).toBe(true);
    expect(sameLink("https://a.dev/x", "https://a.dev/X")).toBe(false);
  });
});
