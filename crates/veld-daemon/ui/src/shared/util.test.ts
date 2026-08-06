import { describe, expect, it } from "vitest";
import {
  extractMsg,
  extractTs,
  fmtBytes,
  fmtTs,
  fmtTsFull,
  fmtWhen,
  runKey,
  shortUrl,
  confirmedUnattached,
  statusBucket,
  unattachedShareIds,
} from "./util";

describe("unattachedShareIds", () => {
  const shares = [
    { id: "shr_live", run_id: "run-1" },
    { id: "shr_orphan", run_id: "run-gone" },
    { id: "shr_noid", run_id: null },
  ];

  it("keeps only shares whose run the dashboard doesn't know", () => {
    expect([...unattachedShareIds(shares, new Set(["run-1"]))]).toEqual([
      "shr_orphan",
      "shr_noid",
    ]);
  });

  it("yields nothing until the environment list has loaded", () => {
    // A slow first /api/environments must not flash every share as orphaned.
    expect(unattachedShareIds(shares, null)).toEqual(new Set());
  });

  it("treats every known run as attachment, whatever the view shows", () => {
    // The run set is all known runs, not the filtered view — otherwise
    // switching to History would orphan every live run's share.
    expect([
      ...unattachedShareIds(shares, new Set(["run-1", "run-gone"])),
    ]).toEqual(["shr_noid"]);
  });
});

describe("confirmedUnattached", () => {
  it("needs a share to look unattached on two consecutive polls", () => {
    // The runs and the shares come from two requests: a share minted between
    // them looks unattached for one poll, and an Unshare button next to a live
    // share is destructive.
    const poll1 = new Set(["shr_a", "shr_b"]);
    expect(confirmedUnattached(poll1, new Set())).toEqual(new Set());

    const poll2 = new Set(["shr_b", "shr_c"]);
    expect(confirmedUnattached(poll2, poll1)).toEqual(new Set(["shr_b"]));
  });

  it("drops a share that re-attached", () => {
    // Its run came back into the listing (a slow fetch resolved) — stop
    // offering to unshare it.
    expect(confirmedUnattached(new Set(), new Set(["shr_a"]))).toEqual(
      new Set(),
    );
  });
});

describe("fmtBytes", () => {
  it("picks units at binary boundaries", () => {
    expect(fmtBytes(512)).toBe("512 B");
    expect(fmtBytes(2048)).toBe("2 KB");
    expect(fmtBytes(3 * 1024 * 1024)).toBe("3.0 MB");
    expect(fmtBytes(2.5 * 1024 * 1024 * 1024)).toBe("2.50 GB");
  });
});

describe("timestamps", () => {
  it("extracts the server's leading [ISO] prefix", () => {
    expect(extractTs("[2026-07-27T10:00:00Z] hello")).toBe(
      "2026-07-27T10:00:00Z",
    );
    expect(extractTs("no prefix")).toBe("");
    expect(extractMsg("[2026-07-27T10:00:00Z] hello")).toBe("hello");
    expect(extractMsg("no prefix")).toBe("no prefix");
  });

  it("fmtTs handles invalid dates", () => {
    expect(fmtTs("garbage", "local")).toBe("");
    expect(fmtTs("2026-07-27T10:00:00Z", "local")).toMatch(
      /^\d{2}:\d{2}:\d{2}\.\d{3}$/,
    );
  });

  it("fmtTs renders UTC as stored and local through the browser's zone", () => {
    // The UTC arm is exact, because it must not depend on the runner's zone — CI is
    // UTC and a developer is not, and a test that passes only in one is not a test.
    expect(fmtTs("2026-07-27T10:00:00.123Z", "utc")).toBe("10:00:00.123");
    // The local arm is asserted as *shape plus agreement with the platform*, for the
    // same reason: `Date`'s own getters are the definition of "the browser's zone".
    const d = new Date("2026-07-27T10:00:00.123Z");
    expect(fmtTs("2026-07-27T10:00:00.123Z", "local")).toBe(
      `${String(d.getHours()).padStart(2, "0")}:00:00.123`,
    );
  });

  it("fmtTsFull carries the date, both zones, and a signed offset", () => {
    const utcFirst = fmtTsFull("2026-07-27T10:00:00.123Z", "utc");
    const [first, second, third] = utcFirst.split("\n");
    // The rendered zone leads; the counterpart follows. Three lines, always.
    expect(first).toBe("2026-07-27 10:00:00.123 (UTC)");
    expect(second).toMatch(/^\d{4}-\d{2}-\d{2} .* \(local, UTC[+-]\d{2}:\d{2}\)$/);
    expect(third).toBe("stored: 2026-07-27T10:00:00.123Z");
    // …and the order of the two rendered lines flips with the setting, so the tooltip
    // always expands the number that is actually on screen first. The stored line does
    // not move: it is the same value either way.
    const localFirst = fmtTsFull("2026-07-27T10:00:00.123Z", "local");
    expect(localFirst.split("\n")[0]).toBe(second);
    expect(localFirst.split("\n")[1]).toBe(first);
    expect(localFirst.split("\n")[2]).toBe(third);
    // The offset is signed even at zero, because it is interpolated after "UTC" and
    // a bare `Z` there would read as `UTCZ`.
    expect(localFirst).not.toContain("UTCZ");
  });

  it("fmtTsFull keeps the microseconds the rendered lines drop", () => {
    // `new Date` holds milliseconds, so both rendered lines round `.123456` to `.123`
    // and two rows 200µs apart would tooltip identically. The CLI keeps the precision,
    // so the stored line is what makes this agree with `veld logs --utc` and what a
    // reader can paste into a bug report.
    const full = fmtTsFull("2026-07-27T10:00:00.123456Z", "utc");
    expect(full).toContain("stored: 2026-07-27T10:00:00.123456Z");
    expect(full.split("\n")[0]).toBe("2026-07-27 10:00:00.123 (UTC)");
  });

  it("fmtTsFull passes an unparseable timestamp through", () => {
    // A log row's timestamp is evidence: a `ts` column holding something unexpected
    // is exactly the row a reader needs to see as-is, so the tooltip must not blank
    // it the way `fmtTs` blanks the dense on-screen form.
    expect(fmtTsFull("garbage", "local")).toBe("garbage");
    expect(fmtTsFull("garbage", "utc")).toBe("garbage");
  });

  it("fmtWhen buckets relative time", () => {
    expect(fmtWhen(null)).toBe("");
    expect(fmtWhen("garbage")).toBe("");
    expect(fmtWhen(new Date(Date.now() - 10_000).toISOString())).toBe(
      "just now",
    );
    expect(fmtWhen(new Date(Date.now() - 5 * 60_000).toISOString())).toBe(
      "5m ago",
    );
    expect(fmtWhen(new Date(Date.now() - 3 * 3_600_000).toISOString())).toBe(
      "3h ago",
    );
    expect(fmtWhen(new Date(Date.now() - 72 * 3_600_000).toISOString())).toBe(
      "3d ago",
    );
  });
});

describe("statusBucket", () => {
  it("maps every known status; unknown transitional → yellow", () => {
    expect(statusBucket("running")).toBe("green");
    expect(statusBucket("healthy")).toBe("green");
    expect(statusBucket("crashed")).toBe("red");
    expect(statusBucket("failed")).toBe("red");
    expect(statusBucket("stopped")).toBe("dim");
    expect(statusBucket("skipped")).toBe("dim");
    expect(statusBucket("starting")).toBe("yellow");
    expect(statusBucket("health_checking")).toBe("yellow");
    expect(statusBucket("anything-else")).toBe("yellow");
  });
});

describe("misc", () => {
  it("runKey compounds project root and run name", () => {
    expect(runKey("/a", "dev")).toBe("/a::dev");
  });
  it("shortUrl falls back on invalid URLs", () => {
    expect(shortUrl("https://frontend.dev.proj.localhost/x")).toBe(
      "frontend.dev.proj.localhost",
    );
    expect(shortUrl("not a url")).toBe("not a url");
  });
});
