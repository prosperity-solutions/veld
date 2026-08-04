import { describe, expect, it } from "vitest";

import {
  BUNDLED_FONTS,
  SYSTEM_FONTS,
  availableFonts,
  firstFamily,
  fontAvailable,
  matchFont,
} from "./terminalFonts";

describe("font lists", () => {
  it("every stack ends in a monospace fallback", () => {
    // A stack whose first choice is missing must still render monospaced; falling
    // through to the browser default gives a proportional terminal, which reads as
    // a broken app rather than as a missing font.
    for (const f of [...BUNDLED_FONTS, ...SYSTEM_FONTS]) {
      expect(f.stack).toMatch(/ui-monospace, monospace$/);
    }
  });

  it("the first bundled font is the default the daemon ships", () => {
    // `terminalPrefs`'s fallback and the Rust default are both JetBrains Mono, so
    // the picker must be able to represent that value as an option rather than
    // showing "Custom…" on a fresh install.
    expect(BUNDLED_FONTS[0].stack).toContain("JetBrains Mono");
  });

  it("bundled and system fonts are distinguishable", () => {
    expect(BUNDLED_FONTS.every((f) => f.bundled)).toBe(true);
    expect(SYSTEM_FONTS.every((f) => !f.bundled)).toBe(true);
  });
});

describe("firstFamily", () => {
  it("takes the family an availability check should ask about", () => {
    expect(firstFamily('"Fira Code Variable", "Fira Code", monospace')).toBe(
      '"Fira Code Variable"',
    );
    expect(firstFamily("Menlo, monospace")).toBe("Menlo");
    expect(firstFamily("Menlo")).toBe("Menlo");
  });
});

describe("fontAvailable", () => {
  it("assumes present when the API is missing", () => {
    // vitest runs with environment: "node", so there is no `document` here — which
    // is exactly the branch that must not hide a font the user may well have.
    expect(fontAvailable("Menlo")).toBe(true);
  });
});

describe("availableFonts", () => {
  it("always offers every bundled font", () => {
    // Bundled fonts are in the binary, so a `check` returning false would mean the
    // stylesheet has not loaded yet — filtering on it would make the list flicker.
    const offered = availableFonts();
    for (const f of BUNDLED_FONTS) {
      expect(offered).toContain(f);
    }
  });
});

describe("matchFont", () => {
  const opts = BUNDLED_FONTS;

  it("matches on the stored stack, not the label", () => {
    expect(matchFont(BUNDLED_FONTS[1].stack, opts)?.label).toBe("Fira Code");
    expect(BUNDLED_FONTS).toHaveLength(2);
  });

  it("tolerates whitespace but not a different fallback", () => {
    expect(matchFont(BUNDLED_FONTS[0].stack.replace(/, /g, ",  "), opts)).not.toBeNull();
    // A stack that differs in its fallback is a different value; treating it as the
    // same would show a font the terminal is not actually using.
    expect(matchFont('"JetBrains Mono Variable", monospace', opts)).toBeNull();
  });

  it("returns null for a custom value", () => {
    expect(matchFont("Comic Mono, monospace", opts)).toBeNull();
  });
});
