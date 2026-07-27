import { describe, expect, it } from "vitest";
import {
  extractMsg,
  extractTs,
  fmtBytes,
  fmtTs,
  fmtWhen,
  runKey,
  shortUrl,
  statusBucket,
} from "./util";

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
