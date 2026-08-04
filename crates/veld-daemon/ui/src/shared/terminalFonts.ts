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
}

/**
 * Every fallback ends in `ui-monospace, monospace` so a stack whose first choice is
 * missing still renders monospaced rather than in the browser's default serif —
 * which is what a proportional terminal looks like, and reads as a broken app.
 */
const TAIL = "ui-monospace, monospace";

export const BUNDLED_FONTS: TerminalFont[] = [
  {
    label: "JetBrains Mono",
    stack: `"JetBrains Mono Variable", "JetBrains Mono", ${TAIL}`,
    bundled: true,
  },
  {
    label: "Fira Code",
    stack: `"Fira Code Variable", "Fira Code", ${TAIL}`,
    bundled: true,
  },
];

/**
 * Monospace fonts common enough to be worth offering, in rough order of how likely
 * a developer is to have one. Zero bytes; availability is checked at render time.
 */
export const SYSTEM_FONTS: TerminalFont[] = [
  { label: "SF Mono", stack: `"SF Mono", ${TAIL}`, bundled: false },
  {
    label: "Source Code Pro",
    stack: `"Source Code Pro", ${TAIL}`,
    bundled: false,
  },
  { label: "Menlo", stack: `Menlo, ${TAIL}`, bundled: false },
  { label: "Monaco", stack: `Monaco, ${TAIL}`, bundled: false },
  { label: "Consolas", stack: `Consolas, ${TAIL}`, bundled: false },
  { label: "Cascadia Code", stack: `"Cascadia Code", ${TAIL}`, bundled: false },
  { label: "IBM Plex Mono", stack: `"IBM Plex Mono", ${TAIL}`, bundled: false },
  { label: "DejaVu Sans Mono", stack: `"DejaVu Sans Mono", ${TAIL}`, bundled: false },
  { label: "Ubuntu Mono", stack: `"Ubuntu Mono", ${TAIL}`, bundled: false },
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
