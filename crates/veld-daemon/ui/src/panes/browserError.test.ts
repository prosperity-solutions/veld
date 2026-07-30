import { describe, expect, it } from "vitest";
import { describeBrowserError } from "./browserError";
import type { BrowserError } from "./browserHost";

const load = (code: number | null, text = "failed"): BrowserError => ({
  kind: "load",
  code,
  text,
  url: "https://web.dev.veld.localhost/",
});

describe("describeBrowserError", () => {
  it("separates a dead dev server from a hostname that doesn't resolve", () => {
    // The whole point of classifying: one means "start the run", the other means
    // "veld doctor". A single generic screen makes the user guess.
    expect(describeBrowserError(load(-102)).kind).toBe("unreachable");
    expect(describeBrowserError(load(-101)).kind).toBe("unreachable");
    expect(describeBrowserError(load(-324)).kind).toBe("unreachable");
    expect(describeBrowserError(load(-109)).kind).toBe("unreachable");
    expect(describeBrowserError(load(-105)).kind).toBe("dns");
    expect(describeBrowserError(load(-106)).kind).toBe("dns");
  });

  it("treats every certificate error as the trust-store problem it is", () => {
    // The -2xx block is all certificate validation, and for a veld URL the answer
    // is always the same: Caddy's local CA is not trusted.
    for (const code of [-200, -201, -202, -299]) {
      expect(describeBrowserError(load(code)).kind).toBe("cert");
    }
    expect(describeBrowserError({ ...load(null), kind: "cert" }).kind).toBe("cert");
    // Just outside the block, so it must not be swallowed by the range test.
    expect(describeBrowserError(load(-199)).kind).not.toBe("cert");
    expect(describeBrowserError(load(-300)).kind).not.toBe("cert");
  });

  it("names timeouts and crashes as themselves", () => {
    expect(describeBrowserError(load(-118)).kind).toBe("timeout");
    expect(describeBrowserError(load(-7)).kind).toBe("timeout");
    expect(describeBrowserError({ ...load(null), kind: "crash", text: "oom" }).kind).toBe(
      "crash",
    );
  });

  it("falls back with the code, since that is the searchable part", () => {
    const copy = describeBrowserError(load(-355, "ERR_WEIRD"));
    expect(copy.kind).toBe("generic");
    expect(copy.hint).toContain("ERR_WEIRD");
    expect(copy.hint).toContain("-355");
    // A locally-raised error has no code and must not read "(null)".
    expect(describeBrowserError(load(null, "Not an http(s) address: nope")).hint).toBe(
      "Not an http(s) address: nope",
    );
  });

  it("always has something to say", () => {
    for (const code of [null, -1, -7, -102, -105, -118, -200, -324, -999]) {
      const copy = describeBrowserError(load(code));
      expect(copy.title.length).toBeGreaterThan(0);
      expect(copy.hint.length).toBeGreaterThan(0);
    }
  });
});
