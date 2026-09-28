/**
 * The pixel face of a worktree marker: a 3×3 pattern in the worktree's colour.
 *
 * **Derived, not stored.** A pattern is the glyph's position in the daemon's emoji
 * allowlist, drawn as pixels — so there is no third column, nothing to backfill,
 * and switching between Colour, Emoji and Pixels stays lossless the way switching
 * between the first two always was. It also inherits the glyph's distinctness for
 * free: the assigner already keeps glyphs unique within a repo, and this mapping is
 * one-to-one, so two checkouts of one repo share a pattern exactly when they share
 * a glyph.
 *
 * **Mirrored left to right**, the trick GitHub's default avatars use. The middle
 * column is its own mirror, so only the left and middle columns are chosen — six
 * cells, 2⁶ = 64 patterns, the same count as the 64 animals. Symmetry is what makes
 * a pattern read as a *shape* (a plus, a cup, a T) rather than as noise, and a shape
 * is what the eye keeps when scanning a collapsed rail.
 *
 * The four mirrored patterns with one lit cell or none are too faint to be told
 * apart, so those four slots hold asymmetric ones instead (the two diagonals and
 * two corner brackets).
 */

/**
 * One 9-bit mask per slot, cell `(row, col)` at bit `row * 3 + col`.
 *
 * **Append only, never reorder or replace** — the same rule `WORKTREE_EMOJI` lives
 * by on the Rust side, for the same reason one step removed: slot *i* is what glyph
 * *i* looks like as pixels, and a user who has learned that their checkout is the
 * cup would find it repainted. Written out as literals rather than generated so that
 * a tidier generator cannot quietly reshuffle every rail.
 */
export const PIXEL_PATTERNS: readonly number[] = [
  0b100010001, 0b000000101, 0b000101000, 0b000101101, 0b101000000, 0b101000101, 0b101101000, 0b101101101,
  0b001010100, 0b000000111, 0b000101010, 0b000101111, 0b101000010, 0b101000111, 0b101101010, 0b101101111,
  0b001001111, 0b000010101, 0b000111000, 0b000111101, 0b101010000, 0b101010101, 0b101111000, 0b101111101,
  0b000010010, 0b000010111, 0b000111010, 0b000111111, 0b101010010, 0b101010111, 0b101111010, 0b101111111,
  0b100100111, 0b010000101, 0b010101000, 0b010101101, 0b111000000, 0b111000101, 0b111101000, 0b111101101,
  0b010000010, 0b010000111, 0b010101010, 0b010101111, 0b111000010, 0b111000111, 0b111101010, 0b111101111,
  0b010010000, 0b010010101, 0b010111000, 0b010111101, 0b111010000, 0b111010101, 0b111111000, 0b111111101,
  0b010010010, 0b010010111, 0b010111010, 0b010111111, 0b111010010, 0b111010111, 0b111111010, 0b111111111,
];

/** How many cells a mask lights. */
export function litCells(mask: number): number {
  let n = 0;
  for (let i = 0; i < 9; i++) if ((mask >> i) & 1) n++;
  return n;
}

/**
 * Every mask with at least three lit cells, for glyphs past the table's end.
 *
 * Three rather than two because nothing curated these: a fallback pattern should
 * be unmistakably a pattern.
 */
const FALLBACK_POOL: readonly number[] = Array.from({ length: 512 }, (_, m) => m).filter(
  (m) => litCells(m) >= 3,
);

/**
 * The pattern for a glyph, given the daemon's allowlist in its own order.
 *
 * A glyph inside the table gets its slot. One past the end — the allowlist grew and
 * this table has not yet — or one the list does not hold at all gets a stable hash
 * into every reasonably-lit 3×3 mask. That case gives up the within-repo uniqueness
 * guarantee, which is why the table is what should grow alongside the list.
 */
export function pixelPattern(emoji: string, order: readonly string[]): number {
  const index = order.indexOf(emoji);
  if (index >= 0 && index < PIXEL_PATTERNS.length) return PIXEL_PATTERNS[index];
  let h = 0;
  for (const ch of emoji) h = (h * 31 + (ch.codePointAt(0) ?? 0)) >>> 0;
  return FALLBACK_POOL[h % FALLBACK_POOL.length];
}

/** Whether cell `(row, col)` of a mask is lit. */
export function cellLit(mask: number, row: number, col: number): boolean {
  return ((mask >> (row * 3 + col)) & 1) === 1;
}

/**
 * Whether a colour is bright enough that its own faint tint vanishes on a light
 * panel.
 *
 * An unlit cell is the colour at low strength, which reads well for the deep hues
 * and not at all for the pale ones: 22% of `#fff827` on a white panel is a cream
 * barely distinguishable from the lit yellow beside it, so the pattern — the one
 * thing this face adds — disappears. Those colours get a darker shade for their
 * unlit cells instead (`styles.css`, `.wt-pixels.bright`). Relative luminance per
 * WCAG, with the cut at 0.45, which splits the palette into its four pale members
 * (yellow, cyan, green, orange) and its four deep ones.
 *
 * Anything not `#rrggbb` (the picker's "no colour yet" fallback) is not bright.
 */
export function isBrightColor(color: string): boolean {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/iu.exec(color);
  if (m === null) return false;
  const [r, g, b] = [m[1], m[2], m[3]].map((h) => {
    const c = Number.parseInt(h, 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.45;
}
