import { describe, expect, it } from "vitest";
import {
  extractMsg,
  extractTs,
  fmtBytes,
  fmtTs,
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
    expect(fmtTs("garbage")).toBe("");
    expect(fmtTs("2026-07-27T10:00:00Z")).toMatch(/^\d{2}:\d{2}:\d{2}\.\d{3}$/);
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
