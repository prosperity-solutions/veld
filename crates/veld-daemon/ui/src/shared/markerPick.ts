/**
 * Choosing a marker for a worktree that does not exist yet.
 *
 * The create dialog opens with a colour and a glyph already selected, because an
 * empty picker asks a question nobody wants asked — the daemon has always been able
 * to assign one, so the dialog's job is to *show* what it would get and let it be
 * changed, not to make it a decision.
 *
 * **Random among the free ones, not the first free one.** The first version took the
 * first unused entry, which is a sequence rather than a choice: with one checkout
 * holding blue, every new worktree in that repo proposed the same cyan, and a rail
 * built over a week looked like it had been dealt from the top of the deck. Randomness
 * is also what the daemon effectively does — it hashes a seed into the list and probes
 * forward from there — so this matches the character of an assigned marker without
 * copying a hash whose input (a path that does not exist yet) the dialog does not
 * have.
 *
 * The two faces are drawn independently, for the same reason the daemon offsets its
 * two seeds: if one index chose both, re-picking a colour would silently imply a
 * glyph, and a user who changed one would find the other had moved.
 *
 * Pure, with the randomness injected, so `vitest` (`environment: "node"`) can pin the
 * selection rule rather than only its type.
 */

/** A holder of a marker face, as `/api/repos` reports it. */
interface Holder {
  id: number;
  alias: string;
}

/** `[0, 1)`, injectable so a test can choose the draw. */
export type Random = () => number;

/**
 * A random entry of `options` that no sibling holds, or a random entry outright when
 * the repo has already used them all.
 *
 * Falling back to a duplicate rather than to nothing matches the daemon: a repo with
 * more checkouts than colours still gets a marker, and the picker marks the duplicate
 * so the ambiguity is visible before it is created.
 */
export function randomFree(
  options: readonly string[],
  usedBy: Record<string, Holder[]>,
  random: Random = Math.random,
): string {
  if (options.length === 0) return "";
  const free = options.filter((o) => (usedBy[o] ?? []).length === 0);
  const pool = free.length > 0 ? free : options;
  // `Math.min` guards the one input that would index past the end: `random()` is
  // documented as `< 1`, but a caller's stub need not be.
  return pool[Math.min(pool.length - 1, Math.floor(random() * pool.length))];
}

/**
 * The marker a new worktree opens with: a free colour and a free glyph, drawn
 * independently.
 *
 * Returns empty strings while the lists are still loading, which reads as "nothing
 * picked" and leaves the daemon's assignment in charge — the right answer for the
 * frame before the fetch resolves.
 *
 * Call this **once per dialog**, not per render: it is random, so a caller that
 * recomputed it on every render would reshuffle the selection under the user's cursor.
 */
export function randomMarker(
  choices: string[] | null,
  colors: string[] | null,
  usedBy: Record<string, Holder[]>,
  colorUsedBy: Record<string, Holder[]>,
  random: Random = Math.random,
): { emoji: string; color: string } {
  return {
    emoji: choices ? randomFree(choices, usedBy, random) : "",
    color: colors ? randomFree(colors, colorUsedBy, random) : "",
  };
}
