import { describe, expect, it } from "vitest";

import {
  MAX_DERIVED_LEN,
  MAX_DISPLAY_NAME_LEN,
  aliasCollides,
  deriveAlias,
  deriveBranch,
  deriveDisplayName,
  worktreeLabel,
} from "./worktreeName";

describe("deriveDisplayName", () => {
  it("keeps everything the alias throws away", () => {
    // The whole reason `display_name` exists beside the alias: the alias of this
    // is `checkout-v2-final`, and that used to be the only name the rail had.
    expect(deriveDisplayName("Checkout V2 (final)")).toBe("Checkout V2 (final)");
    expect(deriveDisplayName("Hello test")).toBe("Hello test");
    expect(deriveDisplayName("Zahlungsübersicht")).toBe("Zahlungsübersicht");
  });

  it("normalises whitespace and nothing else", () => {
    expect(deriveDisplayName("  Hello   test  ")).toBe("Hello test");
    expect(deriveDisplayName("a\tb")).toBe("a b");
    // Punctuation and case are content, not noise.
    expect(deriveDisplayName("A/B_c.d")).toBe("A/B_c.d");
  });

  it("caps at the daemon's bound without leaving a trailing space", () => {
    const long = "word ".repeat(40);
    const capped = deriveDisplayName(long);
    expect(capped.length).toBeLessThanOrEqual(MAX_DISPLAY_NAME_LEN);
    // `slice` can land mid-gap; a name ending in a space renders as one that
    // ends early and compares unequal to the same name retyped.
    expect(capped).toBe(capped.trim());
  });

  it("caps by code point, so a surrogate pair is never cut in half", () => {
    // `String.prototype.slice` cuts UTF-16 code units. A cap landing inside a
    // pair leaves a lone high surrogate, `JSON.stringify` emits it as `\ud83d`,
    // and serde_json rejects the entire request body — so the failure surfaced
    // as an unparseable payload rather than as "that name is too long", with
    // nothing created. The daemon counts characters, so this also makes the
    // client's courtesy bound mean the same thing as the real one.
    const capped = deriveDisplayName("😀".repeat(MAX_DISPLAY_NAME_LEN + 20));
    expect([...capped]).toHaveLength(MAX_DISPLAY_NAME_LEN);
    expect(JSON.stringify(capped)).not.toMatch(/\\u[dD][89abAB]/);
    // Round-trips, which a lone surrogate does not.
    expect(JSON.parse(JSON.stringify(capped))).toBe(capped);

    // The boundary case: one leading ASCII char makes the cap land exactly
    // between the two halves of an emoji.
    const straddle = deriveDisplayName("a" + "😀".repeat(MAX_DISPLAY_NAME_LEN));
    expect(JSON.parse(JSON.stringify(straddle))).toBe(straddle);
  });

  it("collapses a whitespace-only name to nothing", () => {
    // `""` is the "no separate name" sentinel, so this is how typing spaces into
    // the rename dialog gets you back to the alias rather than to a blank row.
    expect(deriveDisplayName("   ")).toBe("");
    expect(deriveDisplayName("")).toBe("");
  });
});

describe("worktreeLabel", () => {
  it("prefers the display name and falls back to the alias", () => {
    expect(worktreeLabel({ alias: "hello-test", display_name: "Hello test" })).toBe(
      "Hello test",
    );
    // Every row created before the column existed is in this state.
    expect(worktreeLabel({ alias: "hello-test", display_name: "" })).toBe(
      "hello-test",
    );
  });

  it("falls back when the field is absent, not just empty", () => {
    // A UI build talking to a daemon that predates v13 gets no key at all, and
    // `w.display_name || w.alias` written inline at a call site would be fine
    // here but `w.display_name` alone would render nothing. Pinned so the
    // fallback cannot be narrowed to `""` later.
    expect(worktreeLabel({ alias: "hello-test" })).toBe("hello-test");
  });
});

describe("deriveAlias", () => {
  it("slugs a typed label into an identifier the daemon accepts", () => {
    // The example from the batch's own description. `validate_alias` rejects spaces
    // and parentheses, so this is the lossy step the dialog has to show.
    expect(deriveAlias("Checkout V2 (final)")).toBe("checkout-v2-final");
    expect(deriveAlias("checkout-v2-final")).toBe("checkout-v2-final");
  });

  it("leaves an already-safe name alone apart from case", () => {
    expect(deriveAlias("auth-retry")).toBe("auth-retry");
    expect(deriveAlias("AuthRetry")).toBe("authretry");
  });

  it("flattens a slash, unlike the branch derivation", () => {
    // An alias is one segment: it becomes a run name and a hostname label.
    expect(deriveAlias("feat/checkout")).toBe("feat-checkout");
  });

  it("returns empty when nothing usable survives", () => {
    // Not "wt": that fallback belongs to a branch discovered on disk, which has to
    // get some row. A person typing punctuation into a name field has chosen nothing,
    // and the dialog keeps its button disabled.
    expect(deriveAlias("///")).toBe("");
    expect(deriveAlias("   ")).toBe("");
    expect(deriveAlias("")).toBe("");
  });

  it("caps at the slug length and never ends in a dash", () => {
    const long = deriveAlias(`${"a".repeat(47)} tail`);
    expect(long.length).toBeLessThanOrEqual(MAX_DERIVED_LEN);
    expect(long.endsWith("-")).toBe(false);
    // Cutting at 48 would have landed on the dash before "tail".
    expect(long).toBe("a".repeat(47));
  });

  it("transliterates German umlauts to their established spellings", () => {
    // `ae oe ue ss` is what a German speaker writes when they have to reach ASCII —
    // and what NFD decomposition would get wrong, since `ü` is not "u with a
    // decoration" in German.
    expect(deriveAlias("Zahlungsübersicht")).toBe("zahlungsuebersicht");
    expect(deriveAlias("Größe Ändern")).toBe("groesse-aendern");
    expect(deriveAlias("Maß")).toBe("mass");
  });

  it("drops accents elsewhere in the Latin script", () => {
    expect(deriveAlias("café")).toBe("cafe");
    expect(deriveAlias("naïve branch")).toBe("naive-branch");
    expect(deriveAlias("Crème Brûlée")).toBe("creme-brulee");
    expect(deriveAlias("Señor Ñandú")).toBe("senor-nandu");
    expect(deriveAlias("Þór Blåbær")).toBe("thor-blaabaer");
  });

  it("handles a decomposed umlaut, which is how macOS types it", () => {
    // "Gro\u0308\u00dfe" — o plus a combining diaeresis, not a precomposed \u00f6. Without the
    // NFC step this hits the generic accent-strip and comes out "grosse".
    expect(deriveAlias("Gro\u0308\u00dfe")).toBe("groesse");
    expect(deriveAlias("cafe\u0301")).toBe("cafe");
  });

  it("still yields nothing for a script it has no table for", () => {
    // Stated scope, not an oversight: transliterating Cyrillic or Greek needs
    // per-language tables. The dialog reports "nothing usable" and the user types a
    // name, which is better than a wrong romanisation.
    expect(deriveAlias("Проверка")).toBe("");
    expect(deriveAlias("δοκιμή")).toBe("");
  });
});

describe("deriveBranch", () => {
  it("transliterates inside each segment", () => {
    expect(deriveBranch("feat/Größe ändern")).toBe("feat/groesse-aendern");
  });

  it("keeps slashes as segment separators", () => {
    expect(deriveBranch("feat/Checkout V2")).toBe("feat/checkout-v2");
    expect(deriveBranch("Checkout V2")).toBe("checkout-v2");
  });

  it("drops empty segments rather than emitting an illegal ref", () => {
    // git refuses `a//b`, `/a` and `a/`.
    expect(deriveBranch("feat//checkout")).toBe("feat/checkout");
    expect(deriveBranch("/feat/checkout/")).toBe("feat/checkout");
  });

  it("never ends in a slash or dash after the length cap", () => {
    const long = deriveBranch(`feat/${"a".repeat(60)}`);
    expect(long.length).toBeLessThanOrEqual(MAX_DERIVED_LEN);
    expect(/[-/]$/.test(long)).toBe(false);
  });

  it("returns empty for input with nothing to slug", () => {
    expect(deriveBranch("///")).toBe("");
  });
});

describe("aliasCollides", () => {
  it("compares as slugs, not as strings", () => {
    // `main-2` and `main_2` resolve to one hostname, so they are one name.
    expect(aliasCollides("main_2", ["main-2"])).toBe(true);
    expect(aliasCollides("Main", ["main"])).toBe(true);
    expect(aliasCollides("other", ["main"])).toBe(false);
  });

  it("compares stored aliases without transliterating them", () => {
    // The daemon decides collisions with `slugify`, which has no table: to it `café`
    // is `caf`. Transliterating the stored side would disagree in both directions —
    // warning about a collision that will not happen, and missing one that will.
    expect(aliasCollides(deriveAlias("café"), ["café"])).toBe(false);
    expect(aliasCollides(deriveAlias("café"), ["cafe"])).toBe(true);
    expect(aliasCollides("caf", ["café"])).toBe(true);
  });

  it("never reports a collision for an empty derivation", () => {
    // Empty means "nothing to create"; reporting a collision on top of that would
    // put a confusing error under an empty field.
    expect(aliasCollides("", ["", "main"])).toBe(false);
  });
});
