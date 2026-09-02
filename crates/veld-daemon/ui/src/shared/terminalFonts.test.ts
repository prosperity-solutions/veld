import { describe, expect, it } from "vitest";

import {
  BUNDLED_FONTS,
  SYSTEM_FONTS,
  availableFonts,
  firstFamily,
  fontAvailable,
  fontHasLigatures,
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

describe("fontHasLigatures", () => {
  it("answers from the flag for a font we offer", () => {
    // Both bundled fonts, and exactly one system font, are the ligature-capable
    // ones. Asserted through the resolver rather than by reading the flag, so the
    // stack-matching is covered too.
    expect(fontHasLigatures(BUNDLED_FONTS[0].stack, BUNDLED_FONTS)).toBe(true);
    expect(fontHasLigatures("Menlo, ui-monospace, monospace", SYSTEM_FONTS)).toBe(
      false,
    );
    expect(
      fontHasLigatures('"Cascadia Code", ui-monospace, monospace', SYSTEM_FONTS),
    ).toBe(true);
  });

  it("answers null — never false — for a family it cannot classify", () => {
    // The load-bearing case. Nothing in a browser can read a font's OpenType
    // tables, and these ligatures are width-preserving so measuring cannot
    // substitute. `null` is what keeps the dialog showing the control for
    // Iosevka, Monaspace, a Nerd-Font patch — all of which do have ligatures.
    expect(fontHasLigatures("Iosevka, monospace", SYSTEM_FONTS)).toBeNull();
    expect(fontHasLigatures("", SYSTEM_FONTS)).toBeNull();
  });

  it("matches the first family, so an added fallback still counts", () => {
    // A user who appends a fallback to a listed font has still chosen that font;
    // `matchFont` would call the stack custom, which would lose the answer.
    expect(matchFont('"Fira Code Variable", Menlo', BUNDLED_FONTS)).toBeNull();
    expect(fontHasLigatures('"Fira Code Variable", Menlo', BUNDLED_FONTS)).toBe(
      true,
    );
  });

  it("ignores quoting and case in the family name", () => {
    expect(fontHasLigatures("menlo", SYSTEM_FONTS)).toBe(false);
    expect(fontHasLigatures("'Cascadia Code'", SYSTEM_FONTS)).toBe(true);
  });
});
