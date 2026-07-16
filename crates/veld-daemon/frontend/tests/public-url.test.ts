import { describe, it, expect } from "vitest";
import { findPublicUrl, toPublicLocation } from "../src/feedback-overlay/public-url";

const list = {
  shares: [
    { public_urls: [] },
    {
      public_urls: [
        {
          node: "app",
          hostname: "app.demo.p.localhost",
          public_url: "https://abc123.share.example",
        },
        {
          node: "api",
          hostname: "api.demo.p.localhost",
          public_url: "https://xyz789.share.example",
        },
      ],
    },
  ],
};

describe("findPublicUrl", () => {
  it("matches the current hostname across shares", () => {
    expect(findPublicUrl(list, "app.demo.p.localhost")).toBe(
      "https://abc123.share.example",
    );
    expect(findPublicUrl(list, "api.demo.p.localhost")).toBe(
      "https://xyz789.share.example",
    );
  });

  it("returns null when nothing is web-shared for this host", () => {
    expect(findPublicUrl(list, "other.demo.p.localhost")).toBeNull();
    expect(findPublicUrl({}, "app.demo.p.localhost")).toBeNull();
    expect(findPublicUrl({ shares: [] }, "app.demo.p.localhost")).toBeNull();
  });
});

describe("toPublicLocation", () => {
  it("keeps path, query, and hash — a deep link survives the copy", () => {
    expect(
      toPublicLocation("https://abc123.share.example", {
        pathname: "/settings/billing",
        search: "?tab=invoices",
        hash: "#row-42",
      }),
    ).toBe("https://abc123.share.example/settings/billing?tab=invoices#row-42");
  });

  it("root location maps to the bare public URL plus slash", () => {
    expect(
      toPublicLocation("https://abc123.share.example", {
        pathname: "/",
        search: "",
        hash: "",
      }),
    ).toBe("https://abc123.share.example/");
  });
});
