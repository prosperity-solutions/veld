/**
 * The terminal font picker's choices.
 *
 * Two kinds, and the distinction is the point:
 *
 * - **Bundled** fonts ship inside the daemon binary. `vite-plugin-singlefile`
 *   base64-inlines every asset into one `index.html`, so each one is a permanent
 *   addition to the binary — the latin subset of a variable mono font is ~40 KB
 *   woff2, ~55 KB once base64'd, against a ~1.6 MB bundle. That is why the list is
 *   two fonts and not thirty: they are guaranteed to render identically on every
 *   machine, and they cost bytes forever.
 * - **System** fonts cost nothing and are guaranteed nowhere. They are offered
 *   because a developer who has already chosen a terminal font usually has it
 *   installed, and refusing to list it would send them to the custom field for no
 *   reason. Availability is *checked* rather than assumed, so the picker can say
 *   which ones are actually present instead of silently falling back.
 *
 * Anything not on either list stays reachable through the custom field — the stored
 * value is a CSS font-family list, and the daemon validates it (bounded length, and
 * no `{ } ; < >`, because xterm interpolates it into a CSS rule).
 *
 * Pure and DOM-optional: `fontAvailable` is the only function that touches
 * `document`, and it degrades to "assume present" where the API is missing, so the
 * rest is testable under `environment: "node"`.
 */

export interface TerminalFont {
  /** What the picker shows. */
  label: string;
  /** The CSS font-family list stored in the setting. */
  stack: string;
  /** Bundled in the binary (always available) vs relying on the OS. */
  bundled: boolean;
  /**
   * Whether the font as published draws programming ligatures and contextual
   * alternates (`calt`) — `=>`, `!=`, `===`.
   *
   * A **static claim about the published font**, and it has to be: nothing in the
   * browser can read a font's OpenType tables. Measuring is not a way around it
   * either — for the sequences this setting is about, the substitution is
   * glyph-for-glyph, so it is width-preserving and the one quantity the DOM
   * exposes is blind to it (measured: `=> != === ->` is 460.8047px in JetBrains
   * Mono and 462.375px in Menlo, each identical with the features on and off).
   *
   * That width-preservation is **verified for the two bundled faces, not assumed
   * of every font this flag is `true` for**. In both bundled faces `calt` reaches
   * `SingleSubst` lookups only. Cascadia Code is the counterexample worth knowing:
   * its `calt` also reaches a `MultipleSubst` and a `LigatureSubst` — but both are
   * Arabic-script only (a lam-lam-heh ligature collapsing four cells into one, and
   * a connector splitting one into two), and neither is reachable from ASCII. So
   * the grid holds for the programming sequences this setting exists for, and a
   * terminal rendering Arabic *in Cascadia Code with the switch on* is the one
   * combination where it may not. Off is the default, and the off half pins `calt`
   * to 0, so nothing here changes unless somebody asks for it.
   *
   * So the honest scope of this flag is the fonts *we* offer. A family the user
   * typed themselves is not classified at all — see {@link fontHasLigatures},
   * which answers `null` there rather than guessing.
   */
  ligatures: boolean;
}

/**
 * Every fallback ends in `ui-monospace, monospace` so a stack whose first choice is
 * missing still renders monospaced rather than in the browser's default serif —
 * which is what a proportional terminal looks like, and reads as a broken app.
 */
const TAIL = "ui-monospace, monospace";

export const BUNDLED_FONTS: TerminalFont[] = [
  // Both `true` from the font files themselves rather than from their reputation:
  // a GSUB dump of the two woff2s we ship shows `calt` and no `liga` table in
  // either, which is also why the renderer tags `calt` and not only `liga`.
  {
    label: "JetBrains Mono",
    stack: `"JetBrains Mono Variable", "JetBrains Mono", ${TAIL}`,
    bundled: true,
    ligatures: true,
  },
  {
    label: "Fira Code",
    stack: `"Fira Code Variable", "Fira Code", ${TAIL}`,
    bundled: true,
    ligatures: true,
  },
];

/**
 * Monospace fonts common enough to be worth offering, in rough order of how likely
 * a developer is to have one. Zero bytes; availability is checked at render time.
 *
 * The `ligatures: false` entries are two different strengths of claim, and it is
 * worth knowing which is which. Three are read from the binaries macOS ships: SF
 * Mono carries no `liga`/`clig`/`dlig`/`calt` at all, and Menlo and Monaco have no
 * `GSUB` table whatsoever. The other five — Source Code Pro, Consolas, IBM Plex
 * Mono, DejaVu Sans Mono, Ubuntu Mono — rest on those families not shipping
 * programming ligatures, not on a dump of a file we had to hand. That weaker claim
 * is tolerable here and would not be for a `true`: a wrong `false` hides a control,
 * while a wrong `true` offers a switch that does nothing.
 */
export const SYSTEM_FONTS: TerminalFont[] = [
  { label: "SF Mono", stack: `"SF Mono", ${TAIL}`, bundled: false, ligatures: false },
  {
    label: "Source Code Pro",
    stack: `"Source Code Pro", ${TAIL}`,
    bundled: false,
    // The upstream family has no programming ligatures; the fork that added them
    // is Fira Code, which is its own entry above.
    ligatures: false,
  },
  { label: "Menlo", stack: `Menlo, ${TAIL}`, bundled: false, ligatures: false },
  { label: "Monaco", stack: `Monaco, ${TAIL}`, bundled: false, ligatures: false },
  { label: "Consolas", stack: `Consolas, ${TAIL}`, bundled: false, ligatures: false },
  {
    label: "Cascadia Code",
    stack: `"Cascadia Code", ${TAIL}`,
    bundled: false,
    // The one ligature-capable font on this list, and the only `true` here that
    // is not a face we bundle — so it got the same GSUB dump rather than a
    // reputation: `calt` present, `liga`/`clig`/`dlig` absent, which is what makes
    // the `calt`-only rule in `terminalHost.ts` actually reach it. Its `calt` does
    // also reach two width-changing lookups, but both are Arabic-only and
    // unreachable from ASCII — see the note on {@link TerminalFont.ligatures}.
    //
    // Microsoft ships the no-ligature cut under a *different* family name —
    // Cascadia Mono, confirmed as family `"Cascadia Mono"` against `"Cascadia
    // Code"` in the release binaries — so this flag does not have to guess which
    // build is installed. JetBrains does the same thing (`JetBrains Mono NL`).
    ligatures: true,
  },
  { label: "IBM Plex Mono", stack: `"IBM Plex Mono", ${TAIL}`, bundled: false, ligatures: false },
  {
    label: "DejaVu Sans Mono",
    stack: `"DejaVu Sans Mono", ${TAIL}`,
    bundled: false,
    ligatures: false,
  },
  { label: "Ubuntu Mono", stack: `"Ubuntu Mono", ${TAIL}`, bundled: false, ligatures: false },
];

/**
 * Whether the OS can actually render a font.
 *
 * `document.fonts.check` needs a full CSS font shorthand, and it answers for the
 * *first* family in the list — which is what we want, since the tail is the
 * fallback we are trying to detect our way past. It throws on a malformed
 * descriptor, so a font whose name needs quoting must arrive quoted.
 *
 * Returns `true` when the API is unavailable: an unknown answer must not hide a
 * font the user may well have.
 */
export function fontAvailable(family: string): boolean {
  if (typeof document === "undefined" || !document.fonts?.check) return true;
  try {
    return document.fonts.check(`12px ${family}`);
  } catch {
    return true;
  }
}

/** The first family of a stack, for an availability check. */
export function firstFamily(stack: string): string {
  return stack.split(",")[0]?.trim() ?? stack;
}

/**
 * The picker's options: every bundled font, plus the system fonts that are actually
 * installed.
 *
 * A bundled font is never filtered — it is in the binary, so a `check` returning
 * false would mean the stylesheet has not finished loading, not that the font is
 * missing, and hiding it would make the list flicker.
 */
export function availableFonts(): TerminalFont[] {
  return [
    ...BUNDLED_FONTS,
    ...SYSTEM_FONTS.filter((f) => fontAvailable(firstFamily(f.stack))),
  ];
}

/**
 * Whether the stored font stack draws ligatures — `null` when we cannot tell.
 *
 * Three answers, not two, and the third is the important one. A stack that
 * matches an offered font answers from that font's {@link TerminalFont.ligatures}
 * flag. A stack the user typed themselves matches nothing, and there is no way to
 * inspect it (see that flag's note), so it answers `null` — *unknown*, never
 * `false`.
 *
 * Callers must let unknown mean "show the control". Hiding it would strand every
 * user of a ligature font we happen not to list — Iosevka, Monaspace, Victor
 * Mono, a Nerd-Font patch — with a stored preference they can no longer reach,
 * and the setting they wanted silently unavailable. That is the same direction
 * `fontAvailable` takes for a missing API, and the same rule the dialog's
 * hardware gates state for a probe that has not answered.
 *
 * Matched on the **first family** rather than the whole stack, because a user who
 * appends a fallback to a listed font ("Fira Code", "Menlo", monospace) has still
 * chosen Fira Code, and {@link matchFont} would call that stack custom.
 *
 * So this deliberately disagrees with {@link matchFont}, which is what the font
 * picker uses to choose between a preset and "Custom…". A stored `Menlo, monospace`
 * reads as *custom* in the picker and as *Menlo* here, and both are right: they
 * answer different questions — which preset this is, versus which font will
 * actually draw. The disagreement stays safe in the hiding direction because the
 * options handed in are already filtered by {@link availableFonts}: a first family
 * that is not installed is not among them, so it answers `null` and shows the row,
 * rather than answering `false` for a font the browser was never going to reach.
 */
export function fontHasLigatures(
  stack: string,
  options: TerminalFont[] = availableFonts(),
): boolean | null {
  const want = firstFamily(stack).replace(/["']/g, "").toLowerCase();
  const hit = options.find(
    (f) => firstFamily(f.stack).replace(/["']/g, "").toLowerCase() === want,
  );
  return hit ? hit.ligatures : null;
}

/**
 * Whether the settings dialog should show the ligature row for a stored stack.
 *
 * The whole rule is the comparison: `!== false`, never `=== true`. Three answers
 * come back from {@link fontHasLigatures} and only one of them may hide the row —
 * a font we classify and know cannot draw them. *Unknown* has to show, or every
 * user of a ligature font we do not list (Iosevka, Monaspace, Victor Mono, a
 * Nerd-Font patch) loses the control, and with it any way to reach a preference
 * they may already have stored.
 *
 * A function of its own, and exported, so that comparison is pinned by a test.
 * Tightening it to `=== true` compiles clean, passes every other test, and breaks
 * exactly the case this setting was designed around — which is the kind of silent
 * inversion the rest of this file's tripwires exist to catch.
 */
export function showsLigatureRow(
  stack: string,
  options: TerminalFont[] = availableFonts(),
): boolean {
  return fontHasLigatures(stack, options) !== false;
}

/**
 * Which option a stored stack corresponds to, or `null` for a custom value.
 *
 * Compared on the *stored stack*, not the label, and normalised for whitespace only
 * — two stacks that differ in a fallback really are different values, and quietly
 * treating them as the same would make the picker show a font the terminal is not
 * using.
 */
export function matchFont(
  stack: string,
  options: TerminalFont[] = availableFonts(),
): TerminalFont | null {
  const norm = (s: string) => s.replace(/\s+/g, " ").trim();
  const want = norm(stack);
  return options.find((f) => norm(f.stack) === want) ?? null;
}
