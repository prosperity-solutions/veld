import { describe, it, expect } from "vitest";
import {
  findPublicUrl,
  toPublicLocation,
} from "../src/feedback-overlay/public-url";

const list = {
  shares: [
    { public_urls: [] },
    {
      web_password: "k7dm-q2xp-9fzt",
      public_urls: [
        {
          node: "app",
          hostname: "app.demo.p.localhost",
          public_url: "https://abc123.share.example",
          access: "password",
        },
        {
          node: "api",
          hostname: "api.demo.p.localhost",
          public_url: "https://xyz789.share.example",
          access: "link",
        },
      ],
    },
  ],
};

describe("findPublicUrl", () => {
  it("matches the current hostname across shares", () => {
    expect(findPublicUrl(list, "app.demo.p.localhost")).toEqual({
      publicUrl: "https://abc123.share.example",
      password: "k7dm-q2xp-9fzt",
    });
    // A link-access node carries no password even when the share has one.
    expect(findPublicUrl(list, "api.demo.p.localhost")).toEqual({
      publicUrl: "https://xyz789.share.example",
      password: null,
    });
  });

  it("treats a pre-access-layer daemon (no access field) as password-gated when the share has a password", () => {
    const legacyish = {
      shares: [
        {
          web_password: "pw",
          public_urls: [
            {
              node: "app",
              hostname: "app.demo.p.localhost",
              public_url: "https://abc123.share.example",
            },
          ],
        },
      ],
    };
    expect(findPublicUrl(legacyish, "app.demo.p.localhost")).toEqual({
      publicUrl: "https://abc123.share.example",
      password: "pw",
    });
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

  it("appends the password as a veld-key fragment (one-link)", () => {
    expect(
      toPublicLocation(
        "https://abc123.share.example",
        { pathname: "/deep", search: "?q=1", hash: "" },
        "k7dm-q2xp-9fzt",
      ),
    ).toBe("https://abc123.share.example/deep?q=1#veld-key=k7dm-q2xp-9fzt");
  });

  it("joins with & when the page already has a hash, and URL-encodes the key", () => {
    expect(
      toPublicLocation(
        "https://abc123.share.example",
        { pathname: "/", search: "", hash: "#row-42" },
        "p&ss wörd",
      ),
    ).toBe(
      "https://abc123.share.example/#row-42&veld-key=p%26ss%20w%C3%B6rd",
    );
  });
});
